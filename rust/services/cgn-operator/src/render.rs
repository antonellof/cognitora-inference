//! Server-side rendering of `InferenceCluster` → Kubernetes objects.
//!
//! We deliberately do not pull in a templating engine — every object
//! is hand-rolled JSON via `serde_json::json!` so the operator binary
//! has no runtime template state to keep in sync with the Helm chart.
//! When the spec changes, this file is the single place to update.

use cgn_k8s::crds::InferenceCluster;
use serde_json::{json, Value};

/// Build the router Deployment + Service for `ic`.
pub fn router_objects(ic: &InferenceCluster, ns: &str, name: &str, image: &str) -> Vec<Value> {
    let labels = labels(ic, name, "router");
    let resources = resource_block(&ic.spec.router.resources);

    let svc_type = ic
        .spec
        .router
        .service_type
        .as_deref()
        .unwrap_or("ClusterIP");

    let deployment = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": format!("{name}-router"), "namespace": ns, "labels": labels },
        "spec": {
            "replicas": ic.spec.router.replicas,
            "selector": { "matchLabels": labels },
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "serviceAccountName": format!("{name}"),
                    "containers": [{
                        "name": "router",
                        "image": image,
                        "args": ["--config", "/etc/cognitora/cognitora.toml"],
                        "ports": [
                            { "name": "http",  "containerPort": 8080 },
                            { "name": "grpc",  "containerPort": 7070 },
                            { "name": "admin", "containerPort": 9091 },
                        ],
                        "readinessProbe": {
                            "httpGet": { "path": "/readyz", "port": "admin" },
                            "initialDelaySeconds": 5,
                            "periodSeconds":       5,
                        },
                        "livenessProbe": {
                            "httpGet": { "path": "/healthz", "port": "admin" },
                            "initialDelaySeconds": 15,
                            "periodSeconds":       10,
                        },
                        "volumeMounts": [
                            { "name": "config", "mountPath": "/etc/cognitora", "readOnly": true },
                            { "name": "pki",    "mountPath": "/etc/cognitora/pki", "readOnly": true },
                        ],
                        "resources": resources,
                    }],
                    "volumes": [
                        { "name": "config", "configMap": { "name": format!("{name}-config") } },
                        { "name": "pki",    "secret":    { "secretName": format!("{name}-pki") } },
                    ],
                },
            },
        },
    });

    let service = json!({
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": { "name": format!("{name}-router"), "namespace": ns, "labels": labels },
        "spec": {
            "type": svc_type,
            "selector": labels,
            "ports": [
                { "name": "http",  "port": 80,   "targetPort": "http"  },
                { "name": "grpc",  "port": 7070, "targetPort": "grpc"  },
                { "name": "admin", "port": 9091, "targetPort": "admin" },
            ],
        },
    });

    vec![deployment, service]
}

/// Build the agent DaemonSet (one per GPU node).
pub fn agent_objects(ic: &InferenceCluster, ns: &str, name: &str, image: &str) -> Vec<Value> {
    let labels = labels(ic, name, "agent");
    let mut node_selector = ic.spec.agent.node_selector.clone();
    node_selector
        .entry("nvidia.com/gpu.present".into())
        .or_insert("true".into());

    let resources = resource_block(&ic.spec.agent.resources);

    let daemonset = json!({
        "apiVersion": "apps/v1",
        "kind": "DaemonSet",
        "metadata": { "name": format!("{name}-agent"), "namespace": ns, "labels": labels },
        "spec": {
            "selector": { "matchLabels": labels },
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "serviceAccountName": format!("{name}"),
                    "nodeSelector": node_selector,
                    "tolerations": ic.spec.agent.tolerations,
                    "hostNetwork": true,
                    "containers": [{
                        "name": "agent",
                        "image": image,
                        "args": ["--config", "/etc/cognitora/cognitora.toml"],
                        "ports": [
                            { "name": "grpc", "containerPort": 7071, "hostPort": 7071 },
                            { "name": "admin","containerPort": 9091 },
                        ],
                        "volumeMounts": [
                            { "name": "config",   "mountPath": "/etc/cognitora", "readOnly": true },
                            { "name": "pki",      "mountPath": "/etc/cognitora/pki", "readOnly": true },
                            { "name": "kv-sock",  "mountPath": "/run/cognitora" },
                        ],
                        "resources": resources,
                    }],
                    "volumes": [
                        { "name": "config",  "configMap": { "name": format!("{name}-config") } },
                        { "name": "pki",     "secret":    { "secretName": format!("{name}-pki") } },
                        { "name": "kv-sock", "emptyDir":  {} },
                    ],
                },
            },
        },
    });

    vec![daemonset]
}

