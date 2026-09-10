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
    let endpoints = supervisor.cfg.cluster.etcd_endpoints.clone();
    if endpoints.is_empty() {
        info!("no etcd endpoints configured; running in single-node mode");
        loop {
            let ready = supervisor.engine.ready().await;
            let _gpu = read_nvml_blocking().unwrap_or_default();
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

    loop {
        let ready = supervisor.engine.ready().await;
        let gpu = read_nvml_blocking().unwrap_or_default();
        let engine_stats =
            crate::telemetry::scrape(supervisor.engine.name(), &supervisor.engine_cfg.url)
                .await
                .unwrap_or_default();
        debug!(ready, ?gpu, ?engine_stats, "health snapshot");

        if let Err(e) = publish_one(
            &mut client,
            supervisor,
            lease_id,
            ready,
            &gpu,
            &engine_stats,
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
}

pub(crate) fn read_nvml_blocking() -> Option<GpuSnapshot> {
    let nvml = nvml_wrapper::Nvml::init().ok()?;
    let count = nvml.device_count().ok()?;
    if count == 0 {
        return None;
    }
    let mut out = GpuSnapshot::default();
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else {
            continue;
        };
        if let Ok(u) = dev.utilization_rates() {
            out.util_pct = u.gpu as f32;
        }
        if let Ok(mem) = dev.memory_info() {
            if mem.total > 0 {
                out.mem_used_pct = (mem.used as f64 / mem.total as f64) as f32 * 100.0;
            }
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

/// Write the node-health entry under the lease, so it disappears
/// automatically if the agent dies or partitions away.
async fn publish_one(
    client: &mut etcd_client::Client,
    supervisor: &Supervisor,
    lease_id: i64,
    ready: bool,
    gpu: &GpuSnapshot,
    engine_stats: &crate::telemetry::EngineStats,
) -> Result<()> {
    use cgn_core::Error;
    let scheme = if supervisor.cfg.security.require_mtls {
        "https"
    } else {
        "http"
    };
    let value = serde_json::json!({
        "node_id": supervisor.cfg.agent.node_id,
        "address": format!("{scheme}://{}", supervisor.cfg.agent.listen),
        "role":    role_to_int(&supervisor.cfg.agent.role),
        "gpu_index": supervisor.cfg.agent.gpu_index,
        "model": supervisor.cfg.models.keys().next().cloned(),
        // Real engine telemetry (vLLM / SGLang /metrics). Engines without
        // a Prometheus endpoint report zeros — the router treats
        // total_blocks == 0 as "capacity unknown".
        "queue_depth": engine_stats.queue_depth,
        "free_blocks": engine_stats.free_blocks,
        "total_blocks": engine_stats.total_blocks,
        "power_watts": gpu.power_watts,
        "ready": ready,
        "servable": true,
        "version": env!("CARGO_PKG_VERSION"),
    });
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
/// `servable = false`, so the router never targets them — only the
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
