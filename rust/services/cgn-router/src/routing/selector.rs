//! Pick a node for an incoming request.
//!
//! 1. Compute *sequence-chained* prefix hashes of the prompt's token IDs
//!    via [`cgn_core::hash::hash_seq_chunks`]. Sequence-chained digests
//!    encode the entire prefix up to each position, so they correctly
//!    identify reusable KV state (independent per-window hashes would
//!    falsely match positionally-distinct token windows).
//! 2. For every candidate node (`cgn-agent`s reporting the requested role
//!    and model), compute the longest contiguous prefix the node holds
//!    via `prefix.longest_prefix_overlap`. This is the actual length of
//!    prefill the node can skip — not just a count of matching chunks.
//! 3. Score each node via [`score_node`], using
//!    `prefix_length / total_chunks` as the KV signal.
//! 4. Pick the highest scorer; randomise on ties.

use std::sync::Arc;

use cgn_core::{Error, Result};
use cgn_proto::v1::NodeRole;

use crate::cluster::NodeEntry;
use crate::state::SharedState;

use super::score::{score_node, Score};

/// Outcome of `pick`.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub node: Arc<NodeEntry>,
    pub score: Score,
    pub overlap: f32,
    pub n_candidates: usize,
    /// Sequence-chained prefix digests of the request, so the dispatcher
    /// can record (digest → node) in the prefix index once the request
    /// has actually been sent to the chosen node.
    pub digests: Vec<[u8; 32]>,
}

/// Pick the best node for `(model, role, token_ids)`.
pub async fn pick(
    state: &SharedState,
    model: &str,
    role: NodeRole,
    token_ids: &[u32],
) -> Result<RoutingDecision> {
    pick_excluding(state, model, role, token_ids, &[]).await
}

