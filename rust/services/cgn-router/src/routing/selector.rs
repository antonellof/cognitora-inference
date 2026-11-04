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
//! 4. Pick the highest scorer. A session's rendezvous-hash node gets a
//!    bonus, and queue depth includes the router's own in-flight count, so
//!    identical idle nodes share a burst instead of one taking all of it.

use std::sync::{Arc, LazyLock};

use cgn_core::{Error, Result};
use cgn_proto::v1::NodeRole;
use cgn_telemetry::histogram_vec;
use cgn_telemetry::prometheus::HistogramVec;

use crate::cluster::NodeEntry;
use crate::state::SharedState;

use super::score::{score_node, Score};

static DECISION_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| {
    histogram_vec!(
        "cgn_router_routing_decision_seconds",
        "Time to filter and score candidates for one request.",
        &["model"],
        vec![25e-6, 50e-6, 100e-6, 250e-6, 500e-6, 1e-3, 2.5e-3, 5e-3, 10e-3, 50e-3]
    )
});

static PREFIX_OVERLAP: LazyLock<HistogramVec> = LazyLock::new(|| {
    histogram_vec!(
        "cgn_router_prefix_overlap_ratio",
        "Share of a prompt's chunks the chosen node is believed to hold.",
        &["model"],
        vec![0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0]
    )
});

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

/// What is being routed.
#[derive(Debug, Clone, Copy)]
pub struct RouteKey<'a> {
    pub model: &'a str,
    /// Prefix-hash salt: the model, plus the tenant under KV isolation.
    pub salt: &'a str,
    /// Conversation key for session affinity.
    pub session: Option<&'a str>,
    pub token_ids: &'a [u32],
}

/// Build the digest salt. An empty tenant keeps the bare model name.
pub fn prefix_salt(state: &SharedState, model: &str, tenant: &str) -> String {
    if tenant.is_empty() || !state.cfg.router.kv_tenant_isolation {
        model.to_string()
    } else {
        format!("{model}\0{tenant}")
    }
}

/// Rendezvous hash: the candidate with the highest `H(session, node)`.
/// Adding or removing a node only moves the sessions it owned.
pub fn rendezvous<'a>(session: &str, nodes: &'a [Arc<NodeEntry>]) -> Option<&'a Arc<NodeEntry>> {
    nodes.iter().max_by_key(|n| {
        let mut h = blake3::Hasher::new();
        h.update(session.as_bytes());
        h.update(&[0]);
        h.update(n.node_id.as_bytes());
        *h.finalize().as_bytes()
    })
}

/// Pick the best node for `key`.
pub async fn pick(
    state: &SharedState,
    key: &RouteKey<'_>,
    role: NodeRole,
) -> Result<RoutingDecision> {
    pick_excluding(state, key, role, &[]).await
}

