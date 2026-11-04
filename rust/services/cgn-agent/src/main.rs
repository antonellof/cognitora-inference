//! `cgn-agent` — per-node engine supervisor.
//!
//! Responsibilities:
//!
//! * Spawn and supervise the inference engine (vLLM, llama.cpp, or any
//!   OpenAI-compatible server; see `engine::EngineKind`).
//! * Translate `Agent.Generate` gRPC into engine HTTP calls.
//! * Report `NodeHealth` (NVML + queue depth + engine readiness) back to
//!   the router via etcd.
//! * Coordinate KV handoff with `cgn-kvcached` over UDS.

#![forbid(unsafe_code)]

mod engine;
mod grpc;
mod health;
mod kv_cache_state;
mod kvcached;
mod supervisor;
mod telemetry;

use std::path::PathBuf;

use cgn_core::{config::Config, Error, Result};
use clap::Parser;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "cgn-agent",
    version,
    about = "Cognitora per-node engine supervisor"
)]
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

    let listen: std::net::SocketAddr = cfg
        .agent
        .listen
        .parse()
        .map_err(|e| Error::Config(format!("agent.listen: {e}")))?;

    // gRPC and the heartbeat run as tasks so they outlive the shutdown
    // branch: a drain needs both to keep serving and to advertise itself.
    let mut grpc_task = tokio::spawn(grpc::serve(supervisor.clone(), listen));
    let mut health_task = tokio::spawn(health::loop_emit(supervisor.clone()));

    tokio::select! {
        r = &mut grpc_task => flatten(r),
        r = &mut health_task => flatten(r),
        r = supervisor.clone().supervise() => r,
        _ = shutdown() => {
            info!("agent draining");
            let left = supervisor.drain(cfg.agent.drain_timeout).await;
            if left > 0 {
                warn!(left, "drain timed out with generations still running");
            }
            supervisor.shutdown().await;
            Ok(())
        }
    }
}

fn flatten(r: std::result::Result<Result<()>, tokio::task::JoinError>) -> Result<()> {
    r.map_err(|e| Error::Internal(format!("task: {e}")))?
}

async fn shutdown() {
    use tokio::signal;
    let term = async {
        let mut s = signal::unix::signal(signal::unix::SignalKind::terminate()).expect("sigterm");
        s.recv().await;
    };
    tokio::select! { _ = term => {}, _ = signal::ctrl_c() => {} }
}
