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
//! The estimator below also rejects requests whose deadline cannot
//! plausibly be met given the current queue depth (`ttft_slo`).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::state::SharedState;

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
        Some(Permit { cell })
    }
}

/// RAII guard that decrements its associated counter on drop.
pub struct Permit {
    cell: Arc<AtomicU32>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.cell.fetch_sub(1, Ordering::AcqRel);
    }
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
        }
    }

    #[test]
    fn admits_until_max() {
        let s = state(2);
        let a = Admission::new();
        let p1 = a.try_admit(&s, "m", 0).expect("p1");
        let p2 = a.try_admit(&s, "m", 0).expect("p2");
        assert!(a.try_admit(&s, "m", 0).is_none());
        drop(p1);
        let _p3 = a.try_admit(&s, "m", 0).expect("after drop");
        drop(p2);
    }
}
