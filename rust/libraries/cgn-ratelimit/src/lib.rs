//! Rate limiting for the gateway surface.
//!
//! Two modes:
//! * **In-process** (default) — `governor` per-key token bucket. Cheap,
//!   accurate, but doesn't share state across replicas.
//! * **Redis-backed** (configured via `[router.rate_limit] redis_url`) —
//!   atomic GCRA with `INCR / PEXPIRE` round-trip per request. Use when
//!   running multiple `cgn-router` replicas behind a load balancer.
//!
//! Keys, in order: the authenticated [`Principal`] set by the auth
//! middleware, then the client IP. The IP is the socket peer unless that
//! peer is a configured trusted proxy, in which case the right-most
//! untrusted `X-Forwarded-For` hop is used. Client-supplied headers alone
//! never choose a bucket.

#![forbid(unsafe_code)]

#[cfg(feature = "redis-backend")]
pub mod redis_backend;

#[cfg(feature = "redis-backend")]
pub use redis_backend::RedisLimiter;

use std::num::NonZeroU32;
use std::sync::{Arc, LazyLock};

use axum::{
    body::Body,
    extract::{ConnectInfo, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use cgn_auth::{Principal, PrincipalKind};
use cgn_telemetry::prometheus::{self, IntCounter};
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use std::net::{IpAddr, SocketAddr};

static RATE_LIMITED: LazyLock<IntCounter> = LazyLock::new(|| {
    let c = prometheus::IntCounter::new(
        "cgn_router_rate_limited_total",
        "Requests rejected by the rate-limit middleware.",
    )
    .expect("metric: rate_limited");
    cgn_telemetry::registry().register(Box::new(c.clone())).ok();
    c
});

/// Live keys above which idle buckets are swept. Bounds memory when an
/// attacker cycles through addresses.
const SWEEP_ABOVE_KEYS: usize = 100_000;

#[derive(Clone)]
pub struct RateLimit {
    limiter: Arc<DefaultKeyedRateLimiter<String>>,
    trusted: Arc<Vec<Cidr>>,
}

impl RateLimit {
    pub fn new(rps: u32, burst: u32) -> Self {
        let quota = Quota::per_second(NonZeroU32::new(rps.max(1)).unwrap())
            .allow_burst(NonZeroU32::new(burst.max(1)).unwrap());
        Self {
            limiter: Arc::new(RateLimiter::keyed(quota)),
            trusted: Arc::new(Vec::new()),
        }
    }

    /// Set the proxies whose `X-Forwarded-For` is believed. Entries that
    /// don't parse are logged and ignored.
    pub fn with_trusted_proxies(mut self, proxies: &[String]) -> Self {
        let parsed = proxies
            .iter()
            .filter_map(|p| {
                let c = Cidr::parse(p);
                if c.is_none() {
                    tracing::warn!(proxy = %p, "ignoring unparsable trusted proxy");
                }
                c
            })
            .collect();
        self.trusted = Arc::new(parsed);
        self
    }

    /// Check + decrement. Returns Ok if the request is admitted.
    pub fn check(
        &self,
        key: &str,
    ) -> Result<(), governor::NotUntil<governor::clock::QuantaInstant>> {
        if self.limiter.len() > SWEEP_ABOVE_KEYS {
            self.limiter.retain_recent();
        }
        self.limiter.check_key(&key.to_string())
    }

    /// Number of live buckets (test and metrics hook).
    pub fn len(&self) -> usize {
        self.limiter.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn is_trusted(&self, ip: IpAddr) -> bool {
        self.trusted.iter().any(|c| c.contains(ip))
    }

    fn key_for(&self, req: &Request<Body>) -> String {
        if let Some(p) = req.extensions().get::<Principal>() {
            if p.kind != PrincipalKind::Anonymous {
                return format!("sub:{}", p.subject);
            }
        }
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(a)| a.ip());
        let xff = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok());
        match client_ip(peer, xff, |ip| self.is_trusted(ip)) {
            Some(ip) => format!("ip:{ip}"),
            None => "anonymous".into(),
        }
    }
}

/// Resolve the client address. `X-Forwarded-For` only counts when the
/// socket peer is trusted; walking right to left, the first untrusted hop
/// is the client, since everything left of it is client-controlled.
fn client_ip(
    peer: Option<IpAddr>,
    xff: Option<&str>,
    trusted: impl Fn(IpAddr) -> bool,
) -> Option<IpAddr> {
    let peer = peer?;
    if !trusted(peer) {
        return Some(peer);
    }
    let mut client = peer;
    for hop in xff.unwrap_or("").rsplit(',') {
        match hop.trim().parse::<IpAddr>() {
            Ok(ip) => {
                client = ip;
                if !trusted(ip) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    Some(client)
}

/// An IP network such as `10.0.0.0/8`, or one bare address.
#[derive(Debug, Clone, Copy)]
struct Cidr {
    net: IpAddr,
    bits: u32,
}

impl Cidr {
    fn parse(s: &str) -> Option<Self> {
        let (ip, bits) = match s.split_once('/') {
            Some((ip, bits)) => (ip.parse().ok()?, bits.parse().ok()?),
            None => {
                let ip: IpAddr = s.parse().ok()?;
                (ip, if ip.is_ipv4() { 32 } else { 128 })
            }
        };
        let max = if matches!(ip, IpAddr::V4(_)) { 32 } else { 128 };
        (bits <= max).then_some(Self { net: ip, bits })
    }

    fn contains(&self, ip: IpAddr) -> bool {
        let shifted = |v: u128, width: u32| {
            if self.bits == 0 {
                0
            } else {
                v >> (width - self.bits)
            }
        };
        match (self.net, ip) {
            (IpAddr::V4(a), IpAddr::V4(b)) => {
                shifted(u32::from(a) as u128, 32) == shifted(u32::from(b) as u128, 32)
            }
            (IpAddr::V6(a), IpAddr::V6(b)) => {
                shifted(u128::from(a), 128) == shifted(u128::from(b), 128)
            }
            _ => false,
        }
    }
}

/// Tower middleware. Mount with `axum::middleware::from_fn_with_state`.
pub async fn ratelimit_middleware(
    State(rl): State<RateLimit>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let key = rl.key_for(&req);
    if let Err(_e) = rl.check(&key) {
        RATE_LIMITED.inc();
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request as HttpRequest;

    fn req(headers: &[(&str, &str)], peer: &str, principal: Option<Principal>) -> Request<Body> {
        let mut b = HttpRequest::builder().uri("/");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let mut r = b.body(Body::empty()).unwrap();
        let addr: SocketAddr = format!("{peer}:1234").parse().unwrap();
        r.extensions_mut().insert(ConnectInfo(addr));
        if let Some(p) = principal {
            r.extensions_mut().insert(p);
        }
        r
    }

    fn key_user() -> Principal {
        Principal {
            subject: "apikey:abc".into(),
            scopes: vec![],
            kind: PrincipalKind::ApiKey,
        }
    }

    #[test]
    fn spoofed_subject_header_is_ignored() {
        let rl = RateLimit::new(1, 1);
        let a = rl.key_for(&req(&[("x-cgn-subject", "one")], "203.0.113.9", None));
        let b = rl.key_for(&req(&[("x-cgn-subject", "two")], "203.0.113.9", None));
        assert_eq!(a, "ip:203.0.113.9");
        assert_eq!(a, b);
    }

    #[test]
    fn rotating_header_does_not_escape_the_limit() {
        let rl = RateLimit::new(1, 1);
        let k = |n: &str| {
            rl.key_for(&req(
                &[("x-cgn-subject", n), ("x-forwarded-for", n)],
                "203.0.113.9",
                None,
            ))
        };
        assert!(rl.check(&k("1.1.1.1")).is_ok());
        assert!(rl.check(&k("2.2.2.2")).is_err());
        assert_eq!(rl.len(), 1);
    }

    #[test]
    fn principal_wins_over_ip() {
        let rl = RateLimit::new(1, 1);
        assert_eq!(
            rl.key_for(&req(&[], "203.0.113.9", Some(key_user()))),
            "sub:apikey:abc"
        );
        assert_eq!(
            rl.key_for(&req(&[], "203.0.113.9", Some(Principal::anonymous()))),
            "ip:203.0.113.9"
        );
    }

    #[test]
    fn xff_ignored_from_untrusted_peer() {
        let rl = RateLimit::new(1, 1).with_trusted_proxies(&["10.0.0.0/8".into()]);
        let r = req(&[("x-forwarded-for", "1.2.3.4")], "203.0.113.9", None);
        assert_eq!(rl.key_for(&r), "ip:203.0.113.9");
    }

    #[test]
    fn xff_walks_past_trusted_hops_only() {
        let rl = RateLimit::new(1, 1).with_trusted_proxies(&["10.0.0.0/8".into()]);
        let r = req(
            &[("x-forwarded-for", "6.6.6.6, 198.51.100.7, 10.1.1.1")],
            "10.0.0.2",
            None,
        );
        assert_eq!(rl.key_for(&r), "ip:198.51.100.7");
    }

    #[test]
    fn trusted_peer_without_xff_keys_on_peer() {
        let rl = RateLimit::new(1, 1).with_trusted_proxies(&["10.0.0.2".into()]);
        assert_eq!(rl.key_for(&req(&[], "10.0.0.2", None)), "ip:10.0.0.2");
    }

    #[test]
    fn cidr_matching() {
        let c = Cidr::parse("192.168.1.0/24").unwrap();
        assert!(c.contains("192.168.1.200".parse().unwrap()));
        assert!(!c.contains("192.168.2.1".parse().unwrap()));
        assert!(Cidr::parse("0.0.0.0/0")
            .unwrap()
            .contains("8.8.8.8".parse().unwrap()));
        assert!(Cidr::parse("2001:db8::/32")
            .unwrap()
            .contains("2001:db8::1".parse().unwrap()));
        assert!(Cidr::parse("10.0.0.0/33").is_none());
        assert!(Cidr::parse("nonsense").is_none());
    }
}
