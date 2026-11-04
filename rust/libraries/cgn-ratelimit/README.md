# cgn-ratelimit

[![crates.io](https://img.shields.io/crates/v/cgn-ratelimit.svg)](https://crates.io/crates/cgn-ratelimit)
[![docs.rs](https://docs.rs/cgn-ratelimit/badge.svg)](https://docs.rs/cgn-ratelimit)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Token-bucket rate limiting for Cognitora's gateway surface.

Two backends:

* **In-process** (default) — `governor` per-key token bucket. Cheap and
  precise, but each replica keeps its own state.
* **Redis-backed** (`features = ["redis-backend"]`) — atomic GCRA with an
  `INCR / PEXPIRE` round-trip per request. `cgn-router` doesn't use this
  backend yet, so its limits are per replica.

The middleware keys on the authenticated
[`cgn-auth`](https://crates.io/crates/cgn-auth) principal, otherwise on the
client IP. The IP is the socket peer unless that peer is a trusted proxy
(`RateLimit::with_trusted_proxies`), in which case it is the right-most
untrusted `X-Forwarded-For` hop. Client headers alone never pick a bucket.

## Use

```toml
[dependencies]
cgn-ratelimit = "0.1"
# or with the Redis backend:
# cgn-ratelimit = { version = "0.1", features = ["redis-backend"] }
```

```rust
use axum::Router;
use cgn_ratelimit::Limiter;

let limiter = Limiter::in_process(/* rps */ 100, /* burst */ 200)?;
let app: Router = Router::new()
    // ... your routes ...
    .layer(limiter.into_layer());
```

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
