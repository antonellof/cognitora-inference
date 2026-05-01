//! `ModelPool` controller.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use cgn_k8s::crds::ModelPool;
use futures::StreamExt;
use kube::{
    api::Api,
    runtime::{controller::Action, watcher::Config, Controller},
    Client,
};
use tracing::{error, info};

use crate::reconcile::Ctx;
use super::inference_cluster::Error;

pub async fn run(client: Client, namespace: Option<String>) -> Result<()> {
    let api: Api<ModelPool> = match &namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None     => Api::all(client.clone()),
    };
    info!("ModelPool controller running");
    let ctx = Arc::new(Ctx { client });
    Controller::new(api, Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            if let Err(e) = res { error!(error=?e, "model pool reconcile error"); }
        })
        .await;
    Ok(())
}

async fn reconcile(_obj: Arc<ModelPool>, _ctx: Arc<Ctx>) -> std::result::Result<Action, Error> {
    // Translate spec into Agent.LoadModel RPCs against the cluster's
    // agents (matched by node selector).
    Ok(Action::requeue(Duration::from_secs(60)))
}

fn error_policy(_o: Arc<ModelPool>, _e: &Error, _c: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(30))
}
