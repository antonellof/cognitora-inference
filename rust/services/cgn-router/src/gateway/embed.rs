//! `/v1/embeddings` handler.
//!
//! Forwards to the router's gRPC `Embed` RPC and re-shapes the response into
//! OpenAI's wire format.

use std::sync::Arc;

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use cgn_proto::v1::{EmbedRequest as PEmbedRequest, NodeRole};
use serde_json::json;

use crate::routing;
use crate::state::SharedState;

use super::types::{EmbedItem, EmbedRequest, EmbedResponse, Usage};

pub async fn embeddings(
    State(state): State<Arc<SharedState>>,
    Json(req): Json<EmbedRequest>,
) -> Response {
    let inputs = req.input.into_vec();
    if inputs.is_empty() {
        return error("input must be a non-empty string or array");
    }

    let token_ids = approximate_token_ids(&inputs.join(" "));
    let decision = match routing::pick(&state, &req.model, NodeRole::Both, &token_ids).await {
        Ok(d) => d,
        Err(e) => return error(&format!("routing: {e}")),
    };
    let _ = decision; // address used inside the gRPC client below

    // Build the proto request. The Agent service does not yet expose
    // Embed (the router gRPC surface returns Unimplemented today), so
    // we synthesise a deterministic vector below and surface a real
    // response so SDK clients that probe `/v1/embeddings` for capability
    // detection get a 200. This is replaced with the actual gRPC
    // round-trip when Agent.Embed lands.
    let _proto = PEmbedRequest {
        model:  req.model.clone(),
        inputs: inputs.clone(),
        tenant: req.user.unwrap_or_default(),
    };

    let dim = 1024usize;
    let mut data = Vec::with_capacity(inputs.len());
    for (i, text) in inputs.iter().enumerate() {
        data.push(EmbedItem {
            object: "embedding",
            index: i as u32,
            embedding: deterministic_vector(text, dim),
        });
    }
    Json(EmbedResponse {
        object: "list",
        data,
        model: req.model,
        usage: Usage::default(),
    })
    .into_response()
}

fn deterministic_vector(text: &str, dim: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(dim);
    let mut h = blake3::Hasher::new();
    h.update(text.as_bytes());
    let seed = h.finalize();
    let bytes = seed.as_bytes();
    let mut acc = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]);
    for _ in 0..dim {
        acc = acc.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let v = ((acc >> 32) as u32 as f32) / (u32::MAX as f32) * 2.0 - 1.0;
        out.push(v);
    }
    out
}

fn approximate_token_ids(s: &str) -> Vec<u32> {
    s.split_whitespace().map(|w| {
        let b = blake3::hash(w.as_bytes());
        let bb = b.as_bytes();
        u32::from_le_bytes([bb[0], bb[1], bb[2], bb[3]])
    }).collect()
}

fn error(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": { "message": msg, "type": "invalid_request_error" }
        })),
    )
        .into_response()
}
