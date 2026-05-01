//! `InferenceCluster` controller.
//!
//! Maintains a Deployment for the router, a DaemonSet for the agent,
//! a Deployment for `cgn-kvcached`, and (optionally) a Deployment for
//! `cgn-metrics`. Each object is server-side-applied with a Cognitora
//! field manager so two operators in the same cluster don't fight over
//! the same fields.

use std::sync::Arc;
use std::time::Duration;

use cgn_core::Result;
use cgn_k8s::crds::{InferenceCluster, InferenceClusterStatus};
use futures::StreamExt;
use kube::{
    api::{Api, Patch, PatchParams, ResourceExt},
    runtime::{controller::Action, watcher::Config, Controller},
    Client,
};
use serde_json::Value;
use tracing::{error, info, warn};

use crate::reconcile::Ctx;

const FIELD_MANAGER: &str = "cgn-operator";

pub async fn run(client: Client, namespace: Option<String>) -> Result<()> {
    let api: Api<InferenceCluster> = match &namespace {
        Some(ns) => Api::namespaced(client.clone(), ns),
        None => Api::all(client.clone()),
    };
    info!("InferenceCluster controller running");
    let ctx = Arc::new(Ctx { client });
    Controller::new(api, Config::default())
        .run(reconcile, error_policy, ctx)
        .for_each(|res| async move {
            match res {
                Ok((obj, _)) => info!(object = ?obj, "reconciled"),
                Err(e) => error!(error=?e, "reconcile error"),
            }
        })
        .await;
    Ok(())
}

async fn reconcile(
    obj: Arc<InferenceCluster>,
    ctx: Arc<Ctx>,
) -> std::result::Result<Action, Error> {
    let name = obj.name_any();
    let ns = obj.namespace().unwrap_or_else(|| "default".into());
    let image = format!(
        "ghcr.io/antonellof/cognitora-inference/{{}}:{}",
        obj.spec
            .image_tag
            .clone()
            .unwrap_or_else(|| "latest".into())
    );
    let router_image = image.replace("{}", "cgn-router");
    let agent_image = image.replace("{}", "cgn-agent");
    let kv_image = image.replace("{}", "cgn-kvcached");
    let metrics_image = image.replace("{}", "cgn-metrics");

    info!(%name, %ns, "reconciling InferenceCluster");

    let mut all = Vec::new();
    all.extend(crate::render::router_objects(
        &obj,
        &ns,
        &name,
        &router_image,
    ));
    all.extend(crate::render::agent_objects(&obj, &ns, &name, &agent_image));
    all.extend(crate::render::kvcached_objects(&obj, &ns, &name, &kv_image));
    all.extend(crate::render::metrics_objects(
        &obj,
        &ns,
        &name,
        &metrics_image,
    ));

    let mut applied = 0u32;
    let mut errored = 0u32;
    for o in &all {
        match apply_object(&ctx.client, &ns, o).await {
            Ok(()) => applied += 1,
            Err(e) => {
                errored += 1;
                warn!(kind = %o["kind"].as_str().unwrap_or("?"), error=?e, "apply failed");
            }
        }
    }

    let phase = if errored > 0 {
        "Degraded"
    } else {
        "Progressing"
    };
    let _ = patch_status(
        &ctx.client,
        &ns,
        &name,
        InferenceClusterStatus {
            phase: phase.into(),
            message: Some(format!("{applied}/{} applied", all.len())),
            ready_replicas: obj.spec.router.replicas,
        },
    )
    .await;

    Ok(Action::requeue(Duration::from_secs(60)))
}

async fn apply_object(
    client: &Client,
    ns: &str,
    obj: &Value,
) -> std::result::Result<(), kube::Error> {
    let kind = obj["kind"].as_str().unwrap_or("");
    let api_version = obj["apiVersion"].as_str().unwrap_or("v1");
    let name = obj["metadata"]["name"].as_str().unwrap_or("");
    let (group, version) = match api_version.split_once('/') {
        Some((g, v)) => (g.to_string(), v.to_string()),
        None => (String::new(), api_version.to_string()),
    };
    let plural = match kind {
        "Deployment" | "DaemonSet" | "StatefulSet" | "ReplicaSet" => {
            format!("{}s", kind.to_lowercase())
        }
        "Service" => "services".into(),
        "ConfigMap" => "configmaps".into(),
        "Secret" => "secrets".into(),
        "ServiceAccount" => "serviceaccounts".into(),
        other => format!("{}s", other.to_lowercase()),
    };

    let ar = kube::api::ApiResource {
        group,
        version,
        api_version: api_version.to_string(),
        kind: kind.to_string(),
        plural,
    };
    let api: Api<kube::api::DynamicObject> = Api::namespaced_with(client.clone(), ns, &ar);
    let pp = PatchParams::apply(FIELD_MANAGER).force();
    api.patch(name, &pp, &Patch::Apply(obj)).await?;
    Ok(())
}

async fn patch_status(
    client: &Client,
    ns: &str,
    name: &str,
    status: InferenceClusterStatus,
) -> std::result::Result<(), kube::Error> {
    let api: Api<InferenceCluster> = Api::namespaced(client.clone(), ns);
    let payload = serde_json::json!({ "status": status });
    let pp = PatchParams::apply(FIELD_MANAGER);
    api.patch_status(name, &pp, &Patch::Merge(&payload)).await?;
    Ok(())
}

fn error_policy(_obj: Arc<InferenceCluster>, _err: &Error, _ctx: Arc<Ctx>) -> Action {
    Action::requeue(Duration::from_secs(30))
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("kube: {0}")]
    Kube(#[from] kube::Error),
}
