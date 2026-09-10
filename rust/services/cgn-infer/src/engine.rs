//! Ties the pieces together: tokenizer + chat template + runtime +
//! scheduler, exposed as a generate-stream API the HTTP layer
//! consumes.
//!
//! Concurrency model (phase 3): requests are submitted to the
//! continuous-batching scheduler ([`crate::scheduler`]), which runs
//! on a dedicated engine thread. Architectures covered by the crate's
//! own batched forward pass (`llama`, `qwen2`) decode several
//! sequences per step; other supported architectures fall back to a
//! sequential runtime (still scheduled fairly, one at a time). In
//! pipeline mode (phase 4) the coordinator uses a distributed runtime
//! whose middle layers live on remote workers.

use std::path::PathBuf;

use cgn_core::{Error, Result};
use tokenizers::Tokenizer;
use tokio::sync::mpsc;
use tracing::info;

use crate::model::{ChatTemplate, GgufModel, ModelMetadata, QLlamaConfig};
use crate::runtime::{pick_device, Backend, BatchModel, BatchedLlama, SequentialModel};
use crate::sampling::SamplingParams;
use crate::scheduler::{self, SchedulerConfig, SchedulerHandle};

/// Startup configuration (from the CLI).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub model_path: PathBuf,
    pub backend: Backend,
    /// Context window; clamped to the model's training context.
    pub ctx: usize,
    /// CPU threads for the compute backend (0 = all cores).
    pub threads: usize,
    /// Model id reported by /v1/models; defaults to the GGUF name.
    pub model_id: Option<String>,
    /// Max sequences decoded per step (subject to the runtime's own cap).
    pub max_batch: usize,
    /// Prefill chunk size in tokens.
    pub prefill_chunk: usize,
    /// KV pool budget in tokens shared by all resident sequences.
    pub kv_pool_tokens: usize,
    /// Pipeline workers (phase 4). Empty = single-node.
    pub pipeline: Option<crate::pipeline::CoordinatorConfig>,
}

impl EngineConfig {
    pub fn single_node(model_path: PathBuf, backend: Backend, ctx: usize, threads: usize) -> Self {
        Self {
            model_path,
            backend,
            ctx,
            threads,
            model_id: None,
            max_batch: 8,
            prefill_chunk: 512,
            kv_pool_tokens: 64 * 1024,
            pipeline: None,
        }
    }
}

/// A single generation request, already normalized (chat templates
/// are applied by the caller).
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub prompt: String,
    pub max_tokens: usize,
    pub stop: Vec<String>,
    pub params: SamplingParams,
}

/// One streamed chunk of output.
#[derive(Debug, Clone)]
pub struct StreamEvent {
    /// New text since the previous event (may be empty on the final
    /// event).
    pub delta: String,
    /// Set on the final event: "stop", "length", or "error".
    pub finish_reason: Option<String>,
    pub prompt_tokens: usize,
    pub completion_tokens: usize,
}

pub struct Engine {
    scheduler: SchedulerHandle,
    tokenizer: Tokenizer,
    template: ChatTemplate,
    pub metadata: ModelMetadata,
    pub model_id: String,
}

impl Engine {
    pub fn load(cfg: &EngineConfig) -> Result<Self> {
        if cfg.threads > 0 {
            // Candle's CPU backend sizes its pool from rayon's global
            // config; this must run before the first kernel.
            std::env::set_var("RAYON_NUM_THREADS", cfg.threads.to_string());
        }
        let gguf = GgufModel::open(&cfg.model_path)?;
        let tokenizer = crate::model::tokenizer_from_gguf(&gguf.content, &cfg.model_path)?;
        let metadata = gguf.metadata.clone();

        let tok_str = |id: Option<u32>| id.and_then(|i| tokenizer.id_to_token(i));
        let template = ChatTemplate::new(
            metadata.chat_template.clone(),
            tok_str(metadata.bos_token_id),
            tok_str(metadata.eos_token_id),
        )?;

        let device = pick_device(cfg.backend)?;
        let model: Box<dyn BatchModel> = if let Some(pipeline) = &cfg.pipeline {
            // Distributed layer pipeline: sequential by design (one
            // activation stream in flight), so max_batch == 1.
            Box::new(crate::pipeline::PipelinedModel::connect(
                &gguf, &device, cfg.ctx, pipeline,
            )?)
        } else if QLlamaConfig::arch_supported(&gguf.content) {
            Box::new(BatchedLlama::load(&gguf, &device, cfg.ctx, cfg.max_batch)?)
        } else {
            info!(
                arch = %metadata.architecture,
                "architecture not covered by the batched runtime; serving sequentially"
            );
            Box::new(SequentialModel::load(&gguf, &device, cfg.ctx)?)
        };

        let scheduler = scheduler::start(
            model,
            SchedulerConfig {
                max_batch: cfg.max_batch,
                prefill_chunk: cfg.prefill_chunk,
                kv_pool_tokens: cfg.kv_pool_tokens,
            },
        );

        let model_id = cfg
            .model_id
            .clone()
            .unwrap_or_else(|| metadata.name.clone());
        info!(model_id = %model_id, "engine ready");
        Ok(Self {
            scheduler,
            tokenizer,
            template,
            metadata,
            model_id,
        })
    }

    /// Render chat messages through the model's template.
    pub fn render_chat(&self, messages: &[crate::model::ChatMessage]) -> Result<String> {
        self.template.render(messages)
    }

    /// Submit a generation request; [`StreamEvent`]s arrive on `tx` as
    /// the scheduler produces tokens.
    pub async fn generate(
        &self,
        req: GenerateRequest,
        tx: mpsc::Sender<StreamEvent>,
    ) -> Result<()> {
        let encoding = self
            .tokenizer
            .encode(req.prompt.as_str(), true)
            .map_err(|e| Error::InvalidArgument(format!("tokenize: {e}")))?;
        let prompt_tokens: Vec<u32> = encoding.get_ids().to_vec();

        self.scheduler.submit(scheduler::Request {
            prompt_tokens,
            max_tokens: req.max_tokens,
            stop: req.stop,
            params: req.params,
            tokenizer: self.tokenizer.clone(),
            eos_token_id: self.metadata.eos_token_id,
            tx,
        })
    }
}
