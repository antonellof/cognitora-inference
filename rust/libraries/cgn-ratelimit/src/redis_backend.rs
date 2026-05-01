//! Redis-backed sliding-window token bucket.
//!
//! Per-key state lives in a single Redis hash:
//!
//!   HSET cgn:rl:<key> tokens <f32> last <unix_ms>
//!
//! On every `check`, we run a tiny Lua script that:
//!   1. Reads the current tokens + last timestamp.
//!   2. Refills based on elapsed time × refill_rate (capped at burst).
//!   3. Decrements one token and writes back.
//! The script is atomic; the round-trip is one PEXPIRE-set HSET per
//! call (~0.5–1 ms for a local Redis, single-digit ms across a region).
//!
//! Compiled when the `redis-backend` feature is on.

#![cfg(feature = "redis-backend")]

use cgn_core::{Error, Result};
use redis::{aio::ConnectionManager, Client};

/// Lua: returns 1 if the request is admitted, 0 otherwise. Atomic.
const SCRIPT: &str = r#"
local key       = KEYS[1]
local cap       = tonumber(ARGV[1])
local refill_ps = tonumber(ARGV[2])
local now_ms    = tonumber(ARGV[3])
local ttl_ms    = tonumber(ARGV[4])

local tokens, last
local data = redis.call('HMGET', key, 'tokens', 'last')
if data[1] == false then
    tokens = cap
    last   = now_ms
else
    tokens = tonumber(data[1])
    last   = tonumber(data[2])
end

local delta = math.max(0, now_ms - last)
tokens = math.min(cap, tokens + delta * refill_ps / 1000.0)

local admitted
if tokens >= 1.0 then
    tokens = tokens - 1.0
    admitted = 1
else
    admitted = 0
end

redis.call('HMSET', key, 'tokens', tokens, 'last', now_ms)
redis.call('PEXPIRE', key, ttl_ms)
return admitted
"#;

#[derive(Clone)]
pub struct RedisLimiter {
    conn: ConnectionManager,
    burst: u32,
    rps: u32,
    ttl_ms: i64,
    script: redis::Script,
}

impl RedisLimiter {
    /// Connect to `url` and return a limiter with the same shape as the
    /// in-process one.
    pub async fn connect(url: &str, rps: u32, burst: u32) -> Result<Self> {
        let client = Client::open(url).map_err(|e| Error::Config(format!("redis url: {e}")))?;
        let conn = client
            .get_connection_manager()
            .await
            .map_err(|e| Error::Unavailable(format!("redis connect: {e}")))?;
        Ok(Self {
            conn,
            burst: burst.max(1),
            rps: rps.max(1),
            // 5× the burst-fill time keeps idle keys in cache without
            // bloating the keyspace.
            ttl_ms: ((burst.max(1) as i64 * 5_000) / rps.max(1) as i64).max(1_000),
            script: redis::Script::new(SCRIPT),
        })
    }

    /// Atomically attempt to reserve one token under `key`.
    pub async fn check(&self, key: &str) -> Result<bool> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let full_key = format!("cgn:rl:{key}");
        let mut conn = self.conn.clone();
        let admitted: i64 = self
            .script
            .key(full_key)
            .arg(self.burst as i64)
            .arg(self.rps as i64)
            .arg(now_ms)
            .arg(self.ttl_ms)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| Error::Unavailable(format!("redis script: {e}")))?;
        Ok(admitted == 1)
    }

    /// Connection liveness probe.
    pub async fn ping(&self) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| Error::Unavailable(format!("redis ping: {e}")))?;
        Ok(())
    }
}
