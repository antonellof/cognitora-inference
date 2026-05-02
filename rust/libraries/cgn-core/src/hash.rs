//! Stable BLAKE3 prefix hashing used by the router and KV daemon.
//!
//! Two flavours are exposed:
//!
//! * [`hash_chunks`] — independent per-chunk digests. Cheap. Correct only
//!   when KV reuse cares about *content overlap* and not *position*.
//! * [`hash_seq_chunks`] — sequence-chained digests. Each block is salted
//!   with the prior block's digest so that the hash for chunk N encodes
//!   the entire prefix `tokens[0..N*TOKENS_PER_CHUNK]`. This matches what
//!   actual KV caches require: the K/V tensors at position P depend on
//!   every token at positions `<P`, so two requests with the same suffix
//!   but different prefixes must produce different digests at every
//!   position past the divergence point. The router uses this variant to
//!   compute *longest-prefix* overlap; the resulting score is what
//!   determines whether a node can skip a prefill.
//!
//! Both variants are model-salted.

/// Number of token IDs hashed per chunk. Keeps the prefix-trie depth bounded
/// while still letting cache hits accumulate at sub-prompt granularity.
pub const TOKENS_PER_CHUNK: usize = 32;

/// Hash a window of token ids into a deterministic 32-byte digest.
///
/// The hash is salted with the model name so that two models with overlapping
/// vocabularies do not collide on cache-hit accounting. Token IDs are
/// serialised in little-endian form for cross-architecture stability.
pub fn hash_chunk(model: &str, tokens: &[u32]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"cognitora.v1\0");
    h.update(model.as_bytes());
    h.update(b"\0");
    for &t in tokens {
        h.update(&t.to_le_bytes());
    }
    *h.finalize().as_bytes()
}

/// Sequence-chained digest for one chunk. `parent` is the digest of the
/// prior chunk, or all-zero for the first chunk.
pub fn hash_seq_chunk(model: &str, parent: &[u8; 32], tokens: &[u32]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"cognitora.seq.v1\0");
    h.update(model.as_bytes());
    h.update(b"\0");
    h.update(parent);
    for &t in tokens {
        h.update(&t.to_le_bytes());
    }
    *h.finalize().as_bytes()
}

/// Walk a full token sequence and yield one digest per `TOKENS_PER_CHUNK`
/// rolling chunk. The last (partial) chunk is also emitted because it is
/// often the most cache-relevant.
///
/// **Position-independent**: equal token-windows at different positions
/// hash to the same digest. Use [`hash_seq_chunks`] when KV-cache reuse
/// requires positional fidelity.
pub fn hash_chunks(model: &str, tokens: &[u32]) -> Vec<[u8; 32]> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(tokens.len().div_ceil(TOKENS_PER_CHUNK));
    for chunk in tokens.chunks(TOKENS_PER_CHUNK) {
        out.push(hash_chunk(model, chunk));
    }
    out
}

/// Walk a full token sequence and yield one *sequence-chained* digest per
/// chunk. Chunk N's digest depends on chunks `0..=N`, so the digest at
/// position N uniquely identifies the prefix `tokens[0..(N+1)*TOKENS_PER_CHUNK]`.
///
/// This is the correct hash for KV-cache prefix matching: a node that
/// reports holding the chain `[d_0, d_1, ..., d_K]` can skip prefilling
/// up to chunk `K` of a request that shares those exact digests in
/// order.
pub fn hash_seq_chunks(model: &str, tokens: &[u32]) -> Vec<[u8; 32]> {
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(tokens.len().div_ceil(TOKENS_PER_CHUNK));
    let mut parent = [0u8; 32];
    for chunk in tokens.chunks(TOKENS_PER_CHUNK) {
        let d = hash_seq_chunk(model, &parent, chunk);
        out.push(d);
        parent = d;
    }
    out
}

/// Format a digest as lowercase hex (16 chars of prefix → 8 bytes is plenty).
pub fn short(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(16);
    for b in &digest[..8] {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic() {
        let a = hash_chunk("llama3", &[1, 2, 3]);
        let b = hash_chunk("llama3", &[1, 2, 3]);
        assert_eq!(a, b);
    }

    #[test]
    fn model_salts() {
        let a = hash_chunk("llama3", &[1, 2, 3]);
        let b = hash_chunk("qwen3", &[1, 2, 3]);
        assert_ne!(a, b);
    }

    #[test]
    fn chunks_partial_tail() {
        let toks: Vec<u32> = (0..70).collect();
        let h = hash_chunks("m", &toks);
        assert_eq!(h.len(), 3); // 32 + 32 + 6
    }

    #[test]
    fn short_is_16_chars() {
        let d = hash_chunk("m", &[1, 2, 3]);
        assert_eq!(short(&d).len(), 16);
    }

    #[test]
    fn seq_chunks_are_position_dependent() {
        // [a, b] and [c, b] both have b at position 1, but the seq hash
        // at position 1 must differ because the prefix differs.
        let toks_ab: Vec<u32> = (0..64u32).collect();
        let toks_cb: Vec<u32> = std::iter::repeat_n(99u32, 32).chain(32..64u32).collect();

        let h1 = hash_seq_chunks("m", &toks_ab);
        let h2 = hash_seq_chunks("m", &toks_cb);
        assert_eq!(h1.len(), 2);
        assert_eq!(h2.len(), 2);
        assert_ne!(h1[0], h2[0]);
        // Same chunk-1 tokens but different prefix → seq hash must differ.
        assert_ne!(h1[1], h2[1]);
    }

    #[test]
    fn seq_chunks_extend_consistently() {
        let toks: Vec<u32> = (0..96u32).collect();
        let prefix: Vec<u32> = toks[..64].to_vec();
        let h_full = hash_seq_chunks("m", &toks);
        let h_prefix = hash_seq_chunks("m", &prefix);
        // Extending the sequence preserves all earlier digests.
        assert_eq!(h_prefix, h_full[..2]);
    }

    #[test]
    fn seq_chunks_diverge_when_independent_chunks_match() {
        // Independent chunk hashing would consider these "equal" at chunk
        // 1, but seq hashing must not.
        let mut a: Vec<u32> = (0..64).collect();
        let mut b: Vec<u32> = (0..64).collect();
        a[0] = 9999; // diverge in chunk 0 only
        b[0] = 1234;
        let ha = hash_seq_chunks("m", &a);
        let hb = hash_seq_chunks("m", &b);
        assert_ne!(ha[0], hb[0]);
        // Chunk-1 token windows are identical but prefixes diverged.
        let ind = hash_chunks("m", &a);
        let ind_b = hash_chunks("m", &b);
        assert_eq!(ind[1], ind_b[1], "independent hashing collides at pos 1");
        assert_ne!(ha[1], hb[1], "seq hashing must diverge at pos 1");
    }
}
