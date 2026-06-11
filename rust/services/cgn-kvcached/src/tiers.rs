//! In-process tier orchestration: probe RAM → SSD; admit promotes upward.
//!
//! The persistent index is provided by `cgn-kv::Index` (RocksDB) when the
//! `persistent-index` feature is enabled; otherwise we fall back to an
//! in-memory `DashMap` so dev builds compile even on hosts that can't
//! build rocksdb. Production releases always set `persistent-index = on`.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cgn_core::{config::KvConfig, Result};
use cgn_kv::{
    block::{BlockAddress, BlockHandle, BlockMeta},
    ssd::SsdTier,
    tier::{RamTier, Tier, TierKind},
};

/// Process-lifetime cache counters, surfaced via the `Stats` RPC and
/// Prometheus.
#[derive(Default)]
pub struct KvStats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    pub evictions: AtomicU64,
    pub spills: AtomicU64,
    pub bytes_pushed: AtomicU64,
    pub bytes_pulled: AtomicU64,
}

pub struct Store {
    pub ram: Arc<RamTier>,
    pub ssd: Arc<SsdTier>,
    pub index: IndexImpl,
    pub stats: KvStats,
}

impl Store {
    pub async fn open(cfg: &KvConfig) -> Result<Self> {
        let ssd_cap = cfg.ssd_gib as u64 * 1024 * 1024 * 1024;
        let ssd = Arc::new(SsdTier::open(&cfg.ssd_dir, ssd_cap)?);
        let index = IndexImpl::open(&cfg.index_dir)?;
        let ram = Arc::new(RamTier::new(cfg.ram_gib as u64 * 1024 * 1024 * 1024));
        Ok(Self {
            ram,
            ssd,
            index,
            stats: KvStats::default(),
        })
    }

