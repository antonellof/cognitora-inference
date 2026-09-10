//! Autoscaler hint consumer — closes the energy-aware scaling loop.
//!
//! `cgn-router`'s autoscaler writes per-node drain hints to etcd at
//! `/cognitora/autoscaler/<node_id>` (`{"drain": bool, "reason": …}`).
//! Until 0.6 nothing consumed them. This task polls the hints and
//! translates them into cordon flags at `/cognitora/cordon/<node_id>`,
//! which the router's cluster watcher already honors: cordoned nodes
//! are excluded from candidate selection, so a drained node stops
//! receiving traffic and its engine can idle down (or the node can be
//! powered off by an external policy).
//!
//! Cordons set by this loop are tagged `"set_by": "autoscaler"` so we
//! never clear a cordon placed manually via `cgn-ctl cluster cordon`.
//!
//! etcd endpoints come from the standard Cognitora config
//! (`[cluster].etcd_endpoints`), resolved via `CGN_CONFIG` /
//! `/etc/cognitora/cognitora.toml` — same lookup as every other daemon.
//! Without etcd the task exits quietly (single-node mode has nothing
//! to scale).

use std::time::Duration;

use cgn_core::{Error, Result};
use etcd_client::{Client, GetOptions};
use serde::Deserialize;
use tracing::{info, warn};

const HINT_PREFIX: &str = "/cognitora/autoscaler/";
const POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct Hint {
    drain: bool,
    #[serde(default)]
    reason: String,
}

pub async fn run(etcd_endpoints: Vec<String>) -> Result<()> {
    if etcd_endpoints.is_empty() {
        info!("no etcd endpoints configured; autoscaler consumer disabled");
        return Ok(());
    }
    info!("autoscaler hint consumer running");
    loop {
        if let Err(e) = tick(&etcd_endpoints).await {
            warn!(error=?e, "autoscaler consumer tick failed");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn tick(endpoints: &[String]) -> Result<()> {
    let mut client = Client::connect(endpoints, None)
        .await
        .map_err(|e| Error::Etcd(format!("connect: {e}")))?;

    let hints = client
        .get(HINT_PREFIX, Some(GetOptions::new().with_prefix()))
        .await
        .map_err(|e| Error::Etcd(format!("get hints: {e}")))?;

    for kv in hints.kvs() {
        let Ok(key) = kv.key_str() else { continue };
        let Some(node_id) = key.strip_prefix(HINT_PREFIX) else {
            continue;
        };
        let Ok(hint) = serde_json::from_slice::<Hint>(kv.value()) else {
            warn!(%node_id, "unparsable autoscaler hint; skipping");
            continue;
        };
        apply_hint(&mut client, node_id, &hint).await?;
    }
    Ok(())
}

async fn apply_hint(client: &mut Client, node_id: &str, hint: &Hint) -> Result<()> {
    let cordon_key = format!("{}{}", cgn_core::etcd_keys::CORDON, node_id);
    let existing = client
        .get(cordon_key.as_str(), None)
        .await
        .map_err(|e| Error::Etcd(format!("get cordon: {e}")))?;
    let existing_val: Option<String> = existing
        .kvs()
        .first()
        .and_then(|kv| kv.value_str().ok().map(String::from));

    if hint.drain {
        // Already cordoned (by anyone) → nothing to do.
        if existing_val.is_some() {
            return Ok(());
        }
        let body = serde_json::json!({
            "set_by": "autoscaler",
            "reason": hint.reason,
            "stamp":  chrono::Utc::now().to_rfc3339(),
        });
        client
            .put(cordon_key.as_str(), body.to_string(), None)
            .await
            .map_err(|e| Error::Etcd(format!("put cordon: {e}")))?;
        info!(%node_id, reason = %hint.reason, "autoscaler: node cordoned (drain)");
    } else {
        // Only clear cordons we set ourselves; never touch manual ones.
        let ours = existing_val
            .as_deref()
            .map(is_autoscaler_cordon)
            .unwrap_or(false);
        if ours {
            client
                .delete(cordon_key.as_str(), None)
                .await
                .map_err(|e| Error::Etcd(format!("delete cordon: {e}")))?;
            info!(%node_id, "autoscaler: cordon cleared (capacity restored)");
        }
    }
    Ok(())
}

fn is_autoscaler_cordon(value: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()
        .and_then(|v| v.get("set_by").and_then(|s| s.as_str()).map(String::from))
        .is_some_and(|s| s == "autoscaler")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_own_cordons() {
        assert!(is_autoscaler_cordon(
            r#"{"set_by":"autoscaler","reason":"idle"}"#
        ));
        assert!(!is_autoscaler_cordon(r#"{"set_by":"cgn-ctl"}"#));
        assert!(!is_autoscaler_cordon(r#"{}"#));
        assert!(!is_autoscaler_cordon("not-json"));
    }

    #[test]
    fn hint_parses() {
        let h: Hint = serde_json::from_str(r#"{"drain":true,"reason":"idle","watts":410.0}"#)
            .expect("hint with extra fields parses");
        assert!(h.drain);
        assert_eq!(h.reason, "idle");
        let h: Hint = serde_json::from_str(r#"{"drain":false}"#).unwrap();
        assert!(!h.drain);
    }
}
