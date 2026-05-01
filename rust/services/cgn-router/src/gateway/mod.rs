//! OpenAI-compatible HTTP gateway.
//!
//! Routes:
//!
//! | Method | Path                          | Handler              |
//! | ------ | ----------------------------- | -------------------- |
//! | POST   | /v1/chat/completions          | `chat::completions`  |
//! | POST   | /v1/completions               | `chat::completions`  |
//! | POST   | /v1/embeddings                | `embed::embeddings`  |
//! | GET    | /v1/models                    | `models::list`       |
//! | GET    | /healthz, /readyz             | static OK            |
//!
//! Streaming requests use Server-Sent Events with the `data: …\n\n` framing
//! that OpenAI's reference clients expect.

mod chat;
mod embed;
mod models;
mod sse;
mod types;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use cgn_core::{Error, Result};
use tower_http::trace::TraceLayer;

use crate::state::SharedState;

pub async fn serve(state: Arc<SharedState>, addr: SocketAddr) -> Result<()> {
    let app = router(state.clone()).layer(TraceLayer::new_for_http());
    tracing::info!(%addr, "openai surface listening");
    let listener = tokio::net::TcpListener::bind(addr).await
        .map_err(Error::Io)?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await
        .map_err(|e| Error::Internal(format!("gateway serve: {e}")))
}

fn router(state: Arc<SharedState>) -> Router {
    let auth = build_auth_state(&state);

    let api = Router::new()
        .route("/v1/chat/completions", post(chat::completions))
        .route("/v1/completions",      post(chat::completions))
        .route("/v1/embeddings",       post(embed::embeddings))
        .route("/v1/models",           get(models::list));

    let api = if auth.required || auth.api_keys.is_some() || auth.oidc.is_some() {
        api.layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            cgn_auth::middleware::auth_middleware,
        ))
        .with_state(auth.clone())
    } else {
        api
    };

    let _ = auth; // future: rate limit reuses the same state container.

    Router::new()
        .merge(api)
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz",  get(|| async { "ok" }))
        .with_state(state)
}

fn build_auth_state(state: &SharedState) -> cgn_auth::middleware::AuthState {
    let mut s = cgn_auth::middleware::AuthState::anonymous();
    if !state.cfg.auth.enabled {
        return s;
    }
    s.required = true;

    if let Some(path) = &state.cfg.auth.api_keys_file {
        match cgn_auth::api_key::ApiKeyStore::from_file(path) {
            Ok(store) => s.api_keys = Some(store),
            Err(e) => tracing::warn!(error=?e, path=%path.display(), "api keys file unreadable"),
        }
    }
    if let Some(issuer) = &state.cfg.auth.oidc_issuer {
        s.oidc = Some(Arc::new(cgn_auth::oidc::OidcVerifier::new(
            issuer.clone(),
            state.cfg.auth.oidc_audience.clone(),
        )));
    }
    s
}
