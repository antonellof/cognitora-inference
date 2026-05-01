//! Engine abstraction. Today: a vLLM HTTP client.
//!
//! New engines (SGLang, TensorRT-LLM) implement this trait and are wired
//! into [`Supervisor`] via the `engine_kind` config field.

use async_trait::async_trait;
use cgn_core::Result;
use cgn_proto::v1::Token;
use tokio::sync::mpsc;

/// What an engine needs to expose to `cgn-agent`.
#[async_trait]
pub trait Engine: Send + Sync {
    /// Human-readable name (e.g. `"vllm"`).
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

pub mod vllm;
