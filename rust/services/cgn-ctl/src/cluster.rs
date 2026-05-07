//! `cgn-ctl cluster …`
//!
//! Thin client over the same etcd key prefixes the router watches:
//!
//! * `nodes` — `GET /cognitora/nodes/*` and pretty-print one row per agent.
//! * `cordon` — `PUT /cognitora/cordon/<node_id>` so the router scoring
//!   skips this node. `uncordon` deletes the same key.
//! * `drain` — connect to the agent's gRPC endpoint (read from its
//!   `/cognitora/nodes/<id>` entry) and call `Agent.Drain`. The agent
//!   supervisor exits cleanly; the autoscaler picks up the gap on its
//!   next tick.
//!
//! The `Control` gRPC service in `proto/cognitora/v1/control.proto` is
//! the eventual home for these RPCs; the etcd path is what works today
//! across both bare-metal and Kubernetes deployments without depending
//! on a Control server being live on the router.

use std::path::Path;

use cgn_core::{etcd_keys, Error, Result};
use cgn_proto::v1::agent_client::AgentClient;
use clap::Subcommand;
use etcd_client::GetOptions;
use serde::Deserialize;
use tonic::transport::Endpoint;
use tracing::{info, warn};

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// List nodes registered in etcd.
    Nodes,
    /// Cordon a node so the router stops sending it new traffic.
    /// Does not interrupt requests already inflight on that node.
    Cordon { node_id: String },
    /// Remove a previously-set cordon.
    Uncordon { node_id: String },
    /// Tell the agent to stop accepting new requests, drain inflight
    /// work, then exit cleanly. The agent's etcd lease then expires.
    Drain { node_id: String },
}

pub async fn run(cmd: Cmd, cfg_path: Option<&Path>) -> Result<()> {
    match cmd {
        Cmd::Nodes => nodes(cfg_path).await,
        Cmd::Cordon { node_id } => set_cordon(cfg_path, &node_id, true).await,
        Cmd::Uncordon { node_id } => set_cordon(cfg_path, &node_id, false).await,
        Cmd::Drain { node_id } => drain(cfg_path, &node_id).await,
    }
}

#[derive(Debug, Deserialize)]
struct NodeRow {
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    address: String,
    #[serde(default)]
    role: i32,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    queue_depth: u32,
    #[serde(default)]
    power_watts: f32,
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    version: String,
}

async fn nodes(cfg_path: Option<&Path>) -> Result<()> {
    let mut client = crate::etcd::connect(cfg_path).await?;
    let resp = client
        .get(etcd_keys::NODES, Some(GetOptions::new().with_prefix()))
        .await
        .map_err(|e| Error::Etcd(format!("get nodes: {e}")))?;
    let kvs = resp.kvs();

    // Same prefix carries the cordon flags; collect them so the table
    // can reflect user-set drains alongside live registrations.
    let cordoned: std::collections::HashSet<String> = client
        .get(etcd_keys::CORDON, Some(GetOptions::new().with_prefix()))
        .await
        .map(|r| {
            r.kvs()
                .iter()
                .filter_map(|kv| kv.key_str().ok().map(String::from))
                .filter_map(|k| k.strip_prefix(etcd_keys::CORDON).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    if kvs.is_empty() {
        println!("(no nodes registered under {})", etcd_keys::NODES);
        return Ok(());
    }

    println!(
        "{:<24} {:<32} {:<8} {:<24} {:>5} {:>6} {:>9} VERSION",
        "NODE_ID", "ADDRESS", "ROLE", "MODEL", "READY", "QDEPTH", "WATTS"
    );
    for kv in kvs {
        let row: NodeRow = match serde_json::from_slice(kv.value()) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    key = %kv.key_str().unwrap_or("?"),
                    error = %e,
                    "skipping malformed node entry"
                );
                continue;
            }
        };
        let role = role_label(row.role);
        let ready = if row.ready { "yes" } else { "no" };
        let mut node_label = row.node_id.clone();
        if cordoned.contains(&row.node_id) {
            node_label.push_str(" *");
        }
        println!(
            "{:<24} {:<32} {:<8} {:<24} {:>5} {:>6} {:>8.1}W {}",
            node_label,
            row.address,
            role,
            row.model.unwrap_or_else(|| "-".into()),
            ready,
            row.queue_depth,
            row.power_watts,
            if row.version.is_empty() {
                "-"
            } else {
                &row.version
            },
        );
    }
    if !cordoned.is_empty() {
        println!("\n* = cordoned (router skips this node when scoring)");
    }
    Ok(())
}

fn role_label(role: i32) -> &'static str {
    match cgn_proto::v1::NodeRole::try_from(role).unwrap_or(cgn_proto::v1::NodeRole::Unspecified) {
        cgn_proto::v1::NodeRole::Prefill => "prefill",
        cgn_proto::v1::NodeRole::Decode => "decode",
        cgn_proto::v1::NodeRole::Both => "both",
        cgn_proto::v1::NodeRole::Unspecified => "?",
    }
}

async fn set_cordon(cfg_path: Option<&Path>, node_id: &str, on: bool) -> Result<()> {
    let mut client = crate::etcd::connect(cfg_path).await?;
    let key = format!("{}{}", etcd_keys::CORDON, node_id);
    if on {
        let body = serde_json::json!({
            "node_id":  node_id,
            "stamp":    chrono::Utc::now().to_rfc3339(),
            "actor":    actor_label(),
        });
        client
            .put(key, body.to_string(), None)
            .await
            .map_err(|e| Error::Etcd(format!("put cordon: {e}")))?;
        info!(%node_id, "node cordoned");
    } else {
        client
            .delete(key, None)
            .await
            .map_err(|e| Error::Etcd(format!("delete cordon: {e}")))?;
        info!(%node_id, "node uncordoned");
    }
    Ok(())
}

async fn drain(cfg_path: Option<&Path>, node_id: &str) -> Result<()> {
    let address = lookup_node_address(cfg_path, node_id).await?;
    info!(%node_id, %address, "calling Agent.Drain");

    let endpoint = Endpoint::from_shared(address.clone())
        .map_err(|e| Error::Internal(format!("agent endpoint {address}: {e}")))?
        .timeout(std::time::Duration::from_secs(10));
    let chan = endpoint
        .connect()
        .await
        .map_err(|e| Error::Unavailable(format!("agent connect {address}: {e}")))?;
    let mut client = AgentClient::new(chan);

    let resp = client
        .drain(())
        .await
        .map_err(|s| Error::Internal(format!("Agent.Drain: {s}")))?
        .into_inner();
    info!(code = resp.code, message = %resp.message, "drain acknowledged");
    Ok(())
}

async fn lookup_node_address(cfg_path: Option<&Path>, node_id: &str) -> Result<String> {
    let mut client = crate::etcd::connect(cfg_path).await?;
    let key = format!("{}{}", etcd_keys::NODES, node_id);
    let resp = client
        .get(key.as_str(), None)
        .await
        .map_err(|e| Error::Etcd(format!("get {key}: {e}")))?;
    let kv = resp
        .kvs()
        .first()
        .ok_or_else(|| Error::NotFound(format!("node {node_id} not registered in etcd")))?;
    let row: NodeRow = serde_json::from_slice(kv.value())
        .map_err(|e| Error::Internal(format!("decode node entry: {e}")))?;
    if row.address.is_empty() {
        return Err(Error::Internal(format!(
            "node {node_id} has no address in its etcd entry"
        )));
    }
    Ok(row.address)
}

fn actor_label() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "cgn-ctl".into())
}
