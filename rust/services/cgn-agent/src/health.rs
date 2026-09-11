//! Node health emitter.
//!
//! Periodically (every 5 s by default) snapshots:
//!
//! * Engine readiness (`engine.ready()`).
//! * NVML telemetry: per-GPU util, memory, temperature, power draw.
//! * Loaded models, queue depth.
//!
//! …and writes a single `NodeHealth` JSON value to etcd at
//! `/cognitora/nodes/<node_id>` so the router watcher picks it up.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use tracing::{debug, info, warn};

use crate::supervisor::Supervisor;

/// Heartbeat interval; the etcd lease TTL is 3× this so two missed
/// heartbeats still leave the entry visible.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

pub async fn loop_emit(supervisor: Arc<Supervisor>) -> Result<()> {
    if supervisor.cfg.cluster.gossip_enabled() {
        return loop_emit_gossip(supervisor).await;
    }
    let endpoints = supervisor.cfg.cluster.etcd_endpoints.clone();
    if endpoints.is_empty() {
        info!("no etcd endpoints configured; running in single-node mode");
        loop {
            let ready = supervisor.engine.ready().await;
            let _gpu = read_gpu_blocking().unwrap_or_default();
            debug!(ready, "single-node health snapshot");
            tokio::time::sleep(HEARTBEAT_INTERVAL).await;
        }
    }

    // Outer loop reconnects on lease loss / etcd hiccups.
    loop {
        match emit_with_lease(&supervisor, &endpoints).await {
            Ok(()) => {
                warn!("etcd publisher exited cleanly; restarting");
            }
            Err(e) => {
                warn!(error=?e, "etcd publisher died; reconnecting in 5s");
            }
        }
        tokio::time::sleep(HEARTBEAT_INTERVAL).await;
    }
}

/// Gossip-mode publisher (`[cluster].state_backend = "gossip"`): join
/// the gossip cluster once, then refresh our node record every
/// heartbeat. There is no lease; peers detect our death through
/// phi-accrual failure detection on the gossip heartbeat, which is the
/// etcd-lease equivalent for this backend.
///
/// Gossip-mode limitations (see `docs/architecture/gossip.md`):
/// confirmed-KV claims and pipeline-worker visibility entries are
/// etcd-only, so they are skipped here.
async fn loop_emit_gossip(supervisor: Arc<Supervisor>) -> Result<()> {
    use cgn_core::Error;
    let cluster = &supervisor.cfg.cluster;
    let listen = cluster
        .gossip_listen
        .parse()
        .map_err(|e| Error::Config(format!("cluster.gossip_listen: {e}")))?;
    let advertise = cluster
        .gossip_advertise_or_listen()
        .parse()
        .map_err(|e| Error::Config(format!("cluster.gossip_advertise: {e}")))?;
    let member = cgn_gossip::GossipMember::spawn(cgn_gossip::GossipConfig {
        cluster_name: cluster.name.clone(),
        node_id: supervisor.cfg.agent.node_id.clone(),
        listen,
        advertise,
        seeds: cluster.gossip_seeds.clone(),
    })
    .await
    .map_err(|e| Error::Gossip(format!("spawn: {e}")))?;
    info!(%listen, %advertise, seeds = ?cluster.gossip_seeds, "gossip member joined");

    let mut kv_cache_state = crate::kv_cache_state::KvCacheState::default();
    loop {
        let ready = supervisor.engine.ready().await;
        let gpu = read_gpu_blocking().unwrap_or_default();
        let engine_stats =
            crate::telemetry::scrape(supervisor.engine.name(), &supervisor.engine_cfg.url)
                .await
                .unwrap_or_default();
        let kv_epoch = kv_cache_state.observe(ready, &engine_stats);
        debug!(ready, ?gpu, ?engine_stats, kv_epoch, "health snapshot (gossip)");
        let record = node_record_json(&supervisor, ready, &gpu, &engine_stats, kv_epoch);
        member.publish_node_record(&record.to_string()).await;
        tokio::time::sleep(HEARTBEAT_INTERVAL).await;
    }
}

