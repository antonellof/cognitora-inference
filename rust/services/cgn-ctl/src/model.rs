//! `cgn-ctl model …`
//!
//! Manage `ModelPool`-shaped desired-state documents under
//! `/cognitora/models/<name>` in etcd. The operator's `ModelPool`
//! controller renders these into ConfigMaps when the cluster runs on
//! Kubernetes; on bare metal the agents read `/cognitora/models/*`
//! straight from etcd on their next heartbeat cycle.
//!
//! `ls` shows both the *desired* state from `/cognitora/models/*` and
//! the *actually-loaded* model reported by each agent under
//! `/cognitora/nodes/*` so operators can see drift at a glance.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cgn_core::{etcd_keys, Error, Result};
use clap::Subcommand;
use etcd_client::GetOptions;
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Apply a ModelPool spec (loads / unloads the engine on matching nodes).
    Load {
        name: String,
        #[arg(long, default_value_t = 1)]
        tp: u32,
        #[arg(long, default_value_t = 1)]
        prefill_replicas: u32,
        #[arg(long, default_value_t = 2)]
        decode_replicas: u32,
        /// Optional max sequence length passed to the engine.
        #[arg(long)]
        max_model_len: Option<u32>,
        /// Optional cascade: tier name → resolves to a recipe profile.
        #[arg(long)]
        cascade: Option<String>,
        /// Free-form engine flags (`--key=val`) appended to the spawn line.
        #[arg(long)]
        extra: Vec<String>,
    },
    /// Tear down a ModelPool.
    Unload { name: String },
    /// List currently loaded models.
    Ls,
}

#[derive(Debug, Serialize, Deserialize)]
struct ModelDoc {
    name: String,
    tp: u32,
    prefill_replicas: u32,
    decode_replicas: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_model_len: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cascade: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    extra_args: Vec<String>,
    /// RFC 3339 timestamp of the last `cgn-ctl model load`.
    stamp: String,
}

#[derive(Debug, Deserialize)]
struct AgentRow {
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    ready: bool,
}

pub async fn run(cmd: Cmd, cfg_path: Option<&Path>) -> Result<()> {
    match cmd {
        Cmd::Load {
            name,
            tp,
            prefill_replicas,
            decode_replicas,
            max_model_len,
            cascade,
            extra,
        } => {
            let doc = ModelDoc {
                name: name.clone(),
                tp,
                prefill_replicas,
                decode_replicas,
                max_model_len,
                cascade,
                extra_args: extra,
                stamp: chrono::Utc::now().to_rfc3339(),
            };
            put_model(cfg_path, &doc).await?;
            info!(
                %name, tp, prefill_replicas, decode_replicas,
                "model load: wrote desired state to etcd"
            );
            Ok(())
        }
        Cmd::Unload { name } => {
            delete_model(cfg_path, &name).await?;
            info!(%name, "model unload: removed from etcd");
            Ok(())
        }
        Cmd::Ls => list(cfg_path).await,
    }
}

async fn put_model(cfg_path: Option<&Path>, doc: &ModelDoc) -> Result<()> {
    let mut client = crate::etcd::connect(cfg_path).await?;
    let key = format!("{}{}", etcd_keys::MODELS, doc.name);
    let body =
        serde_json::to_string(&doc).map_err(|e| Error::Internal(format!("encode model: {e}")))?;
    client
        .put(key, body, None)
        .await
        .map_err(|e| Error::Etcd(format!("put model: {e}")))?;
    Ok(())
}

async fn delete_model(cfg_path: Option<&Path>, name: &str) -> Result<()> {
    let mut client = crate::etcd::connect(cfg_path).await?;
    let key = format!("{}{}", etcd_keys::MODELS, name);
    client
        .delete(key, None)
        .await
        .map_err(|e| Error::Etcd(format!("delete model: {e}")))?;
    Ok(())
}

async fn list(cfg_path: Option<&Path>) -> Result<()> {
    let mut client = crate::etcd::connect(cfg_path).await?;

    let desired = client
        .get(etcd_keys::MODELS, Some(GetOptions::new().with_prefix()))
        .await
        .map_err(|e| Error::Etcd(format!("get models: {e}")))?;
    let mut docs: BTreeMap<String, ModelDoc> = BTreeMap::new();
    for kv in desired.kvs() {
        if let Ok(doc) = serde_json::from_slice::<ModelDoc>(kv.value()) {
            docs.insert(doc.name.clone(), doc);
        }
    }

    let nodes = client
        .get(etcd_keys::NODES, Some(GetOptions::new().with_prefix()))
        .await
        .map_err(|e| Error::Etcd(format!("get nodes: {e}")))?;
    let mut loaded: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for kv in nodes.kvs() {
        if let Ok(row) = serde_json::from_slice::<AgentRow>(kv.value()) {
            if row.ready {
                if let Some(model) = row.model {
                    loaded.entry(model).or_default().insert(row.node_id);
                }
            }
        }
    }

    if docs.is_empty() && loaded.is_empty() {
        println!(
            "(no models registered under {} and no agent reports a loaded model)",
            etcd_keys::MODELS
        );
        return Ok(());
    }

    println!(
        "{:<32} {:>3} {:>8} {:>7} {:>16} {:<24} LIVE_NODES",
        "MODEL", "TP", "PREFILL", "DECODE", "MAX_LEN", "CASCADE"
    );
    let mut all_names: BTreeSet<String> = BTreeSet::new();
    all_names.extend(docs.keys().cloned());
    all_names.extend(loaded.keys().cloned());
    for name in &all_names {
        let live = loaded
            .get(name)
            .map(|s| s.iter().cloned().collect::<Vec<_>>().join(","))
            .unwrap_or_else(|| "-".into());
        match docs.get(name) {
            Some(d) => println!(
                "{:<32} {:>3} {:>8} {:>7} {:>16} {:<24} {}",
                name,
                d.tp,
                d.prefill_replicas,
                d.decode_replicas,
                d.max_model_len
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".into()),
                d.cascade.clone().unwrap_or_else(|| "-".into()),
                live,
            ),
            None => println!(
                "{:<32} {:>3} {:>8} {:>7} {:>16} {:<24} {}    (drift: live but no desired-state)",
                name, "?", "?", "?", "?", "-", live,
            ),
        }
    }
    Ok(())
}
