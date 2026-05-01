//! `cgn-router` — OpenAI-compatible gateway + KV-aware orchestrator.
//!
//! One binary, four listeners:
//!
//! * `:8080`   HTTP (OpenAI surface, SSE).
//! * `:9090`   gRPC (admin / control RPCs over mTLS).
//! * `:9091`   Plain HTTP admin (`/metrics`, `/healthz`, `/readyz`).
//!
//! See `docs/architecture/repo-layout.md` for module responsibilities.

#![forbid(unsafe_code)]

mod admission;
mod cascade;
mod cluster;
mod disagg;
mod gateway;
mod routing;
mod state;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use cgn_core::{config::Config, Error, Result};
use clap::Parser;
use tokio::signal;
use tracing::{error, info};

use crate::state::SharedState;

#[derive(Parser, Debug)]
#[command(name = "cgn-router", version, about = "Cognitora router (gateway + KV-aware orchestrator)")]
struct Cli {
    /// Path to cognitora.toml. Falls back to $CGN_CONFIG / /etc/cognitora/cognitora.toml.
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    cgn_telemetry::init("cgn-router");
    let cli = Cli::parse();

    let cfg_path = Config::locate(cli.config.as_deref());
    let cfg = Config::load(&cfg_path)?;
    info!(path = %cfg_path.display(), "config loaded");

    let state = Arc::new(SharedState::new(cfg.clone()).await?);
    state.bootstrap_cluster_watch().await?;

    let listen_http: SocketAddr = cfg.router.listen_http.parse()
        .map_err(|e| Error::Config(format!("router.listen_http: {e}")))?;
    let listen_admin: SocketAddr = cfg.router.listen_admin.parse()
        .map_err(|e| Error::Config(format!("router.listen_admin: {e}")))?;

    let http  = tokio::spawn(gateway::serve(state.clone(), listen_http));
    let admin = tokio::spawn(serve_admin(listen_admin));
    let grpc  = tokio::spawn(serve_grpc(state.clone()));

    tokio::select! {
        r = http  => log_exit("http",  r),
        r = admin => log_exit("admin", r),
        r = grpc  => log_exit("grpc",  r),
        _ = shutdown_signal() => info!("shutdown signal"),
    }

    state.drain().await;
    Ok(())
}

async fn serve_admin(addr: SocketAddr) -> Result<()> {
    let app = cgn_telemetry::admin_router();
    let listener = tokio::net::TcpListener::bind(addr).await
        .map_err(|e| Error::Io(e))?;
    info!(%addr, "admin listening");
    axum::serve(listener, app).await
        .map_err(|e| Error::Internal(format!("admin serve: {e}")))
}

async fn serve_grpc(state: Arc<SharedState>) -> Result<()> {
    use cgn_proto::v1::router_server::RouterServer;
    use tonic::transport::Server;

    let addr: SocketAddr = state.cfg.router.listen_grpc.parse()
        .map_err(|e| Error::Config(format!("router.listen_grpc: {e}")))?;
    info!(%addr, "grpc listening");

    let mut builder = Server::builder().timeout(std::time::Duration::from_secs(120));

    if state.cfg.security.require_mtls {
        let (Some(ca), Some(cert), Some(key)) = (
            state.cfg.security.ca_file.as_ref(),
            state.cfg.security.cert_file.as_ref(),
            state.cfg.security.key_file.as_ref(),
        ) else {
            return Err(Error::Config("require_mtls=true but cert/key/ca not set".into()));
        };
        let tls = cgn_tls::server_tls(ca, cert, key)?;
        builder = builder.tls_config(tls)
            .map_err(|e| Error::Tls(format!("server tls: {e}")))?;
    }

    let svc = routing::grpc::RouterGrpc::new(state.clone());
    builder
        .add_service(RouterServer::new(svc))
        .serve(addr).await
        .map_err(|e| Error::Internal(format!("grpc serve: {e}")))
}

fn log_exit(name: &str, r: std::result::Result<Result<()>, tokio::task::JoinError>) {
    match r {
        Ok(Ok(()))  => info!(%name,  "task exited cleanly"),
        Ok(Err(e))  => error!(%name, error = ?e, "task error"),
        Err(e)      => error!(%name, error = ?e, "task panicked"),
    }
}

async fn shutdown_signal() {
    let term = async {
        let mut sig = signal::unix::signal(signal::unix::SignalKind::terminate()).expect("sigterm");
        sig.recv().await;
    };
    let int = signal::ctrl_c();
    tokio::select! { _ = term => {}, _ = int => {} }
}
