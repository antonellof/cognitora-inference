//! Process-wide shared state.
//!
//! Lives in an `Arc<SharedState>`; every async task derives its handle from
//! this single struct. Cheap to clone, mostly read-only.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use cgn_core::{config::Config, prefix::PrefixIndex, Error, Result};
use cgn_proto::v1::agent_client::AgentClient;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tonic::transport::{Channel, Endpoint};

use crate::admission::Admission;
use crate::carbon::CarbonTracker;
use crate::cluster::{NodeEntry, NodeRegistry};
use crate::routing::tokenize::Tokenizers;
use crate::usage::UsageLedger;

pub struct SharedState {
    pub cfg: Config,
    pub nodes: Arc<NodeRegistry>,
    pub prefix: Arc<PrefixIndex>,
    pub started: std::time::Instant,
    /// Hot-swappable routing policy (etcd-driven).
    pub policy: Arc<ArcSwap<RoutingPolicy>>,
    /// Latest grid carbon intensity sample (background-polled).
    pub carbon: Arc<CarbonTracker>,
    /// Per-(model, role) inflight admission counters.
    pub admission: Arc<Admission>,
    /// Per-model tokenizers for prefix hashing and token counts.
    pub tokenizers: Tokenizers,
    /// Token totals and per-key limits.
    pub usage: UsageLedger,
    /// One multiplexed channel per agent, opened on first use.
    channels: DashMap<String, Channel>,
    /// Requests this router has running on each node.
    node_inflight: DashMap<String, Arc<AtomicU32>>,
    /// Set on SIGTERM: readiness fails while in-flight work finishes.
    pub draining: AtomicBool,
}

/// Counts one request against a node for the lifetime of the guard.
pub struct NodeGuard(Arc<AtomicU32>);

impl Drop for NodeGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    pub kv: f32,
    pub load: f32,
    pub power: f32,
    pub capacity: f32,
    /// Older etcd policies predate this field.
    #[serde(default)]
    pub session: f32,
}

impl SharedState {
    /// Startup path: fills in the state that needs I/O.
    pub async fn new(cfg: Config) -> Result<Self> {
        let tokenizers = Tokenizers::load(&cfg).await;
        let usage = UsageLedger::open(cfg.auth.usage_file.clone())
            .map_err(|e| Error::Config(format!("auth.usage_file: {e}")))?;
        let mut state = Self::build(cfg);
        state.tokenizers = tokenizers;
        state.usage = usage;
        Ok(state)
    }

    /// State with no I/O: empty tokenizers and an in-memory ledger.
    pub fn build(cfg: Config) -> Self {
        let prefix = PrefixIndex::new(Duration::from_secs(60));
        let policy = RoutingPolicy {
            kv: cfg.router.score_weights.kv,
            load: cfg.router.score_weights.load,
            power: cfg.router.score_weights.power,
            capacity: cfg.router.score_weights.capacity,
            session: cfg.router.score_weights.session,
        };
        Self {
            cfg,
            nodes: Arc::new(NodeRegistry::new()),
            prefix: Arc::new(prefix),
            started: std::time::Instant::now(),
            policy: Arc::new(ArcSwap::from_pointee(policy)),
            carbon: Arc::new(CarbonTracker::new()),
            admission: Arc::new(Admission::new()),
            tokenizers: Tokenizers::default(),
            usage: UsageLedger::default(),
            channels: DashMap::new(),
            node_inflight: DashMap::new(),
            draining: AtomicBool::new(false),
        }
    }

    /// Why the router shouldn't receive traffic, if anything. Discovery
    /// modes need a live node; a router without discovery has nothing to
    /// wait for.
    pub fn readiness(&self) -> std::result::Result<(), String> {
        if self.draining.load(Ordering::Acquire) {
            return Err("draining".into());
        }
        let discovery =
            self.cfg.cluster.gossip_enabled() || !self.cfg.cluster.etcd_endpoints.is_empty();
        if discovery
            && self
                .nodes
                .nodes_for(cgn_proto::v1::NodeRole::Unspecified, None)
                .is_empty()
        {
            return Err("no ready nodes".into());
        }
        Ok(())
    }

    /// Count a request on `node_id` until the guard drops.
    pub fn track_node(&self, node_id: &str) -> NodeGuard {
        let cell = self
            .node_inflight
            .entry(node_id.to_string())
            .or_default()
            .clone();
        cell.fetch_add(1, Ordering::AcqRel);
        NodeGuard(cell)
    }

