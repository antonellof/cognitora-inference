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

pub async fn run_etcd_watcher(
    endpoints: Vec<String>,
    nodes: Arc<NodeRegistry>,
    policy: Arc<ArcSwap<RoutingPolicy>>,
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

    while let Ok(Some(resp)) = stream.message().await {
        for ev in resp.events() {
            let Some(kv) = ev.kv() else { continue };
            match ev.event_type() {
                EventType::Put => {
                    if let Ok(entry) = serde_json::from_slice::<super::NodeEntry>(kv.value()) {
                        nodes.upsert(entry);
                    } else {
                        tracing::warn!(key = %kv.key_str().unwrap_or("?"), "bad node entry");
                    }
                }
                EventType::Delete => {
                    if let Some(id) = kv.key_str().ok().and_then(|s| s.strip_prefix(NODES_PREFIX)) {
                        nodes.forget(id);
                    }
                }
            }
        }
    }
    Ok(())
}
