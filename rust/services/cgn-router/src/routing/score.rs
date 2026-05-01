//! Score function used to pick a node for a request.
//!
//! Combines four normalised signals weighted by [`RoutingPolicy`]:
//!
//! ```text
//! score = w_kv * kv_overlap
//!       + w_load * (1 - normalised_queue_depth)
//!       + w_power * (1 - normalised_power)
//!       + w_capacity * (free_blocks / total_blocks)
//! ```
//!
//! All signals are in `[0, 1]` after normalisation. The kv-overlap is the
//! fraction of request prefix chunks that the node already has cached.

use crate::cluster::NodeEntry;
use crate::state::RoutingPolicy;

/// Per-node score with the four sub-components surfaced for tracing.
#[derive(Debug, Clone, Copy)]
pub struct Score {
    pub total: f32,
    pub kv: f32,
    pub load: f32,
    pub power: f32,
    pub capacity: f32,
}

/// Compute the score for a single node.
///
/// Inputs:
/// * `node`  — the candidate node's last-known telemetry.
/// * `kv_overlap` — fraction of request chunks this node holds (`[0,1]`).
/// * `peer_max_power` — highest power draw seen across all candidates,
///   used to normalise this node's power into `[0,1]`.
pub fn score_node(
    policy: &RoutingPolicy,
    node: &NodeEntry,
    kv_overlap: f32,
    peer_max_power: f32,
) -> Score {
    // Queue depth: assume 64 is "full" at p99 for a single replica.
    const QUEUE_FULL: f32 = 64.0;
    let load = (1.0 - (node.queue_depth as f32 / QUEUE_FULL)).clamp(0.0, 1.0);

    let power = if peer_max_power > 0.0 {
        (1.0 - (node.power_watts / peer_max_power)).clamp(0.0, 1.0)
    } else {
        1.0
    };

    let capacity = if node.total_blocks > 0 {
        (node.free_blocks as f32 / node.total_blocks as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let total = policy.kv * kv_overlap
        + policy.load * load
        + policy.power * power
        + policy.capacity * capacity;

    Score {
        total,
        kv: kv_overlap,
        load,
        power,
        capacity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> RoutingPolicy {
        RoutingPolicy {
            kv: 0.55,
            load: 0.25,
            power: 0.10,
            capacity: 0.10,
        }
    }

    fn node(q: u32, free: u32, total: u32, watts: f32) -> NodeEntry {
        NodeEntry {
            node_id: "n".into(),
            address: String::new(),
            role: 0,
            gpu_index: None,
            model: None,
            queue_depth: q,
            free_blocks: free,
            total_blocks: total,
            power_watts: watts,
            last_heartbeat: std::time::Instant::now(),
        }
    }

    #[test]
    fn idle_full_cache_beats_busy_cold_cache() {
        let n_idle = node(0, 100, 100, 200.0);
        let n_busy = node(60, 100, 100, 200.0);
        let s_idle = score_node(&pol(), &n_idle, 1.0, 200.0);
        let s_busy = score_node(&pol(), &n_busy, 0.0, 200.0);
        assert!(s_idle.total > s_busy.total);
    }

    #[test]
    fn signals_in_unit_range() {
        let n = node(10, 50, 100, 150.0);
        let s = score_node(&pol(), &n, 0.5, 200.0);
        for v in [s.kv, s.load, s.power, s.capacity, s.total] {
            assert!((0.0..=1.0).contains(&v), "{v} out of range");
        }
    }
}
