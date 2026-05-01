//! `cgn-agent` — per-node engine supervisor.
//!
//! Responsibilities:
//!
//! * Spawn and supervise the inference engine (vLLM today; pluggable trait).
//! * Translate `Agent.Generate` gRPC into engine HTTP calls.
//! * Report `NodeHealth` (NVML + queue depth + engine readiness) back to
//!   the router via etcd.
//! * Coordinate KV handoff with `cgn-kvcached` over UDS.

#![forbid(unsafe_code)]

mod engine;
mod grpc;
mod health;
mod supervisor;

use std::path::PathBuf;

use cgn_core::{config::Config, Error, Result};
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "cgn-agent", version, about = "Cognitora per-node engine supervisor")]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    cgn_telemetry::init("cgn-agent");
    let cli = Cli::parse();
    let cfg_path = Config::locate(cli.config.as_deref());
    let cfg = Config::load(&cfg_path)?;
    info!(path = %cfg_path.display(), node = %cfg.agent.node_id, "agent starting");

    let supervisor = supervisor::Supervisor::new(cfg.clone()).await?;
    let supervisor = std::sync::Arc::new(supervisor);

    let listen: std::net::SocketAddr = cfg.agent.listen.parse()
        .map_err(|e| Error::Config(format!("agent.listen: {e}")))?;

    tokio::select! {
        r = grpc::serve(supervisor.clone(), listen) => r,
        r = health::loop_emit(supervisor.clone()) => r,
        _ = shutdown() => {
            info!("agent shutting down");
            supervisor.shutdown().await;
            Ok(())
        }
    }
}

async fn shutdown() {
    use tokio::signal;
    let term = async {
        let mut s = signal::unix::signal(signal::unix::SignalKind::terminate()).expect("sigterm");
        s.recv().await;
    };
    tokio::select! { _ = term => {}, _ = signal::ctrl_c() => {} }
}
