//! RDMA fast-path for cross-host KV transfer.
//!
//! Compiled only when the `rdma` feature is enabled (Linux + ibverbs).
//! On other targets the entire module is gated out so the workspace
//! still builds and the QUIC path remains the only transport.
//!
//! When enabled, this module provides the same `peer_push` /
//! `peer_pull` shape as the QUIC transport but talks over RC verbs
//! (one QP per peer, multi-stream by chunking blocks into MTU-sized
//! `WRITE_WITH_IMM` work requests). The wire format is the same
//! [`Frame`](super::transport::Frame) header so a peer that prefers
//! QUIC can still talk to us.

#![cfg(feature = "rdma")]

use bytes::Bytes;

use crate::block::BlockAddress;

/// Push a block over RDMA to `remote`. Stub: returns `Unsupported`
/// at runtime until the verbs path lands in M3.
pub async fn peer_push(_remote: &str, _addr: BlockAddress, _bytes: Bytes) -> cgn_core::Result<()> {
    Err(cgn_core::Error::Internal(
        "rdma transport: ibverbs path not yet enabled in this build".into(),
    ))
}

/// Pull a block over RDMA from `remote`.
pub async fn peer_pull(_remote: &str, _addr: BlockAddress) -> cgn_core::Result<Bytes> {
    Err(cgn_core::Error::Internal(
        "rdma transport: ibverbs path not yet enabled in this build".into(),
    ))
}

/// Whether RDMA is usable on this host. Always `false` until the verbs
/// path lands; the build of `cgn-kvcached` falls back to QUIC.
pub fn available() -> bool {
    false
}
