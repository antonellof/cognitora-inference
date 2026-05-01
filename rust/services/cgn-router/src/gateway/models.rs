//! `/v1/models` — list every model the cluster currently serves.
//!
//! The list is derived from `cgn-core`'s static config (`[models.<name>]`)
//! plus runtime liveness in the node registry. Unlike OpenAI we surface the
//! union: a model returns 200 only if at least one node reports it as
//! loaded.

use std::sync::Arc;

use axum::{extract::State, response::IntoResponse, Json};

use crate::state::SharedState;

use super::types::{ModelEntry, ModelsResponse};

pub async fn list(State(state): State<Arc<SharedState>>) -> impl IntoResponse {
    use std::collections::BTreeSet;

    let mut names: BTreeSet<String> = state.cfg.models.keys().cloned().collect();
    // Augment with whatever live agents currently advertise.
    for kv in state.nodes.nodes_for(cgn_proto::v1::NodeRole::Unspecified, None) {
        if let Some(m) = &kv.model {
            names.insert(m.clone());
        }
    }

    let created = state.started.elapsed().as_secs() as i64;
    let data = names.into_iter().map(|id| ModelEntry {
        id,
        object: "model",
        created,
        owned_by: "cognitora",
    }).collect();

    Json(ModelsResponse { object: "list", data })
}
