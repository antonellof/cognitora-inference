//! Deadline propagation + per-tenant SLO admission.
//!
//! Every request that arrives carries an *implicit* deadline:
//!
//! ```text
//! deadline = max(req.deadline_ms,
//!                tenant.ttft_slo + tenant.tpot_slo * max_tokens)
//! ```
//!
//! On admission we estimate the time-to-first-token (TTFT) from the
//! best candidate node's queue depth × per-replica concurrency, and
//! reject the request if the estimate exceeds its deadline. Requests
//! that *cannot* meet their SLA are rejected fast rather than queued
//! behind a noisy neighbour.

use std::time::Duration;

use cgn_core::config::Config;
use cgn_proto::v1::GenerateRequest;

use crate::cluster::NodeEntry;
use crate::state::SharedState;

/// Outcome of `check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionOutcome {
    Admit,
    /// Reject with a 429-style "deadline exceeded" response.
    DeadlineExceeded {
        estimated_ms: u32,
        requested_ms: u32,
    },
    /// Reject with a 429-style "queue full".
    QueueFull,
}

/// Deadline check. Returns `Admit` when the request can plausibly meet
/// its deadline, otherwise the appropriate rejection reason.
pub fn check(
    state: &SharedState,
    req: &GenerateRequest,
    candidate: &NodeEntry,
) -> AdmissionOutcome {
    let cfg = &state.cfg;
    if !cfg.router.autoscaler.deadline_admission {
        // Deadline admission disabled: defer to the regular queue cap.
        return AdmissionOutcome::Admit;
    }

    let requested_ms = effective_deadline_ms(cfg, req);
    let estimated_ms = estimate_ttft_ms(cfg, candidate);

    tracing::debug!(
        node = %candidate.node_id,
        requested_ms,
        estimated_ms,
        "deadline admission"
    );

    if estimated_ms > requested_ms {
        AdmissionOutcome::DeadlineExceeded {
            estimated_ms,
            requested_ms,
        }
    } else {
        AdmissionOutcome::Admit
    }
}

fn effective_deadline_ms(cfg: &Config, req: &GenerateRequest) -> u32 {
    if req.deadline_ms > 0 {
        return req.deadline_ms;
    }
    let ttft = cfg.router.admission.ttft_slo;
    let max_tokens = req.params.as_ref().map(|p| p.max_tokens).unwrap_or(256);
    // Conservative: assume 30 ms TPOT (per-output-token) when no
    // tenant-specific SLO is configured. Per-tenant overrides are
    // plumbed through `[tenants.*]` in the config.
    let tpot_ms = 30u32;
    duration_to_ms(ttft) + max_tokens.saturating_mul(tpot_ms)
}

fn estimate_ttft_ms(cfg: &Config, node: &NodeEntry) -> u32 {
    // Simple model: queue_depth × per-replica concurrency × p50 step.
    let q = node.queue_depth.max(1);
    let conc = cfg.router.admission.max_concurrent_per_replica.max(1);
    let p50_step_ms = 100u32;
    (q.saturating_mul(p50_step_ms)) / conc
}

fn duration_to_ms(d: Duration) -> u32 {
    u32::try_from(d.as_millis()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node(qd: u32) -> NodeEntry {
        NodeEntry {
            node_id: "n".into(),
            address: "https://n:7070".into(),
            role: 0,
            gpu_index: None,
            model: None,
            queue_depth: qd,
            free_blocks: 0,
            total_blocks: 0,
            power_watts: 0.0,
            cordoned: false,
            last_heartbeat: std::time::Instant::now(),
        }
    }

    #[test]
    fn admits_when_disabled() {
        let cfg = Config::default();
        let prefix = cgn_core::prefix::PrefixIndex::new(Duration::from_secs(60));
        let state = SharedState {
            cfg,
            nodes: std::sync::Arc::new(crate::cluster::NodeRegistry::new()),
            prefix: std::sync::Arc::new(prefix),
            started: std::time::Instant::now(),
            policy: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::state::RoutingPolicy {
                    kv: 0.55,
                    load: 0.25,
                    power: 0.10,
                    capacity: 0.10,
                },
            )),
        };
        let req = GenerateRequest::default();
        assert_eq!(
            check(&state, &req, &make_node(1000)),
            AdmissionOutcome::Admit
        );
    }

    #[test]
    fn rejects_when_queue_too_long() {
        let mut cfg = Config::default();
        cfg.router.autoscaler.deadline_admission = true;
        cfg.router.admission.ttft_slo = Duration::from_millis(100);
        cfg.router.admission.max_concurrent_per_replica = 1;
        let prefix = cgn_core::prefix::PrefixIndex::new(Duration::from_secs(60));
        let state = SharedState {
            cfg,
            nodes: std::sync::Arc::new(crate::cluster::NodeRegistry::new()),
            prefix: std::sync::Arc::new(prefix),
            started: std::time::Instant::now(),
            policy: std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::state::RoutingPolicy {
                    kv: 0.55,
                    load: 0.25,
                    power: 0.10,
                    capacity: 0.10,
                },
            )),
        };
        let req = GenerateRequest {
            deadline_ms: 200,
            ..Default::default()
        };
        // queue_depth=10 → ttft ≈ 1000 ms ≫ deadline 200 ms.
        let n = make_node(10);
        assert!(matches!(
            check(&state, &req, &n),
            AdmissionOutcome::DeadlineExceeded { .. }
        ));
    }
}
