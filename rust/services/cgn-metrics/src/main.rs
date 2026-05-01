//! `cgn-metrics` — Prometheus aggregator and power telemetry collector.
//!
//! Pulls from:
//!
//! * `cgn-router :9091/metrics` (request rate, tokens generated)
//! * `cgn-agent :9091/metrics`  (NVML)
//! * `cgn-kvcached :9091/metrics`
//! * Redfish chassis power
//! * NVML per-GPU power
//!
//! Exposes the union under `:9092/metrics`. The router's `power` score
//! component subscribes to these gauges to bias requests toward energy-
//! efficient nodes.

#![forbid(unsafe_code)]

mod power;
mod scraper;

use std::path::PathBuf;

use cgn_core::{config::Config, Error, Result};
use clap::Parser;
use tracing::info;

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

    let app = cgn_telemetry::admin_router();
    let scrape = scraper::run(cfg.clone());
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
