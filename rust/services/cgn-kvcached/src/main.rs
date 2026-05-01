//! `cgn-kvcached` — KV cache daemon.
//!
//! Three tiers: GPU (hot, optional pinned pool), RAM (warm, DashMap), SSD
//! (cold, RocksDB-backed). Exposes:
//!
//! * UDS gRPC for the local `cgn-agent` (low latency, same-host).
//! * TCP gRPC + mTLS for cross-host control RPCs (`Lookup`, `Push`, …).
//! * QUIC :7072 for cross-host KV transfer.

#![forbid(unsafe_code)]

mod grpc;
mod tiers;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;

use cgn_core::{config::Config, Error, Result};
use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "cgn-kvcached", version, about = "Cognitora KV cache daemon")]
struct Cli {
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    cgn_telemetry::init("cgn-kvcached");
    let cli = Cli::parse();
    let cfg = Config::load(Config::locate(cli.config.as_deref()))?;
    info!("kvcached starting");

    let store = Arc::new(tiers::Store::open(&cfg.kv).await?);
    let listen: std::net::SocketAddr = cfg
        .kv
        .listen
        .parse()
        .map_err(|e| Error::Config(format!("kv.listen: {e}")))?;
    let quic_listen: std::net::SocketAddr = cfg
        .kv
        .quic_listen
        .parse()
        .map_err(|e| Error::Config(format!("kv.quic_listen: {e}")))?;

    tokio::select! {
        r = grpc::serve(store.clone(), listen, &cfg) => r,
        r = transport::serve_quic(store.clone(), quic_listen) => r,
        _ = tokio::signal::ctrl_c() => {
            info!("kvcached shutting down");
            Ok(())
        }
    }
}