/// Like [`pick`], but never returns a node whose id is in `exclude`.
/// Used by the gateway's dispatch-retry path: a node that just failed
/// to accept a request is excluded from the immediate re-pick.
pub async fn pick_excluding(
    state: &SharedState,
    model: &str,
    role: NodeRole,
    token_ids: &[u32],
    exclude: &[String],
) -> Result<RoutingDecision> {
    let mut candidates = state.nodes.nodes_for(role, Some(model));
    candidates.retain(|n| !exclude.iter().any(|x| x == &n.node_id));

    // Capability constraints (heterogeneous fleets): a model may declare
    // `min_vram_mb` and/or `require_gpu` in its `[models.*]` block. Nodes
    // that don't report GPU identity/VRAM are *not* filtered — the
    // constraint only bites on nodes that affirmatively report an
    // incompatible GPU (backwards compatible with older agents).
    if let Some(mc) = state.cfg.models.get(model) {
        if let Some(min_vram) = mc.min_vram_mb {
            candidates.retain(|n| n.vram_total_mb == 0 || n.vram_total_mb >= min_vram);
        }
        if let Some(req) = &mc.require_gpu {
            let req = req.to_ascii_lowercase();
            candidates.retain(|n| {
                (n.gpu_name.is_empty() && n.gpu_vendor.is_empty())
                    || n.gpu_name.to_ascii_lowercase().contains(&req)
                    || n.gpu_vendor.to_ascii_lowercase().contains(&req)
            });
        }
    }

    // Soft power cap: prefer nodes under their configured watt limit.
    // Only enforced when at least one candidate is under cap — if the
    // whole pool is over, serving still beats browning out a request.
    let under_cap = |n: &Arc<NodeEntry>| n.watt_limit <= 0.0 || n.power_watts < n.watt_limit;
    if candidates.iter().any(&under_cap) {
        candidates.retain(under_cap);
    }

    if candidates.is_empty() {
        return Err(Error::Unavailable(format!(
            "no live node serving model {model} for role {role:?}\
             {}",
            if exclude.is_empty() {
                String::new()
            } else {
                format!(" (excluded after failed dispatch: {exclude:?})")
            }
        )));
    }
    let n_candidates = candidates.len();

    // Step 1: sequence-chained prefix hashes — chunk N depends on chunks
    // 0..N, so equal digests imply identical prefixes. This is what makes
    // longest-prefix matching correct (independent per-window hashes would
    // falsely cross-match windows from unrelated requests).
    let digests = cgn_core::hash::hash_seq_chunks(model, token_ids);

    // Step 2: per-node *longest contiguous prefix length* in chunks.
    // This is the actual prefill we'd skip if we routed here — Smith's-rule
    // / WSPT scheduling later consumes the same number to estimate cost.
    let prefix_len_by_node = if digests.is_empty() {
        Default::default()
    } else {
        state.prefix.longest_prefix_overlap(&digests)
    };

    // Step 3: pre-compute peer_max_power for normalisation.
    let peer_max_power = candidates
        .iter()
        .map(|n| n.power_watts)
        .fold(0.0_f32, f32::max);

    // Step 4: score everyone.
    let policy = state.policy.load();
    let mut best: Option<(Arc<NodeEntry>, Score, f32)> = None;
    for node in &candidates {
        let prefix_chunks = prefix_len_by_node.get(&node.node_id).copied().unwrap_or(0);
        let overlap = if digests.is_empty() {
            0.0
        } else {
            prefix_chunks as f32 / digests.len() as f32
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

    Ok(RoutingDecision {
        node,
        score,
        overlap,
        n_candidates,
        digests,
    })
}

/// Pair pick: prefill + decode for disaggregation. Returns two distinct
/// nodes; if the cluster has only one eligible node it falls back to
/// `Colocate`-style single-node routing and returns the same node twice.
pub async fn pick_pair(
    state: &SharedState,
    model: &str,
    prefill_role: NodeRole,
    decode_role: NodeRole,
    token_ids: &[u32],
    exclude: &[String],
) -> Result<(RoutingDecision, RoutingDecision)> {
    let prefill = pick_excluding(state, model, prefill_role, token_ids, exclude).await?;

    // Decode node: re-pick filtering out the prefill node when possible.
    // We avoid the same node id; if the only eligible decode node is the
    // prefill node (small cluster) we degrade to colocate.
    let candidates = state.nodes.nodes_for(decode_role, Some(model));
    let mut distinct: Vec<_> = candidates
        .iter()
        .filter(|n| n.node_id != prefill.node.node_id && !exclude.iter().any(|x| x == &n.node_id))
        .cloned()
        .collect();
    // Same soft watt-cap preference as the primary pick.
    let under_cap = |n: &Arc<NodeEntry>| n.watt_limit <= 0.0 || n.power_watts < n.watt_limit;
    if distinct.iter().any(&under_cap) {
        distinct.retain(under_cap);
    }
    if distinct.is_empty() {
        return Ok((prefill.clone(), prefill));
    }
    // Score the distinct subset using the same policy. Decode nodes
    // benefit less from prefix overlap (the prefill already paid that
    // cost) so we pick the lowest-load / lowest-watt node.
    let policy = state.policy.load();
    let peer_max_power = distinct
        .iter()
        .map(|n| n.power_watts)
        .fold(0.0_f32, f32::max);
    let mut best: Option<(Arc<NodeEntry>, Score)> = None;
    for n in &distinct {
        let s = score_node(&policy, n, 0.0, peer_max_power);
        match &best {
            Some((_, prev)) if prev.total >= s.total => {}
            _ => best = Some((n.clone(), s)),
        }
    }
    let (decode_node, decode_score) = best.expect("non-empty distinct");
    let digests = prefill.digests.clone();
    Ok((
        prefill,
        RoutingDecision {
            node: decode_node,
            score: decode_score,
            overlap: 0.0,
            n_candidates: distinct.len(),
            digests,
        },
    ))
}

/// Test-only convenience: build a decision from a single hand-crafted node.
#[cfg(test)]
pub fn decision_for_test(node: Arc<NodeEntry>, score: Score) -> RoutingDecision {
    RoutingDecision {
        node,
        score,
        overlap: score.kv,
        n_candidates: 1,
        digests: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::NodeRegistry;
    use crate::state::RoutingPolicy;

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
                kv: 0.55,
                load: 0.25,
                power: 0.10,
                capacity: 0.10,
            })),
        }
    }

    #[tokio::test]
    async fn returns_unavailable_when_no_nodes() {
        let s = fake_state();
        let r = pick(&s, "llama3", NodeRole::Both, &[1, 2, 3]).await;
        assert!(matches!(r, Err(Error::Unavailable(_))));
    }

    fn entry(id: &str, watts: f32, watt_limit: f32, gpu: &str, vram_mb: u64) -> NodeEntry {
        NodeEntry {
            node_id: id.into(),
            address: format!("http://{id}:7070"),
            role: NodeRole::Both as i32,
            gpu_index: None,
            model: Some("llama3".into()),
            queue_depth: 0,
            free_blocks: 100,
            total_blocks: 100,
            power_watts: watts,
            watt_limit,
            gpu_name: gpu.into(),
            gpu_vendor: if gpu.to_ascii_lowercase().contains("nvidia") {
                "nvidia".into()
            } else if gpu.is_empty() {
                String::new()
            } else {
                "amd".into()
            },
            vram_total_mb: vram_mb,
            cordoned: false,
            last_heartbeat: std::time::Instant::now(),
        }
    }

    #[tokio::test]
    async fn watt_cap_prefers_under_cap_nodes() {
        let s = fake_state();
        // n1 is over its cap, n2 under; n2 must win even though scores tie.
        s.nodes.upsert(entry("n1", 400.0, 350.0, "", 0));
        s.nodes.upsert(entry("n2", 200.0, 350.0, "", 0));
        for _ in 0..8 {
            let d = pick(&s, "llama3", NodeRole::Both, &[1, 2, 3])
                .await
                .unwrap();
            assert_eq!(d.node.node_id, "n2");
        }
    }

    #[tokio::test]
    async fn watt_cap_soft_when_all_over() {
        let s = fake_state();
        // Every node over cap: still serve rather than fail.
        s.nodes.upsert(entry("n1", 400.0, 350.0, "", 0));
        let d = pick(&s, "llama3", NodeRole::Both, &[1, 2, 3])
            .await
            .unwrap();
        assert_eq!(d.node.node_id, "n1");
    }

    #[tokio::test]
    async fn capability_filters_apply_only_to_reporting_nodes() {
        let mut state = fake_state();
        let mc = cgn_core::config::ModelConfig {
            min_vram_mb: Some(40_000),
            require_gpu: Some("h100".into()),
            ..Default::default()
        };
        state.cfg.models.insert("llama3".into(), mc);

        // small: reports an incompatible GPU → filtered.
        state
            .nodes
            .upsert(entry("small", 0.0, 0.0, "NVIDIA A10", 24_000));
        // legacy: reports nothing → kept (backwards compatible).
        state.nodes.upsert(entry("legacy", 0.0, 0.0, "", 0));
        // big: matches both constraints → kept.
        state
            .nodes
            .upsert(entry("big", 0.0, 0.0, "NVIDIA H100 80GB HBM3", 81_000));

        for _ in 0..8 {
            let d = pick(&state, "llama3", NodeRole::Both, &[1, 2, 3])
                .await
                .unwrap();
            assert_ne!(d.node.node_id, "small");
        }
    }

    #[tokio::test]
    async fn require_gpu_matches_vendor_string() {
        let mut state = fake_state();
        let mc = cgn_core::config::ModelConfig {
            require_gpu: Some("amd".into()),
            ..Default::default()
        };
        state.cfg.models.insert("llama3".into(), mc);

        state
            .nodes
            .upsert(entry("nv", 0.0, 0.0, "NVIDIA H100 80GB HBM3", 81_000));
        state
            .nodes
            .upsert(entry("mi", 0.0, 0.0, "AMD Instinct MI300X", 192_000));

        let d = pick(&state, "llama3", NodeRole::Both, &[1, 2, 3])
            .await
            .unwrap();
        assert_eq!(d.node.node_id, "mi");
    }
}
