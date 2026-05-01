//! Cognitora CRD types. Reconciled by `cgn-operator`.

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// InferenceCluster
// ---------------------------------------------------------------------------

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "cognitora.dev",
    version = "v1alpha1",
    kind = "InferenceCluster",
    namespaced,
    status = "InferenceClusterStatus",
    shortname = "ic",
    printcolumn = r#"{"name":"Replicas","type":"integer","jsonPath":".spec.router.replicas"}"#,
    printcolumn = r#"{"name":"Ready","type":"string","jsonPath":".status.phase"}"#
)]
pub struct InferenceClusterSpec {
    pub router:    RouterSpec,
    pub agent:     AgentSpec,
    pub kvcached:  KvCachedSpec,
    pub metrics:   MetricsSpec,
    #[serde(default)]
    pub image_tag: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct InferenceClusterStatus {
    /// One of: Pending, Progressing, Ready, Degraded.
    pub phase: String,
    pub message: Option<String>,
    pub ready_replicas: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct RouterSpec {
    pub replicas: u32,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub service_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentSpec {
    /// nodeSelector to pin agents to GPU hosts.
    #[serde(default)]
    pub node_selector: std::collections::BTreeMap<String, String>,
    /// Pod tolerations as a free-form JSON array (matches the upstream
    /// k8s `core/v1` Toleration shape; not validated by schemars).
    #[serde(default)]
    pub tolerations: Vec<serde_json::Value>,
    #[serde(default)]
    pub resources: Resources,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct KvCachedSpec {
    pub ram_gib: u32,
    pub ssd_gib: u32,
    pub ssd_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct MetricsSpec {
    pub enabled: bool,
    pub redfish_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Resources {
    pub cpu: Option<String>,
    pub memory: Option<String>,
    pub gpu: Option<u32>,
}

// ---------------------------------------------------------------------------
// ModelPool
// ---------------------------------------------------------------------------

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "cognitora.dev",
    version = "v1alpha1",
    kind = "ModelPool",
    namespaced,
    status = "ModelPoolStatus",
    shortname = "mp"
)]
pub struct ModelPoolSpec {
    pub model: String,
    pub tp: u32,
    pub prefill_replicas: u32,
    pub decode_replicas: u32,
    #[serde(default)]
    pub cascade: Vec<String>,
    #[serde(default)]
    pub max_model_len: Option<u32>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ModelPoolStatus {
    pub phase: String,
    pub loaded_replicas: u32,
}

// ---------------------------------------------------------------------------
// RoutingPolicy
// ---------------------------------------------------------------------------

#[derive(CustomResource, Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "cognitora.dev",
    version = "v1alpha1",
    kind = "RoutingPolicy",
    namespaced,
    shortname = "rp"
)]
pub struct RoutingPolicySpec {
    pub kv: f32,
    pub load: f32,
    pub power: f32,
    pub capacity: f32,
    #[serde(default)]
    pub max_queue: Option<u32>,
    #[serde(default)]
    pub ttft_slo_ms: Option<u32>,
}
