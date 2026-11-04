//! `/v1/models` — list every model the cluster currently serves.
//!
//! The list is derived from `cgn-core`'s static config (`[models.<name>]`)
//! plus runtime liveness in the node registry. Unlike OpenAI we surface the
//! union: a model returns 200 only if at least one node reports it as
//! loaded.

use std::sync::Arc;

use axum::{extract::State, response::IntoResponse, Extension, Json};
use cgn_auth::Principal;

use crate::state::SharedState;

use super::types::{ModelEntry, ModelsResponse};

pub async fn list(
    State(state): State<Arc<SharedState>>,
    principal: Option<Extension<Principal>>,
) -> impl IntoResponse {
    use std::collections::BTreeMap;

    // id -> parent model, for adapters.
    let mut names: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (model, mc) in &state.cfg.models {
        names.entry(model.clone()).or_default();
        for adapter in mc.lora.keys() {
            names.insert(adapter.clone(), Some(model.clone()));
        }
    }
    // Augment with whatever live agents currently advertise.
    for kv in state
        .nodes
        .nodes_for(cgn_proto::v1::NodeRole::Unspecified, None)
    {
        if let Some(m) = &kv.model {
            names.entry(m.clone()).or_default();
            for adapter in &kv.adapters {
                names.insert(adapter.clone(), Some(m.clone()));
            }
        }
    }

    let created = state.started.elapsed().as_secs() as i64;
    let data = names
        .into_iter()
        .filter(|(id, _)| principal.as_ref().is_none_or(|p| p.allows_model(id)))
        .map(|(id, parent)| ModelEntry {
            id,
            object: "model",
            created,
            owned_by: "cognitora",
            parent,
        })
        .collect();

    Json(ModelsResponse {
        object: "list",
        data,
    })
}

/// `GET /v1/usage`: the caller's own token totals and limits.
pub async fn usage(
    State(state): State<Arc<SharedState>>,
    principal: Option<Extension<Principal>>,
) -> axum::response::Response {
    match principal {
        Some(p) => Json(state.usage.report(&p)).into_response(),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "message": "usage needs auth enabled", "type": "not_found_error" }
            })),
        )
            .into_response(),
    }
}
