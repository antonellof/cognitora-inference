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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock};

use cgn_core::config::Config;
use cgn_proto::v1::NodeRole;
use cgn_telemetry::prometheus::{IntCounterVec, IntGaugeVec};
use cgn_telemetry::{counter_vec, gauge_vec};
use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::oneshot;

use crate::state::SharedState;

static INFLIGHT: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_router_admission_inflight",
        "In-flight requests admitted by the router, per (model, role).",
        &["model", "role"]
    )
});

static WAITING: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    gauge_vec!(
        "cgn_router_admission_waiting",
        "Requests parked in tenant wait queues, per model.",
        &["model"]
    )
});

static REJECTED: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_admission_rejected_total",
        "Requests rejected by router-side admission control.",
        &["model", "reason"]
    )
});

/// Per-(model, role) slot pools.
#[derive(Default)]
pub struct Admission {
    pools: DashMap<(String, i32), Arc<Pool>>,
}

/// Slots for one (model, role). When full, requests park per tenant and
/// released slots go to tenants in turn, so one tenant's backlog can't
/// starve the rest however many requests it queues.
struct Pool {
    model: String,
    role: i32,
    inner: Mutex<PoolInner>,
}

#[derive(Default)]
struct PoolInner {
    inflight: u32,
    /// Parked requests by tenant. Empty queues are removed.
    waiting: HashMap<String, VecDeque<oneshot::Sender<()>>>,
    /// Tenants with parked requests, next to be served at the front.
    turn: VecDeque<String>,
}

impl Pool {
    /// Give the slot to the next live waiter, or free it. A slot handed
    /// over stays counted in `inflight`.
    fn release(&self) {
        let mut g = self.inner.lock();
        while let Some(tenant) = g.turn.pop_front() {
            let Some(queue) = g.waiting.get_mut(&tenant) else {
                continue;
            };
            let waiter = queue.pop_front();
            if queue.is_empty() {
                g.waiting.remove(&tenant);
            } else {
                g.turn.push_back(tenant);
            }
            // A failed send means that waiter timed out or disconnected.
            if waiter.is_some_and(|tx| tx.send(()).is_ok()) {
                self.publish(&g);
                return;
            }
        }
        g.inflight = g.inflight.saturating_sub(1);
        self.publish(&g);
    }

    fn publish(&self, g: &PoolInner) {
        INFLIGHT
            .with_label_values(&[&self.model, role_label(self.role)])
            .set(g.inflight as i64);
        WAITING
            .with_label_values(&[&self.model])
            .set(g.waiting.values().map(|q| q.len() as i64).sum());
    }
}

impl Admission {
    pub fn new() -> Self {
        Self::default()
    }

    fn pool(&self, model: &str, role: i32) -> Arc<Pool> {
        self.pools
            .entry((model.to_string(), role))
            .or_insert_with(|| {
                Arc::new(Pool {
                    model: model.to_string(),
                    role,
                    inner: Mutex::new(PoolInner::default()),
                })
            })
            .clone()
    }

    /// Admit now if a slot is free and nobody is ahead in line.
    pub fn try_admit(&self, state: &SharedState, model: &str, role: i32) -> Option<Permit> {
        let pool = self.pool(model, role);
        let max = state.cfg.router.admission.max_queue;
        let mut g = pool.inner.lock();
        if g.inflight >= max || !g.turn.is_empty() {
            return None;
        }
        g.inflight += 1;
        pool.publish(&g);
        drop(g);
        Some(Permit { pool })
    }

    /// Admit, parking in `tenant`'s queue while saturated.
    pub async fn admit(
        &self,
        state: &SharedState,
        model: &str,
        role: i32,
        tenant: &str,
    ) -> Result<Permit, Rejected> {
        if let Some(p) = self.try_admit(state, model, role) {
            return Ok(p);
        }
        let admission = &state.cfg.router.admission;
        let pool = self.pool(model, role);
        let mut rx = {
            let mut g = pool.inner.lock();
            // A slot may have freed since the fast path.
            if g.inflight < admission.max_queue && g.turn.is_empty() {
                g.inflight += 1;
                pool.publish(&g);
                drop(g);
                return Ok(Permit { pool });
            }
            let queue = g.waiting.entry(tenant.to_string()).or_default();
            if queue.len() >= admission.wait_queue_per_tenant as usize {
                if queue.is_empty() {
                    g.waiting.remove(tenant);
                }
                return Err(Rejected::QueueFull);
            }
            let (tx, rx) = oneshot::channel();
            queue.push_back(tx);
            if queue.len() == 1 {
                g.turn.push_back(tenant.to_string());
            }
            pool.publish(&g);
            rx
        };
        match tokio::time::timeout(admission.wait_timeout, &mut rx).await {
            Ok(Ok(())) => Ok(Permit { pool }),
            Ok(Err(_)) => Err(Rejected::QueueFull),
            Err(_) => {
                // A slot can arrive as the timer fires; keep it if so.
                rx.close();
                match rx.try_recv() {
                    Ok(()) => Ok(Permit { pool }),
                    Err(_) => Err(Rejected::WaitTimeout),
                }
            }
        }
    }
}

