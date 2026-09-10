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
    pub router: RouterSpec,
    pub agent: AgentSpec,
    pub kvcached: KvCachedSpec,
    pub metrics: MetricsSpec,
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
    /// Optional reactive SLO autoscaling ("planner-lite"). When set (and
    /// `maxQueuePerReplica` is present) the operator's planner loop scales
    /// `decode_replicas` from live queue-depth heartbeats. Absent → the
    /// pool is entirely user-driven, exactly as before 0.7.
    #[serde(default)]
    pub slo: Option<SloSpec>,
}

/// SLO knobs for the reactive planner. camelCase on the wire to match
/// the CRD YAML convention (`maxQueuePerReplica`, …).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SloSpec {
    /// Scale up when total queued requests divided by this exceeds the
    /// current replica count (i.e. avg queue per ready replica is too
    /// high). Unset → planner ignores the pool.
    #[serde(default)]
    pub max_queue_per_replica: Option<u32>,
    /// Floor for planner decisions (default 1) so an idle pool never
    /// scales to zero unless explicitly allowed.
    #[serde(default)]
    pub min_replicas: Option<u32>,
    /// Ceiling for planner decisions. Unset → the current
    /// `decode_replicas` acts as the ceiling (the planner may only
    /// scale back down, never above what the user asked for).
    #[serde(default)]
    pub max_replicas: Option<u32>,
    /// Minimum seconds between planner-initiated scales for one pool
    /// (default 120), so heartbeat noise cannot thrash replicas.
    #[serde(default)]
    pub scale_cooldown_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct ModelPoolStatus {
    pub phase: String,
    pub loaded_replicas: u32,
    /// Replica count last decided by the SLA planner (None until the
    /// planner has acted on this pool). camelCase on the wire to match
    /// the CRD YAML status convention (`readyReplicas`, …).
    #[serde(
        default,
        rename = "desiredReplicas",
        skip_serializing_if = "Option::is_none"
    )]
    pub desired_replicas: Option<u32>,
    /// RFC 3339 timestamp of the planner's last scale action; drives
    /// operator observability (the cooldown itself is tracked in-memory).
    #[serde(
        default,
        rename = "lastScaleTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_scale_time: Option<String>,
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
