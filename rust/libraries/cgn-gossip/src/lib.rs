//! Gossip cluster membership: etcd-free node discovery.
//!
//! Wraps [chitchat] (scuttlebutt reconciliation + phi-accrual failure
//! detection, UDP) so small clusters can run multi-node with **zero
//! external infrastructure**: no etcd, no Kubernetes, no NATS. Every
//! daemon joins the same gossip cluster; agents publish their node
//! record as a key-value on their own gossip state, and the router
//! reads the records of all *live* members.
//!
//! Liveness is phi-accrual, computed locally by every member from the
//! observed heartbeat cadence. It is the gossip-mode equivalent of the
//! etcd lease TTL used by the default backend.
//!
//! Selected trade-offs (documented in `docs/architecture/gossip.md`):
//!
//! * The node record rides in gossip deltas, so it must stay small:
//!   a couple of hundred bytes of JSON, the same payload written to
//!   `/cognitora/nodes/<id>` in etcd mode.
//! * Confirmed-KV prefix claims, cordon flags, and autoscaler hints are
//!   etcd-only; in gossip mode the router falls back to optimistic
//!   prefix tracking and TOML-configured score weights.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chitchat::transport::UdpTransport;
use chitchat::{
    spawn_chitchat, ChitchatConfig, ChitchatHandle, ChitchatId, FailureDetectorConfig,
    ProtocolVersion,
};

/// Key on a member's gossip state holding its JSON node record, the
/// same document an agent would write to `/cognitora/nodes/<id>` in
/// etcd mode. Members without this key (e.g. routers) are cluster
/// participants but are never routing candidates.
pub const NODE_RECORD_KEY: &str = "cgn.node";

/// How often a member initiates a gossip round. One second keeps
/// failure detection snappy (phi-accrual converges after a handful of
/// missed heartbeats) at negligible bandwidth for small clusters.
pub const GOSSIP_INTERVAL: Duration = Duration::from_millis(1_000);

/// Configuration for one gossip member.
#[derive(Debug, Clone)]
pub struct GossipConfig {
    /// Cluster name; members with a different name ignore each other.
    pub cluster_name: String,
    /// Unique node id (`agent.node_id` / `router.node_id`).
    pub node_id: String,
    /// UDP socket to bind.
    pub listen: SocketAddr,
    /// Address peers should use to reach this member. Must be routable
    /// from every other member (not `0.0.0.0`).
    pub advertise: SocketAddr,
    /// Seed members (`host:port`), à la `cluster.gossip_seeds`. Any
    /// subset of live members works; DNS names are re-resolved.
    pub seeds: Vec<String>,
}

/// A live gossip member. Dropping the handle leaves the background
/// server running; call [`GossipMember::shutdown`] for a clean exit.
pub struct GossipMember {
    handle: ChitchatHandle,
}

impl GossipMember {
    /// Join (or bootstrap) the gossip cluster over UDP.
    pub async fn spawn(cfg: GossipConfig) -> anyhow::Result<Self> {
        // The generation id must grow across restarts so peers accept
        // the rejoined node's fresh state; wall-clock seconds is the
        // standard trick (also what Quickwit does).
        let generation_id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let chitchat_id = ChitchatId {
            node_id: cfg.node_id.into(),
            generation_id,
            gossip_advertise_addr: cfg.advertise,
        };
        let config = ChitchatConfig {
            chitchat_id,
            cluster_id: cfg.cluster_name,
            gossip_interval: GOSSIP_INTERVAL,
            listen_addr: cfg.listen,
            seed_nodes: cfg.seeds,
            failure_detector_config: FailureDetectorConfig::default(),
            // Tombstone GC grace. Our records are updated, not deleted,
            // so this only matters for explicit key removal; keep the
            // library-recommended 15 minutes.
            marked_for_deletion_grace_period: Duration::from_secs(15 * 60),
            catchup_callback: None,
            extra_liveness_predicate: None,
            // All Cognitora binaries in a cluster run the same release,
            // so the compressed V1 wire format is safe.
            protocol_version: ProtocolVersion::V1,
        };
        let handle = spawn_chitchat(config, Vec::new(), &UdpTransport).await?;
        Ok(Self { handle })
    }

