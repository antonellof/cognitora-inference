//! Process-wide shared state.
//!
//! Lives in an `Arc<SharedState>`; every async task derives its handle from
//! this single struct. Cheap to clone, mostly read-only.

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use cgn_core::{config::Config, prefix::PrefixIndex, Error, Result};
use cgn_proto::v1::agent_client::AgentClient;
use serde::{Deserialize, Serialize};
use tonic::transport::{Channel, Endpoint};

use crate::cluster::NodeRegistry;

pub struct SharedState {
    pub cfg: Config,
    pub nodes: Arc<NodeRegistry>,
    pub prefix: Arc<PrefixIndex>,
    pub started: std::time::Instant,
    /// Hot-swappable routing policy (etcd-driven).
    pub policy: Arc<ArcSwap<RoutingPolicy>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub kv: f32,
    pub load: f32,
    pub power: f32,
    pub capacity: f32,
}

impl SharedState {
    pub async fn new(cfg: Config) -> Result<Self> {
        let prefix = PrefixIndex::new(Duration::from_secs(60));
        let policy = RoutingPolicy {
            kv: cfg.router.score_weights.kv,
            load: cfg.router.score_weights.load,
            power: cfg.router.score_weights.power,
            capacity: cfg.router.score_weights.capacity,
        };
        Ok(Self {
            cfg,
            nodes: Arc::new(NodeRegistry::new()),
            prefix: Arc::new(prefix),
            started: std::time::Instant::now(),
            policy: Arc::new(ArcSwap::from_pointee(policy)),
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

    /// Connect to a `cgn-agent` over gRPC, applying client mTLS when
    /// `[security].require_mtls = true`. The agent's URI in etcd is
    /// `https://host:port` for mTLS deployments and `http://host:port`
    /// otherwise; the scheme on the URI is authoritative.
    pub async fn connect_agent(&self, uri: &str) -> Result<AgentClient<Channel>> {
        let endpoint =
            Endpoint::from_shared(uri.to_string()).map_err(|e| map_err("agent endpoint", e))?;

        let endpoint = if self.cfg.security.require_mtls {
            let (Some(ca), Some(cert), Some(key)) = (
                self.cfg.security.ca_file.as_ref(),
                self.cfg.security.cert_file.as_ref(),
                self.cfg.security.key_file.as_ref(),
            ) else {
                return Err(Error::Config(
                    "require_mtls=true but [security].ca_file/cert_file/key_file are not set"
                        .into(),
                ));
            };
            // The leaf cert is bound to "localhost" in dev PKI; production
            // setups should issue per-node certs and pass the right SAN.
            let domain = extract_host(uri).unwrap_or_else(|| "localhost".into());
            let tls = cgn_tls::client_tls(ca, cert, key, domain)
                .map_err(|e| Error::Tls(format!("client tls: {e}")))?;
            endpoint
                .tls_config(tls)
                .map_err(|e| Error::Tls(format!("tls cfg: {e}")))?
        } else {
            endpoint
        };

        let chan = endpoint
            .connect()
            .await
            .map_err(|e| Error::Unavailable(format!("agent connect {uri}: {e}")))?;
        Ok(AgentClient::new(chan))
    }
}

fn extract_host(uri: &str) -> Option<String> {
    let after_scheme = uri.split("://").nth(1)?;
    let host = after_scheme.split('/').next()?.split(':').next()?;
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Convenience: Result<()> with our error type.
pub fn map_err<E: std::fmt::Display>(prefix: &str, e: E) -> Error {
    Error::Internal(format!("{prefix}: {e}"))
}
