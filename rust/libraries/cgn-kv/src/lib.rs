//! KV cache primitives: block addressing, RocksDB index, tiering, transport.
//!
//! Used by the `cgn-kvcached` daemon and (for prefix accounting) by
//! `cgn-router`. The storage backends here are the safe Rust ones; CUDA and
//! RDMA paths live in feature-gated submodules so the crate builds on a
//! plain Linux box without a GPU.

#![forbid(unsafe_code)]

pub mod block;
#[cfg(feature = "persistent-index")]
pub mod index;
#[cfg(feature = "rdma")]
pub mod rdma;
pub mod ssd;
pub mod tier;
pub mod transport;

pub use block::{BlockAddress, BlockHandle, BlockMeta};
#[cfg(feature = "persistent-index")]
pub use index::Index;
pub use ssd::SsdTier;
pub use tier::{Tier, TierKind};

/// Short hex prefix of a digest, used for filenames and log lines.
pub fn hash_short(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}
