//! KV block addressing and metadata.

use serde::{Deserialize, Serialize};

/// Stable address of a KV block. The 32-byte digest is the prefix hash
/// (BLAKE3 over `TOKENS_PER_CHUNK` token IDs salted by model name) and the
/// `layer` selects a transformer layer's KV slab.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct BlockAddress {
    pub digest: [u8; 32],
    pub layer: u32,
}

/// Metadata returned by the index alongside a block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockMeta {
    pub model: String,
    pub layer: u32,
    pub bytes: u64,
    pub created_unix: u64,
    pub last_seen_unix: u64,
    pub tier: super::TierKind,
}

/// Handle to an in-memory or on-disk block. The transport layer streams
/// the underlying bytes; the metadata stays small.
#[derive(Debug, Clone)]
pub struct BlockHandle {
    pub meta: BlockMeta,
    pub addr: BlockAddress,
}
