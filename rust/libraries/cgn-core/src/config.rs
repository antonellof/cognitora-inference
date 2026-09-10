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
/// block describes which engine to spawn and how. Supported kinds:
///
/// * `vllm` — the agent spawns `vllm serve <model> ...` (GPU).
/// * `sglang` — the agent spawns `python -m sglang.launch_server ...` (GPU).
///   SGLang offers RadixAttention prefix caching and structured-output
///   acceleration; from the router's perspective it speaks the same OpenAI
///   surface as vLLM and is fully interchangeable.
/// * `llama_cpp` — the agent spawns `python -m llama_cpp.server` or a
///   standalone `llama-server` binary (CPU or GPU offload).
/// * `mlx` — the agent spawns `python -m mlx_lm.server ...` (**Apple
///   Silicon / macOS**). See the [mlx-lm](https://github.com/ml-explore/mlx-lm)
///   HTTP server (`mlx_lm/SERVER.md`).
/// * `tensorrt_llm` — the agent spawns `trtllm-serve <model> --host <h>
///   --port <p> ...` (NVIDIA TensorRT-LLM's OpenAI-compatible server).
///   Requires the `tensorrt_llm` Python package on the host. KV offload
///   dials are not injected (TRT-LLM manages its own KV connectors);
///   only `kv_offload = "none"` is valid.
/// * `cgn_infer` — the agent spawns Cognitora's first-party native engine
///   `cgn-infer serve --model <gguf> ...`. Same OpenAI wire contract as the
///   llama.cpp server; the per-model `path` field must point at a GGUF.
/// * `openai_compat` — the agent does not spawn anything; it just proxies
///   to `engine.url`. Use this when the engine is managed by
///   systemd / Kubernetes / a sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineConfig {
    pub kind: EngineKind,
    /// HTTP base URL where the engine exposes the OpenAI surface.
    pub url: String,
    /// Engine-side KV offload / cross-worker connector. Picks the
    /// `--kv-transfer-config` JSON for vLLM and the
    /// `--enable-hierarchical-cache` flag set for SGLang. See [`KvOffload`].
    pub kv_offload: KvOffload,
    /// vLLM-specific knobs (used when `kind = "vllm"`).
    pub vllm: VllmEngineConfig,
    /// SGLang-specific knobs (used when `kind = "sglang"`).
    pub sglang: SglangEngineConfig,
    /// llama.cpp-specific knobs (used when `kind = "llama_cpp"`).
    pub llama_cpp: LlamaCppEngineConfig,
    /// MLX-LM server knobs (used when `kind = "mlx"`).
    pub mlx_lm: MlxLmEngineConfig,
    /// TensorRT-LLM knobs (used when `kind = "tensorrt_llm"`).
    pub tensorrt_llm: TensorrtLlmEngineConfig,
    /// cgn-infer knobs (used when `kind = "cgn_infer"`).
    pub cgn_infer: CgnInferEngineConfig,
}
impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            kind: EngineKind::Vllm,
            url: format!("http://127.0.0.1:{}", crate::ports::VLLM_HTTP),
            kv_offload: KvOffload::None,
            vllm: VllmEngineConfig::default(),
            sglang: SglangEngineConfig::default(),
            llama_cpp: LlamaCppEngineConfig::default(),
            mlx_lm: MlxLmEngineConfig::default(),
            tensorrt_llm: TensorrtLlmEngineConfig::default(),
            cgn_infer: CgnInferEngineConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    Vllm,
    Sglang,
    LlamaCpp,
    /// Apple MLX (`python -m mlx_lm.server`).
    Mlx,
    /// NVIDIA TensorRT-LLM (`trtllm-serve`, OpenAI-compatible).
    TensorrtLlm,
    /// Cognitora's first-party native engine (`cgn-infer serve`).
    CgnInfer,
    OpenaiCompat,
}

