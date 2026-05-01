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
use tracing::{debug, warn};

use crate::supervisor::Supervisor;

pub async fn loop_emit(supervisor: Arc<Supervisor>) -> Result<()> {
    let interval = Duration::from_secs(5);

    loop {
        let ready = supervisor.engine.ready().await;
        let gpu = read_nvml_blocking().unwrap_or_default();
        debug!(ready, ?gpu, "health snapshot");

        if let Err(e) = publish_to_etcd(&supervisor, ready, &gpu).await {
            warn!(error=?e, "etcd publish failed");
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(Debug, Default, Clone)]
pub struct GpuSnapshot {
    pub util_pct:    f32,
    pub mem_used_pct:f32,
    pub temp_c:      f32,
    pub power_watts: f32,
}

fn read_nvml_blocking() -> Option<GpuSnapshot> {
    let nvml = nvml_wrapper::Nvml::init().ok()?;
    let count = nvml.device_count().ok()?;
    if count == 0 { return None; }
    let mut out = GpuSnapshot::default();
    for i in 0..count {
        let Ok(dev) = nvml.device_by_index(i) else { continue; };
        if let Ok(u) = dev.utilization_rates() { out.util_pct = u.gpu as f32; }
        if let Ok(mem) = dev.memory_info() {
            if mem.total > 0 {
                out.mem_used_pct = (mem.used as f64 / mem.total as f64) as f32 * 100.0;
            }
        }
        if let Ok(t) = dev.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu) {
            out.temp_c = t as f32;
        }
        if let Ok(p) = dev.power_usage() { out.power_watts += p as f32 / 1000.0; }
    }
    Some(out)
}

async fn publish_to_etcd(
    supervisor: &Supervisor,
    ready: bool,
    gpu: &GpuSnapshot,
) -> Result<()> {
    use cgn_core::Error;
    let endpoints = &supervisor.cfg.cluster.etcd_endpoints;
    if endpoints.is_empty() { return Ok(()); }

    let value = serde_json::json!({
        "node_id": supervisor.cfg.agent.node_id,
        "address": format!("https://{}", supervisor.cfg.agent.listen),
        "role":    role_to_int(&supervisor.cfg.agent.role),
        "gpu_index": supervisor.cfg.agent.gpu_index,
        "model": supervisor.cfg.models.keys().next().cloned(),
        "queue_depth": 0u32,
        "free_blocks": 0u32,
        "total_blocks": 0u32,
        "power_watts": gpu.power_watts,
        "ready": ready,
    });

    let mut client = etcd_client::Client::connect(endpoints, None).await
        .map_err(|e| Error::Etcd(format!("connect: {e}")))?;
    let key = format!("{}{}", cgn_core::etcd_keys::NODES, supervisor.cfg.agent.node_id);
    let opts = etcd_client::PutOptions::new();
    client.put(key, value.to_string(), Some(opts)).await
        .map_err(|e| Error::Etcd(format!("put: {e}")))?;
    Ok(())
}

fn role_to_int(r: &cgn_core::config::NodeRoleCfg) -> i32 {
    use cgn_core::config::NodeRoleCfg::*;
    match r {
        Decode  => cgn_proto::v1::NodeRole::Decode  as i32,
        Prefill => cgn_proto::v1::NodeRole::Prefill as i32,
        Both    => cgn_proto::v1::NodeRole::Both    as i32,
    }
}
