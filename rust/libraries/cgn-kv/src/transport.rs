//! Block transfer wire format used between `cgn-agent`/`cgn-kvcached`
//! daemons over QUIC (cross-host) and UDS (same-host).
//!
//! The protocol is intentionally tiny: a length-prefixed bincoded
//! [`Frame`] header followed by `header.body_len` bytes of payload.

use serde::{Deserialize, Serialize};

use super::block::BlockAddress;

/// Header of a single transfer frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub op:       Op,
    pub addr:     BlockAddress,
    pub body_len: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    /// Pull request: receiver responds with the block bytes.
    Pull,
    /// Push: sender attaches `body_len` bytes after the header.
    Push,
    /// Ack-only.
    Ack,
}

/// Protocol version included in every QUIC ALPN handshake.
pub const ALPN: &[u8] = b"cgn-kv/1";