/// Engine-side KV connector / offload backend.
///
/// Selects which `--kv-transfer-config` (vLLM) or hierarchical-cache flag
/// set (SGLang) the agent injects when spawning the engine. The router
/// is unaware of this dial — it only sees prefix-overlap signals via
/// `cgn-kvcached` either way.
///
/// Compatibility:
///
/// | Engine        | `none` | `nixl` | `lmcache` | `hicache` | `kvbm` |
/// |---------------|--------|--------|-----------|-----------|--------|
/// | `vllm`        | yes    | yes    | yes       | no        | yes    |
/// | `sglang`      | yes    | yes    | no        | yes       | no     |
/// | `llama_cpp`   | yes    | no     | no        | no        | no     |
/// | `mlx`         | yes    | no     | no        | no        | no     |
/// | `tensorrt_llm`| yes    | no     | no        | no        | no     |
/// | `cgn_infer`   | yes    | no     | no        | no        | no     |
/// | `openai_compat` | yes  | no     | no        | no        | no     |
///
/// In disaggregated topologies (`[agent].role = "prefill"` or `"decode"`)
/// the renderer automatically composes the offload backend with NIXL so
/// blocks produced on the prefill GPU stream to the decode GPU. See
/// `cgn-agent::engine::spawn` for the full table.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KvOffload {
    /// No engine-side connector. Default. Use this for dev loops or
    /// when KV state is purely engine-internal.
    #[default]
    None,
    /// NIXL only — sufficient for prefill→decode handoff in disagg
    /// without any extra offload backend.
    Nixl,
    /// LMCache (`LMCacheConnectorV1`). Adds CPU/SSD/Redis/Mooncake KV
    /// reuse on top of vLLM. In disagg, automatically wraps with a
    /// `PdConnector(LMCache + NIXL)` MultiConnector.
    Lmcache,
    /// SGLang Hierarchical Cache (`--enable-hierarchical-cache`).
    /// SGLang-only. Stacks on top of RadixAttention.
    Hicache,
    /// NVIDIA Dynamo KVBM (`DynamoConnector` from
    /// `kvbm.vllm_integration.connector`). Requires the `kvbm` Python
    /// package on the engine host.
    Kvbm,
}

impl KvOffload {
    /// Lower-case canonical name (matches the TOML serialization).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Nixl => "nixl",
            Self::Lmcache => "lmcache",
            Self::Hicache => "hicache",
            Self::Kvbm => "kvbm",
        }
    }
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

