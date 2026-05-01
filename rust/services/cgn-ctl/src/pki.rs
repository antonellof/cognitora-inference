//! `cgn-ctl pki …`
//!
//! Bootstraps a self-signed CA + leaf certificates for a development
//! cluster. Production deployments should use Vault / cert-manager / step-ca.

use std::path::PathBuf;

use cgn_core::{Error, Result};
use clap::Subcommand;
use tracing::info;

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Create a CA + a leaf cert for a single host.
    Bootstrap {
        /// Output directory.
        #[arg(short, long, default_value = "./pki")]
        out: PathBuf,
        /// Common name on the leaf cert.
        #[arg(long, default_value = "cognitora-router")]
        common_name: String,
        /// SANs (repeatable).
        #[arg(long = "san")]
        sans: Vec<String>,
    },
}

pub async fn run(cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Bootstrap { out, common_name, sans } => {
            std::fs::create_dir_all(&out)
                .map_err(|e| Error::Internal(format!("mkdir {}: {e}", out.display())))?;
            let sans = if sans.is_empty() { vec!["localhost".into()] } else { sans };
            let pki = cgn_tls::generate_dev_pki(&common_name, sans)?;
            std::fs::write(out.join("ca.crt"),     &pki.ca_cert_pem).map_err(Error::Io)?;
            std::fs::write(out.join("ca.key"),     &pki.ca_key_pem).map_err(Error::Io)?;
            std::fs::write(out.join("leaf.crt"),   &pki.leaf_cert_pem).map_err(Error::Io)?;
            std::fs::write(out.join("leaf.key"),   &pki.leaf_key_pem).map_err(Error::Io)?;
            info!(dir=%out.display(), "PKI written; restart daemons with [security].* pointing here");
            Ok(())
        }
    }
}
