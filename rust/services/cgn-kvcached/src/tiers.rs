//! In-process tier orchestration: probe RAM → SSD; admit promotes upward.

use std::path::Path;
use std::sync::Arc;

use cgn_core::{config::KvConfig, Error, Result};
use cgn_kv::{
    block::{BlockAddress, BlockHandle, BlockMeta},
    index::Index,
    tier::{RamTier, Tier, TierKind},
};

pub struct Store {
    pub ram:   Arc<RamTier>,
    pub index: Index,
    pub ssd_dir: std::path::PathBuf,
}

impl Store {
    pub async fn open(cfg: &KvConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.ssd_dir)
            .map_err(|e| Error::Internal(format!("ssd dir: {e}")))?;
        let index = Index::open(&cfg.index_dir)?;
        let ram = Arc::new(RamTier::new(cfg.ram_gib as u64 * 1024 * 1024 * 1024));
        Ok(Self {
            ram,
            index,
            ssd_dir: cfg.ssd_dir.clone(),
        })
    }

    pub fn lookup(&self, addr: &BlockAddress) -> Option<BlockHandle> {
        if let Some(h) = self.ram.get(addr) { return Some(h); }
        // SSD probe: index says it's there?
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
