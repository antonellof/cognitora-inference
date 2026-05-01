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
pub mod tier;
pub mod transport;

pub use block::{BlockAddress, BlockHandle, BlockMeta};
#[cfg(feature = "persistent-index")]
pub use index::Index;
pub use tier::{Tier, TierKind};
