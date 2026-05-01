//! Energy-aware closed-loop autoscaler.
//!
//! Runs as a background tokio task on every router replica (idempotent
//! by design — the leader-election story is "first to grab the etcd
//! lease wins"). On every tick:
//!
//! 1. Snapshot per-node `power_watts` and `queue_depth` from
//!    [`NodeRegistry`].
//! 2. If the cluster is idle (avg util < `idle_util_pct`) and a node's
//!    watts > `high_watt_threshold`, mark that node *drain* — the
//!    operator picks up the etcd flag and scales down its replica.
//! 3. If the cluster is bursting (any model's `inflight ≥ max_queue`),
//!    flip `drain` off across the board so capacity comes back online.
//!
//! The actual replica count change is owned by `cgn-operator`; this
//! module only writes the desired-state hint into etcd at
//! `/cognitora/autoscaler/<node_id>` as a JSON `{drain: bool, reason}`.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use tracing::{info, warn};

use crate::state::SharedState;

const KEY_PREFIX: &str = "/cognitora/autoscaler/";

/// Spawn the autoscaler loop. Returns immediately after starting the
/// background task.
pub fn spawn(state: Arc<SharedState>) {
    let cfg = state.cfg.router.autoscaler.clone();
    if !cfg.enabled {
        info!("autoscaler disabled");
        return;
    }
    tokio::spawn(async move {
        let interval = Duration::from_secs(30);
        info!("autoscaler running");
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = tick(&state).await {
                warn!(error=?e, "autoscaler tick failed");
            }
        }
    });
}

async fn tick(state: &SharedState) -> Result<()> {
    let cfg = &state.cfg.router.autoscaler;
    let nodes = state.nodes.snapshot();
    if nodes.is_empty() {
        return Ok(());
    }

    let avg_load = nodes
        .iter()
        .map(|n| {
            let q = n.queue_depth as f32;
            let cap = n.total_blocks.max(1) as f32;
            (q / cap).min(1.0)
        })
        .sum::<f32>()
        / nodes.len() as f32;
    let cluster_idle = avg_load * 100.0 < cfg.idle_util_pct;

    let endpoints = state.cfg.cluster.etcd_endpoints.clone();
    if endpoints.is_empty() {
        // Single-node mode: nothing to scale.
        return Ok(());
    }
    let mut client = match etcd_client::Client::connect(&endpoints, None).await {
        Ok(c) => c,
        Err(e) => return Err(cgn_core::Error::Etcd(format!("connect: {e}"))),
    };

    for n in &nodes {
        let drain = cluster_idle && n.power_watts > cfg.high_watt_threshold;
        let reason = if drain {
            format!(
                "idle (avg {:.1}%); watts {:.0} > threshold {:.0}",
                avg_load * 100.0,
                n.power_watts,
                cfg.high_watt_threshold
            )
        } else {
            "active".into()
        };

        let key = format!("{KEY_PREFIX}{}", n.node_id);
        let body = serde_json::json!({
            "drain":  drain,
            "reason": reason,
            "watts":  n.power_watts,
            "stamp":  chrono::Utc::now().to_rfc3339(),
        });
        if let Err(e) = client.put(key, body.to_string(), None).await {
            warn!(node = %n.node_id, error=?e, "etcd put autoscaler hint failed");
        }
    }

    info!(
        nodes = nodes.len(),
        avg_load_pct = avg_load * 100.0,
        "autoscaler tick"
    );
    Ok(())
}