/// Why a request was not admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejected {
    QueueFull,
    WaitTimeout,
}

/// RAII slot: on drop it passes to the next waiter or returns to the pool.
pub struct Permit {
    pool: Arc<Pool>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.pool.release();
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
    LazyLock::force(&WAITING);
    LazyLock::force(&REJECTED);
}

/// Admit a request, waiting its turn while saturated. Errors with
/// [`Unavailable`](cgn_core::Error::Unavailable) on a full or timed-out
/// queue, which the gateway reports as HTTP 429.
pub async fn admit_request(
    state: &SharedState,
    model: &str,
    prompt_tokens: u32,
    tenant: &str,
) -> Result<Permit, cgn_core::Error> {
    let role = role_for_dispatch(&state.cfg, prompt_tokens);
    state
        .admission
        .admit(state, model, role, tenant)
        .await
        .map_err(|why| {
            let (reason, detail) = match why {
                Rejected::QueueFull => ("queue_full", "tenant wait queue full"),
                Rejected::WaitTimeout => ("wait_timeout", "wait timed out"),
            };
            record_rejection(model, reason);
            cgn_core::Error::Unavailable(format!(
                "admission queue full for model {model} (max {}): {detail}",
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
        SharedState::build(cfg(max))
    }

    fn waiting_state(max: u32, per_tenant: u32) -> Arc<SharedState> {
        let mut c = cfg(max);
        c.router.admission.wait_queue_per_tenant = per_tenant;
        c.router.admission.wait_timeout = std::time::Duration::from_secs(5);
        Arc::new(SharedState::build(c))
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

    #[tokio::test]
    async fn rejects_at_once_without_a_wait_queue() {
        let s = waiting_state(1, 0);
        let _held = s.admission.admit(&s, "m", 0, "a").await.unwrap();
        assert_eq!(
            s.admission.admit(&s, "m", 0, "a").await.err(),
            Some(Rejected::QueueFull)
        );
    }

    #[tokio::test]
    async fn parked_request_gets_the_released_slot() {
        let s = waiting_state(1, 4);
        let held = s.admission.admit(&s, "m", 0, "a").await.unwrap();
        let s2 = s.clone();
        let parked =
            tokio::spawn(async move { s2.admission.admit(&s2, "m", 0, "a").await.is_ok() });
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        drop(held);
        assert!(parked.await.unwrap());
    }

    #[tokio::test]
    async fn wait_times_out_and_later_requests_still_work() {
        let mut c = cfg(1);
        c.router.admission.wait_timeout = std::time::Duration::from_millis(20);
        let s = SharedState::build(c);
        let held = s.admission.admit(&s, "m", 0, "a").await.unwrap();
        assert_eq!(
            s.admission.admit(&s, "m", 0, "a").await.err(),
            Some(Rejected::WaitTimeout)
        );
        drop(held);
        assert!(s.admission.admit(&s, "m", 0, "b").await.is_ok());
    }

    #[tokio::test]
    async fn per_tenant_cap_does_not_touch_other_tenants() {
        let s = waiting_state(1, 1);
        let _held = s.admission.admit(&s, "m", 0, "noisy").await.unwrap();
        let s2 = s.clone();
        let first =
            tokio::spawn(async move { s2.admission.admit(&s2, "m", 0, "noisy").await.is_ok() });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(
            s.admission.admit(&s, "m", 0, "noisy").await.err(),
            Some(Rejected::QueueFull)
        );
        let s3 = s.clone();
        let quiet =
            tokio::spawn(async move { s3.admission.admit(&s3, "m", 0, "quiet").await.is_ok() });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!first.is_finished() && !quiet.is_finished());
        first.abort();
        quiet.abort();
    }

    /// One tenant floods first, a second arrives late with a single
    /// request: it must be served second, not after the whole backlog.
    #[tokio::test]
    async fn released_slots_rotate_across_tenants() {
        let s = waiting_state(1, 16);
        let held = s.admission.admit(&s, "m", 0, "noisy").await.unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        let enqueue = |tenant: &'static str, tasks: &mut Vec<_>| {
            let (s, order) = (s.clone(), order.clone());
            tasks.push(tokio::spawn(async move {
                let p = s.admission.admit(&s, "m", 0, tenant).await.unwrap();
                order.lock().push(tenant);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                drop(p);
            }));
        };
        for _ in 0..4 {
            enqueue("noisy", &mut tasks);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        enqueue("quiet", &mut tasks);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        drop(held);
        for t in tasks {
            t.await.unwrap();
        }
        let order = order.lock().clone();
        assert_eq!(order.len(), 5);
        assert_eq!(&order[..2], &["noisy", "quiet"], "got {order:?}");
    }

    #[tokio::test]
    async fn abandoned_waiter_does_not_swallow_the_slot() {
        let s = waiting_state(1, 4);
        let held = s.admission.admit(&s, "m", 0, "a").await.unwrap();
        let s2 = s.clone();
        let gone = tokio::spawn(async move {
            let _ = s2.admission.admit(&s2, "m", 0, "a").await;
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gone.abort();
        let _ = gone.await;
        drop(held);
        assert!(s.admission.try_admit(&s, "m", 0).is_some());
    }
}
