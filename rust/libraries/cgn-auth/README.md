# cgn-auth

[![crates.io](https://img.shields.io/crates/v/cgn-auth.svg)](https://crates.io/crates/cgn-auth)
[![docs.rs](https://docs.rs/cgn-auth/badge.svg)](https://docs.rs/cgn-auth)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

OIDC + API-key authentication and RBAC for Cognitora's OpenAI-compatible
HTTP gateway.

Two strategies, both enabled at the same time:

1. **API keys** — bearer tokens of the form `cgn-<base32 payload>`. Keys
   are stored on disk as their SHA-256; the plaintext only appears in the
   operator's `cgn-ctl key create` output. Each key carries a
   comma-separated scope list (`chat`, `embed`, `admin`).
2. **OIDC ID-tokens** — verified against a configurable issuer's JWKS.
   The token's `sub` is recorded on the request span; scopes come from a
   configurable claim.

Exposed as a tower `Layer` that composes with the rest of the axum router.

## Use

```toml
[dependencies]
cgn-auth = "0.1"
```

```rust
use axum::Router;
use cgn_auth::middleware::AuthLayer;

let app: Router = Router::new()
    // ... your routes ...
    .layer(AuthLayer::from_config(&cfg.auth)?);
```

## Modules

| Module       | Role                                                       |
|--------------|------------------------------------------------------------|
| `api_key`    | Hashed-on-disk bearer tokens with scopes.                  |
| `oidc`       | JWKS-backed ID-token verification.                         |
| `middleware` | tower `Layer` that attaches a `Principal` to the request.  |

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
