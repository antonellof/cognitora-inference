//! `RoutingPolicy` controller.
//!
//! On every reconcile the spec is JSON-encoded and written to etcd at
//! `/cognitora/routing/policy`. `cgn-router`'s `ArcSwap<RoutingPolicy>`
//! picks up the change immediately via its etcd watcher (sub-second).
//!
//! Etcd endpoints come from the `COGNITORA_ETCD` env var, falling back
//! to `127.0.0.1:2379`. Operators that run multiple Cognitora clusters
//! in the same Kubernetes cluster should set this per-deployment via
//! the operator Pod spec.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use cgn_k8s::crds::RoutingPolicy;
use futures::StreamExt;
use kube::{
    api::{Api, ResourceExt},
    runtime::{controller::Action, watcher::Config, Controller},
    Client,
};
use tracing::{error, info, warn};

use super::inference_cluster::Error;
use crate::reconcile::Ctx;

const POLICY_KEY: &str = "/cognitora/routing/policy";

pub async fn run(client: Client, namespace: Option<String>) -> Result<()> {
    let api: Api<RoutingPolicy> = match &namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };
    info!("RoutingPolicy controller running");
    let ctx = Arc::new(Ctx { client });
    Controller::new(api, Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            if let Err(e) = res {
                error!(error=?e, "routing policy reconcile error");
            }
        })
        .await;
    Ok(())
}

async fn reconcile(obj: Arc<RoutingPolicy>, _ctx: Arc<Ctx>) -> std::result::Result<Action, Error> {
    let name = obj.name_any();
    let body = serde_json::json!({
        "kv":           obj.spec.kv,
        "load":         obj.spec.load,
        "power":        obj.spec.power,
        "capacity":     obj.spec.capacity,
        "max_queue":    obj.spec.max_queue,
        "ttft_slo_ms":  obj.spec.ttft_slo_ms,
    });

    let endpoints: Vec<String> = std::env::var("COGNITORA_ETCD")
        .unwrap_or_else(|_| "127.0.0.1:2379".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    match etcd_client::Client::connect(&endpoints, None).await {
        Ok(mut client) => {
            if let Err(e) = client.put(POLICY_KEY, body.to_string(), None).await {
                warn!(error=?e, "etcd put failed");
            } else {
                info!(%name, key = POLICY_KEY, "policy synced to etcd");
            }
        }
        Err(e) => warn!(error=?e, "etcd connect failed"),
    }

    Ok(Action::requeue(Duration::from_secs(60)))
}

fn error_policy(_o: Arc<RoutingPolicy>, _e: &Error, _c: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(30))
}