    /// Requests this router has running on `node_id`.
    pub fn node_inflight(&self, node_id: &str) -> u32 {
        self.node_inflight
            .get(node_id)
            .map(|c| c.load(Ordering::Acquire))
            .unwrap_or(0)
    }

    /// Best queue estimate for scoring. Heartbeats arrive every few
    /// seconds, so a burst would all land on the node that looked idle
    /// without the router's live count. Other routers' work reaches us
    /// through the heartbeat, hence the max rather than a sum.
    pub fn effective_queue(&self, node: &NodeEntry) -> u32 {
        node.queue_depth.max(self.node_inflight(&node.node_id))
    }

    /// Drop a pooled channel after a failure, so the next request dials
    /// fresh.
    pub fn forget_agent(&self, uri: &str) {
        self.channels.remove(uri);
    }

    /// Spawn the etcd watcher that keeps `nodes` and `policy` in sync,
    /// plus the periodic prefix-index GC. Returns once the initial
    /// snapshot has been applied.
    pub async fn bootstrap_cluster_watch(&self) -> Result<()> {
        // Prefix-index GC runs in every mode (the index fills up from
        // optimistic inserts even in single-node deployments).
        let gc_prefix = self.prefix.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(30));
            loop {
                tick.tick().await;
                gc_prefix.gc();
            }
        });

        // Gossip backend: etcd-free discovery. The watcher joins the
        // gossip cluster and reconciles the registry from live-member
        // records; failure detection replaces the etcd lease.
        if self.cfg.cluster.gossip_enabled() {
            let cfg = self.cfg.clone();
            let nodes = self.nodes.clone();
            let prefix = self.prefix.clone();
            tokio::spawn(async move {
                loop {
                    match crate::cluster::run_gossip_watcher(
                        cfg.clone(),
                        nodes.clone(),
                        prefix.clone(),
                    )
                    .await
                    {
                        Ok(()) => tracing::warn!("gossip watcher ended; restarting in 5s"),
                        Err(e) => {
                            tracing::error!(error=?e, "gossip watcher exited; restarting in 5s")
                        }
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            });
            return Ok(());
        }

        if self.cfg.cluster.etcd_endpoints.is_empty() {
            tracing::warn!("no etcd endpoints; running in single-node mode");
            return Ok(());
        }
        let endpoints = self.cfg.cluster.etcd_endpoints.clone();
        let nodes = self.nodes.clone();
        let policy = self.policy.clone();
        let prefix = self.prefix.clone();
        tokio::spawn(async move {
            // The watcher returns when its etcd watch stream ends (etcd
            // restart, compaction, network partition). Without a reconnect
            // the node registry would go permanently stale, so loop with a
            // small backoff; each run re-applies the initial snapshot.
            loop {
                match crate::cluster::run_etcd_watcher(
                    endpoints.clone(),
                    nodes.clone(),
                    policy.clone(),
                    prefix.clone(),
                )
                .await
                {
                    Ok(()) => tracing::warn!("etcd watcher stream ended; reconnecting in 5s"),
                    Err(e) => tracing::error!(error=?e, "etcd watcher exited; reconnecting in 5s"),
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        Ok(())
    }

    /// Connect to a `cgn-agent` over gRPC, applying client mTLS when
    /// `[security].require_mtls = true`. The agent's URI in etcd is
    /// `https://host:port` for mTLS deployments and `http://host:port`
    /// otherwise; the scheme on the URI is authoritative.
    ///
    /// Channels are pooled: gRPC multiplexes every request over one
    /// HTTP/2 connection, so only the first request pays for dialing and
    /// the TLS handshake.
    pub async fn connect_agent(&self, uri: &str) -> Result<AgentClient<Channel>> {
        if let Some(chan) = self.channels.get(uri) {
            return Ok(agent_client(chan.clone()));
        }
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
            .connect_timeout(Duration::from_secs(3))
            .tcp_nodelay(true)
            .http2_keep_alive_interval(Duration::from_secs(15))
            .keep_alive_timeout(Duration::from_secs(10))
            .keep_alive_while_idle(true)
            .connect()
            .await
            .map_err(|e| Error::Unavailable(format!("agent connect {uri}: {e}")))?;
        self.channels.insert(uri.to_string(), chan.clone());
        Ok(agent_client(chan))
    }
}

fn agent_client(chan: Channel) -> AgentClient<Channel> {
    AgentClient::new(chan)
        .max_decoding_message_size(cgn_core::MAX_GRPC_MESSAGE_BYTES)
        .max_encoding_message_size(cgn_core::MAX_GRPC_MESSAGE_BYTES)
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
