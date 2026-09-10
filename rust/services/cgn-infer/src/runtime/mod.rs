//! Model runtimes behind the [`BatchModel`] seam.
//!
//! [`BatchModel`] is what the scheduler talks to: sequences are keyed
//! by id, KV state is owned by the runtime, and the scheduler decides
//! *when* to prefill / decode / drop. Three implementations:
//!
//! * [`BatchedLlama`] — the phase-3 path. Uses the crate's own
//!   quantized forward pass ([`crate::model::QLlama`]) with external
//!   per-sequence KV caches, so several sequences can decode in one
//!   step: the heavy weight matmuls run batched, attention runs per
//!   sequence. Handles GGUF architectures `llama` (incl. Mistral
//!   GGUFs) and `qwen2`.
//! * [`SequentialModel`] — the breadth path (phase 5). Wraps the
//!   stock `candle-transformers` quantized models (`llama` MoE,
//!   `qwen3`, `gemma3`, `phi3`), which keep a single internal KV
//!   cache and therefore report `max_batch() == 1`; the scheduler
//!   degrades to one-at-a-time serving for these.
//! * `PipelinedModel` (in [`crate::pipeline`]) — the phase-4 path:
//!   the head/tail stage runs locally, middle layer slices run on
//!   remote workers over gRPC.

use std::collections::HashMap;

use candle_core::{Device, IndexOp, Tensor};
use cgn_core::{Error, Result};
use tracing::info;

use crate::model::{GgufModel, LayerRange, ModelParts, QLlama, QLlamaConfig, SeqKv};

/// Compute backend selection, resolved against compiled features at
/// startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub enum Backend {
    /// Pick the best available: Metal > CUDA > CPU.
    #[default]
    Auto,
    Cpu,
    Metal,
    Cuda,
}

/// A loaded model that serves multiple sequences, each identified by
/// a scheduler-assigned id. The runtime owns all KV state.
///
/// Contract:
/// * `prefill(seq, tokens, start_pos)` processes `tokens` for `seq`
///   whose KV already covers exactly `start_pos` positions
///   (`start_pos == 0` starts / restarts the sequence). Returns
///   last-position logits when `want_logits` is set (the final chunk).
/// * `decode(batch)` appends exactly one token per listed sequence
///   and returns logits in the same order. Every listed sequence must
///   have been prefilled.
/// * `drop_seq` releases all state for a sequence; unknown ids are a
///   no-op.
pub trait BatchModel: Send {
    /// Largest decode batch this runtime supports (1 = sequential).
    fn max_batch(&self) -> usize;
    fn max_seq_len(&self) -> usize;
    fn prefill(
        &mut self,
        seq: u64,
        tokens: &[u32],
        start_pos: usize,
        want_logits: bool,
    ) -> Result<Option<Vec<f32>>>;
    fn decode(&mut self, batch: &[(u64, u32)]) -> Result<Vec<Vec<f32>>>;
    fn drop_seq(&mut self, seq: u64);
}

// ---------------------------------------------------------------------------
// Batched runtime (llama / qwen2)
// ---------------------------------------------------------------------------

/// Continuous-batching runtime over the crate's own quantized
/// forward pass.
pub struct BatchedLlama {
    model: QLlama,
    kvs: HashMap<u64, SeqKv>,
    max_seq_len: usize,
    max_batch: usize,
}

impl BatchedLlama {
    pub fn load(gguf: &GgufModel, device: &Device, ctx: usize, max_batch: usize) -> Result<Self> {
        let cfg = QLlamaConfig::from_gguf(&gguf.content)?;
        let range = LayerRange::new(0, cfg.n_layers)?;
        let mut reader = gguf.cursor();
        let model = QLlama::load(
            &gguf.content,
            &mut reader,
            device,
            range,
            ModelParts::full(),
        )?;
        let max_seq_len = ctx.min(model.cfg.context_length).max(1);
        info!(
            arch = %model.cfg.architecture,
            layers = model.cfg.n_layers,
            ctx = max_seq_len,
            max_batch,
            "batched runtime ready"
        );
        Ok(Self {
            model,
            kvs: HashMap::new(),
            max_seq_len,
            max_batch: max_batch.max(1),
        })
    }
}

