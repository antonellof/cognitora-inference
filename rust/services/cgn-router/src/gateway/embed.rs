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
    Json,
};
use cgn_proto::v1::{EmbedRequest as PEmbedRequest, NodeRole};
use serde_json::json;
use tracing::{info, warn};

use crate::routing;
use crate::state::SharedState;

use super::types::{EmbedItem, EmbedRequest, EmbedResponse, Usage};

pub async fn embeddings(
    State(state): State<Arc<SharedState>>,
    Json(req): Json<EmbedRequest>,
) -> Response {
    let inputs = req.input.into_vec();
    if inputs.is_empty() {
        return error_with(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "input must be a non-empty string or array",
        );
    }

    let token_ids = approximate_token_ids(&inputs.join(" "));
    let decision = match routing::pick(&state, &req.model, NodeRole::Both, &token_ids).await {
        Ok(d) => d,
        Err(e) => {
            return error_with(
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                &format!("routing: {e}"),
            );
        }
    };
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
        tenant: req.user.unwrap_or_default(),
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

    Json(EmbedResponse {
        object: "list",
        data,
        model: model_name,
        usage: Usage {
            prompt_tokens: resp.tokens,
            completion_tokens: 0,
            total_tokens: resp.tokens,
        },
    })
    .into_response()
}

fn approximate_token_ids(s: &str) -> Vec<u32> {
    s.split_whitespace()
        .map(|w| {
            let b = blake3::hash(w.as_bytes());
            let bb = b.as_bytes();
            u32::from_le_bytes([bb[0], bb[1], bb[2], bb[3]])
        })
        .collect()
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
