//! Engine KV-cache epoch tracking for router-side prefix reconciliation.
//!
//! vLLM/SGLang block hashes do not map to Cognitora's BLAKE3 prefix
//! digests, so we cannot consume engine KV-event streams directly.
//! Instead the agent bumps a monotonic `kv_epoch` in its heartbeat when
//! it detects cache resets (engine restart, block-pool resize, or a large
//! sudden increase in free blocks), giving the router a conservative
//! signal to purge stale prefix claims for that node.

use crate::telemetry::EngineStats;

#[derive(Debug, Default)]
pub struct KvCacheState {
    kv_epoch: u64,
    was_ready: bool,
    saw_ready: bool,
    last_free: u32,
    last_total: u32,
}

impl KvCacheState {
    pub fn observe(&mut self, ready: bool, stats: &EngineStats) -> u64 {
        if self.saw_ready && self.was_ready && !ready {
            // Engine dropped offline mid-session — the next ready flip is
            // treated as a full cache reset.
            self.bump("engine_not_ready");
        }

        if ready {
            if self.saw_ready && !self.was_ready {
                self.bump("engine_restart");
            }

            if stats.total_blocks > 0 {
                if self.last_total > 0 && stats.total_blocks != self.last_total {
                    self.bump("total_blocks_changed");
                }

                if self.last_total > 0 && self.last_free > 0 {
                    let delta = stats.free_blocks as i64 - self.last_free as i64;
                    // A large *increase* in free blocks between heartbeats
                    // usually means the engine evicted or reset its pool.
                    let jump_threshold = (stats.total_blocks / 6).max(32) as i64;
                    if delta >= jump_threshold {
                        self.bump("free_blocks_jump");
                    }
                }
            }

            self.saw_ready = true;
            self.last_free = stats.free_blocks;
            self.last_total = stats.total_blocks;
        }

        self.was_ready = ready;
        self.kv_epoch
    }

    fn bump(&mut self, reason: &str) {
        self.kv_epoch = self.kv_epoch.saturating_add(1);
        tracing::info!(kv_epoch = self.kv_epoch, reason, "kv cache epoch bumped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(free: u32, total: u32) -> EngineStats {
        EngineStats {
            queue_depth: 0,
            free_blocks: free,
            total_blocks: total,
        }
    }

    #[test]
    fn epoch_starts_at_zero() {
        let mut s = KvCacheState::default();
        assert_eq!(s.observe(true, &stats(100, 1000)), 0);
    }

    #[test]
    fn restart_bumps_epoch() {
        let mut s = KvCacheState::default();
        assert_eq!(s.observe(true, &stats(500, 1000)), 0);
        assert_eq!(s.observe(false, &stats(500, 1000)), 1);
        assert_eq!(s.observe(true, &stats(500, 1000)), 2);
    }

    #[test]
    fn total_blocks_change_bumps_epoch() {
        let mut s = KvCacheState::default();
        s.observe(true, &stats(500, 1000));
        assert_eq!(s.observe(true, &stats(500, 2000)), 1);
    }

    #[test]
    fn large_free_jump_bumps_epoch() {
        let mut s = KvCacheState::default();
        s.observe(true, &stats(100, 1000));
        assert_eq!(s.observe(true, &stats(300, 1000)), 1);
    }
}
