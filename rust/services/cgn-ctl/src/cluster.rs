//! `cgn-ctl cluster …`

use cgn_core::Result;
use clap::Subcommand;
use tracing::info;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// List nodes (queries etcd via the configured endpoints).
    Nodes,
    /// Cordon a node (router stops sending new traffic).
    Cordon { node_id: String },
    /// Drain inflight requests from a node.
    Drain  { node_id: String },
}

pub async fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Nodes => {
            info!("would list /cognitora/nodes/* from etcd");
            Ok(())
        }
        Cmd::Cordon { node_id } => {
            info!(%node_id, "would mark node cordoned");
            Ok(())
        }
        Cmd::Drain { node_id } => {
            info!(%node_id, "would invoke Agent.Drain");
            Ok(())
        }
    }
}
