//! etcd watcher → keeps `NodeRegistry` and `RoutingPolicy` in sync.

use std::sync::Arc;

use arc_swap::ArcSwap;
use cgn_core::{Error, Result};
use etcd_client::{Client, EventType, GetOptions, WatchOptions};

use super::NodeRegistry;
use crate::state::RoutingPolicy;

const NODES_PREFIX: &str = cgn_core::etcd_keys::NODES;
const POLICY_KEY: &str = cgn_core::etcd_keys::ROUTING;
const CORDON_PREFIX: &str = cgn_core::etcd_keys::CORDON;
const KV_CONFIRMED_PREFIX: &str = cgn_core::etcd_keys::KV_CONFIRMED;

/// Parse a confirmed-KV etcd key `<KV_CONFIRMED>{node_id}/{digest_hex}`
/// into its node id and 32-byte digest.
fn parse_kv_confirmed_key(key: &str) -> Option<(&str, [u8; 32])> {
    let rest = key.strip_prefix(KV_CONFIRMED_PREFIX)?;
    let (node_id, hex) = rest.rsplit_once('/')?;
    if node_id.is_empty() || hex.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    for (i, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some((node_id, digest))
}

pub async fn run_etcd_watcher(
    endpoints: Vec<String>,
    nodes: Arc<NodeRegistry>,
    policy: Arc<ArcSwap<RoutingPolicy>>,
    prefix: Arc<cgn_core::prefix::PrefixIndex>,
) -> Result<()> {
    let mut client = Client::connect(&endpoints, None)
        .await
        .map_err(|e| Error::Etcd(format!("connect: {e}")))?;

    // Initial snapshot.
    let snap = client
        .get(NODES_PREFIX, Some(GetOptions::new().with_prefix()))
        .await
        .map_err(|e| Error::Etcd(format!("get nodes: {e}")))?;
    for kv in snap.kvs() {
        if let Ok(entry) = serde_json::from_slice::<super::NodeEntry>(kv.value()) {
            nodes.upsert(entry);
        }
    }
    if let Ok(snap) = client.get(POLICY_KEY, None).await {
        if let Some(kv) = snap.kvs().first() {
            if let Ok(p) = serde_json::from_slice::<RoutingPolicy>(kv.value()) {
                policy.store(Arc::new(p));
            }
        }
    }
    // Apply any cordon flags written by `cgn-ctl cluster cordon` before
    // we started.
    if let Ok(snap) = client
        .get(CORDON_PREFIX, Some(GetOptions::new().with_prefix()))
        .await
    {
        for kv in snap.kvs() {
            if let Ok(key) = kv.key_str() {
                if let Some(node_id) = key.strip_prefix(CORDON_PREFIX) {
                    nodes.set_cordon(node_id, true);
                }
            }
        }
    }

    // Live watch.
    let (mut watcher, mut stream) = client
        .watch(NODES_PREFIX, Some(WatchOptions::new().with_prefix()))
        .await
        .map_err(|e| Error::Etcd(format!("watch: {e}")))?;
    let _ = watcher.request_progress().await;

    let policy_clone = policy.clone();
    let policy_endpoints = endpoints.clone();
    tokio::spawn(async move {
        let mut p_client = match Client::connect(&policy_endpoints, None).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error=?e, "policy watcher: connect failed");
                return;
            }
        };
        if let Ok((_w, mut s)) = p_client.watch(POLICY_KEY, None).await {
            while let Ok(Some(resp)) = s.message().await {
                for ev in resp.events() {
                    if matches!(ev.event_type(), EventType::Put) {
                        if let Some(kv) = ev.kv() {
                            if let Ok(p) = serde_json::from_slice::<RoutingPolicy>(kv.value()) {
                                policy_clone.store(Arc::new(p));
                                tracing::info!("routing policy updated");
                            }
                        }
                    }
                }
            }
        }
    });

    // Cordon watcher. Tracks user-set drains written by
    // `cgn-ctl cluster cordon <id>` and toggles the corresponding
    // `NodeEntry::cordoned` flag so scoring excludes the node.
    let nodes_for_cordon = nodes.clone();
    let cordon_endpoints = endpoints.clone();
    tokio::spawn(async move {
        let mut c_client = match Client::connect(&cordon_endpoints, None).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error=?e, "cordon watcher: connect failed");
                return;
            }
        };
        let opts = WatchOptions::new().with_prefix();
        if let Ok((_w, mut s)) = c_client.watch(CORDON_PREFIX, Some(opts)).await {
            while let Ok(Some(resp)) = s.message().await {
                for ev in resp.events() {
                    let Some(kv) = ev.kv() else { continue };
                    let Ok(key) = kv.key_str() else { continue };
                    let Some(node_id) = key.strip_prefix(CORDON_PREFIX) else {
                        continue;
                    };
                    match ev.event_type() {
                        EventType::Put => {
                            tracing::info!(%node_id, "cordon set");
                            nodes_for_cordon.set_cordon(node_id, true);
                        }
                        EventType::Delete => {
                            tracing::info!(%node_id, "cordon cleared");
                            nodes_for_cordon.set_cordon(node_id, false);
                        }
                    }
                }
            }
        }
    });

    // Confirmed-KV watcher. Agents publish lease-bound claims for prefix
    // digests *after* a generation completes; mirroring PUT/DELETE into
    // the PrefixIndex turns the overlap score into a truth-fed signal
    // (claims die with the node's lease and are pruned by the agent
    // under cache pressure).
    let prefix_for_kv = prefix.clone();
    let kv_endpoints = endpoints.clone();
    tokio::spawn(async move {
        let mut k_client = match Client::connect(&kv_endpoints, None).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error=?e, "kv-confirmed watcher: connect failed");
                return;
            }
        };
        // Initial snapshot so a restarted router inherits live claims.
        if let Ok(snap) = k_client
            .get(KV_CONFIRMED_PREFIX, Some(GetOptions::new().with_prefix()))
            .await
        {
            for kv in snap.kvs() {
                if let Some((node_id, digest)) = kv.key_str().ok().and_then(parse_kv_confirmed_key)
                {
                    prefix_for_kv.insert(digest, node_id);
                }
            }
        }
        let opts = WatchOptions::new().with_prefix();
        if let Ok((_w, mut s)) = k_client.watch(KV_CONFIRMED_PREFIX, Some(opts)).await {
            while let Ok(Some(resp)) = s.message().await {
                for ev in resp.events() {
                    let Some(kv) = ev.kv() else { continue };
                    let Some((node_id, digest)) =
                        kv.key_str().ok().and_then(parse_kv_confirmed_key)
                    else {
                        continue;
                    };
                    match ev.event_type() {
                        EventType::Put => prefix_for_kv.insert(digest, node_id),
                        EventType::Delete => prefix_for_kv.forget_claim(&digest, node_id),
                    }
                }
            }
        }
    });

    while let Ok(Some(resp)) = stream.message().await {
        for ev in resp.events() {
            let Some(kv) = ev.kv() else { continue };
            match ev.event_type() {
                EventType::Put => {
                    if let Ok(entry) = serde_json::from_slice::<super::NodeEntry>(kv.value()) {
                        // KV-cache pressure: when the engine reports < 5%
                        // free blocks it is LRU-evicting, so our older
                        // optimistic prefix claims for this node are the
                        // ones most likely gone. Prune the stale half.
                        if entry.total_blocks > 0
                            && (entry.free_blocks as f32 / entry.total_blocks as f32) < 0.05
                        {
                            prefix.forget_node_stale(&entry.node_id, prefix.ttl() / 2);
                        }
                        nodes.upsert(entry);
                    } else {
                        tracing::warn!(key = %kv.key_str().unwrap_or("?"), "bad node entry");
                    }
                }
                EventType::Delete => {
                    if let Some(id) = kv.key_str().ok().and_then(|s| s.strip_prefix(NODES_PREFIX)) {
                        nodes.forget(id);
                        // Node went away (lease expired or drained): its KV
                        // blocks are no longer routable; purge it from the
                        // prefix index so overlap scoring stops chasing it.
                        prefix.forget_node(id);
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_kv_confirmed_key;

    #[test]
    fn parses_valid_kv_confirmed_key() {
        let hex = "ab".repeat(32);
        let key = format!("{}node-a/{hex}", super::KV_CONFIRMED_PREFIX);
        let (node, digest) = parse_kv_confirmed_key(&key).unwrap();
        assert_eq!(node, "node-a");
        assert_eq!(digest, [0xab; 32]);
    }

    #[test]
    fn rejects_malformed_keys() {
        assert!(parse_kv_confirmed_key("/other/x").is_none());
        let short = format!("{}node-a/abcd", super::KV_CONFIRMED_PREFIX);
        assert!(parse_kv_confirmed_key(&short).is_none());
        let no_slash = format!("{}{}", super::KV_CONFIRMED_PREFIX, "ff".repeat(32));
        assert!(parse_kv_confirmed_key(&no_slash).is_none());
    }
}
