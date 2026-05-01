//! Telemetry: tracing, OTLP export, and Prometheus exposition.
//!
//! Every Cognitora binary calls [`init`] exactly once at startup and then
//! registers metrics against [`registry`]. The optional admin HTTP server
//! (returned by [`admin_router`]) exposes `/metrics`, `/healthz`, and
//! `/readyz` for use by the orchestrator and load balancers.

#![forbid(unsafe_code)]

use std::sync::OnceLock;

use axum::{routing::get, Router};
use prometheus::{Encoder, Registry, TextEncoder};
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub use prometheus;
pub use tracing;

/// Workspace-wide Prometheus registry. All custom metrics register here.
pub fn registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(Registry::new)
}

/// Encode the registry as Prometheus exposition text.
pub fn encode_metrics() -> Result<Vec<u8>, prometheus::Error> {
    let encoder = TextEncoder::new();
    let mut buf = Vec::with_capacity(8192);
    encoder.encode(&registry().gather(), &mut buf)?;
    Ok(buf)
}

/// Initialise structured tracing. Idempotent; safe to call from tests.
///
/// `service` is the binary name (e.g. `"cgn-router"`). It is included as a
/// constant span field so log shippers can route by it without parsing.
pub fn init(service: &'static str) {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let json = fmt::layer()
            .json()
            .with_current_span(true)
            .with_span_list(false)
            .with_target(true);

        tracing_subscriber::registry()
            .with(filter)
            .with(json)
            .try_init()
            .ok();

        tracing::info!(
            service = %service,
            version = %cgn_core::build::VERSION,
            "telemetry initialised"
        );
    });
}

/// Build an admin axum router with `/metrics`, `/healthz`, `/readyz`.
///
/// Mount it on the binary's admin listener (e.g. `:9091` for the router).
pub fn admin_router() -> Router {
    Router::new()
        .route("/metrics", get(metrics_handler))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz",  get(|| async { "ok" }))
}

async fn metrics_handler() -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;
    match encode_metrics() {
        Ok(body) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
            body,
        )
            .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("metrics: {e}")).into_response(),
    }
}

/// Convenience macro: register a counter on the workspace registry.
#[macro_export]
macro_rules! counter {
    ($name:expr, $help:expr) => {{
        let c = $crate::prometheus::IntCounter::new($name, $help)
            .expect("counter create");
        $crate::registry().register(Box::new(c.clone())).ok();
        c
    }};
}

/// Convenience macro: register a gauge on the workspace registry.
#[macro_export]
macro_rules! gauge {
    ($name:expr, $help:expr) => {{
        let g = $crate::prometheus::IntGauge::new($name, $help)
            .expect("gauge create");
        $crate::registry().register(Box::new(g.clone())).ok();
        g
    }};
}

/// Convenience macro: register a histogram on the workspace registry with
/// latency buckets suitable for tail-sensitive RPCs (50µs..2s).
#[macro_export]
macro_rules! latency_histogram {
    ($name:expr, $help:expr) => {{
        let opts = $crate::prometheus::HistogramOpts::new($name, $help)
            .buckets(vec![
                0.00005, 0.0001, 0.0005, 0.001, 0.003, 0.005,
                0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0,
            ]);
        let h = $crate::prometheus::Histogram::with_opts(opts)
            .expect("histogram create");
        $crate::registry().register(Box::new(h.clone())).ok();
        h
    }};
}