/// Acquire a TTL lease, write the node entry against it, and keep it
/// alive until the connection breaks.
async fn emit_with_lease(supervisor: &Supervisor, endpoints: &[String]) -> Result<()> {
    use cgn_core::Error;
    let mut client = etcd_client::Client::connect(endpoints, None)
        .await
        .map_err(|e| Error::Etcd(format!("connect: {e}")))?;

    // Lease lives 3× heartbeat. KeepAlive ticks every heartbeat.
    let lease_ttl = (HEARTBEAT_INTERVAL.as_secs() as i64) * 3;
    let lease = client
        .lease_grant(lease_ttl, None)
        .await
        .map_err(|e| Error::Etcd(format!("lease_grant: {e}")))?;
    let lease_id = lease.id();

    let (mut keeper, mut stream) = client
        .lease_keep_alive(lease_id)
        .await
        .map_err(|e| Error::Etcd(format!("lease_keep_alive: {e}")))?;
    info!(%lease_id, ttl = lease_ttl, "etcd lease acquired");

    // Confirmed-KV keys this session has published, oldest first. Used
    // to evict our own claims when the engine reports cache pressure.
    let mut published_kv: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut kv_cache_state = crate::kv_cache_state::KvCacheState::default();

    loop {
        let ready = supervisor.engine.ready().await;
        let gpu = read_gpu_blocking().unwrap_or_default();
        let engine_stats =
            crate::telemetry::scrape(supervisor.engine.name(), &supervisor.engine_cfg.url)
                .await
                .unwrap_or_default();
        let kv_epoch = kv_cache_state.observe(ready, &engine_stats);
        debug!(ready, ?gpu, ?engine_stats, kv_epoch, "health snapshot");

        if let Err(e) = publish_one(
            &mut client,
            supervisor,
            lease_id,
            ready,
            &gpu,
            &engine_stats,
            kv_epoch,
        )
        .await
        {
            warn!(error=?e, "publish failed; will retry");
        }

        if let Err(e) = publish_kv_confirmed(
            &mut client,
            supervisor,
            lease_id,
            &engine_stats,
            &mut published_kv,
        )
        .await
        {
            warn!(error=?e, "kv-confirmed publish failed; will retry");
        }

        // Renew the lease.
        keeper
            .keep_alive()
            .await
            .map_err(|e| Error::Etcd(format!("lease keep_alive: {e}")))?;
        // Drain any pending response so the server-side stream stays healthy.
        match tokio::time::timeout(Duration::from_millis(100), stream.message()).await {
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(e)) => return Err(Error::Etcd(format!("keep_alive recv: {e}"))),
            Err(_) => {}
        }

        tokio::time::sleep(HEARTBEAT_INTERVAL).await;
    }
}

/// Cap on lease-bound confirmed-KV keys per node. Beyond this the oldest
/// claims are deleted — they're also the ones the engine's LRU evicts
/// first, so the index converges toward what's actually cached.
const KV_CONFIRMED_MAX_KEYS: usize = 4096;

