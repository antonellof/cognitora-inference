//! Engine abstraction.
//!
//! Cognitora's `cgn-agent` is engine-agnostic — anything that speaks the
//! OpenAI HTTP surface (`/v1/completions`, `/v1/chat/completions`, `/health`,
//! `/v1/models`) plugs in. The bundled drivers cover:
//!
//! * `vllm` — `vllm serve <model> ...` (GPU).
//! * `llama_cpp` — `python -m llama_cpp.server ...` or a standalone
//!   `llama-server` binary (CPU or GPU offload).
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
pub struct GenerateReq {
    pub id: String,
    pub model: String,
    pub prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub top_p: f32,
    pub stop: Vec<String>,
    pub stream: bool,
}

pub mod openai_http;
pub mod spawn;

pub use openai_http::OpenAiHttpEngine;
