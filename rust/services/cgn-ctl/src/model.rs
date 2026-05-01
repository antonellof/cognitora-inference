//! `cgn-ctl model …`

use cgn_core::Result;
use clap::Subcommand;
use tracing::info;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Apply a ModelPool spec (loads / unloads the engine on matching nodes).
    Load {
        name: String,
        #[arg(long, default_value_t = 1)]
        tp: u32,
        #[arg(long, default_value_t = 1)]
        prefill_replicas: u32,
        #[arg(long, default_value_t = 2)]
        decode_replicas: u32,
    },
    /// Tear down a ModelPool.
    Unload { name: String },
    /// List currently loaded models.
    Ls,
}

pub async fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Load {
            name,
            tp,
            prefill_replicas,
            decode_replicas,
        } => {
            info!(%name, tp, prefill_replicas, decode_replicas, "model load");
            Ok(())
        }
        Cmd::Unload { name } => {
            info!(%name, "model unload");
            Ok(())
        }
        Cmd::Ls => {
            info!("model list");
            Ok(())
        }
    }
}