impl BatchModel for BatchedLlama {
    fn max_batch(&self) -> usize {
        self.max_batch
    }

    fn max_seq_len(&self) -> usize {
        self.max_seq_len
    }

    fn prefill(
        &mut self,
        seq: u64,
        tokens: &[u32],
        start_pos: usize,
        want_logits: bool,
    ) -> Result<Option<Vec<f32>>> {
        let kv = self.kvs.entry(seq).or_insert_with(|| self.model.new_kv());
        if start_pos == 0 {
            kv.clear();
        }
        if kv.len() != start_pos {
            return Err(Error::Internal(format!(
                "prefill position mismatch for seq {seq}: kv has {} tokens, start_pos {start_pos}",
                kv.len()
            )));
        }
        let hidden = self.model.embed_tokens(tokens)?;
        let hidden = self.model.forward_hidden(&hidden, start_pos, kv)?;
        if !want_logits {
            return Ok(None);
        }
        let logits = self.model.output(&hidden)?;
        let logits = logits
            .squeeze(0)
            .and_then(|t| t.to_dtype(candle_core::DType::F32))
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(|e| Error::Internal(format!("logits readback: {e}")))?;
        Ok(Some(logits))
    }

    fn decode(&mut self, batch: &[(u64, u32)]) -> Result<Vec<Vec<f32>>> {
        if batch.is_empty() {
            return Ok(vec![]);
        }
        let tokens: Vec<u32> = batch.iter().map(|&(_, t)| t).collect();
        // Take the caches out so we can hold &mut to each while also
        // calling &self methods on the model.
        let mut kvs: Vec<SeqKv> = Vec::with_capacity(batch.len());
        for &(seq, _) in batch {
            let kv = self
                .kvs
                .remove(&seq)
                .ok_or_else(|| Error::Internal(format!("decode of unknown seq {seq}")))?;
            kvs.push(kv);
        }
        let positions: Vec<usize> = kvs.iter().map(|kv| kv.len()).collect();

        let result = (|| -> Result<Vec<Vec<f32>>> {
            let hidden = self.model.embed_tokens(&tokens)?; // (1, B, e)
            let hidden = hidden
                .transpose(0, 1)
                .map_err(|e| Error::Internal(format!("decode reshape: {e}")))?; // (B, 1, e)
            let mut kv_refs: Vec<&mut SeqKv> = kvs.iter_mut().collect();
            let hidden = self.model.decode_step(&hidden, &positions, &mut kv_refs)?;
            let logits = self.model.output(&hidden)?; // (B, vocab)
            logits
                .to_dtype(candle_core::DType::F32)
                .and_then(|t| t.to_vec2::<f32>())
                .map_err(|e| Error::Internal(format!("logits readback: {e}")))
        })();

        for (&(seq, _), kv) in batch.iter().zip(kvs) {
            self.kvs.insert(seq, kv);
        }
        result
    }

    fn drop_seq(&mut self, seq: u64) {
        self.kvs.remove(&seq);
    }
}

// ---------------------------------------------------------------------------
// Sequential fallback (stock candle-transformers quantized models)
// ---------------------------------------------------------------------------

/// The stock candle model behind [`SequentialModel`].
enum ArchWeights {
    /// Also used for Mixtral-style MoE GGUFs (`llama.expert_count > 1`).
    Llama(candle_transformers::models::quantized_llama::ModelWeights),
    Qwen2(candle_transformers::models::quantized_qwen2::ModelWeights),
    Qwen3(candle_transformers::models::quantized_qwen3::ModelWeights),
    Gemma3(candle_transformers::models::quantized_gemma3::ModelWeights),
    Phi3(candle_transformers::models::quantized_phi3::ModelWeights),
}