/// Publish completion-confirmed prefix digests (queued by the gRPC
/// Generate handler) as lease-bound etcd keys
/// `<KV_CONFIRMED>{node_id}/{digest_hex}`. The router watcher mirrors
/// PUT/DELETE into its `PrefixIndex`, turning the KV-overlap score from
/// an optimistic guess into a truth-fed signal:
///
/// * confirmed only after the engine finished the generation,
/// * dies with the node (heartbeat lease),
/// * actively deleted under cache pressure (engine is LRU-evicting, so
///   our oldest claims are the ones most likely gone).
async fn publish_kv_confirmed(
    client: &mut etcd_client::Client,
    supervisor: &Supervisor,
    lease_id: i64,
    engine_stats: &crate::telemetry::EngineStats,
    published: &mut std::collections::VecDeque<String>,
) -> Result<()> {
    use cgn_core::Error;
    let node_id = &supervisor.cfg.agent.node_id;
    let pending: Vec<Vec<u8>> = std::mem::take(&mut *supervisor.kv_confirmed.lock());

    for digest in pending {
        if digest.len() != 32 {
            continue;
        }
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let key = format!("{}{}/{}", cgn_core::etcd_keys::KV_CONFIRMED, node_id, hex);
        // Re-confirmed digests (shared prefixes across requests) must not
        // be queued twice: a duplicate deque entry would let the eviction
        // pass below delete a key that is still tracked by its newer
        // twin, silently dropping a live claim and inflating the deque
        // count against the cap.
        if published.contains(&key) {
            continue;
        }
        client
            .put(
                key.as_str(),
                "1",
                Some(etcd_client::PutOptions::new().with_lease(lease_id)),
            )
            .await
            .map_err(|e| Error::Etcd(format!("kv put: {e}")))?;
        published.push_back(key);
    }

    // Evict our own stale claims: on hard cap overflow, and under KV
    // pressure (<5% free blocks) drop the oldest half.
    let under_pressure = engine_stats.total_blocks > 0
        && (engine_stats.free_blocks as f32 / engine_stats.total_blocks as f32) < 0.05;
    let target = if under_pressure {
        published.len() / 2
    } else {
        KV_CONFIRMED_MAX_KEYS
    };
    while published.len() > target {
        let Some(key) = published.pop_front() else {
            break;
        };
        client
            .delete(key.as_str(), None)
            .await
            .map_err(|e| Error::Etcd(format!("kv delete: {e}")))?;
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
pub(crate) struct GpuSnapshot {
    pub util_pct: f32,
    pub mem_used_pct: f32,
    pub temp_c: f32,
    pub power_watts: f32,
    /// Marketing name of the first GPU (`"NVIDIA H100 80GB HBM3"`,
    /// `"AMD Instinct MI300X"`). Empty when unknown.
    pub gpu_name: String,
    /// `"nvidia"` / `"amd"` / `""` (unknown).
    pub gpu_vendor: String,
    /// Total GPU memory summed across devices, MiB. 0 when unknown.
    pub vram_total_mb: u64,
}

/// Vendor-neutral GPU snapshot: NVML first (NVIDIA), then `rocm-smi`
/// (AMD ROCm). Hosts with neither return `None`.
pub(crate) fn read_gpu_blocking() -> Option<GpuSnapshot> {
    read_nvml_blocking().or_else(read_rocm_blocking)
}

/// NVML handle initialised once per process. Re-initialising the NVML
/// library on every 5 s heartbeat (and every gRPC `Health` call) is
/// needlessly expensive; hosts without NVML simply cache the `None`.
static NVML: std::sync::OnceLock<Option<nvml_wrapper::Nvml>> = std::sync::OnceLock::new();

pub(crate) fn read_nvml_blocking() -> Option<GpuSnapshot> {
    let nvml = NVML
        .get_or_init(|| nvml_wrapper::Nvml::init().ok())
        .as_ref()?;
    let count = nvml.device_count().ok()?;
    if count == 0 {
        return None;
    }
    let mut out = GpuSnapshot {
        gpu_vendor: "nvidia".into(),
        ..Default::default()
    };
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else {
            continue;
        };
        if out.gpu_name.is_empty() {
            if let Ok(name) = dev.name() {
                out.gpu_name = name;
            }
        }
        if let Ok(u) = dev.utilization_rates() {
            out.util_pct = u.gpu as f32;
        }
        if let Ok(mem) = dev.memory_info() {
            if mem.total > 0 {
                out.mem_used_pct = (mem.used as f64 / mem.total as f64) as f32 * 100.0;
            }
            out.vram_total_mb += mem.total / (1024 * 1024);
        }
        if let Ok(t) = dev.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
        {
            out.temp_c = t as f32;
        }
        if let Ok(p) = dev.power_usage() {
            out.power_watts += p as f32 / 1000.0;
        }
    }
    Some(out)
}

/// Whether `rocm-smi` exists on this host, probed once per process.
static ROCM_SMI: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// AMD fallback: shell out to `rocm-smi --json` and parse the per-card
/// object. Field names vary across ROCm releases, so matching is by
/// tolerant substring rather than exact key.
pub(crate) fn read_rocm_blocking() -> Option<GpuSnapshot> {
    let available = *ROCM_SMI.get_or_init(|| {
        std::process::Command::new("rocm-smi")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    });
    if !available {
        return None;
    }
    let out = std::process::Command::new("rocm-smi")
        .args([
            "--showpower",
            "--showuse",
            "--showtemp",
            "--showmeminfo",
            "vram",
            "--showproductname",
            "--json",
        ])
        .output()
        .ok()?;
    parse_rocm_smi(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `rocm-smi --json` output. Public within the crate for tests.
pub(crate) fn parse_rocm_smi(json: &str) -> Option<GpuSnapshot> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let obj = v.as_object()?;
    let num = |val: &serde_json::Value| -> Option<f64> {
        match val {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    };
    let mut out = GpuSnapshot {
        gpu_vendor: "amd".into(),
        ..Default::default()
    };
    let mut vram_total_bytes = 0u64;
    let mut saw_card = false;
    for (card, fields) in obj {
        if !card.starts_with("card") {
            continue; // "system" block etc.
        }
        let Some(fields) = fields.as_object() else {
            continue;
        };
        saw_card = true;
        for (k, val) in fields {
            let kl = k.to_ascii_lowercase();
            if kl.contains("power") && kl.contains("(w)") {
                if let Some(w) = num(val) {
                    out.power_watts += w as f32;
                }
            } else if kl.contains("gpu use") {
                if let Some(u) = num(val) {
                    out.util_pct = u as f32;
                }
            } else if kl.contains("temperature") && kl.contains("edge") {
                if let Some(t) = num(val) {
                    out.temp_c = t as f32;
                }
            } else if kl.contains("vram total memory") {
                if let Some(b) = num(val) {
                    vram_total_bytes += b as u64;
                }
            } else if (kl.contains("card series") || kl.contains("card model"))
                && out.gpu_name.is_empty()
            {
                if let Some(name) = val.as_str() {
                    if !name.trim().is_empty() && !name.contains("0x") {
                        out.gpu_name = name.trim().to_string();
                    }
                }
            }
        }
    }
    if !saw_card {
        return None;
    }
    out.vram_total_mb = vram_total_bytes / (1024 * 1024);
    Some(out)
}

/// Build the node record published to the cluster: the single source
/// of truth for both backends (etcd `/cognitora/nodes/<id>` value and
/// the gossip `cgn.node` key). The router deserializes it into
/// `NodeEntry`.
fn node_record_json(
    supervisor: &Supervisor,
    ready: bool,
    gpu: &GpuSnapshot,
    engine_stats: &crate::telemetry::EngineStats,
    kv_epoch: u64,
) -> serde_json::Value {
    let scheme = if supervisor.cfg.security.require_mtls {
        "https"
    } else {
        "http"
    };
    serde_json::json!({
        "node_id": supervisor.cfg.agent.node_id,
        "address": format!("{scheme}://{}", supervisor.cfg.agent.listen),
        "role":    role_to_int(&supervisor.cfg.agent.role),
        "gpu_index": supervisor.cfg.agent.gpu_index,
        "model": supervisor.cfg.models.keys().next().cloned(),
        // Real engine telemetry (vLLM / SGLang /metrics). Engines without
        // a Prometheus endpoint report zeros; the router treats
        // total_blocks == 0 as "capacity unknown".
        "queue_depth": engine_stats.queue_depth,
        "free_blocks": engine_stats.free_blocks,
        "total_blocks": engine_stats.total_blocks,
        "power_watts": gpu.power_watts,
        "watt_limit": supervisor.cfg.agent.watt_limit,
        "gpu_name": gpu.gpu_name,
        "gpu_vendor": gpu.gpu_vendor,
        "vram_total_mb": gpu.vram_total_mb,
        "kv_epoch": kv_epoch,
        "ready": ready,
        "servable": true,
        "version": env!("CARGO_PKG_VERSION"),
    })
}

/// Write the node-health entry under the lease, so it disappears
/// automatically if the agent dies or partitions away.
async fn publish_one(
    client: &mut etcd_client::Client,
    supervisor: &Supervisor,
    lease_id: i64,
    ready: bool,
    gpu: &GpuSnapshot,
    engine_stats: &crate::telemetry::EngineStats,
    kv_epoch: u64,
) -> Result<()> {
    use cgn_core::Error;
    let value = node_record_json(supervisor, ready, gpu, engine_stats, kv_epoch);
    let key = format!(
        "{}{}",
        cgn_core::etcd_keys::NODES,
        supervisor.cfg.agent.node_id
    );
    let opts = etcd_client::PutOptions::new().with_lease(lease_id);
    client
        .put(key, value.to_string(), Some(opts))
        .await
        .map_err(|e| Error::Etcd(format!("put: {e}")))?;

    publish_pipeline_workers(client, supervisor, lease_id, ready).await
}

/// Register locally spawned cgn-infer pipeline workers as
/// **non-servable** nodes: they appear in cluster state (for
/// visibility and whole-pipeline health) but carry no `model` and
/// `servable = false`, so the router never targets them; only the
/// coordinator (the main node entry above) receives traffic.
async fn publish_pipeline_workers(
    client: &mut etcd_client::Client,
    supervisor: &Supervisor,
    lease_id: i64,
    ready: bool,
) -> Result<()> {
    use cgn_core::Error;
    let Some((model_name, model)) = supervisor.cfg.models.iter().next() else {
        return Ok(());
    };
    let Some(pipeline) = &model.pipeline else {
        return Ok(());
    };
    for (i, w) in pipeline.workers.iter().enumerate().filter(|(_, w)| w.spawn) {
        let worker_id = format!("{}-pipeline-worker-{i}", supervisor.cfg.agent.node_id);
        let value = serde_json::json!({
            "node_id": worker_id,
            "address": format!("http://{}", w.listen),
            "role": "pipeline_worker",
            "model": serde_json::Value::Null,
            "pipeline_model": model_name,
            "layers": w.layers,
            "ready": ready,
            "servable": false,
            "version": env!("CARGO_PKG_VERSION"),
        });
        let key = format!("{}{}", cgn_core::etcd_keys::NODES, worker_id);
        let opts = etcd_client::PutOptions::new().with_lease(lease_id);
        client
            .put(key, value.to_string(), Some(opts))
            .await
            .map_err(|e| Error::Etcd(format!("put worker: {e}")))?;
    }
    Ok(())
}

pub(crate) fn role_to_int(r: &cgn_core::config::NodeRoleCfg) -> i32 {
    use cgn_core::config::NodeRoleCfg::*;
    match r {
        Decode => cgn_proto::v1::NodeRole::Decode as i32,
        Prefill => cgn_proto::v1::NodeRole::Prefill as i32,
        Both => cgn_proto::v1::NodeRole::Both as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rocm_smi_snapshot() {
        let json = r#"{
            "card0": {
                "Average Graphics Package Power (W)": "203.0",
                "GPU use (%)": "87",
                "Temperature (Sensor edge) (C)": "62.0",
                "VRAM Total Memory (B)": "206158430208",
                "Card series": "AMD Instinct MI300X"
            },
            "card1": {
                "Average Graphics Package Power (W)": "121.5",
                "VRAM Total Memory (B)": "206158430208"
            },
            "system": {"Driver version": "6.3.6"}
        }"#;
        let s = parse_rocm_smi(json).expect("parses");
        assert_eq!(s.gpu_vendor, "amd");
        assert_eq!(s.gpu_name, "AMD Instinct MI300X");
        assert!((s.power_watts - 324.5).abs() < 0.01);
        assert!((s.util_pct - 87.0).abs() < 0.01);
        assert!((s.temp_c - 62.0).abs() < 0.01);
        assert_eq!(s.vram_total_mb, 2 * 196_608); // 2 × 192 GiB in MiB
    }

    #[test]
    fn rocm_parse_rejects_no_cards() {
        assert!(parse_rocm_smi(r#"{"system": {}}"#).is_none());
        assert!(parse_rocm_smi("").is_none());
        assert!(parse_rocm_smi("nope").is_none());
    }
}
