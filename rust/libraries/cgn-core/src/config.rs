//! Cognitora configuration.
//!
//! A single TOML document describes every binary. Each daemon reads only the
//! sections it needs; unknown keys are tolerated for forward compat.
//!
//! Lookup order:
//!   1. Path passed on the command line.
//!   2. `$CGN_CONFIG`.
//!   3. `/etc/cognitora/cognitora.toml`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::Result;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub cluster: ClusterConfig,
    pub router: RouterConfig,
    pub agent: AgentConfig,
    pub engine: EngineConfig,
    pub kv: KvConfig,
    pub security: SecurityConfig,
    pub metrics: MetricsConfig,
    pub auth: AuthConfig,
    pub models: HashMap<String, ModelConfig>,
}

// ---------------------------------------------------------------------------
// Cluster
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClusterConfig {
    pub name: String,
    pub state_backend: StateBackend,
    pub etcd_endpoints: Vec<String>,
    pub gossip_seeds: Vec<String>,
}
impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            name: "cognitora".into(),
            state_backend: StateBackend::Etcd,
            etcd_endpoints: vec![],
            gossip_seeds: vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StateBackend {
    Etcd,
    Gossip,
}

// ---------------------------------------------------------------------------
// Router (incorporates the OpenAI HTTP gateway)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterConfig {
    /// OpenAI-compatible HTTP/SSE listener.
    pub listen_http: String,
    /// gRPC admin/control surface.
    pub listen_grpc: String,
    /// Plain-HTTP admin (Prometheus scrape, pprof, /healthz).
    pub listen_admin: String,
    pub node_id: String,
    pub score_weights: ScoreWeights,
    pub admission: AdmissionConfig,
    pub rate_limit: RateLimitConfig,
    pub cascade: CascadeConfig,
    pub disagg: DisaggConfig,
    pub federation: FederationConfig,
    pub autoscaler: AutoscalerConfig,
}
impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            listen_http: format!("0.0.0.0:{}", crate::ports::ROUTER_HTTP),
            listen_grpc: format!("0.0.0.0:{}", crate::ports::ROUTER_GRPC),
            listen_admin: format!("0.0.0.0:{}", crate::ports::ROUTER_ADMIN),
            node_id: default_node_id("router"),
            score_weights: ScoreWeights::default(),
            admission: AdmissionConfig::default(),
            rate_limit: RateLimitConfig::default(),
            cascade: CascadeConfig::default(),
            disagg: DisaggConfig::default(),
            federation: FederationConfig::default(),
            autoscaler: AutoscalerConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScoreWeights {
    pub kv: f32,
    pub load: f32,
    pub power: f32,
    pub capacity: f32,
}
impl Default for ScoreWeights {
    fn default() -> Self {
        Self {
            kv: 0.55,
            load: 0.25,
            power: 0.10,
            capacity: 0.10,
        }
    }
}
impl ScoreWeights {
    /// Validate that weights sum to 1.0 (within tolerance).
    pub fn validate(&self) -> Result<()> {
        let sum = self.kv + self.load + self.power + self.capacity;
        if (sum - 1.0).abs() > 0.01 {
            return Err(crate::Error::Config(format!(
                "router.score_weights must sum to 1.0 (got {sum:.3})"
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdmissionConfig {
    pub max_queue: u32,
    #[serde(with = "humantime_serde")]
    pub ttft_slo: Duration,
    pub max_concurrent_per_replica: u32,
}
impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            max_queue: 1024,
            ttft_slo: Duration::from_millis(800),
            max_concurrent_per_replica: 16,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    pub rps: u32,
    pub burst: u32,
    pub redis_url: Option<String>,
}
impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            rps: 50,
            burst: 200,
            redis_url: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CascadeConfig {
    /// Enable model cascade (SLM -> mid -> LLM).
    pub enabled: bool,
    /// Confidence threshold (logprob avg) below which to escalate.
    pub confidence_threshold: f32,
}
impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            confidence_threshold: -1.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisaggConfig {
    /// Enable prefill/decode disaggregation.
    pub enabled: bool,
    /// Prompt-length threshold under which prefill is colocated.
    pub colocate_below_tokens: u32,
}
impl Default for DisaggConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            colocate_below_tokens: 256,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct FederationConfig {
    /// Forward to peer Cognitora clusters when no local node serves a model.
    pub enabled: bool,
    /// Peer router gRPC endpoints (mTLS).
    pub peers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoscalerConfig {
    /// Energy-aware autoscaler. Watches `cgn-metrics` and drains the
    /// highest-watt nodes when the cluster is idle.
    pub enabled: bool,
    /// Idle threshold: drain a node whose 5m util is below this %.
    pub idle_util_pct: f32,
    /// Wattage above this threshold makes a node a drain candidate.
    pub high_watt_threshold: f32,
    /// Per-tenant deadline propagation (rejects requests whose
    /// deadline cannot be met given the current queue).
    pub deadline_admission: bool,
}
impl Default for AutoscalerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            idle_util_pct: 15.0,
            high_watt_threshold: 350.0,
            deadline_admission: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub listen: String,
    pub role: NodeRoleCfg,
    pub node_id: String,
    pub kv_uds: PathBuf,
    pub gpu_index: Option<u32>,

    // Legacy aliases for the engine block. If `[engine]` is unset we fall
    // back to these fields so older configs keep working.
    #[serde(default)]
    pub vllm_url: Option<String>,
    #[serde(default)]
    pub vllm_cmd: Option<Vec<String>>,
}
impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            listen: format!("0.0.0.0:{}", crate::ports::AGENT_GRPC),
            role: NodeRoleCfg::Both,
            node_id: default_node_id("agent"),
            kv_uds: PathBuf::from("/run/cognitora/kv.sock"),
            gpu_index: None,
            vllm_url: None,
            vllm_cmd: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Engine (vLLM, llama.cpp, or any OpenAI-compatible HTTP server)
// ---------------------------------------------------------------------------

/// Inference engine driver.
///
/// Cognitora's `cgn-agent` proxies to an OpenAI-compatible HTTP server. This
/// block describes which engine to spawn and how. Three kinds are supported:
///
/// * `vllm` — the agent spawns `vllm serve <model> ...` (GPU).
/// * `llama_cpp` — the agent spawns `python -m llama_cpp.server` or a
///   standalone `llama-server` binary (CPU or GPU offload).
/// * `openai_compat` — the agent does not spawn anything; it just proxies
///   to `engine.url`. Use this when the engine is managed by
///   systemd / Kubernetes / a sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub kind: EngineKind,
    /// HTTP base URL where the engine exposes the OpenAI surface.
    pub url: String,
    /// vLLM-specific knobs (used when `kind = "vllm"`).
    pub vllm: VllmEngineConfig,
    /// llama.cpp-specific knobs (used when `kind = "llama_cpp"`).
    pub llama_cpp: LlamaCppEngineConfig,
}
impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            kind: EngineKind::Vllm,
            url: format!("http://127.0.0.1:{}", crate::ports::VLLM_HTTP),
            vllm: VllmEngineConfig::default(),
            llama_cpp: LlamaCppEngineConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Vllm,
    LlamaCpp,
    OpenaiCompat,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VllmEngineConfig {
    /// Path or PATH-name of the `vllm` CLI. Default: `vllm`.
    pub binary: String,
    /// Arguments appended after the auto-rendered `serve <model> --tp <N>
    /// --max-model-len <M>` flags.
    pub extra_args: Vec<String>,
}
impl Default for VllmEngineConfig {
    fn default() -> Self {
        Self {
            binary: "vllm".into(),
            extra_args: vec!["--enable-chunked-prefill".into()],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlamaCppEngineConfig {
    /// Path or PATH-name of the python interpreter (mode = "python_server")
    /// or the standalone server binary (mode = "binary"). Default: `python`.
    pub binary: String,
    /// "python_server" → invoked as `<binary> -m llama_cpp.server …`.
    /// "binary"        → invoked as `<binary> --model … --host … --port …`.
    pub mode: LlamaCppMode,
    pub host: String,
    pub port: u16,
    /// Context window. Mapped to `--n_ctx`.
    pub n_ctx: u32,
    /// CPU thread count. Mapped to `--n_threads`.
    pub n_threads: u32,
    /// GPU layer offload count. -1 = "offload everything to GPU", 0 = "CPU
    /// only". Mapped to `--n_gpu_layers`.
    pub n_gpu_layers: i32,
    /// Arguments appended after the auto-rendered base flags.
    pub extra_args: Vec<String>,
}
impl Default for LlamaCppEngineConfig {
    fn default() -> Self {
        Self {
            binary: "python".into(),
            mode: LlamaCppMode::PythonServer,
            host: "127.0.0.1".into(),
            port: crate::ports::VLLM_HTTP,
            n_ctx: 4096,
            n_threads: 4,
            n_gpu_layers: 0,
            extra_args: vec![],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlamaCppMode {
    PythonServer,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeRoleCfg {
    Decode,
    Prefill,
    Both,
}

// ---------------------------------------------------------------------------
// KV cache
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KvConfig {
    pub listen: String,
    pub uds: PathBuf,
    pub ram_gib: u32,
    pub ssd_dir: PathBuf,
    pub ssd_gib: u32,
    pub index_dir: PathBuf,
    pub transport: KvTransport,
    pub quic_listen: String,
    pub block_size_tokens: u32,
}
impl Default for KvConfig {
    fn default() -> Self {
        Self {
            listen: format!("0.0.0.0:{}", crate::ports::KV_GRPC),
            uds: PathBuf::from("/run/cognitora/kv.sock"),
            ram_gib: 32,
            ssd_dir: PathBuf::from("/var/lib/cognitora/kv"),
            ssd_gib: 1024,
            index_dir: PathBuf::from("/var/lib/cognitora/index"),
            transport: KvTransport::Quic,
            quic_listen: format!("0.0.0.0:{}", crate::ports::KV_QUIC),
            block_size_tokens: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KvTransport {
    Quic,
    Rdma,
}

// ---------------------------------------------------------------------------
// Security / TLS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub ca_file: Option<PathBuf>,
    pub cert_file: Option<PathBuf>,
    pub key_file: Option<PathBuf>,
    pub require_mtls: bool,
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// Enable authentication on the OpenAI surface. Off by default to ease
    /// localhost development; turn on in production.
    pub enabled: bool,
    pub oidc_issuer: Option<String>,
    pub oidc_audience: Option<String>,
    pub api_keys_file: Option<PathBuf>,
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub listen: String,
    pub redfish_url: Option<String>,
    pub redfish_user: Option<String>,
    pub redfish_password: Option<String>,
    pub ipmi_fallback: bool,
    #[serde(with = "humantime_serde")]
    pub scrape_interval: Duration,
}
impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            listen: format!("0.0.0.0:{}", crate::ports::METRICS_HTTP),
            redfish_url: None,
            redfish_user: None,
            redfish_password: None,
            ipmi_fallback: false,
            scrape_interval: Duration::from_secs(15),
        }
    }
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub cascade: Vec<String>,
    pub prefill_replicas: u32,
    pub decode_replicas: u32,
    pub tp: u32,
    pub max_model_len: Option<u32>,
    pub extra_args: Vec<String>,
    /// Filesystem path to the model weights. Required for `engine.kind =
    /// "llama_cpp"` (a `.gguf` file). Optional for `vllm` (which resolves
    /// the model name as a HuggingFace repo id).
    pub path: Option<PathBuf>,
}
impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            cascade: vec![],
            prefill_replicas: 1,
            decode_replicas: 2,
            tp: 1,
            max_model_len: None,
            extra_args: vec![],
            path: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

impl Config {
    /// Load from a path. Missing file yields a `Config::default()`.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            tracing::warn!(path = %path.display(), "config file not found, using defaults");
            return Ok(Self::default());
        }
        let data = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&data).map_err(|e| crate::Error::Config(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Resolve the config path according to the documented lookup order.
    pub fn locate(arg: Option<&Path>) -> PathBuf {
        if let Some(p) = arg {
            return p.to_path_buf();
        }
        if let Ok(env) = std::env::var("CGN_CONFIG") {
            return PathBuf::from(env);
        }
        PathBuf::from(crate::DEFAULT_CONFIG_PATH)
    }

    fn validate(&self) -> Result<()> {
        self.router.score_weights.validate()?;
        Ok(())
    }
}

fn default_node_id(role: &str) -> String {
    format!(
        "{}-{}-{}",
        hostname_or(role),
        role,
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    )
}

fn hostname_or(default: &str) -> String {
    if let Ok(h) = std::env::var("HOSTNAME") {
        if !h.is_empty() {
            return h;
        }
    }
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    default.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn missing_file_yields_default() {
        let cfg = Config::load("/no/such/path/cognitora.toml").unwrap();
        assert_eq!(cfg.cluster.name, "cognitora");
        assert_eq!(cfg.router.score_weights.kv, 0.55);
    }

    #[test]
    fn parses_minimal_toml() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("cognitora.toml");
        std::fs::write(
            &p,
            r#"
[cluster]
name = "prod-eu"

[router.score_weights]
kv = 0.6
load = 0.2
power = 0.1
capacity = 0.1
        "#,
        )
        .unwrap();
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.cluster.name, "prod-eu");
        assert!((cfg.router.score_weights.kv - 0.6).abs() < 1e-6);
    }

    #[test]
    fn weights_must_sum_to_one() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(
            &p,
            r#"
[router.score_weights]
kv = 0.9
load = 0.2
power = 0.1
capacity = 0.1
        "#,
        )
        .unwrap();
        assert!(Config::load(&p).is_err());
    }
}