/// Build the kvcached Deployment (one replica per GPU host today;
/// future M3 work makes this a DaemonSet alongside the agent).
pub fn kvcached_objects(ic: &InferenceCluster, ns: &str, name: &str, image: &str) -> Vec<Value> {
    let labels = labels(ic, name, "kvcached");
    let ssd_class = ic.spec.kvcached.ssd_class.clone();
    let ssd_volume: Value = match ssd_class {
        Some(c) => json!({
            "name": "ssd",
            "persistentVolumeClaim": { "claimName": format!("{name}-kv-ssd"), "storageClassName": c },
        }),
        None => {
            json!({ "name": "ssd", "emptyDir": { "sizeLimit": format!("{}Gi", ic.spec.kvcached.ssd_gib) } })
        }
    };

    let deployment = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": format!("{name}-kvcached"), "namespace": ns, "labels": labels },
        "spec": {
            "replicas": 1,
            "selector": { "matchLabels": labels },
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "serviceAccountName": format!("{name}"),
                    "containers": [{
                        "name": "kvcached",
                        "image": image,
                        "args": ["--config", "/etc/cognitora/cognitora.toml"],
                        "ports": [
                            { "name": "grpc", "containerPort": 7072 },
                            { "name": "quic", "containerPort": 7073, "protocol": "UDP" },
                            { "name": "admin","containerPort": 9091 },
                        ],
                        "volumeMounts": [
                            { "name": "config", "mountPath": "/etc/cognitora", "readOnly": true },
                            { "name": "pki",    "mountPath": "/etc/cognitora/pki", "readOnly": true },
                            { "name": "ssd",    "mountPath": "/var/lib/cognitora/kv/ssd" },
                            { "name": "kv-sock","mountPath": "/run/cognitora" },
                        ],
                        "resources": {
                            "limits": { "memory": format!("{}Gi", ic.spec.kvcached.ram_gib + 2) },
                            "requests": { "memory": format!("{}Gi", ic.spec.kvcached.ram_gib) },
                        },
                    }],
                    "volumes": [
                        { "name": "config", "configMap": { "name": format!("{name}-config") } },
                        { "name": "pki",    "secret":    { "secretName": format!("{name}-pki") } },
                        ssd_volume,
                        { "name": "kv-sock", "emptyDir":  {} },
                    ],
                },
            },
        },
    });

    vec![deployment]
}

/// Build the metrics Deployment when `spec.metrics.enabled = true`.
pub fn metrics_objects(ic: &InferenceCluster, ns: &str, name: &str, image: &str) -> Vec<Value> {
    if !ic.spec.metrics.enabled {
        return Vec::new();
    }
    let labels = labels(ic, name, "metrics");

    let deployment = json!({
        "apiVersion": "apps/v1",
        "kind": "Deployment",
        "metadata": { "name": format!("{name}-metrics"), "namespace": ns, "labels": labels },
        "spec": {
            "replicas": 1,
            "selector": { "matchLabels": labels },
            "template": {
                "metadata": { "labels": labels },
                "spec": {
                    "serviceAccountName": format!("{name}"),
                    "containers": [{
                        "name": "metrics",
                        "image": image,
                        "args": ["--config", "/etc/cognitora/cognitora.toml"],
                        "ports": [{ "name": "metrics", "containerPort": 9092 }],
                        "volumeMounts": [
                            { "name": "config", "mountPath": "/etc/cognitora", "readOnly": true },
                        ],
                    }],
                    "volumes": [
                        { "name": "config", "configMap": { "name": format!("{name}-config") } },
                    ],
                },
            },
        },
    });
    vec![deployment]
}

fn labels(_ic: &InferenceCluster, name: &str, comp: &str) -> serde_json::Map<String, Value> {
    let mut map = serde_json::Map::new();
    map.insert("app.kubernetes.io/name".into(), json!("cognitora"));
    map.insert("app.kubernetes.io/instance".into(), json!(name));
    map.insert("app.kubernetes.io/component".into(), json!(comp));
    map.insert("app.kubernetes.io/managed-by".into(), json!("cgn-operator"));
    map
}

fn resource_block(r: &cgn_k8s::crds::Resources) -> Value {
    let mut requests = serde_json::Map::new();
    let mut limits = serde_json::Map::new();
    if let Some(c) = &r.cpu {
        requests.insert("cpu".into(), json!(c));
        limits.insert("cpu".into(), json!(c));
    }
    if let Some(m) = &r.memory {
        requests.insert("memory".into(), json!(m));
        limits.insert("memory".into(), json!(m));
    }
    if let Some(g) = r.gpu {
        limits.insert("nvidia.com/gpu".into(), json!(g));
    }
    json!({ "requests": requests, "limits": limits })
}
