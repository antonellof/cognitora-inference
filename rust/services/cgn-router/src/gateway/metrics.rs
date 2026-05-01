//! Prometheus metrics for the OpenAI HTTP gateway.
//!
//! Registered once on the workspace registry via `cgn_telemetry::registry()`.
//! Exposed by `cgn-router` on its admin listener at `/metrics`.

use std::sync::LazyLock;

use cgn_telemetry::prometheus::{HistogramOpts, HistogramVec, IntCounterVec, Opts};

pub static CHAT_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    let v = IntCounterVec::new(
        Opts::new(
            "cgn_router_chat_requests_total",
            "OpenAI chat-completions requests handled, labelled by model + status.",
        ),
        &["model", "status"],
    )
    .expect("metric: chat_requests");
    cgn_telemetry::registry().register(Box::new(v.clone())).ok();
    v
});

pub static CHAT_COMPLETION_TOKENS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    let v = IntCounterVec::new(
        Opts::new(
            "cgn_router_chat_completion_tokens_total",
            "Total completion tokens emitted, labelled by model.",
        ),
        &["model"],
    )
    .expect("metric: chat_completion_tokens");
    cgn_telemetry::registry().register(Box::new(v.clone())).ok();
    v
});

pub static CHAT_LATENCY: LazyLock<HistogramVec> = LazyLock::new(|| {
    let h = HistogramVec::new(
        HistogramOpts::new(
            "cgn_router_chat_latency_seconds",
            "End-to-end chat-completion latency (router-observed).",
        )
        .buckets(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 60.0]),
        &["model", "stream"],
    )
    .expect("metric: chat_latency");
    cgn_telemetry::registry().register(Box::new(h.clone())).ok();
    h
});

/// Touch every static so they are eagerly registered (otherwise an empty
/// /metrics scrape would return 0 bytes until first traffic arrives).
///
/// The `cgn_router_rate_limited_total` counter is registered inside
/// `cgn-ratelimit` itself (the only crate that increments it).
pub fn warm_up() {
    LazyLock::force(&CHAT_REQUESTS);
    LazyLock::force(&CHAT_COMPLETION_TOKENS);
    LazyLock::force(&CHAT_LATENCY);
}
