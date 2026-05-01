//! SSD tier — file-per-block cold storage.
//!
//! Each block is stored at `<root>/<short(digest)>-<layer>.kvb`. The
//! short prefix makes the directory listings sane even with millions of
//! blocks. We use plain async I/O (`tokio::fs`) here; an `io_uring`
//! fast path is gated behind the `io-uring` feature on Linux.

use std::path::{Path, PathBuf};

use bytes::Bytes;
use cgn_core::{Error, Result};

use super::block::BlockAddress;

#[derive(Clone)]
pub struct SsdTier {
    root: PathBuf,
    capacity_bytes: u64,
}

impl SsdTier {
    pub fn open(root: impl Into<PathBuf>, capacity_bytes: u64) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)
            .map_err(|e| Error::Internal(format!("ssd dir: {e}")))?;
        Ok(Self { root, capacity_bytes })
    }

    pub fn capacity(&self) -> u64 { self.capacity_bytes }

    pub fn path_for(&self, addr: &BlockAddress) -> PathBuf {
        let hex = super::hash_short(&addr.digest);
        self.root.join(format!("{hex}-{}.kvb", addr.layer))
    }

    pub async fn read(&self, addr: &BlockAddress) -> Result<Option<Bytes>> {
        let path = self.path_for(addr);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(Some(Bytes::from(bytes))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Internal(format!("ssd read {}: {e}", path.display()))),
        }
    }

    pub async fn write(&self, addr: &BlockAddress, bytes: &[u8]) -> Result<()> {
        let path = self.path_for(addr);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| Error::Internal(format!("ssd mkdir: {e}")))?;
        }
        // Atomic write: temp file + rename.
        let tmp = path.with_extension("kvb.tmp");
        tokio::fs::write(&tmp, bytes).await
            .map_err(|e| Error::Internal(format!("ssd write tmp: {e}")))?;
        tokio::fs::rename(&tmp, &path).await
            .map_err(|e| Error::Internal(format!("ssd rename: {e}")))?;
        Ok(())
    }

    pub async fn evict(&self, addr: &BlockAddress) -> Result<()> {
        let path = self.path_for(addr);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::Internal(format!("ssd evict: {e}"))),
        }
    }

    /// Cheap-ish check: returns the directory's reported size (best
    /// effort, walks immediate children only).
    pub fn used_bytes(&self) -> u64 {
        std::fs::read_dir(&self.root)
            .map(|it| it.flatten()
                .filter_map(|d| d.metadata().ok().map(|m| m.len()))
                .sum())
            .unwrap_or(0)
    }

    pub fn root(&self) -> &Path { &self.root }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn round_trip() {
        let dir = tempdir().unwrap();
        let ssd = SsdTier::open(dir.path(), 1 << 30).unwrap();
        let addr = BlockAddress { digest: [9u8; 32], layer: 3 };
        ssd.write(&addr, b"hello world").await.unwrap();
        let r = ssd.read(&addr).await.unwrap();
        assert_eq!(r.as_deref(), Some(&b"hello world"[..]));
        ssd.evict(&addr).await.unwrap();
        assert!(ssd.read(&addr).await.unwrap().is_none());
    }
}
