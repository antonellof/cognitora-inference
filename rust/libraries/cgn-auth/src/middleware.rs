//! Tower middleware that validates `Authorization: Bearer …` against
//! the configured API-key store and/or OIDC verifier and attaches the
//! resulting [`Principal`] as a request extension.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::{api_key::ApiKeyStore, oidc::OidcVerifier, Principal};

/// Authentication state shared across handlers.
#[derive(Clone)]
pub struct AuthState {
    pub api_keys: Option<ApiKeyStore>,
    pub oidc: Option<Arc<OidcVerifier>>,
    /// When true, requests without credentials are rejected with 401.
    pub required: bool,
}

impl AuthState {
    pub fn anonymous() -> Self {
        Self {
            api_keys: None,
            oidc: None,
            required: false,
        }
    }
}

pub async fn auth_middleware(
    state: axum::extract::State<AuthState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    let presented = header
        .as_deref()
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string());

    let mut principal: Option<Principal> = None;

    if let Some(token) = &presented {
        if let Some(keys) = &state.api_keys {
            if let Some(p) = keys.check(token) {
                principal = Some(p);
            }
        }
        if principal.is_none() {
            if let Some(oidc) = &state.oidc {
                match oidc.verify(token).await {
                    Ok(p) => principal = Some(p),
                    Err(e) => tracing::debug!(error=?e, "oidc verify failed"),
                }
            }
        }
    }

    if principal.is_none() && state.required {
        return (StatusCode::UNAUTHORIZED, "missing or invalid token").into_response();
    }

    let p = principal.unwrap_or_else(Principal::anonymous);
    tracing::debug!(subject = %p.subject, "authenticated");
    req.extensions_mut().insert(p);
    next.run(req).await
}
