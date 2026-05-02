# KV strategy: how Cognitora composes engines, offload backends, and the cross-cluster index

This document explains *what* KV caching is in Cognitora, *which* third-party
KV systems we integrate with (LMCache, SGLang HiCache, NVIDIA KVBM, NIXL),
and *why* we don't reinvent any of them. It also positions us against
[NVIDIA Dynamo](https://github.com/ai-dynamo/dynamo) — the closest
analogue — and highlights the points where we are deliberately ahead.

If you are looking for the **runtime data plane** (RAM tier, SSD tier,
cross-node QUIC fetch), see [`kv-tiering.md`](kv-tiering.md). This page
is about **strategy**, not implementation.

## The four KV layers

A request that hits Cognitora touches up to four distinct KV systems,
each with a different owner:

| # | Layer                         | Owner              | What lives here                                            | Latency target        |
|---|-------------------------------|--------------------|------------------------------------------------------------|-----------------------|
| 1 | Engine-internal KV            | vLLM / SGLang / llama.cpp | The KV blocks the model writes during prefill. Pinned in GPU HBM. | µs (GPU local)        |
| 2 | Engine-side offload connector | The connector (`LMCacheConnectorV1`, `DynamoConnector`, `--enable-hierarchical-cache`) | Spillover blocks in CPU RAM, NVMe, Redis, Mooncake, S3. Stacks on layer 1 via vLLM/SGLang's connector ABI. | sub-ms → ms           |
| 3 | Cross-worker KV transfer      | `NixlConnector` (vLLM) or NIXL inside KVBM/HiCache | The KV produced on a prefill GPU streamed to a decode GPU in disagg topologies. | RDMA-bound, sub-ms    |
| 4 | Cross-cluster KV-aware routing | **`cgn-kvcached` + `cgn-router`** | A persistent index of *which node holds which prefix*. Used to score candidate workers before a request is dispatched. | sub-ms (same DC)      |

Cognitora **owns layer 4**. We **integrate** layers 2 and 3. Layer 1 is
left untouched — it's the engine's internal accounting.

## What we use today

| Capability                        | Today | Future-work tag |
|-----------------------------------|-------|-----------------|
| Layer 4 — cross-cluster prefix index (Rust, RocksDB, QUIC peer fetch) | yes  | mature          |
| Layer 3 — `NixlConnector` for vLLM disagg                            | yes  | mature          |
| Layer 2 — `LMCacheConnectorV1` for vLLM (agg + disagg)               | yes (auto-wired by `kv_offload = "lmcache"`) | mature |
| Layer 2 — SGLang HiCache (`--enable-hierarchical-cache`)             | yes (auto-wired by `kv_offload = "hicache"`) | mature |
| Layer 2 — `DynamoConnector` (KVBM)                                   | yes (auto-wired by `kv_offload = "kvbm"`) — for parity benchmarks | benchmarking |
| Layer 2 — FlexKV                                                     | no    | considering     |
| Mooncake-backed shared HiCache pool                                  | no, but the recipe TOML can pass through `--hicache-storage-backend mooncake` | compatible |

**No, we do not currently use LMCache by default.** It is now opt-in via
`[engine].kv_offload = "lmcache"` and ships in two reference recipes:

* `recipes/llama3-8b/vllm/agg-lmcache/`
* `recipes/llama3-8b/vllm/disagg-lmcache/`

## Why we don't ship our own KVBM-equivalent

Three reasons:

1. **Coupling tax.** KVBM lives behind vLLM's `KVConnectorBase_V1` and
   TRT-LLM's PyTorch backend. Reimplementing it would require tracking
   two engine ABIs whose only consumers in practice are KVBM, LMCache,
   and FlexKV. We would spend most of our time chasing breaking
   changes in those ABIs.
2. **Diminishing returns.** Once you have a working connector
   (LMCache, KVBM, FlexKV, HiCache) the marginal gain from yet
   another connector is small — they all hit the same ~3-10× TTFT
   improvement on RAG workloads. The big wins come from *layer 4*
   (smart routing) and *layer 3* (disagg topologies), which we
   already own.
3. **Coverage matters more than depth.** Dynamo is vLLM/TRT-LLM
   centric for KVBM; we get strictly broader coverage by using LMCache
   for vLLM, HiCache for SGLang, and KVBM for benchmark parity, all
   selectable via one TOML knob.

## Single dial: `engine.kv_offload`

```toml
[engine]
kind       = "vllm"        # or "sglang", "llama_cpp", "openai_compat"
kv_offload = "lmcache"     # or "none" | "nixl" | "hicache" | "kvbm"
```

`cgn-agent` translates this to the right CLI flags at engine launch.
The compatibility matrix is:

| engine          | `none` | `nixl` | `lmcache` | `hicache` | `kvbm` |
|-----------------|--------|--------|-----------|-----------|--------|
| `vllm`          | yes    | yes    | yes       | no        | yes    |
| `sglang`        | yes    | yes    | no        | yes       | no     |
| `llama_cpp`     | yes    | no     | no        | no        | no     |
| `openai_compat` | yes    | no     | no        | no        | no     |

Disagg topologies (`[agent].role = "prefill"` or `"decode"`)
auto-stack the chosen offload backend with NIXL, mirroring NVIDIA's
recommended pattern from
[`docs/integrations/lmcache-integration.md`](https://github.com/ai-dynamo/dynamo/blob/main/docs/integrations/lmcache-integration.md).

The full rendering table is in `cgn-agent::engine::spawn`. For
convenience:

* `vllm × lmcache × prefill` →
  `--kv-transfer-config '{"kv_connector":"PdConnector",...,"connectors":[LMCache,NIXL]}'`
* `vllm × lmcache × decode` →
  `--kv-transfer-config '{"kv_connector":"NixlConnector","kv_role":"kv_both"}'`
* `vllm × kvbm × *` →
  `--kv-transfer-config '{"kv_connector":"DynamoConnector",...,"kv_connector_module_path":"kvbm.vllm_integration.connector"}'`
* `sglang × hicache × *` →
  `--enable-hierarchical-cache --hicache-ratio 2 --hicache-write-policy write_through --hicache-storage-backend nixl`

## Comparison: Cognitora vs Dynamo

| Concern                                            | Dynamo                                          | Cognitora                                                                |
|----------------------------------------------------|-------------------------------------------------|--------------------------------------------------------------------------|
| Built-in offload backend                           | KVBM (Rust + Python)                            | None — we integrate LMCache / HiCache / KVBM as alternatives             |
| Cross-cluster prefix index                         | `kv-router` + Python events                     | `cgn-kvcached` + `cgn-router` (all-Rust, RocksDB, QUIC peer fetch)        |
| Routing correctness                                | Radix tree of GPU-resident blocks               | Same idea, plus **sequence-chained BLAKE3 digests** so the router never confuses repeated chunks at different positions (see `cgn-core::hash::hash_seq_chunks`) |
| Engine support                                     | vLLM, TRT-LLM, SGLang                            | vLLM, **SGLang**, **llama.cpp**, openai-compat                            |
| Disaggregation                                     | Yes (1P1D, 2P2D)                                | Yes (`vllm/disagg-single-node`, `vllm/disagg-lmcache`)                    |
| Deployment artefact                                | Kubernetes operator + CRDs                       | Flat TOML profiles + `up.sh`; optional `cgn-ctl recipe up`                |
| Python footprint                                   | Required (frontend + most backends)              | None for the Rust binaries; engines bring their own                       |
| LMCache support                                    | Yes (one-off launch script)                      | Yes (`kv_offload = "lmcache"`, agg & disagg)                              |
| HiCache support                                    | Yes (Mooncake-backed)                            | Yes (`kv_offload = "hicache"`, NIXL backend by default; Mooncake via passthrough) |
| KVBM support                                       | Yes (native)                                     | Yes (`kv_offload = "kvbm"`) — benchmark parity                            |
| Single-binary install                              | No                                               | Yes — three Rust binaries, no Python                                      |

## How "beating Dynamo" looks concretely

We don't claim a faster KV offload backend in absolute terms — KVBM and
LMCache are both well-tuned and our recipes piggyback on them. What we
**do** claim:

1. **Better routing correctness.** Sequence-chained digests +
   longest-prefix overlap make our KV scoring positionally correct
   where Dynamo's chunk-overlap can mis-score interleaved prefixes.
2. **Broader engine coverage.** SGLang HiCache and llama.cpp are
   first-class, not afterthoughts.
3. **Lower operational tax.** Three Rust binaries, no Kubernetes
   operator, no Python control plane. Same recipes work bare-metal
   and in Kubernetes.
4. **One dial for KV.** `engine.kv_offload` swaps backends without
   editing the engine argv yourself; recipes don't drift.
5. **Federation-ready.** `cgn-kvcached`'s QUIC peer fetch composes
   across clusters. KVBM's leader/worker topology is single-cluster.

## When to pick which `kv_offload`

| Workload                                            | Recommended `kv_offload` | Recipe                              |
|-----------------------------------------------------|--------------------------|-------------------------------------|
| Dev / smoke tests / one-shot completions            | `none`                    | `vllm/agg`                          |
| Long sessions, RAG, chat, repeated system prompts   | `lmcache` (vLLM) / `hicache` (SGLang) | `vllm/agg-lmcache`, `sglang/agg-hicache` |
| 2-GPU node, want best TTFT under load               | `lmcache` (disagg)        | `vllm/disagg-lmcache`               |
| Head-to-head benchmark vs Dynamo                    | `kvbm`                    | `vllm/agg-kvbm`                     |
| Air-gapped / no extra Python deps                   | `none` + cgn-kvcached     | `vllm/agg`, `llama-cpp/cpu`         |

## Future work

* `kv_offload = "flexkv"` — Tencent's
  [FlexKV](https://github.com/taco-project/FlexKV) connector. The
  rendering shape is the same as LMCache; we just need a renderer
  branch and a recipe.
* `[engine.sglang].hicache_storage_backend` — first-class TOML for
  picking `nixl | mooncake | nvme | s3` instead of overriding via
  `extra_args`.
* WSPT prefill scheduling in `cgn-router` — Dynamo's "weighted shortest
  predicted task" admission. The KV-overlap signal needed for it
  already exists; the queue restructure is what's missing.
* Federated peer fetch policy — a routing knob for "prefer
  intra-cluster cache hit over remote LMCache hit" to bound egress
  cost.
