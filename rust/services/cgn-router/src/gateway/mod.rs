//! OpenAI-compatible HTTP gateway.
//!
//! Routes:
//!
//! | Method | Path                          | Handler              |
//! | ------ | ----------------------------- | -------------------- |
//! | POST   | /v1/chat/completions          | `chat::completions`  |
//! | POST   | /v1/completions               | `chat::text_completions` |
//! | POST   | /v1/embeddings                | `embed::embeddings`  |
//! | GET    | /v1/models                    | `models::list`       |
//! | GET    | /v1/usage                     | `models::usage`      |
//! | GET    | /healthz                      | static OK            |
//! | GET    | /readyz                       | `SharedState::readiness` |
//!
//! Streaming requests use Server-Sent Events with the `data: …\n\n` framing
//! that OpenAI's reference clients expect.

mod authz;
mod chat;
mod embed;
mod generate;
pub(crate) mod metrics;
mod models;
mod sse;
mod types;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{get, post},
    Router,
};
use cgn_core::{Error, Result};
use tower_http::trace::TraceLayer;

use crate::state::SharedState;

/// How often the API-keys file is re-read.
const KEY_RELOAD_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Bind the HTTP port. Done before the admin listener starts, so a passing
/// `/healthz` means the gateway can accept connections.
pub async fn bind(addr: SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr).await.map_err(Error::Io)
}

pub async fn serve(
    state: Arc<SharedState>,
    listener: tokio::net::TcpListener,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    // Eagerly register every metric so /metrics returns the full schema
    // even before any request has been served.
    metrics::warm_up();
    crate::admission::warm_up_metrics();
    let app = router(state.clone()).layer(TraceLayer::new_for_http());
    tracing::info!(addr = ?listener.local_addr().ok(), "openai surface listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = shutdown.changed().await;
        tracing::info!("gateway no longer accepting connections");
    })
    .await
    .map_err(|e| Error::Internal(format!("gateway serve: {e}")))
}

fn router(state: Arc<SharedState>) -> Router {
    let auth = build_auth_state(&state);
    let auth_active = auth.required || auth.api_keys.is_some() || auth.oidc.is_some();

    let rl_cfg = &state.cfg.router.rate_limit;
    let rl = cgn_ratelimit::RateLimit::new(rl_cfg.rps, rl_cfg.burst)
        .with_trusted_proxies(&rl_cfg.trusted_proxies);
    if rl_cfg.redis_url.is_some() {
        tracing::warn!(
            "[router.rate_limit].redis_url is not implemented; limits are per-router in-process"
        );
    }

    let mut api = Router::new()
        .route("/v1/chat/completions", post(chat::completions))
        .route("/v1/completions", post(chat::text_completions))
        .route("/v1/embeddings", post(embed::embeddings))
        .route("/v1/models", get(models::list))
        .route("/v1/usage", get(models::usage))
        .layer(DefaultBodyLimit::max(
            state.cfg.router.max_body_mb as usize * 1024 * 1024,
        ))
        .with_state(state.clone());

    // Rate limit is applied first (innermost layer runs last on the request
    // path; we want auth to populate the principal before the limiter keys
    // on it via the `x-cgn-subject` header).
    api = api.layer(axum::middleware::from_fn_with_state(
        rl,
        cgn_ratelimit::ratelimit_middleware,
    ));

    if auth_active {
        api = api.layer(axum::middleware::from_fn_with_state(
            auth,
            cgn_auth::middleware::auth_middleware,
        ));
    }

    Router::new()
        .merge(api)
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/readyz",
            get(move || {
                let state = state.clone();
                async move {
                    match state.readiness() {
                        Ok(()) => (StatusCode::OK, "ok".to_string()),
                        Err(why) => (StatusCode::SERVICE_UNAVAILABLE, why),
                    }
                }
            }),
        )
}

fn build_auth_state(state: &SharedState) -> cgn_auth::middleware::AuthState {
    let mut s = cgn_auth::middleware::AuthState::anonymous();
    if !state.cfg.auth.enabled {
        return s;
    }
    s.required = true;

    if let Some(path) = &state.cfg.auth.api_keys_file {
        match cgn_auth::api_key::ApiKeyStore::from_file(path) {
            Ok(store) => {
                spawn_key_reload(store.clone());
                s.api_keys = Some(store);
            }
            Err(e) => tracing::warn!(error=?e, path=%path.display(), "api keys file unreadable"),
        }
    }
    if let Some(issuer) = &state.cfg.auth.oidc_issuer {
        let auth = &state.cfg.auth;
        let mut oidc =
            cgn_auth::oidc::OidcVerifier::new(issuer.clone(), auth.oidc_audience.clone());
        if !auth.oidc_group_scopes.is_empty() {
            oidc = oidc.with_group_scope_map(group_scope_pairs(&auth.oidc_group_scopes));
        }
        if let Some(claim) = &auth.oidc_groups_claim {
            oidc = oidc.with_groups_claim(claim.clone());
        }
        s.oidc = Some(Arc::new(oidc));
    }
    s
}

fn group_scope_pairs(
    map: &std::collections::BTreeMap<String, Vec<String>>,
) -> Vec<(String, String)> {
    map.iter()
        .flat_map(|(g, scopes)| scopes.iter().map(move |s| (g.clone(), s.clone())))
        .collect()
}