impl ArchWeights {
    fn forward(&mut self, input: &Tensor, index_pos: usize) -> candle_core::Result<Tensor> {
        match self {
            Self::Llama(m) => m.forward(input, index_pos),
            Self::Qwen2(m) => m.forward(input, index_pos),
            Self::Qwen3(m) => {
                // qwen3 does not reset its cache on offset 0; the
                // others do. Normalize to the trait contract.
                if index_pos == 0 {
                    m.clear_kv_cache();
                }
                m.forward(input, index_pos)
            }
            Self::Gemma3(m) => m.forward(input, index_pos),
            Self::Phi3(m) => m.forward(input, index_pos),
        }
    }
}

/// One-sequence-at-a-time runtime for architectures the batched path
/// doesn't cover. KV state lives inside the candle model, so only a
/// single sequence can be resident; `max_batch() == 1` makes the
/// scheduler serialize accordingly.
pub struct SequentialModel {
    weights: ArchWeights,
    device: Device,
    max_seq_len: usize,
    /// (seq id, tokens resident in the internal KV cache).
    resident: Option<(u64, usize)>,
}

/// GGUF architectures servable by [`SequentialModel`].
pub const SEQUENTIAL_ARCHS: &[&str] = &["llama", "qwen2", "qwen3", "gemma3", "phi3"];

impl SequentialModel {
    pub fn load(gguf: &GgufModel, device: &Device, ctx: usize) -> Result<Self> {
        use candle_transformers::models as m;
        let arch = gguf.metadata.architecture.clone();
        let ce = |e: candle_core::Error| Error::Internal(format!("candle weight load: {e}"));
        // The candle loaders consume a `Content`; re-parse the header
        // (cheap — tensor data stays mmapped and is read lazily).
        let mut reader = gguf.cursor();
        let mut header_cursor = gguf.cursor();
        let content = candle_core::quantized::gguf_file::Content::read(&mut header_cursor)
            .map_err(|e| Error::Config(format!("gguf re-parse: {e}")))?;
        let weights = match arch.as_str() {
            "llama" => ArchWeights::Llama(
                m::quantized_llama::ModelWeights::from_gguf(content, &mut reader, device)
                    .map_err(ce)?,
            ),
            "qwen2" => ArchWeights::Qwen2(
                m::quantized_qwen2::ModelWeights::from_gguf(content, &mut reader, device)
                    .map_err(ce)?,
            ),
            "qwen3" => ArchWeights::Qwen3(
                m::quantized_qwen3::ModelWeights::from_gguf(content, &mut reader, device)
                    .map_err(ce)?,
            ),
            "gemma3" => ArchWeights::Gemma3(
                m::quantized_gemma3::ModelWeights::from_gguf(content, &mut reader, device)
                    .map_err(ce)?,
            ),
            "phi3" => ArchWeights::Phi3(
                m::quantized_phi3::ModelWeights::from_gguf(false, content, &mut reader, device)
                    .map_err(ce)?,
            ),
            other => {
                return Err(Error::Config(format!(
                    "unsupported GGUF architecture `{other}` (supported: {})",
                    SEQUENTIAL_ARCHS.join(", ")
                )))
            }
        };
        let max_seq_len = ctx.min(gguf.metadata.context_length).max(1);
        info!(arch = %arch, ctx = max_seq_len, "sequential runtime ready");
        Ok(Self {
            weights,
            device: device.clone(),
            max_seq_len,
            resident: None,
        })
    }

