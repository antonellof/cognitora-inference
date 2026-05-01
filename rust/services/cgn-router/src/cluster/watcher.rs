//! etcd watcher → keeps `NodeRegistry` and `RoutingPolicy` in sync.

use std::sync::Arc;

use arc_swap::ArcSwap;
use cgn_core::{Error, Result};
use etcd_client::{Client, EventType, GetOptions, WatchOptions};

use super::NodeRegistry;
use crate::state::RoutingPolicy;

const NODES_PREFIX: &str = cgn_core::etcd_keys::NODES;
const POLICY_KEY: &str = cgn_core::etcd_keys::ROUTING;

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

    // Live watch.
    let (mut watcher, mut stream) = client
        .watch(NODES_PREFIX, Some(WatchOptions::new().with_prefix()))
        .await
        .map_err(|e| Error::Etcd(format!("watch: {e}")))?;
    let _ = watcher.request_progress().await;

    let policy_clone = policy.clone();
    let nodes_clone = nodes.clone();
    tokio::spawn(async move {
        let mut p_client = match Client::connect(&endpoints, None).await {
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
        let _ = nodes_clone;
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