    /// Publish (or refresh) this member's node record. Setting the same
    /// key bumps its version, which is what feeds every peer's failure
    /// detector, so call it on the heartbeat cadence.
    pub async fn publish_node_record(&self, record_json: &str) {
        self.set_self(NODE_RECORD_KEY, record_json).await;
    }

    /// Set an arbitrary key on our own gossip state.
    pub async fn set_self(&self, key: &str, value: &str) {
        let chitchat = self.handle.chitchat();
        chitchat.lock().await.self_node_state().set(key, value);
    }

    /// `(node_id, record_json)` for every **live** member that has
    /// published a node record. Members without the record key (e.g.
    /// routers) are skipped.
    pub async fn live_node_records(&self) -> Vec<(String, String)> {
        let chitchat = self.handle.chitchat();
        let guard = chitchat.lock().await;
        let live: Vec<ChitchatId> = guard.live_nodes().cloned().collect();
        live.into_iter()
            .filter_map(|id| {
                let record = guard.node_state(&id)?.get(NODE_RECORD_KEY)?;
                Some((id.node_id.to_string(), record.to_string()))
            })
            .collect()
    }

    /// Number of live members (including self, including record-less
    /// members such as routers).
    pub async fn live_members(&self) -> usize {
        let chitchat = self.handle.chitchat();
        let guard = chitchat.lock().await;
        guard.live_nodes().count()
    }

    /// Gracefully leave the cluster and stop the background server.
    pub async fn shutdown(self) -> anyhow::Result<()> {
        self.handle.shutdown().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(port: u16) -> SocketAddr {
        format!("127.0.0.1:{port}").parse().unwrap()
    }

    /// Grab two distinct free UDP ports.
    fn free_udp_ports() -> (u16, u16) {
        let a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        (
            a.local_addr().unwrap().port(),
            b.local_addr().unwrap().port(),
        )
    }

    #[tokio::test]
    async fn two_members_exchange_node_records() {
        let (port_a, port_b) = free_udp_ports();

        let a = GossipMember::spawn(GossipConfig {
            cluster_name: "test".into(),
            node_id: "node-a".into(),
            listen: local(port_a),
            advertise: local(port_a),
            seeds: vec![],
        })
        .await
        .expect("spawn a");
        a.publish_node_record(r#"{"node_id":"node-a","queue_depth":3}"#)
            .await;

        // B seeds off A. B publishes no record (router-like member).
        let b = GossipMember::spawn(GossipConfig {
            cluster_name: "test".into(),
            node_id: "node-b".into(),
            listen: local(port_b),
            advertise: local(port_b),
            seeds: vec![format!("127.0.0.1:{port_a}")],
        })
        .await
        .expect("spawn b");

        // Wait for convergence: B must observe A's record.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let records = b.live_node_records().await;
            if records
                .iter()
                .any(|(id, json)| id == "node-a" && json.contains("\"queue_depth\":3"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "gossip did not converge in time; records = {records:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // A refreshes its record; B must observe the update.
        a.publish_node_record(r#"{"node_id":"node-a","queue_depth":7}"#)
            .await;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let records = b.live_node_records().await;
            if records
                .iter()
                .any(|(id, json)| id == "node-a" && json.contains("\"queue_depth\":7"))
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "record update did not propagate; records = {records:?}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        // B never published a record, so it must not appear as a
        // routing candidate even though both members are live.
        let records = b.live_node_records().await;
        assert!(records.iter().all(|(id, _)| id != "node-b"));
        assert!(b.live_members().await >= 2);

        a.shutdown().await.expect("shutdown a");
        b.shutdown().await.expect("shutdown b");
    }
}
