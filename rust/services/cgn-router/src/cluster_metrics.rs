//! Cluster-state exposition: mirrors the in-memory `NodeRegistry` into
//! labeled Prometheus gauges on the router's `/metrics` endpoint.
//!
//! This is deliberately the *only* wire format for cluster monitoring —
//! no separate JSON API. One exposition surface serves Prometheus,
//! Grafana, and the standalone browser dashboard (`dashboard/`), and
//! because `cgn-metrics` federates every node's `/metrics`, a dashboard
//! pointed at `cgn-metrics` sees the whole fleet through the same
//! series.
//!
//! All metrics register through the `cgn_telemetry` convenience macros
//! (`gauge!`, `gauge_vec!`, `float_gauge_vec!`) so registration lives in
//! one place workspace-wide.
//!
//! Series (all gauges, refreshed every [`REFRESH_INTERVAL`]):
//!
//! | Metric                             | Labels                          |
//! |------------------------------------|---------------------------------|
//! | `cgn_cluster_node_up`              | `node`                          |
//! | `cgn_cluster_node_cordoned`        | `node`                          |
//! | `cgn_cluster_node_queue_depth`     | `node`                          |
//! | `cgn_cluster_node_power_watts`     | `node`                          |
//! | `cgn_cluster_node_watt_limit`      | `node`                          |
//! | `cgn_cluster_node_kv_free_blocks`  | `node`                          |
//! | `cgn_cluster_node_kv_total_blocks` | `node`                          |
//! | `cgn_cluster_node_vram_total_mb`   | `node`                          |
//! | `cgn_cluster_node_info`            | `node`, `address`, `model`, `role`, `gpu`, `gpu_vendor` (always 1) |
//! | `cgn_cluster_nodes_total`          | —                               |
//! | `cgn_cluster_power_watts_total`    | —                               |
//! | `cgn_cluster_tokens_per_watt`      | —                               |
//! | `cgn_router_prefix_index_digests`  | —                               |
//!
//! The vectors are `reset()` before each refresh so series for departed
//! nodes disappear instead of lingering at their last value.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use cgn_proto::v1::NodeRole;
use cgn_telemetry::prometheus::{Gauge, GaugeVec, IntGauge, IntGaugeVec};
use cgn_telemetry::{float_gauge_vec, gauge, gauge_vec};
use parking_lot::Mutex;

use crate::state::SharedState;

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

static NODE_UP: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_cluster_node_up",
        "1 when the node's engine reports ready, 0 otherwise.",
        &["node"]
    )
});

static NODE_CORDONED: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_cluster_node_cordoned",
        "1 when the node is cordoned (excluded from routing).",
        &["node"]
    )
});

static NODE_QUEUE_DEPTH: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_cluster_node_queue_depth",
        "Engine queue depth (waiting + running requests) per node.",
        &["node"]
    )
});

static NODE_KV_FREE: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_cluster_node_kv_free_blocks",
        "Free KV-cache blocks reported by the node's engine (0 when unknown).",
        &["node"]
    )
});

static NODE_KV_TOTAL: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_cluster_node_kv_total_blocks",
        "Total KV-cache blocks reported by the node's engine (0 when unknown).",
        &["node"]
    )
});

static NODE_POWER: LazyLock<GaugeVec> = LazyLock::new(|| {
    float_gauge_vec!(
        "cgn_cluster_node_power_watts",
        "GPU power draw per node (NVML), watts.",
        &["node"]
    )
});

static NODE_WATT_LIMIT: LazyLock<GaugeVec> = LazyLock::new(|| {
    float_gauge_vec!(
        "cgn_cluster_node_watt_limit",
        "Configured soft power cap per node, watts (0 = uncapped).",
        &["node"]
    )
});

static NODE_VRAM_TOTAL: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_cluster_node_vram_total_mb",
        "Total GPU memory across devices per node, MiB (0 when unknown).",
        &["node"]
    )
});

