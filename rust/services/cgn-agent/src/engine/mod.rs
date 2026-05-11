//! Engine abstraction.
//!
//! Cognitora's `cgn-agent` is engine-agnostic — anything that speaks the
//! OpenAI HTTP surface (`/v1/completions`, `/v1/chat/completions`, `/health`,
//! `/v1/models`) plugs in. The bundled drivers cover:
//!
//! * `vllm` — `vllm serve <model> ...` (GPU).
//! * `llama_cpp` — `python -m llama_cpp.server ...` or a standalone
//!   `llama-server` binary (CPU or GPU offload).
//! * `mlx` — `python3 -m mlx_lm.server …` (Apple Silicon / mlx-lm).
//! * `openai_compat` — externally managed engine; the agent only proxies.
//!
//! The trait below describes what an engine driver needs to expose.

use async_trait::async_trait;
use cgn_core::Result;
use cgn_proto::v1::Token;
use tokio::sync::mpsc;

/// What an engine needs to expose to `cgn-agent`.
#[async_trait]
pub trait Engine: Send + Sync {
    /// Human-readable name (e.g. `"vllm"`, `"llama_cpp"`).
    fn name(&self) -> &'static str;

    /// Tell the engine to load a model. Idempotent.
    async fn load_model(&self, spec: ModelSpec) -> Result<()>;

    /// Stream tokens for a single request into `tx`.
    async fn generate(&self, req: GenerateReq, tx: mpsc::Sender<Token>) -> Result<()>;

    /// Compute embeddings for a batch of inputs. Returns one vector per
    /// input plus the prompt-token count reported by the engine. Default
    /// impl returns `Unimplemented` so engines that don't support
    /// embeddings (cascade SLMs, llama.cpp without an embed model loaded)
    /// don't have to override.
    async fn embed(&self, _req: EmbedReq) -> Result<EmbedResp> {
        Err(cgn_core::Error::Unavailable(
            "engine does not support embeddings".into(),
        ))
    }

    /// Probe readiness; returns `Ok(true)` once the engine accepts traffic.
    async fn ready(&self) -> bool;
}

#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub name: String,
    pub tp: u32,
    pub max_model_len: Option<u32>,
    pub extra_args: Vec<String>,
    pub path: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct GenerateReq {
    pub id: String,
    pub model: String,
    /// Original chat messages from the OpenAI client. When non-empty
    /// the engine driver hits `/v1/chat/completions` so the engine
    /// applies the model's chat template. When empty, the driver
    /// falls back to legacy `/v1/completions` with `prompt`.
    pub messages: Vec<ChatMessage>,
    /// Legacy plain-text completion path. Only used when `messages`
    /// is empty (e.g. raw `/v1/completions` with no chat history).
    pub prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub top_p: f32,
    pub stop: Vec<String>,
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct EmbedReq {
    pub model: String,
    pub inputs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct EmbedResp {
    /// One vector per `inputs[i]`. Engines that fail mid-batch return an
    /// error; partial responses are not allowed (callers can split if
    /// they need finer granularity).
    pub embeddings: Vec<Vec<f32>>,
    /// Token usage reported by the engine. 0 when the engine omits the
    /// `usage` block.
    pub prompt_tokens: u32,
}

pub mod openai_http;
pub mod spawn;

pub use openai_http::OpenAiHttpEngine;