/// Like [`pick`], but never returns a node whose id is in `exclude`.
/// Used by the gateway's dispatch-retry path: a node that just failed
/// to accept a request is excluded from the immediate re-pick.
pub async fn pick_excluding(
    state: &SharedState,
    key: &RouteKey<'_>,
    role: NodeRole,
    exclude: &[String],
) -> Result<RoutingDecision> {
    let started = std::time::Instant::now();
    let model = key.model;
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
    let digests = cgn_core::hash::hash_seq_chunks(key.salt, key.token_ids);

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
    let session_node = key
        .session
        .and_then(|s| rendezvous(s, &candidates))
        .map(|n| n.node_id.clone());
    let mut best: Option<(Arc<NodeEntry>, Score, f32)> = None;
    for node in &candidates {
        let prefix_chunks = prefix_len_by_node.get(&node.node_id).copied().unwrap_or(0);
        let overlap = if digests.is_empty() {
            0.0
        } else {
            prefix_chunks as f32 / digests.len() as f32
        };
        let queue = state.effective_queue(node);
        let mut s = score_node(&policy, node, queue, overlap, peer_max_power);
        if session_node.as_deref() == Some(node.node_id.as_str()) {
            s.total += policy.session;
        }
        match &best {
            Some((_, prev, _)) if prev.total >= s.total => {}
            _ => best = Some((node.clone(), s, overlap)),
        }
    }
    let (node, score, overlap) = best.expect("non-empty candidates");
    // Only reached for models a live node serves, so labels stay bounded.
    DECISION_SECONDS
        .with_label_values(&[model])
        .observe(started.elapsed().as_secs_f64());
    PREFIX_OVERLAP
        .with_label_values(&[model])
        .observe(overlap as f64);

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
    key: &RouteKey<'_>,
    prefill_role: NodeRole,
    decode_role: NodeRole,
    exclude: &[String],
) -> Result<(RoutingDecision, RoutingDecision)> {
    let model = key.model;
    let prefill = pick_excluding(state, key, prefill_role, exclude).await?;

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
        let s = score_node(&policy, n, state.effective_queue(n), 0.0, peer_max_power);
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

    /// A route for model `llama3` with no tenant or session.
    fn key(ids: &[u32]) -> RouteKey<'_> {
        RouteKey {
            model: "llama3",
            salt: "llama3",
            session: None,
            token_ids: ids,
        }
    }

    fn fake_state() -> SharedState {
        SharedState::build(cgn_core::config::Config::default())
    }

    #[tokio::test]
    async fn returns_unavailable_when_no_nodes() {
        let s = fake_state();
        let r = pick(&s, &key(&[1, 2, 3]), NodeRole::Both).await;
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
            kv_epoch: 0,
            ready: true,
            draining: false,
            adapters: vec![],
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
            let d = pick(&s, &key(&[1, 2, 3]), NodeRole::Both).await.unwrap();
            assert_eq!(d.node.node_id, "n2");
        }
    }

    #[tokio::test]
    async fn watt_cap_soft_when_all_over() {
        let s = fake_state();
        // Every node over cap: still serve rather than fail.
        s.nodes.upsert(entry("n1", 400.0, 350.0, "", 0));
        let d = pick(&s, &key(&[1, 2, 3]), NodeRole::Both).await.unwrap();
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
            let d = pick(&state, &key(&[1, 2, 3]), NodeRole::Both)
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

        let d = pick(&state, &key(&[1, 2, 3]), NodeRole::Both)
            .await
            .unwrap();
        assert_eq!(d.node.node_id, "mi");
    }

    #[tokio::test]
    async fn burst_spreads_before_the_next_heartbeat() {
        let s = fake_state();
        s.nodes.upsert(entry("n1", 0.0, 0.0, "", 0));
        s.nodes.upsert(entry("n2", 0.0, 0.0, "", 0));
        let mut guards = Vec::new();
        let mut seen = std::collections::HashMap::<String, u32>::new();
        for _ in 0..10 {
            let d = pick(&s, &key(&[]), NodeRole::Both).await.unwrap();
            guards.push(s.track_node(&d.node.node_id));
            *seen.entry(d.node.node_id.clone()).or_default() += 1;
        }
        assert_eq!(seen.get("n1"), Some(&5), "{seen:?}");
        assert_eq!(seen.get("n2"), Some(&5), "{seen:?}");
    }

    #[tokio::test]
    async fn inflight_count_is_released_with_the_guard() {
        let s = fake_state();
        let g = s.track_node("n1");
        assert_eq!(s.node_inflight("n1"), 1);
        drop(g);
        assert_eq!(s.node_inflight("n1"), 0);
    }

    #[tokio::test]
    async fn session_sticks_while_load_is_equal() {
        let s = fake_state();
        for id in ["n1", "n2", "n3", "n4"] {
            s.nodes.upsert(entry(id, 0.0, 0.0, "", 0));
        }
        let keyed = RouteKey {
            session: Some("conversation-7"),
            ..key(&[])
        };
        let first = pick(&s, &keyed, NodeRole::Both)
            .await
            .unwrap()
            .node
            .node_id
            .clone();
        for _ in 0..8 {
            let d = pick(&s, &keyed, NodeRole::Both).await.unwrap();
            assert_eq!(d.node.node_id, first);
        }
    }

    #[tokio::test]
    async fn sessions_land_across_nodes() {
        let nodes: Vec<_> = ["n1", "n2", "n3", "n4"]
            .iter()
            .map(|id| Arc::new(entry(id, 0.0, 0.0, "", 0)))
            .collect();
        let hit: std::collections::HashSet<_> = (0..200)
            .map(|i| {
                rendezvous(&format!("s{i}"), &nodes)
                    .unwrap()
                    .node_id
                    .clone()
            })
            .collect();
        assert_eq!(hit.len(), 4);
    }

    #[tokio::test]
    async fn removing_a_node_only_moves_its_own_sessions() {
        let all: Vec<_> = ["n1", "n2", "n3", "n4"]
            .iter()
            .map(|id| Arc::new(entry(id, 0.0, 0.0, "", 0)))
            .collect();
        let fewer: Vec<_> = all.iter().filter(|n| n.node_id != "n4").cloned().collect();
        for i in 0..200 {
            let s = format!("s{i}");
            let before = rendezvous(&s, &all).unwrap().node_id.clone();
            let after = rendezvous(&s, &fewer).unwrap().node_id.clone();
            if before != "n4" {
                assert_eq!(before, after);
            }
        }
    }

    #[tokio::test]
    async fn tenants_never_see_each_others_prefix_claims() {
        let s = fake_state();
        s.nodes.upsert(entry("n1", 0.0, 0.0, "", 0));
        let ids: Vec<u32> = (0..64).collect();
        let salt_a = prefix_salt(&s, "llama3", "a");
        let salt_b = prefix_salt(&s, "llama3", "b");
        let of = |salt: &'_ str| {
            let salt = salt.to_string();
            let ids = ids.clone();
            let s = &s;
            async move {
                let k = RouteKey {
                    salt: &salt,
                    ..key(&ids)
                };
                pick(s, &k, NodeRole::Both).await.unwrap()
            }
        };
        let a = of(&salt_a).await;
        s.prefix.insert_many(&a.digests, "n1");
        assert_eq!(of(&salt_a).await.overlap, 1.0);
        assert_eq!(of(&salt_b).await.overlap, 0.0);
    }

    #[test]
    fn isolation_can_be_turned_off_and_is_skipped_without_a_tenant() {
        let s = fake_state();
        assert_eq!(prefix_salt(&s, "m", ""), "m");
        assert_ne!(prefix_salt(&s, "m", "t"), "m");
        let mut c = cgn_core::config::Config::default();
        c.router.kv_tenant_isolation = false;
        assert_eq!(prefix_salt(&SharedState::build(c), "m", "t"), "m");
    }

    #[tokio::test]
    async fn draining_and_unready_nodes_get_no_new_work() {
        let s = fake_state();
        let mut draining = entry("draining", 0.0, 0.0, "", 0);
        draining.draining = true;
        let mut cold = entry("cold", 0.0, 0.0, "", 0);
        cold.ready = false;
        s.nodes.upsert(draining);
        s.nodes.upsert(cold);
        assert!(pick(&s, &key(&[]), NodeRole::Both).await.is_err());
        s.nodes.upsert(entry("live", 0.0, 0.0, "", 0));
        let d = pick(&s, &key(&[]), NodeRole::Both).await.unwrap();
        assert_eq!(d.node.node_id, "live");
    }

    #[tokio::test]
    async fn lora_adapters_route_to_nodes_serving_them() {
        let s = fake_state();
        let mut with = entry("with-adapter", 0.0, 0.0, "", 0);
        with.adapters = vec!["sql".into()];
        s.nodes.upsert(with);
        s.nodes.upsert(entry("without", 0.0, 0.0, "", 0));
        let k = RouteKey {
            model: "sql",
            salt: "sql",
            session: None,
            token_ids: &[],
        };
        let d = pick(&s, &k, NodeRole::Both).await.unwrap();
        assert_eq!(d.node.node_id, "with-adapter");
    }
}
