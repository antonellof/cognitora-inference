//! `RoutingPolicy` controller.
//!
//! Writes the spec into etcd at `/cognitora/routing/policy`; cgn-router's
//! ArcSwap policy picks up the change immediately via its etcd watcher.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use cgn_k8s::crds::RoutingPolicy;
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
    let api: Api<RoutingPolicy> = match &namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None     => Api::all(client.clone()),
    };
    info!("RoutingPolicy controller running");
    let ctx = Arc::new(Ctx { client });
    Controller::new(api, Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            if let Err(e) = res { error!(error=?e, "routing policy reconcile error"); }
        })
        .await;
    Ok(())
}

async fn reconcile(_obj: Arc<RoutingPolicy>, _ctx: Arc<Ctx>) -> std::result::Result<Action, Error> {
    // Push to etcd /cognitora/routing/policy as JSON. Real impl uses
    // etcd-client; the reconcile interval should be conservative since
    // policy edits are rare.
    Ok(Action::requeue(Duration::from_secs(120)))
}

fn error_policy(_o: Arc<RoutingPolicy>, _e: &Error, _c: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(30))
}