    fn forward_tokens(&mut self, tokens: &[u32], index_pos: usize) -> Result<Vec<f32>> {
        let input = Tensor::new(tokens, &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(|e| Error::Internal(format!("input tensor: {e}")))?;
        let logits = self
            .weights
            .forward(&input, index_pos)
            .map_err(|e| Error::Internal(format!("forward: {e}")))?;
        // Some candle models return (b, vocab), others (b, t, vocab).
        let logits = match logits.dims().len() {
            3 => {
                let (_b, t, _v) = logits.dims3().map_err(|e| Error::Internal(e.to_string()))?;
                logits
                    .i((0, t - 1, ..))
                    .map_err(|e| Error::Internal(format!("last logits: {e}")))?
            }
            _ => logits
                .squeeze(0)
                .map_err(|e| Error::Internal(format!("squeeze logits: {e}")))?,
        };
        logits
            .to_dtype(candle_core::DType::F32)
            .and_then(|t| t.to_vec1::<f32>())
            .map_err(|e| Error::Internal(format!("logits readback: {e}")))
    }
}

impl BatchModel for SequentialModel {
    fn max_batch(&self) -> usize {
        1
    }

    fn max_seq_len(&self) -> usize {
        self.max_seq_len
    }

    fn prefill(
        &mut self,
        seq: u64,
        tokens: &[u32],
        start_pos: usize,
        want_logits: bool,
    ) -> Result<Option<Vec<f32>>> {
        if start_pos == 0 {
            self.resident = Some((seq, 0));
        }
        match self.resident {
            Some((s, n)) if s == seq && n == start_pos => {}
            _ => {
                return Err(Error::Internal(format!(
                    "sequential runtime: seq {seq}@{start_pos} is not the resident sequence"
                )))
            }
        }
        let logits = if start_pos == 0 {
            self.forward_tokens(tokens, 0)?
        } else {
            // The stock candle models build a (t, t) causal mask that
            // does not compose with a non-empty cache, so continuation
            // chunks are fed token by token.
            let mut last = Vec::new();
            for (i, &tok) in tokens.iter().enumerate() {
                last = self.forward_tokens(&[tok], start_pos + i)?;
            }
            last
        };
        self.resident = Some((seq, start_pos + tokens.len()));
        Ok(want_logits.then_some(logits))
    }

    fn decode(&mut self, batch: &[(u64, u32)]) -> Result<Vec<Vec<f32>>> {
        match batch {
            [] => Ok(vec![]),
            [(seq, token)] => {
                let (s, n) = self
                    .resident
                    .ok_or_else(|| Error::Internal("decode with no resident sequence".into()))?;
                if s != *seq {
                    return Err(Error::Internal(format!(
                        "sequential runtime: decode of seq {seq} but {s} is resident"
                    )));
                }
                let logits = self.forward_tokens(&[*token], n)?;
                self.resident = Some((s, n + 1));
                Ok(vec![logits])
            }
            _ => Err(Error::Internal(
                "sequential runtime cannot decode a batch larger than 1".into(),
            )),
        }
    }

    fn drop_seq(&mut self, seq: u64) {
        if matches!(self.resident, Some((s, _)) if s == seq) {
            self.resident = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Device selection
// ---------------------------------------------------------------------------

pub fn pick_device(backend: Backend) -> Result<Device> {
    match backend {
        Backend::Cpu => Ok(Device::Cpu),
        Backend::Metal => {
            #[cfg(feature = "metal")]
            {
                Device::new_metal(0).map_err(|e| Error::Config(format!("metal: {e}")))
            }
            #[cfg(not(feature = "metal"))]
            Err(Error::Config(
                "metal backend requested but cgn-infer was built without the `metal` feature"
                    .into(),
            ))
        }
        Backend::Cuda => {
            #[cfg(feature = "cuda")]
            {
                Device::new_cuda(0).map_err(|e| Error::Config(format!("cuda: {e}")))
            }
            #[cfg(not(feature = "cuda"))]
            Err(Error::Config(
                "cuda backend requested but cgn-infer was built without the `cuda` feature".into(),
            ))
        }
        Backend::Auto => {
            #[cfg(feature = "metal")]
            if let Ok(d) = Device::new_metal(0) {
                return Ok(d);
            }
            #[cfg(feature = "cuda")]
            if let Ok(d) = Device::new_cuda(0) {
                return Ok(d);
            }
            Ok(Device::Cpu)
        }
    }
}
