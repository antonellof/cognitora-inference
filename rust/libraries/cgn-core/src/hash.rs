//! Stable BLAKE3 prefix hashing used by the router and KV daemon.

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

/// Walk a full token sequence and yield one digest per `TOKENS_PER_CHUNK`
/// rolling chunk. The last (partial) chunk is also emitted because it is
/// often the most cache-relevant.
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
}
