//! In-memory node registry. Read-mostly; writers come from the etcd watcher
//! (or local probing for single-node deployments).

use std::sync::Arc;
use std::time::Instant;

use cgn_proto::v1::NodeRole;
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEntry {
    pub node_id: String,
    pub address: String, // grpc endpoint, e.g. "https://10.0.1.7:7070"
    pub role: i32,       // matches NodeRole proto enum
    pub gpu_index: Option<u32>,
    pub model: Option<String>,
    pub queue_depth: u32,
    pub free_blocks: u32,
    pub total_blocks: u32,
    pub power_watts: f32,
    /// Operator-set flag mirrored from `/cognitora/cordon/<node_id>`.
    /// Cordoned nodes are excluded from candidate selection so the
    /// router stops sending new traffic to them. Inflight requests
    /// continue until they finish or the agent is drained explicitly.
    #[serde(default)]
    pub cordoned: bool,
    #[serde(skip, default = "Instant::now")]
    pub last_heartbeat: Instant,
}

impl NodeEntry {
    pub fn role_enum(&self) -> NodeRole {
        NodeRole::try_from(self.role).unwrap_or(NodeRole::Unspecified)
    }

    pub fn fresh(&self, ttl: std::time::Duration) -> bool {
        self.last_heartbeat.elapsed() < ttl
    }
}

/// Concurrent map keyed by `node_id`.
#[derive(Default)]
pub struct NodeRegistry {
    inner: DashMap<String, RwLock<Arc<NodeEntry>>>,
}

impl NodeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, entry: NodeEntry) {
        let id = entry.node_id.clone();
        match self.inner.get_mut(&id) {
            Some(lock) => {
                *lock.value().write() = Arc::new(entry);
            }
            None => {
                self.inner.insert(id, RwLock::new(Arc::new(entry)));
            }
        }
    }

    pub fn get(&self, node_id: &str) -> Option<Arc<NodeEntry>> {
        self.inner.get(node_id).map(|e| e.value().read().clone())
    }

    pub fn forget(&self, node_id: &str) {
        self.inner.remove(node_id);
    }

    /// Toggle the `cordoned` flag on a node already in the registry.
    /// No-op if the node is not currently registered (the flag is only
    /// useful when the node is live anyway).
    pub fn set_cordon(&self, node_id: &str, cordoned: bool) {
        let Some(slot) = self.inner.get(node_id) else {
            return;
        };
        let mut guard = slot.value().write();
        if guard.cordoned == cordoned {
            return;
        }
        let mut next = (**guard).clone();
        next.cordoned = cordoned;
        *guard = Arc::new(next);
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Snapshot every registered node (no filtering).
    pub fn snapshot(&self) -> Vec<Arc<NodeEntry>> {
        self.inner
            .iter()
            .map(|kv| kv.value().read().clone())
            .collect()
    }

    /// Snapshot all live nodes filtered by role. Cordoned nodes are
    /// excluded — the router scoring never picks a cordoned node, so
    /// `cgn-ctl cluster cordon <id>` immediately stops new traffic.
    pub fn nodes_for(&self, role: NodeRole, model: Option<&str>) -> Vec<Arc<NodeEntry>> {
        self.inner
            .iter()
            .filter_map(|kv| {
                let n = kv.value().read().clone();
                if n.cordoned {
                    return None;
                }
                let role_match = matches!(role, NodeRole::Unspecified)
                    || n.role_enum() == role
                    || n.role_enum() == NodeRole::Both;
                let model_match = model.is_none_or(|m| n.model.as_deref() == Some(m));
                if role_match && model_match {
                    Some(n)
                } else {
                    None
                }
            })
            .collect()
    }
}
