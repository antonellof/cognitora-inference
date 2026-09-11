# cgn-infer: the native inference engine

> **Status: experimental / preview.** `cgn-infer` ships alongside the
> other Cognitora binaries. Phases 1–5 (single-node engine, platform
> integration, continuous batching, distributed layer pipeline, and
> architecture breadth) are implemented; for production GPU workloads
> vLLM or SGLang remain the battle-tested choices.

`cgn-infer` is Cognitora's **first-party inference engine**, the
seventh binary in the workspace. Where the other six binaries
orchestrate *external* engines (vLLM, SGLang, llama.cpp, MLX,
OpenAI-compatible), `cgn-infer` loads and runs local GGUF models
itself, making Cognitora a self-sufficient serving stack rather than
only a control plane.

It is written in Rust on top of [Candle](https://github.com/huggingface/candle)
(HuggingFace's Rust tensor library), which provides CPU / Metal / CUDA
backends and quantized-GGUF support out of the box. Custom hand-written
GPU kernels are deliberately deferred: Candle's kernels sit behind a
`Runtime` trait and can be swapped later without touching the server or
scheduler layers.

## Design

```
cgn-router ──OpenAI HTTP / gRPC──▶ cgn-agent ──spawns + supervises──▶ cgn-infer
                                                                        │
                                              ┌─────────────────────────┤
                                              │  axum OpenAI API (SSE)  │
                                              │  scheduler / batching   │
                                              │  Runtime trait          │
                                              │   └─ Candle backend     │
                                              │      (CPU/Metal/CUDA)   │
                                              │  paged KV + prefix reuse│
                                              └─────────────────────────┘
```

Key decisions:

* **Model format: GGUF via mmap.** Weights are memory-mapped
  (page-cache resident, no heap copy), matching llama.cpp conventions
  so existing model files work unchanged. safetensors is not loaded at
  runtime; conversion to GGUF is an offline step.
* **Wire protocol: OpenAI HTTP/SSE.** The engine exposes exactly the
  surface `cgn-agent` already assumes (`/v1/chat/completions`,
  `/v1/completions`, `/v1/models`, `/healthz`), so it integrates like
  any other engine and fits KV-aware routing and `cgn-kvcached`
  naturally as a first-party citizen.
* **KV cache: paged with prefix reuse.** Block hashing aligns with
  `cgn-kvcached`'s sequence-chained BLAKE3 block scheme, so the
  router's prefix-overlap scoring is positionally correct against
  `cgn-infer` replicas.
* **Distribution: layer-pipeline parallelism.** Models are split by
  layer ranges across processes/nodes; the coordinator streams hidden
  states to workers over tonic gRPC with mTLS (f16 activations,
  optional int8), reusing etcd for discovery instead of a bespoke TCP
  protocol.

## Continuous batching (Phase 3)

The engine runs a dedicated scheduler thread (`src/scheduler/`) that
multiplexes many sequences over one model:

* **Chunked prefill**: prompts are processed `--prefill-chunk`
  (default 512) tokens per step, so a long prompt cannot starve
  decoding sequences.
* **Batched decode**: up to `--max-batch` (default 8) sequences
  advance one token per step. Candle's stock `quantized_llama` keeps
  a single KV cache *inside* the model, which forces sequential
  serving; `cgn-infer` therefore implements its own forward pass over
  the quantized GGUF weights (`src/model/llama.rs`) with **external
  per-sequence KV caches**. Per decode step the heavy weight matmuls
  (QKV, attention output, MLP, LM head) run once over the whole
  batch, while RoPE + attention run per sequence (each sequence has
  its own position and history length).
* **Paged KV accounting with preemption**: KV space is reserved in
  16-token blocks (the cgn-kvcached granularity) against a
  `--kv-pool-tokens` budget. Blocks are logical: tensors stay
  contiguous per sequence, but admission and eviction are decided at
  block granularity. Under pressure the youngest running sequence is
  preempted (KV dropped, request re-queued with its generated tokens
  preserved) rather than failing requests.

Batched decode covers GGUF architectures `llama` (including Mistral
GGUFs, which declare `general.architecture = "llama"`) and `qwen2`.
Other supported architectures fall back to a **sequential runtime**
(stock candle-transformers models, one sequence at a time) behind the
same scheduler; fairness and queueing still apply, only the batching
degree drops to 1.

## Distributed layer pipeline (Phase 4)

```bash
# Workers first (each binds one layer slice of the same GGUF):
cgn-infer worker --model llama3-8b.gguf --layers 11:22 --listen 10.0.0.2:9101
cgn-infer worker --model llama3-8b.gguf --layers 22:32 --listen 10.0.0.3:9101

# Coordinator: embedding + layers 0:11 + LM head, remote middle:
cgn-infer serve --model llama3-8b.gguf --role coordinator --layers 0:11 \
  --workers http://10.0.0.2:9101,http://10.0.0.3:9101 --port 8001
```

* The `cognitora.v1.InferPipeline` gRPC service
  (`proto/cognitora/v1/infer.proto`) carries a long-lived
  bidirectional activation stream: one `ActivationChunk` per forward
  step per sequence, f16-encoded by default
  (`--activation-encoding int8` halves bandwidth at some accuracy
  cost).
* Workers mmap the full GGUF but bind only their slice, so per-worker
  memory tracks the slice size. The coordinator validates at startup
  that coordinator + worker slices exactly tile the model's layers.
* mTLS: pass `--tls-ca/--tls-cert/--tls-key` to both roles (same PKI
  as the rest of the platform, `cgn-ctl pki bootstrap` for dev).
* Pipeline mode is sequential (`max_batch = 1`): one activation
  stream is in flight at a time. Chunked prefill and scheduling still
  apply. Only `llama`/`qwen2` architectures can run in pipeline mode.

Under `cgn-agent`, the `[models.*.pipeline]` TOML block renders this
topology automatically: locally spawned workers start before the
coordinator, workers register in etcd as **non-servable** (no `model`
field, `servable = false`) so the router only targets the
coordinator, and the supervisor restarts the **whole pipeline** if
any member dies (surviving members would hold KV state inconsistent
with a fresh peer).

## Supported architectures and quantizations (Phase 5)

| GGUF `general.architecture` | Runtime | Notes |
|-----------------------------|---------|-------|
| `llama` (incl. Mistral GGUFs) | batched | pipeline-capable |
| `qwen2`                     | batched | QKV bias, NEOX RoPE; pipeline-capable |
| `llama` with `expert_count > 1` (Mixtral-style MoE) | sequential | stock candle path |
| `qwen3`                     | sequential | q/k per-head norms |
| `gemma3`                    | sequential | |
| `phi3`                      | sequential | |

Quantization support comes from Candle's GGML/GGUF kernels and applies
to every architecture above: `Q4_0`, `Q4_1`, `Q5_0`, `Q5_1`, `Q8_0`,
`Q8_1`, the k-quants `Q2_K`–`Q8_K` (e.g. `Q4_K_M`, `Q5_K_M`,
`Q6_K`), plus unquantized `F16`/`F32` tensors. I-quants (`IQ*`) are
**not** supported by Candle 0.9 and will fail at load time.

## CLI

```bash
cgn-infer serve --model /models/llama-3.1-8b-q4_k_m.gguf \
  --host 127.0.0.1 --port 8001 --ctx 8192 --threads 8 \
  --max-batch 8 --prefill-chunk 512 --kv-pool-tokens 65536
```

Under `cgn-agent`, set `engine.kind = "cgn_infer"` and the agent
renders this argv automatically; see the
[configuration reference](../reference/config.md).

## Crate layout

`rust/services/cgn-infer/` is a binary + library crate:

| Module           | Responsibility |
|------------------|----------------|
| `src/main.rs`    | CLI (`serve`, `--role coordinator\|worker`, `--layers A:B`) |
| `src/server/`    | axum OpenAI endpoints, SSE streaming |
| `src/model/`     | GGUF mmap loader, architecture configs, weight binding |
| `src/runtime/`   | `Runtime` trait + Candle implementation (forward pass, layer slicing) |
| `src/kv/`        | paged KV cache, block-hash prefix reuse |
| `src/sampling/`  | logits processing: temperature, top-p, top-k, repetition penalty |
| `src/scheduler/` | request queue, continuous batching (Phase 3) |
| `src/pipeline/`  | distributed coordinator/worker gRPC (Phase 4) |

The activation-streaming service for pipeline mode is defined in
`proto/cognitora/v1/infer.proto`.

## Phases

| Phase | Scope | Status |
|-------|-------|--------|
| **1. Single-node MVP** | Llama-family GGUF via Candle, OpenAI chat/completions with SSE, greedy + standard sampling, per-request KV cache | **done** |
| **2. Platform integration** | `EngineKind::CgnInfer` in `cgn-core`, spawn/supervision in `cgn-agent`, recipes, same release tarball/image as the other binaries | **done** |
| **3. Continuous batching** | Scheduler admitting multiple sequences per forward pass (prefill chunking + batched decode), paged KV blocks with preemption | **done** |
| **4. Distributed layer pipeline** | Layer-range sharding across nodes, gRPC (mTLS) activation streaming, coordinator/worker topology from `[models.*.pipeline]`, etcd registration with non-servable worker role, whole-pipeline restart | **done** |
| **5. Breadth** | Qwen2 in the batched runtime; Qwen3 / Gemma3 / Phi-3 / MoE-llama via the sequential fallback | **done** (disk KV persistence + speculative decoding still open) |

## Current limitations

* **Batched decode is llama/qwen2 only.** Other architectures serve
  sequentially (one sequence at a time) behind the same scheduler.
* **Logical paging.** KV blocks are an accounting/eviction unit; the
  tensors themselves are stored contiguously per sequence, so
  fragmentation-free physical paging (vLLM-style) is future work.
* **Pipeline mode is sequential.** One activation stream in flight;
  no pipelined micro-batches yet.
* **`kv_offload = "none"` only.** No LMCache / HiCache / KVBM /
  NIXL connector support; the engine's own paged KV cache is the only
  KV layer.
* **Candle kernels.** No custom GPU kernels yet; performance tracks
  Candle's CPU / Metal / CUDA backends.

## Non-goals (for now)

* Custom hand-written GPU kernels (swappable later behind `Runtime`).
* Tensor parallelism within a layer (pipeline parallelism only).
* Training / fine-tuning, runtime safetensors loading.

## Related docs

* [Configuration reference: `engine.cgn_infer`](../reference/config.md)
* [KV strategy](kv-strategy.md) · [KV tiering](kv-tiering.md)
* [Routing](routing.md)