/// SGLang launch knobs. SGLang is invoked as `python -m sglang.launch_server`
/// and exposes an OpenAI-compatible HTTP surface on `host:port`. `engine.url`
/// must point at this surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SglangEngineConfig {
    /// Path or PATH-name of the python interpreter (or the `sglang` CLI).
    /// Default: `python`.
    pub binary: String,
    /// Host the launcher binds to. Mapped to `--host`.
    pub host: String,
    /// Port the launcher binds to. Mapped to `--port`.
    pub port: u16,
    /// Default context window when [models.\*].max_model_len is unset.
    /// Mapped to `--context-length`.
    pub context_length: u32,
    /// Mem fraction for SGLang's RadixAttention KV pool. Mapped to
    /// `--mem-fraction-static`. Defaults to `0.85`.
    pub mem_fraction_static: f32,
    /// Arguments appended after the auto-rendered base flags.
    pub extra_args: Vec<String>,
}
impl Default for SglangEngineConfig {
    fn default() -> Self {
        Self {
            binary: "python".into(),
            host: "127.0.0.1".into(),
            port: crate::ports::VLLM_HTTP,
            context_length: 4096,
            mem_fraction_static: 0.85,
            extra_args: vec![],
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

/// MLX-LM HTTP server (`python -m mlx_lm.server`). Apple Silicon only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MlxLmEngineConfig {
    /// Python interpreter that can `import mlx_lm` (default: `python3`).
    pub binary: String,
    /// `--host` for `mlx_lm.server`.
    pub host: String,
    /// `--port` for `mlx_lm.server` (default [`crate::ports::MLX_LM_HTTP`]
    /// avoids clashing with [`crate::ports::ROUTER_HTTP`] on one machine).
    pub port: u16,
    /// Extra flags after `--model … --host … --port …`.
    pub extra_args: Vec<String>,
}
impl Default for MlxLmEngineConfig {
    fn default() -> Self {
        Self {
            binary: "python3".into(),
            host: "127.0.0.1".into(),
            port: crate::ports::MLX_LM_HTTP,
            extra_args: vec![],
        }
    }
}

/// NVIDIA TensorRT-LLM's OpenAI-compatible server (`trtllm-serve`).
/// Spawned as `trtllm-serve <model> --host <h> --port <p> [extra ...]`.
/// `engine.url` must point at `http://<host>:<port>`. The model is the
/// HuggingFace repo id or a local checkpoint/engine directory
/// (`[models.*].path` wins when set).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TensorrtLlmEngineConfig {
    /// Path or PATH-name of the `trtllm-serve` CLI. Default: `trtllm-serve`.
    pub binary: String,
    /// Host the server binds to. Mapped to `--host`.
    pub host: String,
    /// Port the server binds to. Mapped to `--port`.
    pub port: u16,
    /// Arguments appended after the auto-rendered base flags (e.g.
    /// `["--backend", "pytorch"]`).
    pub extra_args: Vec<String>,
}
impl Default for TensorrtLlmEngineConfig {
    fn default() -> Self {
        Self {
            binary: "trtllm-serve".into(),
            host: "127.0.0.1".into(),
            port: crate::ports::VLLM_HTTP,
            extra_args: vec![],
        }
    }
}

/// Cognitora's first-party native inference engine. Spawned as
/// `cgn-infer serve --model <gguf> --host <h> --port <p> [--ctx N]
/// [--threads N]` and speaks the same OpenAI HTTP surface as the
/// llama.cpp server (`/v1/chat/completions`, `/v1/completions`,
/// `/v1/models`, `/healthz`). Only `kv_offload = "none"` is valid.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CgnInferEngineConfig {
    /// Path or PATH-name of the `cgn-infer` binary. Default: `cgn-infer`.
    pub binary_path: String,
    /// Host the server binds to. Mapped to `--host`.
    pub host: String,
    /// Port the server binds to. Mapped to `--port`.
    pub port: u16,
    /// Context window when [models.\*].max_model_len is unset.
    /// Mapped to `--ctx`. `None` = engine default.
    pub ctx: Option<u32>,
    /// CPU thread count. Mapped to `--threads`. `None` = engine default.
    pub threads: Option<u32>,
    /// Arguments appended after the auto-rendered base flags.
    pub extra_args: Vec<String>,
}
impl Default for CgnInferEngineConfig {
    fn default() -> Self {
        Self {
            binary_path: "cgn-infer".into(),
            host: "127.0.0.1".into(),
            port: crate::ports::VLLM_HTTP,
            ctx: None,
            threads: None,
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
    /// SSD blocks not accessed for this many seconds are deleted by the
    /// background eviction loop. 0 disables the TTL pass.
    pub ssd_ttl_secs: u64,
    /// Interval of the background eviction loop in milliseconds.
    pub evict_interval_ms: u64,
    /// RAM occupancy fraction above which the eviction loop spills the
    /// coldest blocks to SSD (0.0–1.0).
    pub ram_high_watermark: f32,
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
            ssd_ttl_secs: 86_400,
            evict_interval_ms: 1_000,
            ram_high_watermark: 0.90,
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
    /// Targets `cgn-metrics` pulls every `scrape_interval`. Each entry's
    /// metrics are tagged with `cgn_target = "<name>"` and exposed under
    /// the federation endpoint (`/federate`) on the metrics aggregator.
    /// Per-request timeout is fixed at 5s; a target that times out is
    /// dropped from that scrape and surfaced in the
    /// `cgn_metrics_scrape_errors_total` counter.
    pub scrape_targets: Vec<ScrapeTarget>,
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
            scrape_targets: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapeTarget {
    /// Free-form label written into the federated body's `cgn_target`
    /// label, e.g. `"router"`, `"agent-1"`, `"kvcached-rack-a"`.
    pub name: String,
    /// `/metrics` URL of the target. Must be reachable from the metrics
    /// aggregator (cgn-metrics) host; mTLS is honoured when the URL is
    /// `https://`.
    pub url: String,
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
    /// Distributed layer-pipeline topology (`engine.kind = "cgn_infer"`
    /// only). When set, the agent spawns one `cgn-infer worker` per
    /// `spawn = true` member plus a coordinator wired to every worker,
    /// registers the workers in etcd as non-servable, and restarts the
    /// whole pipeline if any member dies.
    pub pipeline: Option<PipelineTopologyConfig>,
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
            pipeline: None,
        }
    }
}

/// `[models.*.pipeline]` — cgn-infer layer-pipeline topology.
///
/// The coordinator binds the embedding, layers
/// `coordinator_layers = "0:B"`, and the LM head; each worker binds
/// one contiguous slice. The slices (coordinator first, then workers
/// in listed order) must exactly tile the model's layer count — this
/// is validated by the coordinator at startup against each worker's
/// `Info` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineTopologyConfig {
    /// Coordinator's local layer slice, half-open `"A:B"`; must start
    /// at 0.
    pub coordinator_layers: String,
    /// Pipeline members, in layer order after the coordinator.
    pub workers: Vec<PipelineWorkerConfig>,
    /// Activation wire encoding: `"f16"` (default) or `"int8"`.
    pub activation_encoding: String,
}
impl Default for PipelineTopologyConfig {
    fn default() -> Self {
        Self {
            coordinator_layers: String::new(),
            workers: vec![],
            activation_encoding: "f16".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineWorkerConfig {
    /// gRPC listen address the worker binds (`host:port`).
    pub listen: String,
    /// Layer slice this worker serves, half-open `"A:B"`.
    pub layers: String,
    /// Endpoint the coordinator dials. Defaults to
    /// `http://<listen>` (`https://` when mTLS is configured).
    pub endpoint: Option<String>,
    /// Spawn this worker locally (`true`, default) or expect it to be
    /// managed elsewhere — e.g. by the agent on another node
    /// (`false`).
    pub spawn: bool,
}
impl Default for PipelineWorkerConfig {
    fn default() -> Self {
        Self {
            listen: String::new(),
            layers: String::new(),
            endpoint: None,
            spawn: true,
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
    fn parses_cgn_infer_engine_block() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(
            &p,
            r#"
[engine]
kind = "cgn_infer"
url  = "http://127.0.0.1:8001"

[engine.cgn_infer]
binary_path = "/opt/cognitora/bin/cgn-infer"
host        = "127.0.0.1"
port        = 8001
ctx         = 8192
threads     = 8
        "#,
        )
        .unwrap();
        let cfg = Config::load(&p).unwrap();
        assert_eq!(cfg.engine.kind, EngineKind::CgnInfer);
        assert_eq!(
            cfg.engine.cgn_infer.binary_path,
            "/opt/cognitora/bin/cgn-infer"
        );
        assert_eq!(cfg.engine.cgn_infer.ctx, Some(8192));
        assert_eq!(cfg.engine.cgn_infer.threads, Some(8));
    }

    #[test]
    fn parses_pipeline_block() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(
            &p,
            r#"
[engine]
kind = "cgn_infer"

[models."llama3-8b"]
path = "/models/llama3-8b.gguf"

[models."llama3-8b".pipeline]
coordinator_layers = "0:11"
activation_encoding = "int8"

[[models."llama3-8b".pipeline.workers]]
listen = "127.0.0.1:9101"
layers = "11:22"

[[models."llama3-8b".pipeline.workers]]
listen = "10.0.0.5:9101"
layers = "22:32"
endpoint = "https://worker-b:9101"
spawn = false
        "#,
        )
        .unwrap();
        let cfg = Config::load(&p).unwrap();
        let pipe = cfg.models["llama3-8b"].pipeline.as_ref().unwrap();
        assert_eq!(pipe.coordinator_layers, "0:11");
        assert_eq!(pipe.activation_encoding, "int8");
        assert_eq!(pipe.workers.len(), 2);
        assert_eq!(pipe.workers[0].listen, "127.0.0.1:9101");
        assert_eq!(pipe.workers[0].layers, "11:22");
        assert!(pipe.workers[0].spawn);
        assert_eq!(pipe.workers[0].endpoint, None);
        assert!(!pipe.workers[1].spawn);
        assert_eq!(
            pipe.workers[1].endpoint.as_deref(),
            Some("https://worker-b:9101")
        );
    }

    #[test]
    fn models_without_pipeline_default_to_none() {
        let dir = TempDir::new().unwrap();
        let p = dir.path().join("c.toml");
        std::fs::write(&p, "[models.\"m\"]\npath = \"/m.gguf\"\n").unwrap();
        let cfg = Config::load(&p).unwrap();
        assert!(cfg.models["m"].pipeline.is_none());
    }

    #[test]
    fn cgn_infer_defaults() {
        let c = CgnInferEngineConfig::default();
        assert_eq!(c.binary_path, "cgn-infer");
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.ctx, None);
        assert_eq!(c.threads, None);
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
