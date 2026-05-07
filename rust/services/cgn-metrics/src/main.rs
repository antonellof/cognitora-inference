//! `cgn-metrics` — Prometheus aggregator and power telemetry collector.
//!
//! Two listeners share the configured `[metrics].listen` port:
//!
//! * `/metrics` — exposes this binary's local Prometheus metrics
//!   (collected from the workspace registry, i.e. power gauges).
//! * `/federate` — exposes the union of every `[metrics].scrape_targets`
//!   pulled by the scraper, with `cgn_target = "<name>"` injected on
//!   every line. The upstream Prometheus server scrapes this endpoint
//!   to get a single per-cluster federated stream.
//!
//! `/healthz` and `/readyz` come from `cgn_telemetry::admin_router` and
//! always return 200 OK; failure modes (etcd, scrape) are visible via
//! the `cgn_metrics_scrape_errors_total` counter on `/metrics`.

#![forbid(unsafe_code)]

mod power;
mod scraper;

use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use cgn_core::{config::Config, Error, Result};
use clap::Parser;
use tracing::info;

use crate::scraper::Cache;

#[derive(Parser, Debug)]
#[command(name = "cgn-metrics", version)]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    cgn_telemetry::init("cgn-metrics");
    let cli = Cli::parse();
    let cfg = Config::load(Config::locate(cli.config.as_deref()))?;
    info!("metrics starting");

    let listen: std::net::SocketAddr = cfg
        .metrics
        .listen
        .parse()
        .map_err(|e| Error::Config(format!("metrics.listen: {e}")))?;

    let cache = Arc::new(Cache::new());
    let app = build_router(cache.clone());
    let scrape = scraper::run(cfg.clone(), cache.clone());
    let power_loop = power::run(cfg.clone());

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(Error::Io)?;
    info!(%listen, "metrics listening");

    tokio::select! {
        r = axum::serve(listener, app) => r.map_err(|e| Error::Internal(format!("metrics serve: {e}"))),
        r = scrape  => r,
        r = power_loop => r,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}

fn build_router(cache: Arc<Cache>) -> Router {
    cgn_telemetry::admin_router().route("/federate", get(federate).with_state(cache))
}

async fn federate(State(cache): State<Arc<Cache>>) -> Response {
    let body = cache.snapshot();
    if body.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "no scrape targets configured or first scrape pending",
        )
            .into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        (*body).clone(),
    )
        .into_response()
}
