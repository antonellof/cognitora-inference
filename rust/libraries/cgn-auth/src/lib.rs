//! Authentication for the OpenAI-compatible HTTP surface.
//!
//! Two strategies, both enabled at the same time:
//!
//! 1. **API keys** — bearer tokens of the form `cgn-<base32 payload>`. The
//!    file format is one line per key, optionally suffixed with a comma-
//!    separated list of scopes (`chat`, `embed`, `admin`). Keys are stored
//!    on disk as their SHA-256; the plaintext lives only in the operator's
//!    `cgn-ctl key create` output.
//! 2. **OIDC ID-tokens** — verified against a configurable issuer's JWKS.
//!    The token's `sub` is recorded on the request span; scopes come from
//!    a configurable claim.
//!
//! The middleware exposes itself as a tower `Layer` so it composes with
//! the rest of the axum router.

#![forbid(unsafe_code)]

pub mod api_key;
pub mod middleware;
pub mod oidc;

use serde::{Deserialize, Serialize};

/// Authenticated principal attached to a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Principal {
    pub subject: String,
    pub scopes: Vec<String>,
    pub kind: PrincipalKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    ApiKey,
    Oidc,
    Anonymous,
}

impl Principal {
    pub fn anonymous() -> Self {
        Self {
            subject: "anonymous".into(),
            scopes: vec![],
            kind: PrincipalKind::Anonymous,
        }
    }

    /// Has any of the given scopes (or a global `*`).
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope || s == "*")
    }
}
