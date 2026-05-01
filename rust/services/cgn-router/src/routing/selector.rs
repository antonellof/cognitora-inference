//! Pick a node for an incoming request.
//!
//! 1. Compute prefix hashes of the prompt's token IDs.
//! 2. For every candidate node (`cgn-agent`s reporting the requested role
//!    and model), compute KV overlap from the prefix index.
//! 3. Score each node via [`score_node`].
//! 4. Pick the highest scorer; randomise on ties.

use std::sync::Arc;

use cgn_core::{Error, Result};
use cgn_proto::v1::NodeRole;

use crate::cluster::{NodeEntry, NodeRegistry};
use crate::state::{RoutingPolicy, SharedState};

use super::score::{score_node, Score};

/// Outcome of `pick`.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub node:     Arc<NodeEntry>,
    pub score:    Score,
    pub overlap:  f32,
    pub n_candidates: usize,
}

/// Pick the best node for `(model, role, token_ids)`.
pub async fn pick(
    state: &SharedState,
    model: &str,
    role:  NodeRole,
    token_ids: &[u32],
) -> Result<RoutingDecision> {
    let candidates = state.nodes.nodes_for(role, Some(model));
    if candidates.is_empty() {
        return Err(Error::Unavailable(format!(
            "no live node serving model {model} for role {role:?}"
        )));
    }
    let n_candidates = candidates.len();

    // Step 1: hash the request's prefix chunks once.
    let digests = cgn_core::hash::hash_chunks(model, token_ids);

    // Step 2: per-node KV overlap = (chunks held / total chunks).
    let overlap_by_node = if digests.is_empty() {
        Default::default()
    } else {
        state.prefix.overlap(&digests)
    };

    // Step 3: pre-compute peer_max_power for normalisation.
    let peer_max_power = candidates.iter()
        .map(|n| n.power_watts)
        .fold(0.0_f32, f32::max);

    // Step 4: score everyone.
    let policy = state.policy.load();
    let mut best: Option<(Arc<NodeEntry>, Score, f32)> = None;
    for node in &candidates {
        let cached = overlap_by_node.get(&node.node_id).copied().unwrap_or(0);
        let overlap = if digests.is_empty() {
            0.0
        } else {
            cached as f32 / digests.len() as f32
        };
        let s = score_node(&policy, node, overlap, peer_max_power);
        match &best {
            Some((_, prev, _)) if prev.total >= s.total => {}
            _ => best = Some((node.clone(), s, overlap)),
        }
    }
    let (node, score, overlap) = best.expect("non-empty candidates");

    tracing::debug!(
        node = %node.node_id,
        score = score.total,
        overlap,
        n_candidates,
        "routing decision"
    );

    Ok(RoutingDecision { node, score, overlap, n_candidates })
}

/// Test-only convenience: build a decision from a single hand-crafted node.
#[cfg(test)]
pub fn decision_for_test(node: Arc<NodeEntry>, score: Score) -> RoutingDecision {
    RoutingDecision { node, score, overlap: score.kv, n_candidates: 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_state() -> SharedState {
        let cfg = cgn_core::config::Config::default();
        // Bypass async with a blocking helper in tests.
        let prefix = cgn_core::prefix::PrefixIndex::new(std::time::Duration::from_secs(60));
        SharedState {
            cfg,
            nodes: Arc::new(NodeRegistry::new()),
            prefix: Arc::new(prefix),
            started: std::time::Instant::now(),
            policy: Arc::new(arc_swap::ArcSwap::from_pointee(RoutingPolicy {
                kv: 0.55, load: 0.25, power: 0.10, capacity: 0.10,
            })),
        }
    }

    #[tokio::test]
    async fn returns_unavailable_when_no_nodes() {
        let s = fake_state();
        let r = pick(&s, "llama3", NodeRole::Both, &[1,2,3]).await;
        assert!(matches!(r, Err(Error::Unavailable(_))));
    }
}
