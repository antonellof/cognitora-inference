//! Custom quantized Llama-family forward pass with **external**
//! per-sequence KV caches and **layer slicing**.
//!
//! Candle's `quantized_llama::ModelWeights` keeps one KV cache inside
//! the model, which forces strictly sequential serving: a second
//! request clobbers the first request's attention state. This module
//! re-implements the same GGUF-weight forward pass (structure follows
//! `candle_transformers::models::quantized_llama`) with two changes
//! that unlock phases 3 and 4:
//!
//! * KV state lives in caller-owned [`SeqKv`] values, one per
//!   sequence, so many sequences can be in flight and decode steps
//!   can be **batched**: the heavy weight matmuls (QKV, output, MLP)
//!   run once over the whole batch while attention — which is cheap
//!   and depends on per-sequence history length — runs per sequence.
//! * The loader can bind an arbitrary layer range `[start, end)` and
//!   optionally skip the embedding / output head, so a pipeline
//!   worker (phase 4) mmaps the full GGUF but only pays for its
//!   slice.
//!
//! Supported GGUF architectures: `llama` (which also covers Mistral
//! GGUFs — they ship with `general.architecture = "llama"`) and
//! `qwen2` (adds QKV bias and NEOX-style RoPE). MoE (Mixtral-style
//! `llama.expert_count > 1`) is intentionally not handled here; those
//! files are served through the sequential fallback runtime.

use candle_core::quantized::{gguf_file, QMatMul};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::Module;
use candle_transformers::quantized_nn::RmsNorm;
use candle_transformers::utils::repeat_kv;
use cgn_core::{Error, Result};

/// Half-open layer interval `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerRange {
    pub start: usize,
    pub end: usize,
}

impl LayerRange {
    pub fn new(start: usize, end: usize) -> Result<Self> {
        if start >= end {
            return Err(Error::InvalidArgument(format!(
                "layer range {start}:{end} is empty"
            )));
        }
        Ok(Self { start, end })
    }

    /// Parse the CLI form `A:B` (half-open).
    pub fn parse(s: &str) -> Result<Self> {
        let (a, b) = s.split_once(':').ok_or_else(|| {
            Error::InvalidArgument(format!("layer range `{s}` must be of the form A:B"))
        })?;
        let start = a
            .trim()
            .parse::<usize>()
            .map_err(|e| Error::InvalidArgument(format!("layer range start `{a}`: {e}")))?;
        let end = b
            .trim()
            .parse::<usize>()
            .map_err(|e| Error::InvalidArgument(format!("layer range end `{b}`: {e}")))?;
        Self::new(start, end)
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Display for LayerRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.start, self.end)
    }
}

/// Split `total` layers into `parts` contiguous ranges, front-loading
/// the remainder so earlier stages (which also pay for the embedding
/// or output head) get at most one extra layer.
pub fn split_layers(total: usize, parts: usize) -> Result<Vec<LayerRange>> {
    if parts == 0 || parts > total {
        return Err(Error::InvalidArgument(format!(
            "cannot split {total} layers into {parts} parts"
        )));
    }
    let base = total / parts;
    let rem = total % parts;
    let mut out = Vec::with_capacity(parts);
    let mut start = 0;
    for i in 0..parts {
        let len = base + usize::from(i < rem);
        out.push(LayerRange {
            start,
            end: start + len,
        });
        start += len;
    }
    Ok(out)
}

/// Check that `ranges`, in order, exactly tile `[0, total)`.
pub fn validate_coverage(total: usize, ranges: &[LayerRange]) -> Result<()> {
    let mut next = 0usize;
    for r in ranges {
        if r.start != next {
            return Err(Error::InvalidArgument(format!(
                "pipeline layer ranges must be contiguous: expected a range starting at {next}, got {r}"
            )));
        }
        next = r.end;
    }
    if next != total {
        return Err(Error::InvalidArgument(format!(
            "pipeline layer ranges cover [0, {next}) but the model has {total} layers"
        )));
    }
    Ok(())
}

