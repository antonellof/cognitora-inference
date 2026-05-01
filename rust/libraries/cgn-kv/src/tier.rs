//! KV tiering: GPU (hot) → RAM (warm) → SSD (cold).
//!
//! `cgn-kvcached` runs three tiers per node and a small policy engine that
//! promotes / demotes blocks based on access frequency and pressure. This
//! module defines the trait surface; concrete implementations are gated
//! behind features so a developer can build the crate without CUDA.

use serde::{Deserialize, Serialize};

use super::block::{BlockAddress, BlockHandle};

/// Coarse identification of where a block lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TierKind {
    Gpu,
    Ram,
    Ssd,
}

/// A storage tier that holds KV blocks.
///
/// All operations are synchronous and intentionally infallible at the trait
/// level (errors are logged + counted in metrics by the caller). Per-tier
/// implementations decide their own admission policy.
pub trait Tier: Send + Sync {
    fn kind(&self) -> TierKind;

    /// Probe membership without touching the bytes.
    fn contains(&self, addr: &BlockAddress) -> bool;

    /// Best-effort fetch a handle. Returns `None` on miss.
    fn get(&self, addr: &BlockAddress) -> Option<BlockHandle>;

    /// Insert or replace a block. Returns whether a previous version was evicted.
    fn put(&self, addr: BlockAddress, bytes: bytes::Bytes) -> bool;

    /// Drop a single block.
    fn evict(&self, addr: &BlockAddress);

    /// Total occupied bytes (approximate).
    fn used_bytes(&self) -> u64;

    /// Total capacity in bytes.
    fn capacity_bytes(&self) -> u64;
}

/// In-memory RAM tier (DashMap-backed, LRU-ish via touch timestamps).
///
/// Useful for small dev clusters and as a fallback when no GPU pinning
/// pool is available. Production deployments configure a real HBM/CMB
/// allocator inside `cgn-kvcached`.
pub struct RamTier {
    inner: dashmap::DashMap<BlockAddress, RamSlot>,
    capacity: u64,
}

struct RamSlot {
    bytes: bytes::Bytes,
    last:  std::sync::atomic::AtomicU64,
}

impl RamTier {
    pub fn new(capacity_bytes: u64) -> Self {
        Self { inner: dashmap::DashMap::new(), capacity: capacity_bytes }
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default()
    }

    /// Return a clone of the bytes for `addr`, or `None` on miss.
    /// Used by the QUIC transport to serve `Pull` requests without
    /// re-hashing the prefix.
    pub fn get_bytes(&self, addr: &BlockAddress) -> Option<bytes::Bytes> {
        let slot = self.inner.get(addr)?;
        slot.last.store(Self::now(), std::sync::atomic::Ordering::Relaxed);
        Some(slot.bytes.clone())
    }

    /// Approximate count of resident blocks.
    pub fn block_count(&self) -> usize { self.inner.len() }
}

impl Tier for RamTier {
    fn kind(&self) -> TierKind { TierKind::Ram }

    fn contains(&self, addr: &BlockAddress) -> bool {
        self.inner.contains_key(addr)
    }

    fn get(&self, addr: &BlockAddress) -> Option<BlockHandle> {
        let slot = self.inner.get(addr)?;
        slot.last.store(Self::now(), std::sync::atomic::Ordering::Relaxed);
        Some(BlockHandle {
            addr: *addr,
            meta: super::BlockMeta {
                model: String::new(),
                layer: addr.layer,
                bytes: slot.bytes.len() as u64,
                created_unix: 0,
                last_seen_unix: Self::now(),
                tier: TierKind::Ram,
            },
        })
    }

    fn put(&self, addr: BlockAddress, bytes: bytes::Bytes) -> bool {
        let prev = self.inner.insert(addr, RamSlot {
            bytes,
            last: std::sync::atomic::AtomicU64::new(Self::now()),
        });
        prev.is_some()
    }

    fn evict(&self, addr: &BlockAddress) {
        self.inner.remove(addr);
    }

    fn used_bytes(&self) -> u64 {
        self.inner.iter().map(|e| e.value().bytes.len() as u64).sum()
    }

    fn capacity_bytes(&self) -> u64 { self.capacity }
}
