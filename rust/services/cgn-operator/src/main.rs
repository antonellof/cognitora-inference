//! `cgn-operator` — Kubernetes operator for Cognitora.
//!
//! Reconciles three CRDs defined in `cgn-k8s::crds`:
//!
//! * `InferenceCluster.cognitora.dev/v1alpha1` — the top-level desired
//!   state. Owns the router StatefulSet/Deployment, agent DaemonSet,
//!   kvcached Deployment, and metrics Deployment.
//! * `ModelPool.cognitora.dev/v1alpha1` — declarative model loading.
//!   Translates to `cgn-ctl model load` invocations against the cluster.
//! * `RoutingPolicy.cognitora.dev/v1alpha1` — score weights + admission
//!   tunables, written into etcd at `/cognitora/routing/policy`.

#![forbid(unsafe_code)]

mod controllers;
mod reconcile;

use cgn_core::Result;
use clap::Parser;
use kube::Client;
use tracing::info;

#[derive(Parser, Debug)]
#[command(name = "cgn-operator", version, about = "Cognitora Kubernetes operator")]
struct Cli {
    /// Namespace to watch. Defaults to all namespaces (cluster-scoped).
    #[arg(short, long)]
    namespace: Option<String>,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    cgn_telemetry::init("cgn-operator");
    let cli = Cli::parse();

    let client = Client::try_default()
        .await
        .map_err(|e| cgn_core::Error::Unavailable(format!("kube client: {e}")))?;
    info!(namespace = ?cli.namespace, "operator starting");

    tokio::try_join!(
        controllers::inference_cluster::run(client.clone(), cli.namespace.clone()),
        controllers::model_pool::run(client.clone(), cli.namespace.clone()),
        controllers::routing_policy::run(client, cli.namespace),
    )?;
    Ok(())
}
