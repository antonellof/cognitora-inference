//! Cognitora performance harness.
//!
//! Each `bench/*.rs` exercises a single SLI from
//! [`docs/operations/slo.md`](../../../docs/operations/slo.md).
//! On every PR the CI job runs these benches in `--profile bench` and
//! compares the median to the documented SLO budget. A regression > 10%
//! over the historical p50 fails the job.
//!
//! The harness is a tiny Rust crate so it compiles without pulling in
//! the entire router runtime — most of what we want to measure is in
//! `cgn-core` (hashing, prefix index) or in pure functions on
//! [`crate::dummy`].

#![forbid(unsafe_code)]

use blake3::Hasher;

/// Approximate the router's per-token hash cost. The real router hashes
/// every chunk of TOKENS_PER_CHUNK tokens; we expose a single-token
/// version here so benchmarks can compare absolute hash latency.
pub fn hash_token(model: &str, token: u32) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(model.as_bytes());
    h.update(&token.to_le_bytes());
    *h.finalize().as_bytes()
}

/// Hash a whole prompt's tokens. Chunked at the same rate the router
/// uses (`TOKENS_PER_CHUNK = 32`). Returns one digest per chunk.
pub fn hash_prompt(model: &str, tokens: &[u32]) -> Vec<[u8; 32]> {
    cgn_core::hash::hash_chunks(model, tokens)
}