    /// Probe RAM, SSD, then the persistent index. Returns a handle when
    /// present in any tier.
    pub fn lookup(&self, addr: &BlockAddress) -> Option<BlockHandle> {
        if let Some(h) = self.ram.get(addr) {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            return Some(h);
        }
        if let Ok(Some(meta)) = self.index.get(addr) {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            return Some(BlockHandle { addr: *addr, meta });
        }
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    /// Async lookup that may hit SSD. Returns the bytes (and promotes
    /// them into RAM as a side effect) when the block is found cold.
    pub async fn lookup_with_promote(&self, addr: &BlockAddress) -> Option<bytes::Bytes> {
        if let Some(b) = self.ram.get_bytes(addr) {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            return Some(b);
        }
        if let Ok(Some(b)) = self.ssd.read(addr).await {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            // Promote: bring back into RAM, update tier hint.
            let bytes = b.clone();
            self.ram.put(*addr, bytes);
            let _ = self.index.put(
                addr,
                &BlockMeta {
                    model: String::new(),
                    layer: addr.layer,
                    bytes: b.len() as u64,
                    created_unix: chrono::Utc::now().timestamp() as u64,
                    last_seen_unix: chrono::Utc::now().timestamp() as u64,
                    tier: TierKind::Ram,
                },
            );
            return Some(b);
        }
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    pub fn put_ram(&self, addr: BlockAddress, bytes: bytes::Bytes, model: &str) -> Result<()> {
        let len = bytes.len() as u64;
        self.ram.put(addr, bytes);
        self.index.put(
            &addr,
            &BlockMeta {
                model: model.to_string(),
                layer: addr.layer,
                bytes: len,
                created_unix: chrono::Utc::now().timestamp() as u64,
                last_seen_unix: chrono::Utc::now().timestamp() as u64,
                tier: TierKind::Ram,
            },
        )?;
        Ok(())
    }

    /// Spill a block from RAM to SSD. Used by the eviction policy.
    pub async fn spill_to_ssd(&self, addr: BlockAddress, model: &str) -> Result<()> {
        let Some(bytes) = self.ram.get_bytes(&addr) else {
            return Ok(());
        };
        self.ssd.write(&addr, &bytes).await?;
        self.ram.evict(&addr);
        self.index.put(
            &addr,
            &BlockMeta {
                model: model.to_string(),
                layer: addr.layer,
                bytes: bytes.len() as u64,
                created_unix: chrono::Utc::now().timestamp() as u64,
                last_seen_unix: chrono::Utc::now().timestamp() as u64,
                tier: TierKind::Ssd,
            },
        )?;
        Ok(())
    }

    /// One eviction pass, run by the background loop:
    ///
    /// 1. RAM pressure: while occupancy exceeds `high_watermark`, spill
    ///    the coldest blocks (approximate LRU via touch timestamps) down
    ///    to SSD.
    /// 2. SSD TTL: blocks whose `last_seen_unix` is older than `ssd_ttl`
    ///    are deleted from disk and the index.
    pub async fn evict_pass(&self, high_watermark: f32, ssd_ttl_secs: u64) {
        // ---- RAM → SSD spill -------------------------------------------
        let cap = self.ram.capacity_bytes();
        let target = (cap as f64 * high_watermark as f64) as u64;
        if cap > 0 && self.ram.used_bytes() > target {
            // Spill in small batches until under the watermark. Bail if a
            // batch makes no progress (e.g. SSD write failures) so the
            // loop can't spin forever.
            while self.ram.used_bytes() > target {
                let before = self.ram.used_bytes();
                let victims = self.ram.coldest(32);
                if victims.is_empty() {
                    break;
                }
                for addr in victims {
                    let model = self
                        .index
                        .get(&addr)
                        .ok()
                        .flatten()
                        .map(|m| m.model)
                        .unwrap_or_default();
                    if let Err(e) = self.spill_to_ssd(addr, &model).await {
                        tracing::warn!(error=?e, "spill_to_ssd failed");
                    } else {
                        self.stats.spills.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if self.ram.used_bytes() >= before {
                    break;
                }
            }
        }

        // ---- SSD TTL ----------------------------------------------------
        if ssd_ttl_secs == 0 {
            return;
        }
        let now = chrono::Utc::now().timestamp() as u64;
        for (addr, meta) in self.index.scan() {
            if meta.tier == TierKind::Ssd && now.saturating_sub(meta.last_seen_unix) > ssd_ttl_secs
            {
                if let Err(e) = self.forget(&addr).await {
                    tracing::warn!(error=?e, "ssd ttl evict failed");
                } else {
                    self.stats.evictions.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    /// Count of index entries currently resident on SSD.
    pub fn cold_block_count(&self) -> u64 {
        self.index
            .scan()
            .into_iter()
            .filter(|(_, m)| m.tier == TierKind::Ssd)
            .count() as u64
    }

    pub async fn forget(&self, addr: &BlockAddress) -> Result<()> {
        self.ram.evict(addr);
        self.ssd.evict(addr).await?;
        self.index.delete(addr)
    }

    pub fn ssd_path(&self, addr: &BlockAddress) -> std::path::PathBuf {
        self.ssd.path_for(addr)
    }

    pub fn ssd_root(&self) -> &Path {
        self.ssd.root()
    }
}

// ---------------------------------------------------------------------------
// IndexImpl: RocksDB when feature is on, in-memory DashMap fallback otherwise.
// ---------------------------------------------------------------------------

#[cfg(feature = "persistent-index")]
pub struct IndexImpl(cgn_kv::Index);

#[cfg(feature = "persistent-index")]
impl IndexImpl {
    pub fn open(dir: &Path) -> Result<Self> {
        Ok(Self(cgn_kv::Index::open(dir)?))
    }
    pub fn put(&self, a: &BlockAddress, m: &BlockMeta) -> Result<()> {
        self.0.put(a, m)
    }
    pub fn get(&self, a: &BlockAddress) -> Result<Option<BlockMeta>> {
        self.0.get(a)
    }
    pub fn delete(&self, a: &BlockAddress) -> Result<()> {
        self.0.delete(a)
    }
    pub fn scan(&self) -> Vec<(BlockAddress, BlockMeta)> {
        self.0.scan()
    }
}

#[cfg(not(feature = "persistent-index"))]
pub struct IndexImpl {
    map: dashmap::DashMap<BlockAddress, BlockMeta>,
}

#[cfg(not(feature = "persistent-index"))]
impl IndexImpl {
    pub fn open(_dir: &Path) -> Result<Self> {
        Ok(Self {
            map: dashmap::DashMap::new(),
        })
    }
    pub fn put(&self, a: &BlockAddress, m: &BlockMeta) -> Result<()> {
        self.map.insert(*a, m.clone());
        Ok(())
    }
    pub fn get(&self, a: &BlockAddress) -> Result<Option<BlockMeta>> {
        Ok(self.map.get(a).map(|v| v.clone()))
    }
    pub fn delete(&self, a: &BlockAddress) -> Result<()> {
        self.map.remove(a);
        Ok(())
    }
    pub fn scan(&self) -> Vec<(BlockAddress, BlockMeta)> {
        self.map
            .iter()
            .map(|e| (*e.key(), e.value().clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(n: u8) -> BlockAddress {
        BlockAddress {
            digest: [n; 32],
            layer: 0,
        }
    }

    async fn tiny_store(ram_bytes: u64) -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let ssd = Arc::new(SsdTier::open(dir.path().join("ssd"), 1 << 30).unwrap());
        let index = IndexImpl::open(&dir.path().join("ix")).unwrap();
        let ram = Arc::new(RamTier::new(ram_bytes));
        (
            Store {
                ram,
                ssd,
                index,
                stats: KvStats::default(),
            },
            dir,
        )
    }

    #[tokio::test]
    async fn evict_pass_spills_over_watermark() {
        let (store, _dir) = tiny_store(1024).await;
        // 4 × 512 B blocks = 2 KiB used in a 1 KiB tier.
        for i in 0..4u8 {
            store
                .put_ram(addr(i), bytes::Bytes::from(vec![i; 512]), "m")
                .unwrap();
        }
        assert!(store.ram.used_bytes() > 1024);
        store.evict_pass(0.5, 0).await;
        assert!(store.ram.used_bytes() <= 512);
        // Spilled blocks remain readable (promoted back from SSD).
        for i in 0..4u8 {
            assert!(store.lookup_with_promote(&addr(i)).await.is_some());
        }
        assert!(store.stats.spills.load(Ordering::Relaxed) >= 2);
    }

    #[tokio::test]
    async fn evict_pass_expires_old_ssd_blocks() {
        let (store, _dir) = tiny_store(1 << 20).await;
        store
            .put_ram(addr(1), bytes::Bytes::from_static(b"x"), "m")
            .unwrap();
        store.spill_to_ssd(addr(1), "m").await.unwrap();
        // Backdate the index entry far past the TTL.
        let mut meta = store.index.get(&addr(1)).unwrap().unwrap();
        meta.last_seen_unix = 1;
        store.index.put(&addr(1), &meta).unwrap();

        store.evict_pass(1.0, 60).await;
        assert!(store.lookup(&addr(1)).is_none());
        assert_eq!(store.stats.evictions.load(Ordering::Relaxed), 1);
    }
}
