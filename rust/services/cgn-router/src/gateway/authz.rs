//! Per-request authorization, run by each handler once the body names a
//! model. Authentication already happened in `cgn_auth::middleware`; this
//! decides what the principal may do and pins down who to bill.

use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use cgn_auth::Principal;
use serde_json::json;

use crate::state::SharedState;

/// Who a request runs as.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    /// Ledger key; empty when auth is off.
    pub subject: String,
    /// Server-assigned tenant; empty when auth is off.
    pub tenant: String,
}

impl Caller {
    /// Charge tokens, if there is anyone to charge.
    pub fn charge(&self, state: &SharedState, prompt: u32, completion: u32) {
        if !self.subject.is_empty() {
            state
                .usage
                .charge(&self.subject, &self.tenant, prompt, completion);
        }
    }
}

fn forbidden(message: String) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": { "message": message, "type": "permission_error", "code": null }
        })),
    )
        .into_response()
}

/// Check `scope`, the model and token limits. `principal` is `None` when
/// auth is disabled, which grants everything.
pub fn authorize(
    state: &SharedState,
    principal: Option<&Principal>,
    scope: &str,
    model: &str,
) -> Result<Caller, Response> {
    let Some(p) = principal else {
        return Ok(Caller::default());
    };
    if state.cfg.auth.enforce_scopes && !p.has_scope(scope) {
        return Err(forbidden(format!("token lacks the `{scope}` scope")));
    }
    if !p.allows_model(model) {
        return Err(forbidden(format!("token may not use model `{model}`")));
    }
    if let Err(e) = state.usage.check(p) {
        let mut resp = (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "error": { "message": e.message, "type": "insufficient_quota", "code": e.limit }
            })),
        )
            .into_response();
        if e.retry_after_secs > 0 {
            resp.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                HeaderValue::from(e.retry_after_secs),
            );
        }
        return Err(resp);
    }
    Ok(Caller {
        subject: p.subject.clone(),
        tenant: p.tenant(),
    })
}

/// Metric label for models that are neither configured nor loaded, so
/// client-chosen names can't create series.
pub const UNKNOWN_MODEL: &str = "unknown";

/// Refuse a model that is neither in `[models.*]` nor on any registered
/// node. Run after `authorize`, so only callers allowed to use the name
/// learn whether it exists.
///
/// An empty registry can't tell a typo from a cluster that is still
/// starting or has lost every node, so that case stays a retryable 503.
pub fn known_model(state: &SharedState, model: &str) -> Result<(), Response> {
    let configured = state
        .cfg
        .models
        .iter()
        .any(|(name, mc)| name == model || mc.lora.contains_key(model));
    if configured || state.nodes.knows_model(model) {
        return Ok(());
    }
    if state.nodes.is_empty() {
        return Err(super::generate::error_json(&cgn_core::Error::Unavailable(
            format!("no live node serving model {model}"),
        )));
    }
    Err((
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": {
                "message": format!("The model `{model}` does not exist or you do not have access to it."),
                "type": "invalid_request_error",
                "code": "model_not_found",
            }
        })),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cgn_auth::PrincipalKind;
    use cgn_core::config::Config;

    fn key(scopes: &[&str]) -> Principal {
        Principal {
            subject: "apikey:abc".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            kind: PrincipalKind::ApiKey,
        }
    }

    fn state() -> SharedState {
        SharedState::build(Config::default())
    }

    fn status(r: Result<Caller, Response>) -> StatusCode {
        r.map(|_| StatusCode::OK).unwrap_or_else(|e| e.status())
    }

    /// A state with one registered node serving `base` and adapter `sql`.
    fn state_with_node(ready: bool) -> SharedState {
        let s = state();
        let node = json!({
            "node_id": "a", "address": "http://a:7070", "role": 3,
            "model": "base", "queue_depth": 0, "free_blocks": 0,
            "total_blocks": 0, "power_watts": 0.0,
            "ready": ready, "adapters": ["sql"],
        });
        s.nodes.upsert(serde_json::from_value(node).unwrap());
        s
    }

    fn known(s: &SharedState, model: &str) -> StatusCode {
        known_model(s, model)
            .map(|_| StatusCode::OK)
            .unwrap_or_else(|e| e.status())
    }

    #[test]
    fn a_model_no_node_carries_is_404() {
        let s = state_with_node(true);
        assert_eq!(known(&s, "typo"), StatusCode::NOT_FOUND);
        assert_eq!(known(&s, "base"), StatusCode::OK);
        assert_eq!(known(&s, "sql"), StatusCode::OK);
    }

    /// A node that is still loading makes its model unavailable, not unknown.
    #[test]
    fn a_model_on_an_unready_node_is_known() {
        assert_eq!(known(&state_with_node(false), "base"), StatusCode::OK);
    }

    #[test]
    fn configured_models_and_adapters_are_known_without_nodes() {
        let mut cfg = Config::default();
        let mc = cfg.models.entry("big".into()).or_default();
        mc.lora.insert("big-sql".into(), "/adapters/sql".into());
        let s = SharedState::build(cfg);
        assert_eq!(known(&s, "big"), StatusCode::OK);
        assert_eq!(known(&s, "big-sql"), StatusCode::OK);
    }

    /// With no nodes at all a typo and an outage look the same.
    #[test]
    fn an_empty_registry_is_503_not_404() {
        assert_eq!(known(&state(), "anything"), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn auth_off_allows_everything() {
        let c = authorize(&state(), None, "chat", "m").unwrap();
        assert!(c.subject.is_empty() && c.tenant.is_empty());
    }

    #[test]
    fn missing_scope_is_forbidden() {
        let s = state();
        assert_eq!(
            status(authorize(&s, Some(&key(&["embed"])), "chat", "m")),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(authorize(&s, Some(&key(&[])), "chat", "m")),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(authorize(&s, Some(&key(&["chat"])), "chat", "m")),
            StatusCode::OK
        );
        assert_eq!(
            status(authorize(&s, Some(&key(&["*"])), "chat", "m")),
            StatusCode::OK
        );
    }

    #[test]
    fn scope_enforcement_can_be_disabled_for_migration() {
        let mut c = Config::default();
        c.auth.enforce_scopes = false;
        let s = SharedState::build(c);
        assert_eq!(
            status(authorize(&s, Some(&key(&[])), "chat", "m")),
            StatusCode::OK
        );
    }

    #[test]
    fn model_restriction_applies() {
        let s = state();
        let k = key(&["chat", "model:allowed"]);
        assert_eq!(
            status(authorize(&s, Some(&k), "chat", "allowed")),
            StatusCode::OK
        );
        assert_eq!(
            status(authorize(&s, Some(&k), "chat", "other")),
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn spent_budget_is_refused_with_429() {
        let s = state();
        let k = key(&["chat", "budget:10"]);
        s.usage.charge(&k.subject, "t", 10, 0);
        assert_eq!(
            status(authorize(&s, Some(&k), "chat", "m")),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn tenant_comes_from_the_key() {
        let c = authorize(&state(), Some(&key(&["chat", "tenant:acme"])), "chat", "m").unwrap();
        assert_eq!(c.tenant, "acme");
    }
}
