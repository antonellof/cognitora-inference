//! Tiny shared etcd helper for the CLI.
//!
//! `cgn-ctl` is intentionally stateless — every invocation reads the
//! cognitora.toml lookup path (`-c <path>` → `$CGN_CONFIG` →
//! `/etc/cognitora/cognitora.toml`) just to discover the etcd
//! endpoints. The actual cluster state lives in etcd; the CLI is a
//! thin client over the same key prefixes the router watches.

use std::path::Path;

use cgn_core::{config::Config, Error, Result};
use etcd_client::Client;

/// Resolve etcd endpoints from the configured cognitora.toml.
pub fn endpoints_from_config(path: Option<&Path>) -> Result<Vec<String>> {
    let cfg_path = Config::locate(path);
    let cfg = Config::load(&cfg_path)?;
    if cfg.cluster.etcd_endpoints.is_empty() {
        return Err(Error::Config(format!(
            "no [cluster].etcd_endpoints in {}; the CLI needs at least one \
             reachable etcd node for cluster/model commands",
            cfg_path.display(),
        )));
    }
    Ok(cfg.cluster.etcd_endpoints)
}

/// Connect to etcd via the configured endpoints.
pub async fn connect(path: Option<&Path>) -> Result<Client> {
    let endpoints = endpoints_from_config(path)?;
    Client::connect(&endpoints, None)
        .await
        .map_err(|e| Error::Etcd(format!("connect {endpoints:?}: {e}")))
}
