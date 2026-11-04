//! Authentication for the OpenAI-compatible HTTP surface.
//!
//! Two strategies, both enabled at the same time:
//!
//! 1. **API keys** — bearer tokens of the form `cgn-<base32 payload>`. The
//!    file format is one line per key, optionally suffixed with a comma-
//!    separated list of scopes (`chat`, `embed`, `admin`). Keys are stored
//!    on disk as their SHA-256; the plaintext lives only in the operator's
//!    `cgn-ctl key create` output.
//!    Scopes also carry policy: `model:<name>` (repeatable, `*` suffix
//!    allowed) restricts models, `tenant:<id>` groups keys into a tenant,
//!    `tpm:<n>` caps tokens per minute and `budget:<n>` caps lifetime
//!    tokens. A key line is therefore a virtual key.
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

    /// May this principal use `model`? No `model:` scope means any model.
    pub fn allows_model(&self, model: &str) -> bool {
        let mut restricted = false;
        for pat in self.scopes.iter().filter_map(|s| s.strip_prefix("model:")) {
            restricted = true;
            let hit = match pat.strip_suffix('*') {
                Some(prefix) => model.starts_with(prefix),
                None => pat == model,
            };
            if hit {
                return true;
            }
        }
        !restricted || self.has_scope("*")
    }

    /// Server-assigned tenant: the `tenant:` scope, else the subject.
    /// Anonymous callers have none. Never derived from client input.
    pub fn tenant(&self) -> String {
        if self.kind == PrincipalKind::Anonymous {
            return String::new();
        }
        self.scopes
            .iter()
            .find_map(|s| s.strip_prefix("tenant:"))
            .unwrap_or(&self.subject)
            .to_string()
    }

    /// Numeric policy scope such as `tpm:60000`.
    pub fn limit(&self, name: &str) -> Option<u64> {
        self.scopes
            .iter()
            .find_map(|s| s.strip_prefix(name)?.strip_prefix(':')?.parse::<u64>().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(scopes: &[&str]) -> Principal {
        Principal {
            subject: "apikey:abc".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            kind: PrincipalKind::ApiKey,
        }
    }

    #[test]
    fn model_scopes_restrict_only_when_present() {
        assert!(p(&["chat"]).allows_model("anything"));
        let limited = p(&["chat", "model:llama3-8b", "model:qwen*"]);
        assert!(limited.allows_model("llama3-8b"));
        assert!(limited.allows_model("qwen3-7b"));
        assert!(!limited.allows_model("deepseek"));
    }

    #[test]
    fn tenant_prefers_scope_then_subject() {
        assert_eq!(p(&["tenant:acme"]).tenant(), "acme");
        assert_eq!(p(&["chat"]).tenant(), "apikey:abc");
        assert_eq!(Principal::anonymous().tenant(), "");
    }

    #[test]
    fn limits_parse_and_ignore_garbage() {
        let k = p(&["tpm:60000", "budget:oops"]);
        assert_eq!(k.limit("tpm"), Some(60_000));
        assert_eq!(k.limit("budget"), None);
        assert_eq!(k.limit("tp"), None);
    }
}
