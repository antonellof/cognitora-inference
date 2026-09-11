//! Prometheus metrics for the OpenAI HTTP gateway.
//!
//! Registered once on the workspace registry via `cgn_telemetry::registry()`.
//! Exposed by `cgn-router` on its admin listener at `/metrics`.

use std::sync::LazyLock;

use cgn_telemetry::prometheus::{HistogramVec, IntCounterVec};
use cgn_telemetry::{counter_vec, histogram_vec};

pub static CHAT_REQUESTS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_chat_requests_total",
        "OpenAI chat-completions requests handled, labelled by model + status.",
        &["model", "status"]
    )
});

pub static CHAT_COMPLETION_TOKENS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_chat_completion_tokens_total",
        "Total completion tokens emitted, labelled by model.",
        &["model"]
    )
});

pub static CHAT_LATENCY: LazyLock<HistogramVec> = LazyLock::new(|| {
    histogram_vec!(
        "cgn_router_chat_latency_seconds",
        "End-to-end chat-completion latency (router-observed).",
        &["model", "stream"],
        vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 20.0, 60.0]
    )
});

pub static FEDERATION_FORWARDS: LazyLock<IntCounterVec> = LazyLock::new(|| {
    counter_vec!(
        "cgn_router_federation_forwards_total",
        "Requests forwarded to a peer cluster because no local node was eligible.",
        &["model", "peer"]
    )
});

pub static CHAT_TTFT: LazyLock<HistogramVec> = LazyLock::new(|| {
    histogram_vec!(
        "cgn_router_chat_ttft_seconds",
        "Time from dispatch to first token for streaming chat completions.",
        &["model"],
        vec![0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]
    )
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
    LazyLock::force(&CHAT_TTFT);
    LazyLock::force(&FEDERATION_FORWARDS);
}

/// Sum completion tokens across all model labels.
pub fn total_completion_tokens() -> u64 {
    LazyLock::force(&CHAT_COMPLETION_TOKENS);
    cgn_telemetry::registry()
        .gather()
        .iter()
        .find(|mf| mf.get_name() == "cgn_router_chat_completion_tokens_total")
        .map(|mf| {
            mf.get_metric()
                .iter()
                .map(|m| m.get_counter().get_value() as u64)
                .sum()
        })
        .unwrap_or(0)
}
