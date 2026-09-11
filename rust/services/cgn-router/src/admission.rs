//! Admission control.
//!
//! Two layers, applied in this order:
//!
//! 1. **Gateway-side rate limit** — already enforced by `cgn-ratelimit` for
//!    the OpenAI HTTP surface.
//! 2. **Router-side queue admission** — caps the *aggregate* number of
//!    requests in flight per (model, role) pair so that bursts can't push
//!    any single agent past `max_concurrent_per_replica * replicas`.
//!
//! [`crate::deadline`] optionally rejects requests whose deadline cannot
//! plausibly be met given the chosen node's queue depth.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};

use cgn_core::config::Config;
use cgn_proto::v1::NodeRole;
use cgn_telemetry::prometheus::{IntCounterVec, IntGaugeVec};
use cgn_telemetry::{counter_vec, gauge_vec};
use dashmap::DashMap;

use crate::state::SharedState;

static INFLIGHT: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_router_admission_inflight",
        "In-flight requests admitted by the router, per (model, role).",
        &["model", "role"]
    )
});

static REJECTED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_admission_rejected_total",
        "Requests rejected by router-side admission control.",
        &["model", "reason"]
    )
});

/// Per-(model, role) inflight counter. Lock-free.
#[derive(Default)]
pub struct Admission {
    counters: DashMap<(String, i32), Arc<AtomicU32>>,
}

impl Admission {
    pub fn new() -> Self {
        Self::default()
    }

    fn slot(&self, model: &str, role: i32) -> Arc<AtomicU32> {
        self.counters
            .entry((model.to_string(), role))
            .or_insert_with(|| Arc::new(AtomicU32::new(0)))
            .clone()
    }

    /// Try to admit one in-flight request. Returns `Some(Permit)` on
    /// success; the permit decrements the counter on drop.
    pub fn try_admit(&self, state: &SharedState, model: &str, role: i32) -> Option<Permit> {
        let max = state.cfg.router.admission.max_queue;
        let cell = self.slot(model, role);
        let cur = cell.fetch_add(1, Ordering::AcqRel);
        if cur >= max {
            cell.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        let inflight = cur + 1;
        INFLIGHT
            .with_label_values(&[model, role_label(role)])
            .set(inflight as i64);
        Some(Permit {
            cell,
            model: model.to_string(),
            role,
        })
    }
}

/// RAII guard that decrements its associated counter on drop.
pub struct Permit {
    cell: Arc<AtomicU32>,
    model: String,
    role: i32,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let inflight = self.cell.fetch_sub(1, Ordering::AcqRel).saturating_sub(1);
        INFLIGHT
            .with_label_values(&[&self.model, role_label(self.role)])
            .set(inflight as i64);
    }
}

/// Role bucket used for admission counters and deadline checks.
pub fn role_for_dispatch(cfg: &Config, prompt_tokens: u32) -> i32 {
    if cfg.router.disagg.enabled && prompt_tokens >= cfg.router.disagg.colocate_below_tokens {
        NodeRole::Decode as i32
    } else {
        NodeRole::Both as i32
    }
}

fn role_label(role: i32) -> &'static str {
    match NodeRole::try_from(role).unwrap_or(NodeRole::Unspecified) {
        NodeRole::Prefill => "prefill",
        NodeRole::Decode => "decode",
        NodeRole::Both => "both",
        NodeRole::Unspecified => "unspecified",
    }
}

pub fn record_rejection(model: &str, reason: &str) {
    REJECTED.with_label_values(&[model, reason]).inc();
}

pub fn warm_up_metrics() {
    LazyLock::force(&INFLIGHT);
    LazyLock::force(&REJECTED);
}

/// Admit a request or return an [`Unavailable`](cgn_core::Error::Unavailable)
/// error when the per-(model, role) queue is full.
pub fn try_admit_request(
    state: &SharedState,
    model: &str,
    prompt_tokens: u32,
) -> Result<Permit, cgn_core::Error> {
    let role = role_for_dispatch(&state.cfg, prompt_tokens);
    state
        .admission
        .try_admit(state, model, role)
        .ok_or_else(|| {
            record_rejection(model, "queue_full");
            cgn_core::Error::Unavailable(format!(
                "admission queue full for model {model} (max {})",
                state.cfg.router.admission.max_queue
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max: u32) -> cgn_core::config::Config {
        let mut c = cgn_core::config::Config::default();
        c.router.admission.max_queue = max;
        c
    }

    fn state(max: u32) -> SharedState {
        let cfg = cfg(max);
        SharedState {
            cfg,
            nodes: Arc::new(crate::cluster::NodeRegistry::new()),
            prefix: Arc::new(cgn_core::prefix::PrefixIndex::new(
                std::time::Duration::from_secs(60),
            )),
            started: std::time::Instant::now(),
            policy: Arc::new(arc_swap::ArcSwap::from_pointee(
                crate::state::RoutingPolicy {
                    kv: 0.55,
                    load: 0.25,
                    power: 0.10,
                    capacity: 0.10,
                },
            )),
            carbon: Arc::new(crate::carbon::CarbonTracker::new()),
            admission: Arc::new(Admission::new()),
        }
    }

    #[test]
    fn admits_until_max() {
        warm_up_metrics();
        let s = state(2);
        let p1 = s.admission.try_admit(&s, "m", 0).expect("p1");
        let p2 = s.admission.try_admit(&s, "m", 0).expect("p2");
        assert!(s.admission.try_admit(&s, "m", 0).is_none());
        drop(p1);
        let _p3 = s.admission.try_admit(&s, "m", 0).expect("after drop");
        drop(p2);
    }

    #[test]
    fn disagg_uses_decode_role() {
        let mut c = Config::default();
        c.router.disagg.enabled = true;
        c.router.disagg.colocate_below_tokens = 256;
        assert_eq!(role_for_dispatch(&c, 300), NodeRole::Decode as i32);
        assert_eq!(role_for_dispatch(&c, 100), NodeRole::Both as i32);
    }
}
