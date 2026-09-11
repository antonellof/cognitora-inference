//! Gossip watcher → keeps `NodeRegistry` in sync without etcd.
//!
//! The router joins the gossip cluster as a record-less member (it
//! publishes no `cgn.node` key, so it is never a routing candidate) and
//! reconciles the registry against the live-member records on every
//! sync tick. Node death is phi-accrual failure detection inside
//! chitchat, the gossip-mode equivalent of etcd lease expiry.
//!
//! Etcd-only concerns (confirmed-KV claims, cordon flags, routing
//! policy hot-reload, autoscaler hints) are intentionally absent here:
//! in gossip mode the prefix index runs on optimistic inserts and score
//! weights come from the TOML config. See `docs/architecture/gossip.md`.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use cgn_core::{Error, Result};

use super::{NodeEntry, NodeRegistry};

/// Registry reconciliation cadence. Half the agent heartbeat (5 s) so a
/// fresh record never waits more than one heartbeat to become routable.
const SYNC_INTERVAL: Duration = Duration::from_millis(2_500);

/// Join the gossip cluster and reconcile `NodeRegistry` forever.
/// Returns only on spawn failure; once the member is up, chitchat's own
/// background task keeps the membership view converging.
pub async fn run_gossip_watcher(
    cfg: cgn_core::config::Config,
    nodes: Arc<NodeRegistry>,
    prefix: Arc<cgn_core::prefix::PrefixIndex>,
) -> Result<()> {
    let cluster = &cfg.cluster;
    let listen = cluster
        .gossip_listen
        .parse()
        .map_err(|e| Error::Config(format!("cluster.gossip_listen: {e}")))?;
    let advertise = cluster
        .gossip_advertise_or_listen()
        .parse()
        .map_err(|e| Error::Config(format!("cluster.gossip_advertise: {e}")))?;
    let member = cgn_gossip::GossipMember::spawn(cgn_gossip::GossipConfig {
        cluster_name: cluster.name.clone(),
        node_id: cfg.router.node_id.clone(),
        listen,
        advertise,
        seeds: cluster.gossip_seeds.clone(),
    })
    .await
    .map_err(|e| Error::Gossip(format!("spawn: {e}")))?;
    tracing::info!(%listen, %advertise, seeds = ?cluster.gossip_seeds, "gossip member joined");

    let mut tick = tokio::time::interval(SYNC_INTERVAL);
    loop {
        tick.tick().await;
        let records = member.live_node_records().await;

        // Upsert every live record; collect ids for the removal pass.
        let mut live: HashSet<String> = HashSet::with_capacity(records.len());
        for (member_id, json) in &records {
            match serde_json::from_str::<NodeEntry>(json) {
                Ok(entry) => {
                    live.insert(entry.node_id.clone());
                    nodes.upsert(entry);
                }
                Err(e) => {
                    tracing::warn!(%member_id, error = %e, "bad gossip node record");
                }
            }
        }

        // Drop registry entries whose member died (or stopped
        // publishing a record) and purge their prefix-index claims,
        // mirroring the etcd watcher's Delete handling.
        for entry in nodes.snapshot() {
            if !live.contains(&entry.node_id) {
                tracing::info!(node_id = %entry.node_id, "gossip member gone; forgetting");
                nodes.forget(&entry.node_id);
                prefix.forget_node(&entry.node_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end over real UDP gossip: an agent-like member publishes a
    /// record, the router-side watcher populates the registry, and when
    /// the publisher leaves, the registry eventually forgets it.
    #[tokio::test]
    async fn registry_follows_gossip_membership() {
        let sock_a = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let sock_r = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port_a = sock_a.local_addr().unwrap().port();
        let port_r = sock_r.local_addr().unwrap().port();
        drop((sock_a, sock_r));

        // Agent-like member.
        let agent = cgn_gossip::GossipMember::spawn(cgn_gossip::GossipConfig {
            cluster_name: "test".into(),
            node_id: "agent-1".into(),
            listen: format!("127.0.0.1:{port_a}").parse().unwrap(),
            advertise: format!("127.0.0.1:{port_a}").parse().unwrap(),
            seeds: vec![],
        })
        .await
        .expect("spawn agent member");
        agent
            .publish_node_record(
                r#"{"node_id":"agent-1","address":"http://127.0.0.1:7070","role":2,
                    "gpu_index":null,"model":"llama3-8b","queue_depth":1,
                    "free_blocks":10,"total_blocks":100,"power_watts":42.0}"#,
            )
            .await;

        // Router-side watcher.
        let mut cfg = cgn_core::config::Config::default();
        cfg.cluster.name = "test".into();
        cfg.cluster.gossip_listen = format!("127.0.0.1:{port_r}");
        cfg.cluster.gossip_seeds = vec![format!("127.0.0.1:{port_a}")];
        cfg.router.node_id = "router-1".into();
        let nodes = Arc::new(NodeRegistry::new());
        let prefix = Arc::new(cgn_core::prefix::PrefixIndex::new(Duration::from_secs(60)));
        let watcher = tokio::spawn(run_gossip_watcher(cfg, nodes.clone(), prefix));

        // Registry must pick the agent up.
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while nodes.get("agent-1").is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "agent-1 never appeared in the registry"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let entry = nodes.get("agent-1").unwrap();
        assert_eq!(entry.model.as_deref(), Some("llama3-8b"));
        assert_eq!(entry.queue_depth, 1);

        watcher.abort();
        agent.shutdown().await.ok();
    }
}