static NODE_INFO: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_cluster_node_info",
        "Static node metadata carried as labels; value is always 1.",
        &["node", "address", "model", "role", "gpu", "gpu_vendor"]
    )
});

static NODES_TOTAL: LazyLock<IntGauge> = LazyLock::new(|| {
    gauge!(
        "cgn_cluster_nodes_total",
        "Nodes currently registered in the router's cluster registry."
    )
});

static PREFIX_DIGESTS: LazyLock<IntGauge> = LazyLock::new(|| {
    gauge!(
        "cgn_router_prefix_index_digests",
        "Distinct prefix digests currently tracked in the router's KV prefix index."
    )
});

static POWER_TOTAL: LazyLock<Gauge> = LazyLock::new(|| {
    let g = Gauge::new(
        "cgn_cluster_power_watts_total",
        "Sum of cgn_cluster_node_power_watts across live nodes.",
    )
    .expect("gauge create");
    cgn_telemetry::registry().register(Box::new(g.clone())).ok();
    g
});

static TOKENS_PER_WATT: LazyLock<Gauge> = LazyLock::new(|| {
    let g = Gauge::new(
        "cgn_cluster_tokens_per_watt",
        "Recent completion-token throughput divided by fleet power draw (tokens/s per watt).",
    )
    .expect("gauge create");
    cgn_telemetry::registry().register(Box::new(g.clone())).ok();
    g
});

static LAST_ENERGY: LazyLock<Mutex<Option<(u64, Instant)>>> = LazyLock::new(|| Mutex::new(None));

fn role_str(role: NodeRole) -> &'static str {
    match role {
        NodeRole::Prefill => "prefill",
        NodeRole::Decode => "decode",
        NodeRole::Both => "both",
        NodeRole::Unspecified => "unspecified",
    }
}

/// One refresh pass: reset the vectors (so departed nodes' series drop
/// out) and repopulate from the registry snapshot.
fn refresh(state: &SharedState) {
    NODE_UP.reset();
    NODE_CORDONED.reset();
    NODE_QUEUE_DEPTH.reset();
    NODE_KV_FREE.reset();
    NODE_KV_TOTAL.reset();
    NODE_POWER.reset();
    NODE_WATT_LIMIT.reset();
    NODE_VRAM_TOTAL.reset();
    NODE_INFO.reset();

    let nodes = state.nodes.snapshot();
    NODES_TOTAL.set(nodes.len() as i64);
    PREFIX_DIGESTS.set(state.prefix.len() as i64);

    let mut power_sum = 0.0_f64;
    for n in &nodes {
        let id = n.node_id.as_str();
        // Registry entries exist while the etcd lease is alive; a live
        // lease means the agent heartbeats, which requires engine ready.
        NODE_UP.with_label_values(&[id]).set(1);
        NODE_CORDONED
            .with_label_values(&[id])
            .set(i64::from(n.cordoned));
        NODE_QUEUE_DEPTH
            .with_label_values(&[id])
            .set(n.queue_depth as i64);
        NODE_KV_FREE
            .with_label_values(&[id])
            .set(n.free_blocks as i64);
        NODE_KV_TOTAL
            .with_label_values(&[id])
            .set(n.total_blocks as i64);
        let watts = n.power_watts as f64;
        power_sum += watts;
        NODE_POWER.with_label_values(&[id]).set(watts);
        NODE_WATT_LIMIT
            .with_label_values(&[id])
            .set(n.watt_limit as f64);
        NODE_VRAM_TOTAL
            .with_label_values(&[id])
            .set(n.vram_total_mb as i64);
        NODE_INFO
            .with_label_values(&[
                id,
                n.address.as_str(),
                n.model.as_deref().unwrap_or(""),
                role_str(n.role_enum()),
                n.gpu_name.as_str(),
                n.gpu_vendor.as_str(),
            ])
            .set(1);
    }
    refresh_energy(power_sum);
}

