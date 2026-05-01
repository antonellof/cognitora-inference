//! `cgn-ctl install` — bare-metal, Kubernetes, and cloud installs.

use std::path::PathBuf;

use cgn_core::{Error, Result};
use clap::{Args as ClapArgs, ValueEnum};
use tracing::info;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Where to install.
    #[arg(long, value_enum, default_value_t = Target::SingleNode)]
    pub target: Target,

    /// Model to preload (e.g. "llama3-8b").
    #[arg(long)]
    pub model: Option<String>,

    /// Override the chart path used for k8s installs.
    #[arg(long)]
    pub chart: Option<PathBuf>,

    /// Kubernetes namespace.
    #[arg(long, default_value = "cognitora")]
    pub namespace: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Target {
    SingleNode,
    Kubernetes,
    Aws,
    Gcp,
    Azure,
    Hetzner,
    Baremetal,
}

pub async fn run(args: Args) -> Result<()> {
    info!(?args.target, model = ?args.model, "install");
    match args.target {
        Target::SingleNode => single_node(args).await,
        Target::Kubernetes => kubernetes(args).await,
        Target::Aws | Target::Gcp | Target::Azure | Target::Hetzner | Target::Baremetal => {
            terraform(args).await
        }
    }
}

async fn single_node(args: Args) -> Result<()> {
    info!("preflight: docker?");
    if which::which("docker").is_err() && which::which("podman").is_err() {
        return Err(Error::Unavailable(
            "neither docker nor podman found in PATH".into(),
        ));
    }
    info!("preflight: NVIDIA driver?");
    if which::which("nvidia-smi").is_err() {
        tracing::warn!("nvidia-smi not found; falling back to CPU mode");
    }
    info!(target = "single-node", "would render docker-compose and `docker compose up -d`");
    let _ = args; // placeholder until the renderer lands
    Ok(())
}

async fn kubernetes(args: Args) -> Result<()> {
    info!("preflight: helm");
    let v = cgn_helm::version().await?;
    info!(helm = %v, "helm available");

    let chart = args.chart.unwrap_or_else(|| PathBuf::from("deploy/kubernetes/helm/cognitora"));
    let install = cgn_helm::Install {
        release:   "cognitora".into(),
        chart,
        namespace: args.namespace,
        create_namespace: true,
        values:    vec![],
        set:       vec![],
        wait:      true,
        timeout:   Some("10m".into()),
    };
    let out = install.run().await?;
    println!("{out}");
    Ok(())
}

async fn terraform(args: Args) -> Result<()> {
    info!(target = ?args.target, "would shell out to `terraform apply` for the chosen module");
    // Real implementation: locate `deploy/terraform/modules/<target>`,
    // generate a `cluster.json` with model + cluster size, run terraform.
    Ok(())
}
