//! Prefix-aware lookup index used by the router.
//!
//! Each chunk hash (32 bytes, BLAKE3 over `TOKENS_PER_CHUNK` token ids — see
//! [`crate::hash`]) is mapped to the set of nodes that currently hold that
//! chunk, with TTL and last-seen timestamps for staleness control.
//!
//! The index is concurrent-safe and read-mostly. It is rebuilt incrementally
//! from etcd / gossip events; the router queries it on every request to
//! compute KV-overlap scores.

use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::RwLock;

/// Per-node entry stored under each prefix.
#[derive(Debug, Clone)]
pub struct NodeEntry {
    pub node_id:    String,
    pub last_seen:  Instant,
}

/// Concurrent prefix → nodes index. The internal map is `DashMap` keyed by
/// the 32-byte digest. Per-prefix node lists are guarded by a `RwLock` to
/// avoid lock contention on hot keys.
#[derive(Default)]
pub struct PrefixIndex {
    /// digest → list of nodes that own this chunk.
    inner: DashMap<[u8; 32], RwLock<Vec<NodeEntry>>>,
    /// Entries older than this are garbage-collected on access.
    ttl: Duration,
}

impl PrefixIndex {
    pub fn new(ttl: Duration) -> Self {
        Self { inner: DashMap::new(), ttl }
    }

    /// Record that `node_id` currently holds `digest`.
    pub fn insert(&self, digest: [u8; 32], node_id: &str) {
        let now = Instant::now();
        let entry = self.inner.entry(digest).or_default();
        let mut v = entry.write();
        if let Some(e) = v.iter_mut().find(|e| e.node_id == node_id) {
            e.last_seen = now;
        } else {
            v.push(NodeEntry { node_id: node_id.to_string(), last_seen: now });
        }
    }

    /// Drop a node from the index entirely (invoked on graceful drain).
    pub fn forget_node(&self, node_id: &str) {
        for mut e in self.inner.iter_mut() {
            e.value_mut().write().retain(|n| n.node_id != node_id);
        }
        // Lazy purge of empty entries.
        self.inner.retain(|_, v| !v.read().is_empty());
    }

    /// Look up the live nodes that currently hold `digest`.
    pub fn lookup(&self, digest: &[u8; 32]) -> Vec<String> {
        let Some(entry) = self.inner.get(digest) else { return Vec::new() };
        let now = Instant::now();
        let g = entry.read();
        g.iter()
            .filter(|n| now.duration_since(n.last_seen) < self.ttl)
            .map(|n| n.node_id.clone())
            .collect()
    }

    /// Compute, for every node, how many of `digests` it holds. Returns a
    /// hash map of `node_id -> count`. The router uses this to score nodes
    /// by KV overlap.
    pub fn overlap(&self, digests: &[[u8; 32]]) -> std::collections::HashMap<String, usize> {
        let mut out: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let now = Instant::now();
        for d in digests {
            if let Some(entry) = self.inner.get(d) {
                let g = entry.read();
                for n in g.iter() {
                    if now.duration_since(n.last_seen) < self.ttl {
                        *out.entry(n.node_id.clone()).or_insert(0) += 1;
                    }
                }
            }
        }
        out
    }

    /// Total tracked digests.
    pub fn len(&self) -> usize { self.inner.len() }
    pub fn is_empty(&self) -> bool { self.inner.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(byte: u8) -> [u8; 32] { let mut x = [0u8; 32]; x[0] = byte; x }

    #[test]
    fn insert_and_lookup() {
        let ix = PrefixIndex::new(Duration::from_secs(60));
        ix.insert(d(1), "node-a");
        ix.insert(d(1), "node-b");
        ix.insert(d(2), "node-a");
        assert_eq!(ix.len(), 2);

        let mut nodes = ix.lookup(&d(1));
        nodes.sort();
        assert_eq!(nodes, vec!["node-a".to_string(), "node-b".to_string()]);
    }

    #[test]
    fn forget_node_purges_entries() {
        let ix = PrefixIndex::new(Duration::from_secs(60));
        ix.insert(d(1), "node-a");
        ix.insert(d(2), "node-a");
        ix.forget_node("node-a");
        assert!(ix.is_empty());
    }

    #[test]
    fn overlap_counts() {
        let ix = PrefixIndex::new(Duration::from_secs(60));
        ix.insert(d(1), "n1");
        ix.insert(d(2), "n1");
        ix.insert(d(3), "n2");
        let counts = ix.overlap(&[d(1), d(2), d(3), d(4)]);
        assert_eq!(counts.get("n1").copied(), Some(2));
        assert_eq!(counts.get("n2").copied(), Some(1));
    }
}
