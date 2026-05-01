//! In-process tier orchestration: probe RAM → SSD; admit promotes upward.
//!
//! The persistent index is provided by `cgn-kv::Index` (RocksDB) when the
//! `persistent-index` feature is enabled; otherwise we fall back to an
//! in-memory `DashMap` so dev builds compile even on hosts that can't
//! build rocksdb. Production releases always set `persistent-index = on`.

use std::path::Path;
use std::sync::Arc;

use cgn_core::{config::KvConfig, Result};
use cgn_kv::{
    block::{BlockAddress, BlockHandle, BlockMeta},
    ssd::SsdTier,
    tier::{RamTier, Tier, TierKind},
};

pub struct Store {
    pub ram: Arc<RamTier>,
    pub ssd: Arc<SsdTier>,
    pub index: IndexImpl,
}

impl Store {
    pub async fn open(cfg: &KvConfig) -> Result<Self> {
        let ssd_cap = cfg.ssd_gib as u64 * 1024 * 1024 * 1024;
        let ssd = Arc::new(SsdTier::open(&cfg.ssd_dir, ssd_cap)?);
        let index = IndexImpl::open(&cfg.index_dir)?;
        let ram = Arc::new(RamTier::new(cfg.ram_gib as u64 * 1024 * 1024 * 1024));
        Ok(Self { ram, ssd, index })
    }

    /// Probe RAM, SSD, then the persistent index. Returns a handle when
    /// present in any tier.
    pub fn lookup(&self, addr: &BlockAddress) -> Option<BlockHandle> {
        if let Some(h) = self.ram.get(addr) {
            return Some(h);
        }
        if let Ok(Some(meta)) = self.index.get(addr) {
            return Some(BlockHandle { addr: *addr, meta });
        }
        None
    }

    /// Async lookup that may hit SSD. Returns the bytes (and promotes
    /// them into RAM as a side effect) when the block is found cold.
    pub async fn lookup_with_promote(&self, addr: &BlockAddress) -> Option<bytes::Bytes> {
        if let Some(b) = self.ram.get_bytes(addr) {
            return Some(b);
        }
        if let Ok(Some(b)) = self.ssd.read(addr).await {
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
}