fn refresh_energy(total_power: f64) {
    POWER_TOTAL.set(total_power);
    let tokens = crate::gateway::metrics::total_completion_tokens();
    let now = Instant::now();
    let mut last = LAST_ENERGY.lock();
    if let Some((prev_tokens, prev_at)) = *last {
        let dt = now.duration_since(prev_at).as_secs_f64();
        if dt > 0.0 && total_power > 0.0 {
            let rate = tokens.saturating_sub(prev_tokens) as f64 / dt;
            TOKENS_PER_WATT.set(rate / total_power);
        }
    }
    *last = Some((tokens, now));
}

/// Spawn the background refresher. Called once at router startup.
pub fn spawn(state: Arc<SharedState>) {
    // Force registration so an empty cluster still exposes the schema.
    LazyLock::force(&NODES_TOTAL);
    LazyLock::force(&PREFIX_DIGESTS);
    LazyLock::force(&NODE_UP);
    LazyLock::force(&NODE_CORDONED);
    LazyLock::force(&NODE_QUEUE_DEPTH);
    LazyLock::force(&NODE_KV_FREE);
    LazyLock::force(&NODE_KV_TOTAL);
    LazyLock::force(&NODE_POWER);
    LazyLock::force(&NODE_WATT_LIMIT);
    LazyLock::force(&NODE_VRAM_TOTAL);
    LazyLock::force(&NODE_INFO);
    LazyLock::force(&POWER_TOTAL);
    LazyLock::force(&TOKENS_PER_WATT);

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(REFRESH_INTERVAL);
        loop {
            tick.tick().await;
            refresh(&state);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::{NodeEntry, NodeRegistry};

    fn entry(id: &str, queue: u32, watts: f32, cordoned: bool) -> NodeEntry {
        NodeEntry {
            node_id: id.into(),
            address: format!("http://{id}:7070"),
            role: NodeRole::Both as i32,
            gpu_index: None,
            model: Some("llama3-8b".into()),
            queue_depth: queue,
            free_blocks: 900,
            total_blocks: 1000,
            power_watts: watts,
            watt_limit: 0.0,
            gpu_name: String::new(),
            gpu_vendor: String::new(),
            vram_total_mb: 0,
            cordoned,
            kv_epoch: 0,
            last_heartbeat: std::time::Instant::now(),
        }
    }

    #[test]
    fn refresh_populates_and_drops_series() {
        let registry = NodeRegistry::new();
        registry.upsert(entry("n1", 3, 250.0, false));
        registry.upsert(entry("n2", 0, 90.5, true));
        let state = SharedState {
            cfg: Default::default(),
            nodes: Arc::new(registry),
            prefix: Arc::new(cgn_core::prefix::PrefixIndex::new(
                std::time::Duration::from_secs(60),
            )),
            started: std::time::Instant::now(),
            policy: Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::state::RoutingPolicy {
                    kv: 0.55,
                    load: 0.25,
                    power: 0.10,
                    capacity: 0.10,
                },
            )),
            carbon: Arc::new(crate::carbon::CarbonTracker::new()),
            admission: Arc::new(crate::admission::Admission::new()),
        };

        refresh(&state);
        assert_eq!(NODES_TOTAL.get(), 2);
        assert_eq!(NODE_QUEUE_DEPTH.with_label_values(&["n1"]).get(), 3);
        assert_eq!(NODE_CORDONED.with_label_values(&["n2"]).get(), 1);
        assert!((NODE_POWER.with_label_values(&["n1"]).get() - 250.0).abs() < f64::EPSILON);
        assert!((POWER_TOTAL.get() - 340.5).abs() < f64::EPSILON);

        // Node departs → its series must disappear on the next refresh.
        state.nodes.forget("n2");
        refresh(&state);
        assert_eq!(NODES_TOTAL.get(), 1);
        let gathered = cgn_telemetry::registry().gather();
        let cordoned = gathered
            .iter()
            .find(|mf| mf.get_name() == "cgn_cluster_node_cordoned")
            .expect("family registered");
        assert_eq!(cordoned.get_metric().len(), 1, "n2 series should be gone");
    }
}