/// Pick up `cgn-ctl key create/revoke` without a restart, so a revoked key
/// stops working within one interval.
fn spawn_key_reload(store: cgn_auth::api_key::ApiKeyStore) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(KEY_RELOAD_INTERVAL);
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = store.reload() {
                tracing::warn!(error = ?e, "api keys reload failed; keeping previous keys");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use cgn_core::config::Config;
    use tower::ServiceExt;

    fn app(cfg: Config) -> Router {
        router(Arc::new(SharedState::build(cfg)))
    }

    async fn call(app: Router, req: Request<Body>) -> (StatusCode, String) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    fn post(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// No nodes exist, so any request reaching dispatch fails with 503.
    #[tokio::test]
    async fn streaming_failure_is_a_real_status_not_an_empty_200() {
        let body = serde_json::json!({
            "model": "nope", "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let (status, text) = call(app(Config::default()), post("/v1/chat/completions", body)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(text.contains("error"), "{text}");
        assert!(!text.contains("[DONE]"));
    }

    #[tokio::test]
    async fn streaming_admission_rejection_is_429() {
        let mut cfg = Config::default();
        cfg.router.admission.max_queue = 0;
        cfg.router.admission.wait_queue_per_tenant = 0;
        let body = serde_json::json!({
            "model": "m", "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let (status, _) = call(app(cfg), post("/v1/chat/completions", body)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn legacy_completions_accept_a_prompt() {
        let body = serde_json::json!({"model": "nope", "prompt": "hi"});
        let (status, text) = call(app(Config::default()), post("/v1/completions", body)).await;
        // Past parsing and into routing: not the 422 it used to be.
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    }

    #[tokio::test]
    async fn multiple_choices_are_refused_not_ignored() {
        let body = serde_json::json!({"model": "m", "n": 2, "messages": []});
        let (status, _) = call(app(Config::default()), post("/v1/chat/completions", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn bodies_above_axum_default_are_accepted() {
        let image = "x".repeat(3 * 1024 * 1024);
        let body = serde_json::json!({
            "model": "nope",
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": image}}
            ]}]
        });
        let (status, _) = call(app(Config::default()), post("/v1/chat/completions", body)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn bodies_above_the_limit_are_rejected() {
        let mut cfg = Config::default();
        cfg.router.max_body_mb = 1;
        let body = serde_json::json!({"model": "m", "prompt": "x".repeat(2 * 1024 * 1024)});
        let (status, _) = call(app(cfg), post("/v1/completions", body)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn auth_rejects_wrong_scope_and_wrong_model() {
        let dir = tempfile::tempdir().unwrap();
        let keys = dir.path().join("keys");
        std::fs::write(
            &keys,
            "embed-only embed\nchat-limited chat,model:allowed\nnoscopes\n",
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.auth.enabled = true;
        cfg.auth.api_keys_file = Some(keys);
        let a = app(cfg);
        let chat = |key: &str, model: &str| {
            Request::post("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {key}"))
                .body(Body::from(
                    serde_json::json!({"model": model, "messages": []}).to_string(),
                ))
                .unwrap()
        };
        assert_eq!(
            call(a.clone(), chat("bad", "m")).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(a.clone(), chat("embed-only", "m")).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(a.clone(), chat("noscopes", "m")).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(a.clone(), chat("chat-limited", "other")).await.0,
            StatusCode::FORBIDDEN
        );
        // Passes authorization, then fails routing: no such node.
        assert_eq!(
            call(a, chat("chat-limited", "allowed")).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn spoofed_subject_cannot_escape_the_limiter() {
        let mut cfg = Config::default();
        cfg.router.rate_limit.rps = 1;
        cfg.router.rate_limit.burst = 1;
        let a = app(cfg);
        let peer: SocketAddr = "203.0.113.9:1".parse().unwrap();
        let req = |n: u32| {
            let mut r = Request::get("/v1/models")
                .header("x-cgn-subject", n.to_string())
                .body(Body::empty())
                .unwrap();
            r.extensions_mut().insert(axum::extract::ConnectInfo(peer));
            r
        };
        assert_eq!(call(a.clone(), req(1)).await.0, StatusCode::OK);
        assert_eq!(call(a, req(2)).await.0, StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn readyz_tracks_discovery_and_drain() {
        let mut cfg = Config::default();
        cfg.cluster.etcd_endpoints = vec!["127.0.0.1:1".into()];
        let state = Arc::new(SharedState::build(cfg));
        let probe = || Request::get("/readyz").body(Body::empty()).unwrap();
        let (status, why) = call(router(state.clone()), probe()).await;
        assert_eq!(
            (status, why.as_str()),
            (StatusCode::SERVICE_UNAVAILABLE, "no ready nodes")
        );
        state
            .draining
            .store(true, std::sync::atomic::Ordering::Release);
        let (_, why) = call(router(state), probe()).await;
        assert_eq!(why, "draining");
    }

    /// Overload isn't an outage: alerts page on `5xx`, so a 429 can't use it.
    #[tokio::test]
    async fn admission_rejections_are_not_counted_as_server_errors() {
        let mut cfg = Config::default();
        cfg.router.admission.max_queue = 0;
        cfg.router.admission.wait_queue_per_tenant = 0;
        let body = serde_json::json!({"model": "metrics-429", "messages": []});
        let (status, _) = call(app(cfg), post("/v1/chat/completions", body)).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        let count = |label: &str| {
            metrics::CHAT_REQUESTS
                .with_label_values(&["metrics-429", label])
                .get()
        };
        assert_eq!((count("429"), count("5xx")), (1, 0));
    }

    #[tokio::test]
    async fn readyz_is_ok_without_discovery() {
        let probe = Request::get("/readyz").body(Body::empty()).unwrap();
        assert_eq!(call(app(Config::default()), probe).await.0, StatusCode::OK);
    }

    #[test]
    fn group_scopes_flatten() {
        let map = [(
            "ml".to_string(),
            vec!["chat".to_string(), "tenant:ml".to_string()],
        )]
        .into();
        assert_eq!(group_scope_pairs(&map).len(), 2);
    }
}
