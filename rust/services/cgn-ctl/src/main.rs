//! `cgn-ctl` — Cognitora admin CLI.
//!
//! ```text
//! cgn-ctl install   <target>          # bare-metal / k8s / cloud install
//! cgn-ctl cluster   <list|node|drain> # node operations
//! cgn-ctl model     <load|unload|ls>  # model orchestration
//! cgn-ctl pki       <bootstrap|...>   # mTLS material
//! cgn-ctl key       <create|revoke>   # API keys
//! cgn-ctl bench     <chat|embed|...>  # micro-benchmarks
//! ```

#![forbid(unsafe_code)]

mod bench;
mod cluster;
mod install;
mod key;
mod model;
mod pki;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "cgn-ctl", version, about = "Cognitora admin CLI")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Install Cognitora on the chosen target.
    Install(install::Args),
    /// Cluster operations (list, drain, cordon).
    Cluster {
        #[command(subcommand)]
        cmd: cluster::Cmd,
    },
    /// Model lifecycle (load, unload, list).
    Model {
        #[command(subcommand)]
        cmd: model::Cmd,
    },
    /// PKI / mTLS material.
    Pki {
        #[command(subcommand)]
        cmd: pki::Cmd,
    },
    /// API-key administration.
    Key {
        #[command(subcommand)]
        cmd: key::Cmd,
    },
    /// Local benchmarks.
    Bench(bench::Args),
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> cgn_core::Result<()> {
    cgn_telemetry::init("cgn-ctl");
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Install(args) => install::run(args).await,
        Cmd::Cluster { cmd } => cluster::run(cmd).await,
        Cmd::Model { cmd } => model::run(cmd).await,
        Cmd::Pki { cmd } => pki::run(cmd).await,
        Cmd::Key { cmd } => key::run(cmd).await,
        Cmd::Bench(args) => bench::run(args).await,
    }
}
