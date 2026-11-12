//! `/v1/embeddings` handler.
//!
//! Pipeline:
//!
//! 1. Pick a target node via the same scoring path as `/v1/chat/completions`
//!    (KV-aware, role-filtered, cordon-aware).
//! 2. Connect to the agent over gRPC mTLS via `state.connect_agent`.
//! 3. Call `Agent.Embed` and reshape the response into OpenAI's wire
//!    format (`{ object:"list", data:[{embedding:[…]}], model, usage }`).
//!
//! Errors are translated as:
//!   * empty input → 400 invalid_request_error
//!   * routing error → 503 service_unavailable
//!   * agent UNAVAILABLE (e.g. engine returned 404 from /v1/embeddings) →
//!     503 with the engine's message
//!   * other agent gRPC errors → 502 bad_gateway

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Extension, Json,
};
use cgn_auth::Principal;
use cgn_proto::v1::{EmbedRequest as PEmbedRequest, NodeRole};
use serde_json::json;
use tracing::{info, warn};

use crate::routing;
use crate::state::SharedState;

use super::types::{EmbedItem, EmbedRequest, EmbedResponse, Usage};

pub async fn embeddings(
    State(state): State<Arc<SharedState>>,
    principal: Option<Extension<Principal>>,
    Json(req): Json<EmbedRequest>,
) -> Response {
    let caller = match super::authz::authorize(&state, principal.as_deref(), "embed", &req.model) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let inputs = req.input.into_vec();
    if inputs.is_empty() {
        return error_with(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "input must be a non-empty string or array",
        );
    }
    if let Err(resp) = super::authz::known_model(&state, &req.model) {
        return resp;
    }

    let token_ids = state
        .tokenizers
        .token_ids(&req.model, &inputs.join(" "))
        .await;
    let prompt_tokens = token_ids.len() as u32;
    let _permit =
        match crate::admission::admit_request(&state, &req.model, prompt_tokens, &caller.tenant)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                return error_with(
                    StatusCode::TOO_MANY_REQUESTS,
                    "server_error",
                    &e.to_string(),
                );
            }
        };
    let salt = routing::prefix_salt(&state, &req.model, &caller.tenant);
    let key = routing::RouteKey {
        model: &req.model,
        salt: &salt,
        session: None,
        token_ids: &token_ids,
    };
    let decision = match routing::pick(&state, &key, NodeRole::Both).await {
        Ok(d) => d,
        Err(e) => {
            return error_with(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                &format!("routing: {e}"),
            );
        }
    };
    let _node = state.track_node(&decision.node.node_id);
    info!(
        node = %decision.node.node_id,
        score = decision.score.total,
        candidates = decision.n_candidates,
        "embed decision"
    );

    let mut client = match state.connect_agent(&decision.node.address).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error=?e, node=%decision.node.node_id, "agent connect failed for embed");
            return error_with(
                StatusCode::BAD_GATEWAY,
                "bad_gateway",
                &format!("agent connect: {e}"),
            );
        }
    };

    let proto = PEmbedRequest {
        model: req.model.clone(),
        inputs: inputs.clone(),
        tenant: caller.tenant.clone(),
    };

    let resp = match client.embed(proto).await {
        Ok(r) => r.into_inner(),
        Err(s) => {
            let (code, ty) = match s.code() {
                tonic::Code::Unavailable => {
                    (StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
                }
                tonic::Code::InvalidArgument => (StatusCode::BAD_REQUEST, "invalid_request_error"),
                tonic::Code::Unimplemented => (StatusCode::NOT_IMPLEMENTED, "not_implemented"),
                _ => (StatusCode::BAD_GATEWAY, "bad_gateway"),
            };
            return error_with(code, ty, s.message());
        }
    };

    if resp.embeddings.len() != inputs.len() {
        return error_with(
            StatusCode::BAD_GATEWAY,
            "bad_gateway",
            &format!(
                "agent returned {} embeddings for {} inputs",
                resp.embeddings.len(),
                inputs.len()
            ),
        );
    }

    let data = resp
        .embeddings
        .into_iter()
        .enumerate()
        .map(|(i, vec)| EmbedItem {
            object: "embedding",
            index: i as u32,
            embedding: vec.values,
        })
        .collect();

    let model_name = if resp.model.is_empty() {
        req.model.clone()
    } else {
        resp.model
    };

    let tokens = if resp.tokens > 0 {
        resp.tokens
    } else {
        prompt_tokens
    };
    caller.charge(&state, tokens, 0);

    Json(EmbedResponse {
        object: "list",
        data,
        model: model_name,
        usage: Usage {
            prompt_tokens: tokens,
            completion_tokens: 0,
            total_tokens: tokens,
        },
    })
    .into_response()
}

fn error_with(status: StatusCode, ty: &str, msg: &str) -> Response {
    (
        status,
        Json(json!({
            "error": { "message": msg, "type": ty }
        })),
    )
        .into_response()
}
