//! `InferenceCluster` controller.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use cgn_k8s::crds::InferenceCluster;
use futures::StreamExt;
use kube::{
    api::Api,
    runtime::{controller::Action, watcher::Config, Controller},
    Client,
};
use tracing::{error, info};

use crate::reconcile::Ctx;

pub async fn run(client: Client, namespace: Option<String>) -> Result<()> {
    let api: Api<InferenceCluster> = match &namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None     => Api::all(client.clone()),
    };
    info!("InferenceCluster controller running");
    let ctx = Arc::new(Ctx { client });
    Controller::new(api, Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(object = ?obj, "reconciled"),
                Err(e)       => error!(error=?e, "reconcile error"),
            }
        })
        .await;
    Ok(())
}

async fn reconcile(_obj: Arc<InferenceCluster>, _ctx: Arc<Ctx>) -> std::result::Result<Action, Error> {
    // Real impl:
    // 1. Render the equivalent helm values from the spec.
    // 2. Apply server-side a Deployment (router), DaemonSet (agent),
    //    Deployment (kvcached), Deployment (metrics).
    // 3. Patch status with phase + ready_replicas.
    Ok(Action::requeue(Duration::from_secs(60)))
}

fn error_policy(_obj: Arc<InferenceCluster>, _err: &Error, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(30))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube: {0}")]
    Kube(#[from] kube::Error),
}
