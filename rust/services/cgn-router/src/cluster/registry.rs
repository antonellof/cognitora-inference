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
    pub node_id:        String,
    pub address:        String,         // grpc endpoint, e.g. "https://10.0.1.7:7070"
    pub role:           i32,             // matches NodeRole proto enum
    pub gpu_index:      Option<u32>,
    pub model:          Option<String>,
    pub queue_depth:    u32,
    pub free_blocks:    u32,
    pub total_blocks:   u32,
    pub power_watts:    f32,
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
    pub fn new() -> Self { Self::default() }

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

    pub fn len(&self) -> usize { self.inner.len() }
    pub fn is_empty(&self) -> bool { self.inner.is_empty() }

    /// Snapshot all live nodes filtered by role.
    pub fn nodes_for(&self, role: NodeRole, model: Option<&str>) -> Vec<Arc<NodeEntry>> {
        self.inner.iter()
            .filter_map(|kv| {
                let n = kv.value().read().clone();
                let role_match = matches!(role, NodeRole::Unspecified)
                    || n.role_enum() == role
                    || n.role_enum() == NodeRole::Both;
                let model_match = model.map_or(true, |m| n.model.as_deref() == Some(m));
                if role_match && model_match { Some(n) } else { None }
            })
            .collect()
    }
}
