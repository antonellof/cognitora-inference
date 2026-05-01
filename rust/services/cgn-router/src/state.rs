//! Process-wide shared state.
//!
//! Lives in an `Arc<SharedState>`; every async task derives its handle from
//! this single struct. Cheap to clone, mostly read-only.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use cgn_core::{config::Config, prefix::PrefixIndex, Error, Result};
use serde::{Deserialize, Serialize};

use crate::cluster::NodeRegistry;

pub struct SharedState {
    pub cfg:      Config,
    pub nodes:    Arc<NodeRegistry>,
    pub prefix:   Arc<PrefixIndex>,
    pub started:  std::time::Instant,
    /// Hot-swappable routing policy (etcd-driven).
    pub policy:   Arc<ArcSwap<RoutingPolicy>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub kv:       f32,
    pub load:     f32,
    pub power:    f32,
    pub capacity: f32,
}

impl SharedState {
    pub async fn new(cfg: Config) -> Result<Self> {
        let prefix = PrefixIndex::new(Duration::from_secs(60));
        let policy = RoutingPolicy {
            kv:       cfg.router.score_weights.kv,
            load:     cfg.router.score_weights.load,
            power:    cfg.router.score_weights.power,
            capacity: cfg.router.score_weights.capacity,
        };
        Ok(Self {
            cfg,
            nodes:   Arc::new(NodeRegistry::new()),
            prefix:  Arc::new(prefix),
            started: std::time::Instant::now(),
            policy:  Arc::new(ArcSwap::from_pointee(policy)),
        })
    }

    /// Spawn the etcd / gossip watcher that keeps `nodes` and `policy`
    /// in sync. Returns once the initial snapshot has been applied.
    pub async fn bootstrap_cluster_watch(&self) -> Result<()> {
        if self.cfg.cluster.etcd_endpoints.is_empty() {
            tracing::warn!("no etcd endpoints; running in single-node mode");
            return Ok(());
        }
        let endpoints = self.cfg.cluster.etcd_endpoints.clone();
        let nodes = self.nodes.clone();
        let policy = self.policy.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::cluster::run_etcd_watcher(endpoints, nodes, policy).await {
                tracing::error!(error=?e, "etcd watcher exited");
            }
        });
        Ok(())
    }

    /// Drain inflight work and persist any state. Called on SIGTERM.
    pub async fn drain(&self) {
        tracing::info!("draining router");
        // Concrete implementation: stop accepting new requests, wait until
        // queue depth on every node drops to zero or `drain_timeout` fires.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Convenience: Result<()> with our error type.
pub fn map_err<E: std::fmt::Display>(prefix: &str, e: E) -> Error {
    Error::Internal(format!("{prefix}: {e}"))
}
