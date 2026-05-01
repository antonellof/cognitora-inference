//! Lightweight OIDC bearer-token verifier.
//!
//! Only validates JWTs against a JWKS fetched from the issuer's `jwks_uri`.
//! Full OIDC code-flow login is out of scope — Cognitora is server-to-server
//! and assumes the operator already has an ID token from their IdP.

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use cgn_core::{Error, Result};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use parking_lot::Mutex;
use serde::Deserialize;

use super::{Principal, PrincipalKind};

#[derive(Clone)]
pub struct OidcVerifier {
    issuer:   String,
    audience: Option<String>,
    jwks:     Arc<ArcSwap<JwksCache>>,
    refresh:  Arc<Mutex<Instant>>,
}

#[derive(Default)]
struct JwksCache {
    keys: Vec<JwkEntry>,
}

struct JwkEntry {
    kid: String,
    alg: Algorithm,
    key: DecodingKey,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    alg: Option<String>,
    n: Option<String>,
    e: Option<String>,
    x: Option<String>,
    y: Option<String>,
    #[allow(dead_code)] // some IdPs send `crv` for EC keys; we infer it from the components.
    crv: Option<String>,
    kty: String,
}

#[derive(Deserialize)]
struct OidcDiscovery {
    jwks_uri: String,
}

impl OidcVerifier {
    /// Construct a verifier against `issuer`. Lazy: keys are fetched on first
    /// successful verification.
    pub fn new(issuer: impl Into<String>, audience: Option<String>) -> Self {
        Self {
            issuer:   issuer.into(),
            audience,
            jwks:     Arc::new(ArcSwap::from_pointee(JwksCache::default())),
            refresh:  Arc::new(Mutex::new(Instant::now() - Duration::from_secs(86400))),
        }
    }

    pub async fn verify(&self, token: &str) -> Result<Principal> {
        // Refresh JWKS once an hour (or if cache is empty).
        if self.jwks.load().keys.is_empty() || self.refresh.lock().elapsed() > Duration::from_secs(3600) {
            self.refresh_jwks().await?;
        }

        let header = decode_header(token)
            .map_err(|e| Error::InvalidArgument(format!("oidc header: {e}")))?;
        let kid = header.kid.ok_or_else(|| Error::InvalidArgument("oidc: no kid".into()))?;

        let snap = self.jwks.load();
        let entry = snap.keys.iter()
            .find(|k| k.kid == kid)
            .ok_or_else(|| Error::InvalidArgument(format!("oidc: kid {kid} not in jwks")))?;

        let mut v = Validation::new(entry.alg);
        v.set_issuer(&[&self.issuer]);
        if let Some(aud) = &self.audience { v.set_audience(&[aud]); }
        let token_data = decode::<Claims>(token, &entry.key, &v)
            .map_err(|e| Error::InvalidArgument(format!("oidc verify: {e}")))?;
        let scopes = token_data.claims
            .scope
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();

        Ok(Principal {
            subject: token_data.claims.sub,
            scopes,
            kind: PrincipalKind::Oidc,
        })
    }

    async fn refresh_jwks(&self) -> Result<()> {
        let disc_url = format!("{}/.well-known/openid-configuration", self.issuer.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| Error::Internal(format!("reqwest: {e}")))?;
        let disc: OidcDiscovery = client.get(&disc_url).send().await
            .map_err(|e| Error::Unavailable(format!("oidc discovery: {e}")))?
            .json().await
            .map_err(|e| Error::InvalidArgument(format!("oidc discovery body: {e}")))?;
        let jwks: Jwks = client.get(&disc.jwks_uri).send().await
            .map_err(|e| Error::Unavailable(format!("oidc jwks: {e}")))?
            .json().await
            .map_err(|e| Error::InvalidArgument(format!("oidc jwks body: {e}")))?;

        let entries = jwks.keys.into_iter().filter_map(|k| build_entry(&k)).collect();
        self.jwks.store(Arc::new(JwksCache { keys: entries }));
        *self.refresh.lock() = Instant::now();
        Ok(())
    }
}

#[derive(Deserialize)]
struct Claims {
    sub:   String,
    scope: Option<String>,
}

fn build_entry(k: &Jwk) -> Option<JwkEntry> {
    let alg = match k.alg.as_deref() {
        Some("RS256") => Algorithm::RS256,
        Some("RS384") => Algorithm::RS384,
        Some("RS512") => Algorithm::RS512,
        Some("ES256") => Algorithm::ES256,
        Some("ES384") => Algorithm::ES384,
        _ => return None,
    };
    let key = match k.kty.as_str() {
        "RSA" => {
            let n = k.n.as_deref()?;
            let e = k.e.as_deref()?;
            DecodingKey::from_rsa_components(n, e).ok()?
        }
        "EC" => {
            let x = k.x.as_deref()?;
            let y = k.y.as_deref()?;
            DecodingKey::from_ec_components(x, y).ok()?
        }
        _ => return None,
    };
    Some(JwkEntry { kid: k.kid.clone(), alg, key })
}
