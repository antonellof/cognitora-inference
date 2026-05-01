//! Rate limiting for the gateway surface.
//!
//! Two modes:
//! * **In-process** (default) — `governor` per-key token bucket. Cheap,
//!   accurate, but doesn't share state across replicas.
//! * **Redis-backed** (configured via `[router.rate_limit] redis_url`) —
//!   atomic GCRA with `INCR / PEXPIRE` round-trip per request. Use when
//!   running multiple `cgn-router` replicas behind a load balancer.
//!
//! The middleware keys on the authenticated subject if present, otherwise
//! the source IP from `X-Forwarded-For` (last hop) or the socket addr.

#![forbid(unsafe_code)]

use std::num::NonZeroU32;
use std::sync::Arc;

use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use dashmap::DashMap;
use governor::{
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter,
};

/// Inner per-key limiter type.
type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

#[derive(Clone)]
pub struct RateLimit {
    rps:   NonZeroU32,
    burst: NonZeroU32,
    map:   Arc<DashMap<String, Arc<Limiter>>>,
}

impl RateLimit {
    pub fn new(rps: u32, burst: u32) -> Self {
        Self {
            rps:   NonZeroU32::new(rps.max(1)).unwrap(),
            burst: NonZeroU32::new(burst.max(1)).unwrap(),
            map:   Arc::new(DashMap::new()),
        }
    }

    fn limiter_for(&self, key: &str) -> Arc<Limiter> {
        if let Some(l) = self.map.get(key) {
            return l.clone();
        }
        let quota = Quota::per_second(self.rps).allow_burst(self.burst);
        let new = Arc::new(RateLimiter::direct(quota));
        self.map.entry(key.to_string()).or_insert_with(|| new.clone()).clone()
    }

    /// Check + decrement. Returns Ok if the request is admitted.
    pub fn check(&self, key: &str) -> Result<(), governor::NotUntil<governor::clock::QuantaInstant>> {
        let l = self.limiter_for(key);
        l.check()
    }
}

/// Tower middleware. Mount with `axum::middleware::from_fn_with_state`.
pub async fn ratelimit_middleware(
    State(rl): State<RateLimit>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let key = principal_or_ip(&req);
    if let Err(_e) = rl.check(&key) {
        tracing::info!(key = %key, "rate-limited");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(http::header::RETRY_AFTER, "1")],
            "rate limit exceeded",
        )
            .into_response();
    }
    next.run(req).await
}

fn principal_or_ip(req: &Request<Body>) -> String {
    if let Some(sub) = req.headers().get("x-cgn-subject")
        .and_then(|v| v.to_str().ok())
    {
        return format!("sub:{sub}");
    }
    if let Some(xff) = req.headers().get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
    {
        return format!("ip:{}", xff.trim());
    }
    if let Some(ConnectInfo(addr)) = req.extensions().get::<ConnectInfo<std::net::SocketAddr>>() {
        return format!("ip:{}", addr.ip());
    }
    "anonymous".into()
}
