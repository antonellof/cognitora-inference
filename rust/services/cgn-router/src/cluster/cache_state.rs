//! Prefix-index reconciliation from agent-reported KV cache state.
//!
//! Complements confirmed-KV etcd keys and LRU-pressure heuristics with
//! two additional signals carried in the node heartbeat:
//!
//! * **`kv_epoch`** — monotonic counter bumped by the agent on cache
//!   resets. A change wipes every prefix claim for that node.
//! * **Eviction burst** — a large drop in `free_blocks` between
//!   heartbeats triggers aggressive stale-claim pruning.

use std::sync::LazyLock;

use cgn_core::prefix::PrefixIndex;
use cgn_telemetry::prometheus::IntCounterVec;
use cgn_telemetry::counter_vec;
use dashmap::DashMap;

use super::NodeEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneReason {
    EpochReset,
    EvictionBurst,
    Pressure,
}

#[derive(Debug, Clone, Copy, Default)]
struct Snapshot {
    kv_epoch: u64,
    free_blocks: u32,
    total_blocks: u32,
}

/// Per-node previous heartbeat snapshot for delta detection.
#[derive(Default)]
pub struct CacheStateTracker {
    inner: DashMap<String, Snapshot>,
}

impl CacheStateTracker {
    /// Reconcile prefix claims after a fresh node heartbeat. Returns the
    /// action taken, if any.
    pub fn reconcile(
        &self,
        prefix: &PrefixIndex,
        entry: &NodeEntry,
    ) -> Option<PruneReason> {
        let had_prev = self.inner.contains_key(&entry.node_id);
        let prev = self
            .inner
            .get(&entry.node_id)
            .map(|s| *s)
            .unwrap_or_default();

        let reason = if had_prev && entry.kv_epoch != prev.kv_epoch {
            prefix.forget_node(&entry.node_id);
            Some(PruneReason::EpochReset)
        } else if entry.total_blocks > 0 && prev.total_blocks > 0 {
            let drop = prev.free_blocks.saturating_sub(entry.free_blocks);
            let burst_threshold = (entry.total_blocks / 10).max(32);
            if drop >= burst_threshold {
                prefix.forget_node_stale(&entry.node_id, prefix.ttl() / 4);
                Some(PruneReason::EvictionBurst)
            } else if (entry.free_blocks as f32 / entry.total_blocks as f32) < 0.05 {
                prefix.forget_node_stale(&entry.node_id, prefix.ttl() / 2);
                Some(PruneReason::Pressure)
            } else {
                None
            }
        } else if entry.total_blocks > 0
            && (entry.free_blocks as f32 / entry.total_blocks as f32) < 0.05
        {
            prefix.forget_node_stale(&entry.node_id, prefix.ttl() / 2);
            Some(PruneReason::Pressure)
        } else {
            None
        };

        if let Some(r) = reason {
            record_prune(&entry.node_id, r);
            tracing::debug!(
                node = %entry.node_id,
                ?r,
                kv_epoch = entry.kv_epoch,
                free_blocks = entry.free_blocks,
                total_blocks = entry.total_blocks,
                "prefix index reconciled from cache state"
            );
        }

        self.inner.insert(
            entry.node_id.clone(),
            Snapshot {
                kv_epoch: entry.kv_epoch,
                free_blocks: entry.free_blocks,
                total_blocks: entry.total_blocks,
            },
        );
        reason
    }

    pub fn forget_node(&self, node_id: &str) {
        self.inner.remove(node_id);
    }
}

static PRUNED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_prefix_index_pruned_total",
        "Prefix-index claims pruned after agent-reported KV cache state changes.",
        &["node", "reason"]
    )
});

fn record_prune(node: &str, reason: PruneReason) {
    let label = match reason {
        PruneReason::EpochReset => "epoch_reset",
        PruneReason::EvictionBurst => "eviction_burst",
        PruneReason::Pressure => "pressure",
    };
    PRUNED.with_label_values(&[node, label]).inc();
}

pub fn warm_up_metrics() {
    LazyLock::force(&PRUNED);
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_proto::v1::NodeRole;
    use std::time::Duration;

    fn entry(id: &str, epoch: u64, free: u32, total: u32) -> NodeEntry {
        NodeEntry {
            node_id: id.into(),
            address: format!("http://{id}:7070"),
            role: NodeRole::Both as i32,
            gpu_index: None,
            model: None,
            queue_depth: 0,
            free_blocks: free,
            total_blocks: total,
            power_watts: 0.0,
            watt_limit: 0.0,
            gpu_name: String::new(),
            gpu_vendor: String::new(),
            vram_total_mb: 0,
            cordoned: false,
            kv_epoch: epoch,
            last_heartbeat: std::time::Instant::now(),
        }
    }

    #[test]
    fn epoch_change_wipes_node_claims() {
        let prefix = PrefixIndex::new(Duration::from_secs(60));
        prefix.insert([1u8; 32], "n1");
        let tracker = CacheStateTracker::default();
        tracker.reconcile(&prefix, &entry("n1", 0, 500, 1000));
        assert_eq!(prefix.lookup(&[1u8; 32]), vec!["n1".to_string()]);
        assert_eq!(
            tracker.reconcile(&prefix, &entry("n1", 1, 500, 1000)),
            Some(PruneReason::EpochReset)
        );
        assert!(prefix.lookup(&[1u8; 32]).is_empty());
    }

    #[test]
    fn eviction_burst_prunes_stale_half() {
        let prefix = PrefixIndex::new(Duration::from_millis(0));
        prefix.insert([1u8; 32], "n1");
        std::thread::sleep(Duration::from_millis(2));
        prefix.insert([2u8; 32], "n1");
        let tracker = CacheStateTracker::default();
        tracker.reconcile(&prefix, &entry("n1", 0, 500, 1000));
        assert_eq!(
            tracker.reconcile(&prefix, &entry("n1", 0, 400, 1000)),
            Some(PruneReason::EvictionBurst)
        );
        // Oldest claim (digest 1) should be gone; newest may remain.
        assert!(prefix.lookup(&[1u8; 32]).is_empty());
    }
}
