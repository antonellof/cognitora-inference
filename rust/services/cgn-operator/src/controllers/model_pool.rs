//! `ModelPool` controller.
//!
//! Each `ModelPool` describes a desired model loaded onto the cluster's
//! agents. The controller patches a tiny ConfigMap describing the model
//! list (`cognitora-models-<name>`) which the router watches alongside
//! its main config; the agent picks up loads on its next heartbeat
//! cycle by checking the `[models]` section in the ConfigMap.
//!
//! Future: speak `Agent.LoadModel` directly over gRPC to each
//! matching agent so loads happen synchronously instead of via config
//! propagation. That's tracked under M5 follow-ups.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use cgn_k8s::crds::{ModelPool, ModelPoolStatus};
use futures::StreamExt;
use kube::{
    api::{Api, Patch, PatchParams, ResourceExt},
    runtime::{controller::Action, watcher::Config, Controller},
    Client,
};
use tracing::{error, info, warn};

use crate::reconcile::Ctx;
use super::inference_cluster::Error;

const FIELD_MANAGER: &str = "cgn-operator";

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

async fn reconcile(obj: Arc<ModelPool>, ctx: Arc<Ctx>) -> std::result::Result<Action, Error> {
    let name = obj.name_any();
    let ns = obj.namespace().unwrap_or_else(|| "default".into());
    info!(%name, %ns, model = %obj.spec.model, "reconciling ModelPool");

    let cm_name = format!("cognitora-models-{}", name);
    let body = serde_json::json!({
        "apiVersion": "v1",
        "kind": "ConfigMap",
        "metadata": {
            "name": cm_name,
            "namespace": ns,
            "labels": {
                "app.kubernetes.io/name": "cognitora",
                "app.kubernetes.io/component": "model-pool",
                "app.kubernetes.io/managed-by": FIELD_MANAGER,
            },
        },
        "data": {
            "model.json": serde_json::to_string(&serde_json::json!({
                "model": obj.spec.model,
                "tp":    obj.spec.tp,
                "prefill_replicas": obj.spec.prefill_replicas,
                "decode_replicas":  obj.spec.decode_replicas,
                "cascade":          obj.spec.cascade,
                "max_model_len":    obj.spec.max_model_len,
                "extra_args":       obj.spec.extra_args,
            })).unwrap_or_default(),
        },
    });

    let cm_api: Api<k8s_openapi::api::core::v1::ConfigMap> = Api::namespaced(ctx.client.clone(), &ns);
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    if let Err(e) = cm_api.patch(&cm_name, &pp, &Patch::Apply(&body)).await {
        warn!(error=?e, "configmap apply failed");
    }

    let _ = patch_status(&ctx.client, &ns, &name, ModelPoolStatus {
        phase:           "Synced".into(),
        loaded_replicas: obj.spec.prefill_replicas + obj.spec.decode_replicas,
    }).await;

    Ok(Action::requeue(Duration::from_secs(60)))
}

async fn patch_status(
    client: &Client,
    ns: &str,
    name: &str,
    status: ModelPoolStatus,
) -> std::result::Result<(), kube::Error> {
    let api: Api<ModelPool> = Api::namespaced(client.clone(), ns);
    let payload = serde_json::json!({ "status": status });
    let pp = PatchParams::apply(FIELD_MANAGER);
    api.patch_status(name, &pp, &Patch::Merge(&payload)).await?;
    Ok(())
}

fn error_policy(_o: Arc<ModelPool>, _e: &Error, _c: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(30))
}
