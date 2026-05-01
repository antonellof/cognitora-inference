//! In-process tier orchestration: probe RAM → SSD; admit promotes upward.
//!
//! The persistent index is provided by `cgn-kv::Index` (RocksDB) when the
//! `persistent-index` feature is enabled; otherwise we fall back to an
//! in-memory `DashMap` so dev builds compile even on hosts that can't
//! build rocksdb. Production releases always set `persistent-index = on`.

use std::path::Path;
use std::sync::Arc;

use cgn_core::{config::KvConfig, Error, Result};
use cgn_kv::{
    block::{BlockAddress, BlockHandle, BlockMeta},
    tier::{RamTier, Tier, TierKind},
};

pub struct Store {
    pub ram:     Arc<RamTier>,
    pub index:   IndexImpl,
    pub ssd_dir: std::path::PathBuf,
}

impl Store {
    pub async fn open(cfg: &KvConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.ssd_dir)
            .map_err(|e| Error::Internal(format!("ssd dir: {e}")))?;
        let index = IndexImpl::open(&cfg.index_dir)?;
        let ram = Arc::new(RamTier::new(cfg.ram_gib as u64 * 1024 * 1024 * 1024));
        Ok(Self { ram, index, ssd_dir: cfg.ssd_dir.clone() })
    }

    pub fn lookup(&self, addr: &BlockAddress) -> Option<BlockHandle> {
        if let Some(h) = self.ram.get(addr) { return Some(h); }
        if let Ok(Some(meta)) = self.index.get(addr) {
            return Some(BlockHandle { addr: *addr, meta });
        }
        None
    }

    pub fn put_ram(&self, addr: BlockAddress, bytes: bytes::Bytes, model: &str) -> Result<()> {
        let len = bytes.len() as u64;
        self.ram.put(addr, bytes);
        self.index.put(&addr, &BlockMeta {
            model: model.to_string(),
            layer: addr.layer,
            bytes: len,
            created_unix: chrono::Utc::now().timestamp() as u64,
            last_seen_unix: chrono::Utc::now().timestamp() as u64,
            tier: TierKind::Ram,
        })?;
        Ok(())
    }

    pub fn forget(&self, addr: &BlockAddress) -> Result<()> {
        self.ram.evict(addr);
        self.index.delete(addr)
    }

    pub fn ssd_path(&self, addr: &BlockAddress) -> std::path::PathBuf {
        let hex = cgn_core::hash::short(&addr.digest);
        self.ssd_dir.join(format!("{}-{}.kvb", hex, addr.layer))
    }

    pub fn _ssd_dir(&self) -> &Path { &self.ssd_dir }
}

// ---------------------------------------------------------------------------
// IndexImpl: RocksDB when feature is on, in-memory DashMap fallback otherwise.
// ---------------------------------------------------------------------------

#[cfg(feature = "persistent-index")]
pub struct IndexImpl(cgn_kv::Index);

#[cfg(feature = "persistent-index")]
impl IndexImpl {
    pub fn open(dir: &Path) -> Result<Self> { Ok(Self(cgn_kv::Index::open(dir)?)) }
    pub fn put(&self, a: &BlockAddress, m: &BlockMeta) -> Result<()> { self.0.put(a, m) }
    pub fn get(&self, a: &BlockAddress) -> Result<Option<BlockMeta>> { self.0.get(a) }
    pub fn delete(&self, a: &BlockAddress) -> Result<()> { self.0.delete(a) }
}

#[cfg(not(feature = "persistent-index"))]
pub struct IndexImpl {
    map: dashmap::DashMap<BlockAddress, BlockMeta>,
}

#[cfg(not(feature = "persistent-index"))]
impl IndexImpl {
    pub fn open(_dir: &Path) -> Result<Self> {
        Ok(Self { map: dashmap::DashMap::new() })
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
