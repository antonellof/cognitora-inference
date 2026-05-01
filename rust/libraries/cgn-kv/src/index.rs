//! RocksDB-backed durable KV-block index.
//!
//! Holds `(BlockAddress -> BlockMeta)` so `cgn-kvcached` can survive
//! restart and the router can ask "where on this host?" cheaply.

use std::path::Path;
use std::sync::Arc;

use rocksdb::{Options, DB};

use cgn_core::{Error, Result};

use super::block::{BlockAddress, BlockMeta};

/// Durable index keyed by `digest || layer (LE u32)` → bincoded `BlockMeta`.
#[derive(Clone)]
pub struct Index {
    db: Arc<DB>,
}

impl Index {
    /// Open or create an index at `dir`.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .map_err(|e| Error::Internal(format!("kv index mkdir: {e}")))?;

        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_compression_type(rocksdb::DBCompressionType::Lz4);
        opts.set_max_open_files(1024);
        opts.increase_parallelism(num_cpus::get() as i32);

        let db = DB::open(&opts, dir).map_err(|e| Error::Internal(format!("rocksdb open: {e}")))?;
        Ok(Self { db: Arc::new(db) })
    }

    pub fn put(&self, addr: &BlockAddress, meta: &BlockMeta) -> Result<()> {
        let key = key_of(addr);
        let val =
            bincode::serialize(meta).map_err(|e| Error::Internal(format!("bincode meta: {e}")))?;
        self.db.put(key, val).map_err(rdb)
    }

    pub fn get(&self, addr: &BlockAddress) -> Result<Option<BlockMeta>> {
        let key = key_of(addr);
        match self.db.get(key).map_err(rdb)? {
            None => Ok(None),
            Some(bytes) => {
                Ok(Some(bincode::deserialize(&bytes).map_err(|e| {
                    Error::Internal(format!("bincode meta de: {e}"))
                })?))
            }
        }
    }

    pub fn contains(&self, addr: &BlockAddress) -> Result<bool> {
        Ok(self.db.get_pinned(key_of(addr)).map_err(rdb)?.is_some())
    }

    pub fn delete(&self, addr: &BlockAddress) -> Result<()> {
        self.db.delete(key_of(addr)).map_err(rdb)
    }

    /// Approximate count (rocksdb stat).
    pub fn approximate_len(&self) -> u64 {
        self.db
            .property_int_value("rocksdb.estimate-num-keys")
            .ok()
            .flatten()
            .unwrap_or(0)
    }
}

fn key_of(a: &BlockAddress) -> [u8; 36] {
    let mut k = [0u8; 36];
    k[..32].copy_from_slice(&a.digest);
    k[32..].copy_from_slice(&a.layer.to_le_bytes());
    k
}

fn rdb(e: rocksdb::Error) -> Error {
    Error::Internal(format!("rocksdb: {e}"))
}

// Locally re-export num_cpus crate via std::thread to avoid an extra dep:
mod num_cpus {
    pub fn get() -> usize {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TierKind;
    use tempfile::TempDir;

    #[test]
    fn round_trip() {
        let d = TempDir::new().unwrap();
        let ix = Index::open(d.path()).unwrap();
        let addr = BlockAddress {
            digest: [7u8; 32],
            layer: 4,
        };
        let meta = BlockMeta {
            model: "llama3-8b".into(),
            layer: 4,
            bytes: 4096,
            created_unix: 1,
            last_seen_unix: 2,
            tier: TierKind::Ram,
        };
        ix.put(&addr, &meta).unwrap();
        assert!(ix.contains(&addr).unwrap());
        let got = ix.get(&addr).unwrap().unwrap();
        assert_eq!(got.bytes, 4096);
    }
}