/// Which non-layer parts of the model to bind.
#[derive(Debug, Clone, Copy)]
pub struct ModelParts {
    /// Token embedding table (needed by the stage that sees token ids).
    pub embedding: bool,
    /// Final norm + LM head (needed by the stage that emits logits).
    pub output: bool,
}

impl ModelParts {
    /// Single-node: everything.
    pub fn full() -> Self {
        Self {
            embedding: true,
            output: true,
        }
    }

    /// Pipeline worker: hidden states in, hidden states out.
    pub fn middle() -> Self {
        Self {
            embedding: false,
            output: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RopeStyle {
    /// GGUF llama convention (`rope_i`, interleaved pairs).
    Interleaved,
    /// NEOX / qwen2 convention (`rope`, split halves).
    Neox,
}

/// Hyperparameters read from the GGUF header.
#[derive(Debug, Clone)]
pub struct QLlamaConfig {
    pub architecture: String,
    pub n_layers: usize,
    pub n_head: usize,
    pub n_kv_head: usize,
    pub head_dim: usize,
    pub embedding_length: usize,
    pub rms_eps: f64,
    pub rope_freq_base: f32,
    pub rope_dim: usize,
    pub context_length: usize,
    qkv_bias: bool,
    rope_style: RopeStyle,
}

impl QLlamaConfig {
    /// Whether this module can run the given GGUF architecture.
    pub fn arch_supported(content: &gguf_file::Content) -> bool {
        let arch = content
            .metadata
            .get("general.architecture")
            .and_then(|v| v.to_string().ok().cloned())
            .unwrap_or_default();
        match arch.as_str() {
            "qwen2" => true,
            "llama" => {
                // Mixtral-style MoE goes through the sequential fallback.
                let n_expert = content
                    .metadata
                    .get("llama.expert_count")
                    .and_then(|v| v.to_u32().ok())
                    .unwrap_or(0);
                n_expert <= 1
            }
            _ => false,
        }
    }

    pub fn from_gguf(content: &gguf_file::Content) -> Result<Self> {
        let md = &content.metadata;
        let arch = md
            .get("general.architecture")
            .and_then(|v| v.to_string().ok().cloned())
            .ok_or_else(|| Error::Config("gguf: missing general.architecture".into()))?;
        let get_u32 = |k: &str| -> Result<u32> {
            md.get(&format!("{arch}.{k}"))
                .and_then(|v| v.to_u32().ok())
                .ok_or_else(|| Error::Config(format!("gguf: missing {arch}.{k}")))
        };
        let n_head = get_u32("attention.head_count")? as usize;
        let n_kv_head = get_u32("attention.head_count_kv")? as usize;
        let n_layers = get_u32("block_count")? as usize;
        let embedding_length = get_u32("embedding_length")? as usize;
        let context_length = get_u32("context_length").unwrap_or(4096) as usize;
        let head_dim = embedding_length / n_head;
        let rms_eps = md
            .get(&format!("{arch}.attention.layer_norm_rms_epsilon"))
            .and_then(|v| v.to_f32().ok())
            .unwrap_or(1e-5) as f64;
        let rope_freq_base = md
            .get(&format!("{arch}.rope.freq_base"))
            .and_then(|v| v.to_f32().ok())
            .unwrap_or(10_000.0);
        let (qkv_bias, rope_style, rope_dim) = match arch.as_str() {
            "qwen2" => (true, RopeStyle::Neox, head_dim),
            "llama" => {
                let rope_dim = md
                    .get("llama.rope.dimension_count")
                    .and_then(|v| v.to_u32().ok())
                    .map(|v| v as usize)
                    .unwrap_or(head_dim);
                (false, RopeStyle::Interleaved, rope_dim)
            }
            other => {
                return Err(Error::Config(format!(
                    "architecture `{other}` is not supported by the batched runtime"
                )))
            }
        };
        Ok(Self {
            architecture: arch,
            n_layers,
            n_head,
            n_kv_head,
            head_dim,
            embedding_length,
            rms_eps,
            rope_freq_base,
            rope_dim,
            context_length,
            qkv_bias,
            rope_style,
        })
    }
}

/// Per-sequence KV cache: one `(k, v)` pair per bound layer, each of
/// shape `(1, n_kv_head, seq_len, head_dim)`, grown by concatenation.
#[derive(Debug, Default)]
pub struct SeqKv {
    entries: Vec<Option<(Tensor, Tensor)>>,
    tokens: usize,
}

impl SeqKv {
    pub fn new(n_layers: usize) -> Self {
        Self {
            entries: vec![None; n_layers],
            tokens: 0,
        }
    }

    /// Number of positions cached.
    pub fn len(&self) -> usize {
        self.tokens
    }

    pub fn is_empty(&self) -> bool {
        self.tokens == 0
    }

    pub fn clear(&mut self) {
        for e in &mut self.entries {
            *e = None;
        }
        self.tokens = 0;
    }
}

struct QkvProj {
    weight: QMatMul,
    bias: Option<Tensor>,
}

impl QkvProj {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let y = self.weight.forward(x)?;
        match &self.bias {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }
}

struct Block {
    attn_norm: RmsNorm,
    wq: QkvProj,
    wk: QkvProj,
    wv: QkvProj,
    wo: QMatMul,
    ffn_norm: RmsNorm,
    w_gate: QMatMul,
    w_down: QMatMul,
    w_up: QMatMul,
}

impl Block {
    fn mlp(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let gate = self.w_gate.forward(x)?;
        let up = self.w_up.forward(x)?;
        let act = (candle_nn::ops::silu(&gate)? * up)?;
        self.w_down.forward(&act)
    }
}

/// A (possibly partial) quantized Llama-family model with external KV.
pub struct QLlama {
    pub cfg: QLlamaConfig,
    pub range: LayerRange,
    device: Device,
    embed: Option<candle_nn::Embedding>,
    blocks: Vec<Block>,
    out: Option<(RmsNorm, QMatMul)>,
    cos: Tensor,
    sin: Tensor,
    neg_inf: Tensor,
}

impl QLlama {
    /// Bind weights for `range` (plus embedding / output head per
    /// `parts`) from a parsed GGUF, reading tensor data through
    /// `reader` (a cursor over the mmap).
    pub fn load<R: std::io::Read + std::io::Seek>(
        content: &gguf_file::Content,
        reader: &mut R,
        device: &Device,
        range: LayerRange,
        parts: ModelParts,
    ) -> Result<Self> {
        let cfg = QLlamaConfig::from_gguf(content)?;
        if range.end > cfg.n_layers {
            return Err(Error::InvalidArgument(format!(
                "layer range {range} exceeds model layer count {}",
                cfg.n_layers
            )));
        }
        let ce = |e: candle_core::Error| Error::Internal(format!("weight load: {e}"));
        let tensor = |reader: &mut R, name: &str| -> Result<_> {
            content.tensor(reader, name, device).map_err(ce)
        };

        // RoPE tables over the full training context (candle's
        // quantized_llama caps these at 4096 which silently limits
        // long-context models; we size them from the header).
        let (cos, sin) = precompute_freqs(cfg.rope_dim, cfg.rope_freq_base, cfg.context_length, device)?;
        let neg_inf = Tensor::new(f32::NEG_INFINITY, device).map_err(ce)?;

        let embed = if parts.embedding {
            let w = tensor(reader, "token_embd.weight")?
                .dequantize(device)
                .map_err(ce)?;
            Some(candle_nn::Embedding::new(w, cfg.embedding_length))
        } else {
            None
        };

        let out = if parts.output {
            let norm = RmsNorm::from_qtensor(tensor(reader, "output_norm.weight")?, cfg.rms_eps)
                .map_err(ce)?;
            // Tied-embedding models have no separate output.weight.
            let head_q = match content.tensor(reader, "output.weight", device) {
                Ok(t) => t,
                Err(_) => tensor(reader, "token_embd.weight")?,
            };
            Some((norm, QMatMul::from_qtensor(head_q).map_err(ce)?))
        } else {
            None
        };

        let mut blocks = Vec::with_capacity(range.len());
        for idx in range.start..range.end {
            let p = format!("blk.{idx}");
            let proj = |reader: &mut R, name: &str| -> Result<QkvProj> {
                let weight =
                    QMatMul::from_qtensor(tensor(reader, &format!("{p}.{name}.weight"))?)
                        .map_err(ce)?;
                let bias = if cfg.qkv_bias && name != "attn_output" {
                    Some(
                        tensor(reader, &format!("{p}.{name}.bias"))?
                            .dequantize(device)
                            .map_err(ce)?,
                    )
                } else {
                    None
                };
                Ok(QkvProj { weight, bias })
            };
            let wq = proj(reader, "attn_q")?;
            let wk = proj(reader, "attn_k")?;
            let wv = proj(reader, "attn_v")?;
            let wo = QMatMul::from_qtensor(tensor(reader, &format!("{p}.attn_output.weight"))?)
                .map_err(ce)?;
            let attn_norm =
                RmsNorm::from_qtensor(tensor(reader, &format!("{p}.attn_norm.weight"))?, cfg.rms_eps)
                    .map_err(ce)?;
            let ffn_norm =
                RmsNorm::from_qtensor(tensor(reader, &format!("{p}.ffn_norm.weight"))?, cfg.rms_eps)
                    .map_err(ce)?;
            let w_gate = QMatMul::from_qtensor(tensor(reader, &format!("{p}.ffn_gate.weight"))?)
                .map_err(ce)?;
            let w_down = QMatMul::from_qtensor(tensor(reader, &format!("{p}.ffn_down.weight"))?)
                .map_err(ce)?;
            let w_up = QMatMul::from_qtensor(tensor(reader, &format!("{p}.ffn_up.weight"))?)
                .map_err(ce)?;
            blocks.push(Block {
                attn_norm,
                wq,
                wk,
                wv,
                wo,
                ffn_norm,
                w_gate,
                w_down,
                w_up,
            });
        }

        Ok(Self {
            cfg,
            range,
            device: device.clone(),
            embed,
            blocks,
            out,
            cos,
            sin,
            neg_inf,
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Fresh KV cache sized to this model's bound layer count.
    pub fn new_kv(&self) -> SeqKv {
        SeqKv::new(self.blocks.len())
    }

    /// Token ids → embeddings, shape `(1, t, embd)`.
    pub fn embed_tokens(&self, tokens: &[u32]) -> Result<Tensor> {
        let embed = self
            .embed
            .ok_or_embed()?;
        let x = Tensor::new(tokens, &self.device)
            .and_then(|t| t.unsqueeze(0))
            .map_err(internal("embed input"))?;
        use candle_nn::Module;
        embed.forward(&x).map_err(internal("embed"))
    }

    /// Run this stage's layers over `hidden` `(1, t, embd)` for a
    /// single sequence whose KV already covers `start_pos` positions.
    /// Appends `t` positions to `kv` and returns the transformed
    /// hidden states `(1, t, embd)`.
    pub fn forward_hidden(
        &self,
        hidden: &Tensor,
        start_pos: usize,
        kv: &mut SeqKv,
    ) -> Result<Tensor> {
        if kv.entries.len() != self.blocks.len() {
            return Err(Error::Internal("SeqKv layer count mismatch".into()));
        }
        let (_b, t, _e) = hidden.dims3().map_err(internal("hidden dims"))?;
        let mask = self.causal_mask(t, start_pos)?;
        let mut x = hidden.clone();
        for (block, slot) in self.blocks.iter().zip(kv.entries.iter_mut()) {
            x = self
                .block_forward_single(block, &x, start_pos, t, mask.as_ref(), slot)
                .map_err(internal("layer forward"))?;
        }
        kv.tokens = start_pos + t;
        Ok(x)
    }

    /// Batched decode: one new token per sequence. `hiddens` is
    /// `(B, 1, embd)`; `positions[i]` is the number of positions
    /// already cached for sequence `i` (which must equal
    /// `kvs[i].len()`). Returns `(B, 1, embd)`.
    ///
    /// The QKV / output / MLP matmuls run once over the whole batch;
    /// RoPE and attention run per sequence because each sequence has
    /// its own position and KV history length.
    pub fn decode_step(
        &self,
        hiddens: &Tensor,
        positions: &[usize],
        kvs: &mut [&mut SeqKv],
    ) -> Result<Tensor> {
        let (b, t, _e) = hiddens.dims3().map_err(internal("decode dims"))?;
        if t != 1 || b != positions.len() || b != kvs.len() {
            return Err(Error::Internal(format!(
                "decode_step batch mismatch: b={b} t={t} positions={} kvs={}",
                positions.len(),
                kvs.len()
            )));
        }
        let mut x = hiddens.clone();
        for (l, block) in self.blocks.iter().enumerate() {
            x = self
                .block_forward_batch(block, l, &x, positions, kvs)
                .map_err(internal("batched layer forward"))?;
        }
        for kv in kvs.iter_mut() {
            kv.tokens += 1;
        }
        Ok(x)
    }

    /// Final norm + LM head over the **last** position of `hidden`
    /// `(b, t, embd)` → logits `(b, vocab)`.
    pub fn output(&self, hidden: &Tensor) -> Result<Tensor> {
        let (norm, head) = self
            .out
            .as_ref()
            .ok_or_else(|| Error::Internal("this stage has no output head".into()))?;
        use candle_nn::Module;
        let (_b, t, _e) = hidden.dims3().map_err(internal("output dims"))?;
        let x = norm.forward(hidden).map_err(internal("output norm"))?;
        let x = x.i((.., t - 1, ..)).map_err(internal("last position"))?;
        head.forward(&x).map_err(internal("lm head"))
    }

    // -- internals ----------------------------------------------------------

    fn rope(&self, x: &Tensor, pos: usize, t: usize) -> candle_core::Result<Tensor> {
        let cos = self.cos.narrow(0, pos, t)?;
        let sin = self.sin.narrow(0, pos, t)?;
        match self.cfg.rope_style {
            RopeStyle::Interleaved => candle_nn::rotary_emb::rope_i(&x.contiguous()?, &cos, &sin),
            RopeStyle::Neox => candle_nn::rotary_emb::rope(&x.contiguous()?, &cos, &sin),
        }
    }

    /// `(t, start_pos + t)` mask where entry `(i, j)` is 1 for
    /// positions the query at `start_pos + i` must NOT attend to.
    fn causal_mask(&self, t: usize, start_pos: usize) -> Result<Option<Tensor>> {
        if t == 1 {
            return Ok(None);
        }
        let total = start_pos + t;
        let mask: Vec<u8> = (0..t)
            .flat_map(|i| (0..total).map(move |j| u8::from(j > start_pos + i)))
            .collect();
        Tensor::from_slice(&mask, (t, total), &self.device)
            .map(Some)
            .map_err(internal("mask"))
    }

    fn attend(
        &self,
        q: &Tensor,
        k: &Tensor,
        v: &Tensor,
        mask: Option<&Tensor>,
    ) -> candle_core::Result<Tensor> {
        let k = repeat_kv(k.clone(), self.cfg.n_head / self.cfg.n_kv_head)?;
        let v = repeat_kv(v.clone(), self.cfg.n_head / self.cfg.n_kv_head)?;
        let att = (q.matmul(&k.t()?)? / (self.cfg.head_dim as f64).sqrt())?;
        let att = match mask {
            None => att,
            Some(mask) => {
                let mask = mask.broadcast_as(att.shape())?;
                mask.where_cond(&self.neg_inf.broadcast_as(mask.shape())?, &att)?
            }
        };
        let att = candle_nn::ops::softmax_last_dim(&att)?;
        att.matmul(&v.contiguous()?)
    }

    fn block_forward_single(
        &self,
        block: &Block,
        x: &Tensor,
        start_pos: usize,
        t: usize,
        mask: Option<&Tensor>,
        slot: &mut Option<(Tensor, Tensor)>,
    ) -> candle_core::Result<Tensor> {
        let (b, _t, e) = x.dims3()?;
        let residual = x;
        let xn = block.attn_norm.forward(x)?;
        let q = block.wq.forward(&xn)?;
        let k = block.wk.forward(&xn)?;
        let v = block.wv.forward(&xn)?;
        let q = q
            .reshape((b, t, self.cfg.n_head, self.cfg.head_dim))?
            .transpose(1, 2)?;
        let k = k
            .reshape((b, t, self.cfg.n_kv_head, self.cfg.head_dim))?
            .transpose(1, 2)?;
        let v = v
            .reshape((b, t, self.cfg.n_kv_head, self.cfg.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;
        let q = self.rope(&q, start_pos, t)?;
        let k = self.rope(&k, start_pos, t)?;

        let (k, v) = match slot.take() {
            Some((kc, vc)) if start_pos > 0 => {
                (Tensor::cat(&[&kc, &k], 2)?, Tensor::cat(&[&vc, &v], 2)?)
            }
            _ => (k, v),
        };
        *slot = Some((k.clone(), v.clone()));

        let y = self.attend(&q, &k, &v, mask)?;
        let y = y.transpose(1, 2)?.reshape((b, t, e))?;
        let y = block.wo.forward(&y)?;
        let x = (y + residual)?;

        let residual = &x;
        let xn = block.ffn_norm.forward(&x)?;
        let y = block.mlp(&xn)?;
        y + residual
    }

    fn block_forward_batch(
        &self,
        block: &Block,
        layer: usize,
        x: &Tensor,
        positions: &[usize],
        kvs: &mut [&mut SeqKv],
    ) -> candle_core::Result<Tensor> {
        let (b, _t, e) = x.dims3()?;
        let residual = x;
        let xn = block.attn_norm.forward(x)?;
        // Heavy projections over the whole batch at once.
        let q = block.wq.forward(&xn)?;
        let k = block.wk.forward(&xn)?;
        let v = block.wv.forward(&xn)?;
        let q = q
            .reshape((b, 1, self.cfg.n_head, self.cfg.head_dim))?
            .transpose(1, 2)?;
        let k = k
            .reshape((b, 1, self.cfg.n_kv_head, self.cfg.head_dim))?
            .transpose(1, 2)?;
        let v = v
            .reshape((b, 1, self.cfg.n_kv_head, self.cfg.head_dim))?
            .transpose(1, 2)?
            .contiguous()?;

        // Per-sequence RoPE + attention (histories differ in length).
        let mut ys = Vec::with_capacity(b);
        for (i, kv) in kvs.iter_mut().enumerate() {
            let pos = positions[i];
            let qi = self.rope(&q.i(i..i + 1)?, pos, 1)?;
            let ki = self.rope(&k.i(i..i + 1)?, pos, 1)?;
            let vi = v.i(i..i + 1)?.contiguous()?;
            let (ki, vi) = match kv.entries[layer].take() {
                Some((kc, vc)) if pos > 0 => {
                    (Tensor::cat(&[&kc, &ki], 2)?, Tensor::cat(&[&vc, &vi], 2)?)
                }
                _ => (ki, vi),
            };
            kv.entries[layer] = Some((ki.clone(), vi.clone()));
            ys.push(self.attend(&qi, &ki, &vi, None)?);
        }
        let y = Tensor::cat(&ys, 0)?;
        let y = y.transpose(1, 2)?.reshape((b, 1, e))?;
        let y = block.wo.forward(&y)?;
        let x = (y + residual)?;

        let residual = &x;
        let xn = block.ffn_norm.forward(&x)?;
        let y = block.mlp(&xn)?;
        y + residual
    }
}

trait EmbedExt {
    fn ok_or_embed(&self) -> Result<&candle_nn::Embedding>;
}
impl EmbedExt for Option<candle_nn::Embedding> {
    fn ok_or_embed(&self) -> Result<&candle_nn::Embedding> {
        self.as_ref()
            .ok_or_else(|| Error::Internal("this stage has no embedding table".into()))
    }
}

fn internal(what: &'static str) -> impl Fn(candle_core::Error) -> Error {
    move |e| Error::Internal(format!("{what}: {e}"))
}

fn precompute_freqs(
    rope_dim: usize,
    freq_base: f32,
    context_length: usize,
    device: &Device,
) -> Result<(Tensor, Tensor)> {
    let ce = internal("rope tables");
    let theta: Vec<f32> = (0..rope_dim)
        .step_by(2)
        .map(|i| 1f32 / freq_base.powf(i as f32 / rope_dim as f32))
        .collect();
    let theta = Tensor::new(theta.as_slice(), device).map_err(&ce)?;
    let idx_theta = Tensor::arange(0, context_length as u32, device)
        .and_then(|t| t.to_dtype(DType::F32))
        .and_then(|t| t.reshape((context_length, 1)))
        .and_then(|t| t.matmul(&theta.reshape((1, theta.elem_count()))?))
        .map_err(&ce)?;
    let cos = idx_theta.cos().map_err(&ce)?;
    let sin = idx_theta.sin().map_err(&ce)?;
    Ok((cos, sin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_layer_range() {
        let r = LayerRange::parse("0:16").unwrap();
        assert_eq!((r.start, r.end, r.len()), (0, 16, 16));
        assert_eq!(LayerRange::parse("16:32").unwrap().len(), 16);
        assert!(LayerRange::parse("16").is_err());
        assert!(LayerRange::parse("8:8").is_err());
        assert!(LayerRange::parse("9:8").is_err());
        assert!(LayerRange::parse("a:b").is_err());
    }

    #[test]
    fn splits_layers_evenly() {
        let r = split_layers(32, 2).unwrap();
        assert_eq!(r, vec![LayerRange { start: 0, end: 16 }, LayerRange { start: 16, end: 32 }]);
    }

    #[test]
    fn splits_layers_with_remainder_front_loaded() {
        let r = split_layers(10, 3).unwrap();
        assert_eq!(
            r,
            vec![
                LayerRange { start: 0, end: 4 },
                LayerRange { start: 4, end: 7 },
                LayerRange { start: 7, end: 10 },
            ]
        );
        validate_coverage(10, &r).unwrap();
    }

    #[test]
    fn split_rejects_degenerate_inputs() {
        assert!(split_layers(4, 0).is_err());
        assert!(split_layers(4, 5).is_err());
    }

    #[test]
    fn coverage_validation() {
        let ok = vec![LayerRange { start: 0, end: 3 }, LayerRange { start: 3, end: 8 }];
        validate_coverage(8, &ok).unwrap();

        let gap = vec![LayerRange { start: 0, end: 3 }, LayerRange { start: 4, end: 8 }];
        assert!(validate_coverage(8, &gap).is_err());

        let short = vec![LayerRange { start: 0, end: 3 }];
        assert!(validate_coverage(8, &short).is_err());

        let over = vec![LayerRange { start: 0, end: 9 }];
        assert!(validate_coverage(8, &over).is_err());
    }

    #[test]
    fn seq_kv_bookkeeping() {
        let mut kv = SeqKv::new(4);
        assert!(kv.is_empty());
        kv.tokens = 12;
        assert_eq!(kv.len(), 12);
        kv.clear();
        assert!(kv.is_empty());
        assert!(kv.entries.iter().all(|e| e.is_none()));
    }
}
