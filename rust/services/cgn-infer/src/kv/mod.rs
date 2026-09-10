//! Per-request KV cache bookkeeping and block-hash prefix reuse.
//!
//! Phase 1 keeps the actual KV tensors inside Candle's
//! `quantized_llama::ModelWeights` (one cache per model instance,
//! reset between requests since serving is sequential). What lives
//! here is the *indexing* layer: prompts are chunked into fixed-size
//! token blocks and hashed with BLAKE3 chained over the prefix — the
//! same scheme `cgn-kvcached` uses — so we can tell how many leading
//! blocks of a new prompt match a previously served one. In phase 1
//! the runtime uses that to skip re-prefilling a shared prefix within
//! a session; in phase 3 the same index keys paged KV blocks.

use std::collections::HashMap;

/// Tokens per KV block. 16 matches cgn-kvcached's block granularity.
pub const BLOCK_TOKENS: usize = 16;

/// Chained BLAKE3 hash of a token block: `hash(parent_hash ‖ tokens)`.
/// Chaining makes a block hash identify the whole prefix up to and
/// including that block, not just the block's own tokens.
pub type BlockHash = [u8; 32];

fn hash_block(parent: Option<&BlockHash>, tokens: &[u32]) -> BlockHash {
    let mut hasher = blake3::Hasher::new();
    if let Some(p) = parent {
        hasher.update(p);
    }
    for t in tokens {
        hasher.update(&t.to_le_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// Hash every complete block of a token sequence (the tail shorter
/// than `BLOCK_TOKENS` is not hashed — it is never reusable).
pub fn block_hashes(tokens: &[u32]) -> Vec<BlockHash> {
    let mut out = Vec::with_capacity(tokens.len() / BLOCK_TOKENS);
    let mut parent: Option<BlockHash> = None;
    for chunk in tokens.chunks_exact(BLOCK_TOKENS) {
        let h = hash_block(parent.as_ref(), chunk);
        out.push(h);
        parent = Some(h);
    }
    out
}

/// Tracks which prefix blocks are resident in the engine's KV cache.
///
/// Sequential serving means at most one sequence's KV state exists at
/// a time, so this is a single chain of block hashes plus a lookup
/// set — deliberately simple. `match_prefix` answers "how many tokens
/// of this prompt are already computed?".
#[derive(Default)]
pub struct PrefixCache {
    /// Block hash → 1-based block depth in the cached chain.
    resident: HashMap<BlockHash, usize>,
    /// Total tokens currently held in the engine KV cache (blocks may
    /// cover fewer tokens than this; the tail is unindexed).
    cached_tokens: usize,
}

/// Result of a prefix lookup.
#[derive(Debug, PartialEq, Eq)]
pub struct PrefixMatch {
    /// Number of leading prompt tokens whose KV state is resident.
    pub tokens: usize,
    /// Number of whole blocks matched.
    pub blocks: usize,
}

impl PrefixCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Longest chain of leading blocks of `prompt` that is resident.
    pub fn match_prefix(&self, prompt: &[u32]) -> PrefixMatch {
        let mut blocks = 0;
        for (i, h) in block_hashes(prompt).iter().enumerate() {
            // Depth must line up: chained hashing already guarantees
            // the parent chain matches if the hash matches.
            if self.resident.get(h) == Some(&(i + 1)) {
                blocks = i + 1;
            } else {
                break;
            }
        }
        PrefixMatch {
            tokens: blocks * BLOCK_TOKENS,
            blocks,
        }
    }

    /// Record that the engine KV cache now holds exactly `tokens`
    /// (prompt + generated so far). Replaces the previous chain.
    pub fn record(&mut self, tokens: &[u32]) {
        self.resident.clear();
        for (i, h) in block_hashes(tokens).into_iter().enumerate() {
            self.resident.insert(h, i + 1);
        }
        self.cached_tokens = tokens.len();
    }

    /// Drop all state (engine KV cache was cleared).
    pub fn clear(&mut self) {
        self.resident.clear();
        self.cached_tokens = 0;
    }

    pub fn cached_tokens(&self) -> usize {
        self.cached_tokens
    }
}

/// Paged KV block accounting for the continuous-batching scheduler.
///
/// Sequences allocate KV space in [`BLOCK_TOKENS`]-sized blocks from a
/// fixed budget. The pool tracks *logical* blocks — the actual KV
/// tensors live per-sequence in the runtime (`SeqKv`), stored
/// contiguously — so this layer decides admission and eviction while
/// the tensor layout stays simple. When the pool is exhausted the
/// scheduler preempts a victim sequence, frees its blocks here, and
/// re-queues it (its KV is recomputed on re-admission).
#[derive(Debug)]
pub struct KvPool {
    total_blocks: usize,
    used_blocks: usize,
    /// seq id → blocks currently held.
    held: HashMap<u64, usize>,
}

impl KvPool {
    pub fn new(total_blocks: usize) -> Self {
        Self {
            total_blocks,
            used_blocks: 0,
            held: HashMap::new(),
        }
    }

    /// Blocks needed to hold `tokens` positions.
    pub fn blocks_for(tokens: usize) -> usize {
        tokens.div_ceil(BLOCK_TOKENS)
    }

    pub fn free_blocks(&self) -> usize {
        self.total_blocks - self.used_blocks
    }

    pub fn used_blocks(&self) -> usize {
        self.used_blocks
    }

    pub fn total_blocks(&self) -> usize {
        self.total_blocks
    }

    /// Blocks currently held by `seq`.
    pub fn held_by(&self, seq: u64) -> usize {
        self.held.get(&seq).copied().unwrap_or(0)
    }

    /// Grow (or create) `seq`'s reservation so it covers `tokens`
    /// positions. Returns `false` — with no state change — if the pool
    /// cannot satisfy the growth.
    pub fn reserve(&mut self, seq: u64, tokens: usize) -> bool {
        let want = Self::blocks_for(tokens);
        let have = self.held_by(seq);
        if want <= have {
            return true;
        }
        let extra = want - have;
        if extra > self.free_blocks() {
            return false;
        }
        self.used_blocks += extra;
        *self.held.entry(seq).or_insert(0) = want;
        true
    }

    /// Release everything held by `seq`.
    pub fn release(&mut self, seq: u64) {
        if let Some(blocks) = self.held.remove(&seq) {
            self.used_blocks -= blocks;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(n: usize) -> Vec<u32> {
        (0..n as u32).collect()
    }

    #[test]
    fn empty_cache_matches_nothing() {
        let c = PrefixCache::new();
        assert_eq!(c.match_prefix(&toks(64)).tokens, 0);
    }

    #[test]
    fn full_prefix_reuse() {
        let mut c = PrefixCache::new();
        let prompt = toks(64);
        c.record(&prompt);
        let m = c.match_prefix(&prompt);
        assert_eq!(
            m,
            PrefixMatch {
                tokens: 64,
                blocks: 4
            }
        );
    }

    #[test]
    fn partial_prefix_reuse() {
        let mut c = PrefixCache::new();
        c.record(&toks(64));
        // Same first 32 tokens, divergent afterwards.
        let mut other = toks(32);
        other.extend([9999u32; 32]);
        let m = c.match_prefix(&other);
        assert_eq!(
            m,
            PrefixMatch {
                tokens: 32,
                blocks: 2
            }
        );
    }

    #[test]
    fn divergence_at_first_block_matches_nothing() {
        let mut c = PrefixCache::new();
        c.record(&toks(64));
        let mut other = toks(64);
        other[0] = 12345;
        assert_eq!(c.match_prefix(&other).tokens, 0);
    }

    #[test]
    fn incomplete_tail_block_is_not_indexed() {
        let mut c = PrefixCache::new();
        c.record(&toks(20)); // one full block + 4-token tail
        let m = c.match_prefix(&toks(20));
        assert_eq!(
            m,
            PrefixMatch {
                tokens: 16,
                blocks: 1
            }
        );
    }

    #[test]
    fn chained_hashes_distinguish_same_block_different_prefix() {
        // Block content [16..32) appears at depth 2 in `a` but would
        // hash differently if it followed a different first block.
        let a = toks(32);
        let mut b = a.clone();
        b[0] = 7;
        let ha = block_hashes(&a);
        let hb = block_hashes(&b);
        assert_ne!(ha[1], hb[1]);
    }

    #[test]
    fn pool_blocks_for_rounds_up() {
        assert_eq!(KvPool::blocks_for(0), 0);
        assert_eq!(KvPool::blocks_for(1), 1);
        assert_eq!(KvPool::blocks_for(16), 1);
        assert_eq!(KvPool::blocks_for(17), 2);
    }

    #[test]
    fn pool_reserve_grows_incrementally() {
        let mut p = KvPool::new(4);
        assert!(p.reserve(1, 16)); // 1 block
        assert_eq!(p.used_blocks(), 1);
        assert!(p.reserve(1, 17)); // grow to 2
        assert_eq!(p.used_blocks(), 2);
        assert!(p.reserve(1, 20)); // still 2, no-op
        assert_eq!(p.used_blocks(), 2);
        assert_eq!(p.held_by(1), 2);
    }

    #[test]
    fn pool_rejects_overflow_without_state_change() {
        let mut p = KvPool::new(2);
        assert!(p.reserve(1, 32)); // both blocks
        assert!(!p.reserve(2, 1));
        assert_eq!(p.held_by(2), 0);
        assert_eq!(p.used_blocks(), 2);
    }

    #[test]
    fn pool_release_frees_blocks() {
        let mut p = KvPool::new(2);
        assert!(p.reserve(1, 32));
        p.release(1);
        assert_eq!(p.free_blocks(), 2);
        assert!(p.reserve(2, 32));
        p.release(99); // unknown seq is a no-op
        assert_eq!(p.used_blocks(), 2);
    }

    #[test]
    fn clear_resets_state() {
        let mut c = PrefixCache::new();
        c.record(&toks(64));
        c.clear();
        assert_eq!(c.match_prefix(&toks(64)).tokens, 0);
        assert_eq!(c.cached_tokens(), 0);
    }
}
