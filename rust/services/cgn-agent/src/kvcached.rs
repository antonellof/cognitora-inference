//! Host-local `cgn-kvcached` lookups from the agent.

use cgn_core::config::Config;
use cgn_proto::v1::{kv_client::KvClient, HashList};

/// Return prefix digests currently resident in the local kvcached tiers.
pub async fn resident_digests(cfg: &Config, digests: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let values: Vec<Vec<u8>> = digests
        .iter()
        .filter(|d| d.len() == 32)
        .cloned()
        .collect();
    if values.is_empty() {
        return Vec::new();
    }

    let listen = cfg.kv.listen.replace("0.0.0.0", "127.0.0.1");
    let uri = format!("http://{listen}");
    let mut kv = match KvClient::connect(uri).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let resp = match kv.batch_lookup(HashList { values }).await {
        Ok(r) => r.into_inner(),
        Err(_) => return Vec::new(),
    };
    resp.entries
        .into_iter()
        .filter(|e| e.size_bytes > 0 && e.prefix_hash.len() == 32)
        .map(|e| e.prefix_hash)
        .collect()
}
