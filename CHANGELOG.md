# Changelog

All notable changes to Cognitora are documented here.

The format is loosely based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Pre-1.0 releases may make small breaking changes between minor versions;
each one is called out under **Breaking** below.

## [Unreleased]

### Added

- **Carbon-aware admission** (`[carbon]` config section): the router polls
  a pluggable grid-intensity provider (`static`, `electricitymaps`, or
  `watttime`) on a background interval and rejects low-priority OpenAI HTTP
  requests (`X-CGN-Priority: low`) while observed gCO₂/kWh exceeds
  `intensity_threshold`. Exposes `cgn_carbon_intensity_gco2_per_kwh{zone}`
  and `cgn_router_carbon_admission_rejected_total`. Fails open until the
  first successful poll.

## [0.8.0] - 2026-09-11

The "etcd-optional" release. Multi-node clusters can now discover each
other over a UDP gossip mesh with no external services, and the fleet
gets power capping, cross-cluster federation, capability-aware routing,
ROCm support, and a standalone monitoring dashboard.

### Added

- **Gossip discovery backend** (`state_backend = "gossip"`): multi-node
  clusters without etcd. New `cgn-gossip` library crate wraps
  [chitchat](https://github.com/quickwit-oss/chitchat) (scuttlebutt
  gossip with phi-accrual failure detection) over UDP (default port
  7946). Agents republish the same JSON node record they write to etcd;
  the router joins as a record-less member and reconciles live members
  into its `NodeRegistry` every 2.5s. New `[cluster]` keys:
  `gossip_seeds`, `gossip_listen`, `gossip_advertise`. Etcd-backed
  control-plane features (confirmed-KV claims, cordon flags, routing
  policy hot-reload, autoscaler hints) stay etcd-only by design; see
  `docs/architecture/gossip.md`.
- **Soft power cap** (`[agent].watt_limit`): the limit rides the
  heartbeat and is mirrored to `cgn_cluster_node_watt_limit`; the
  selector prefers under-cap nodes and only routes to over-cap nodes
  when every candidate is over (serving beats brown-out). The agent
  Health RPC reports the real rack watt limit instead of a hardcoded 0.
- **Cross-cluster federation, actually wired**: when local routing finds
  no eligible node and `[router.federation]` is enabled, the gateway
  forwards to a peer cluster. Peers are probed concurrently and the
  lowest-connect-latency reachable peer wins. Forwards are counted in
  `cgn_router_federation_forwards_total{model,peer}`. Single-hop by
  construction: the peer's gRPC surface only routes locally.
- **Capability-aware routing** for heterogeneous fleets: agents publish
  `gpu_name` / `gpu_vendor` / `vram_total_mb` (NVML, or rocm-smi on
  AMD); per-model `min_vram_mb` and `require_gpu` constraints filter
  candidates. Nodes reporting no GPU identity are never filtered, so
  older agents keep working.
- **ROCm support**: `cgn-power` gains a rocm-smi reader feeding
  `cgn_power_watts_gpu` on AMD hosts; the agent GPU snapshot falls back
  from NVML to rocm-smi; engine spawn pins `CUDA_VISIBLE_DEVICES` and
  `HIP_VISIBLE_DEVICES` from `[agent].gpu_index` (ambient values win).
- **Standalone monitoring dashboard** (`dashboard/`): zero-dependency
  web app that polls any `/metrics` endpoint; in-browser ring buffers
  and canvas charts for req/s, tokens/s, latency p50/p95, TTFT p95,
  queue depth, power draw vs cap, KV used %, J/token, plus a live node
  table with GPU identity. Deep-linkable via `?endpoint=` and
  `?interval=`. A stdlib-only mock fleet generator
  (`dashboard/mock_metrics.py`) simulates a 16-node mixed GPU fleet for
  demos.
- **Cluster gauges and TTFT histogram**: the router mirrors its node
  registry into `cgn_cluster_node_*` gauges every 5s, and
  `cgn_router_chat_ttft_seconds` is observed on the first streamed
  token. `/metrics` now answers CORS preflight so browser apps can
  scrape any Cognitora listener directly.
- Release workflow publishes the library crates to crates.io in
  dependency order (skips with a warning until the `CRATES_IO_TOKEN`
  secret is configured).

### Fixed

- Agent OpenAI HTTP driver: SSE frames delimited by CRLF blank lines
  never completed and grew the buffer without bound; the frame parser
  now handles LF and CRLF and extracts the `data:` line from multi-line
  frames.
- Router: the etcd watcher reconnects with 5s backoff when its watch
  stream ends instead of leaving the registry stale forever.
- Agent: re-confirmed KV digests are deduplicated before queueing so
  eviction can't delete a key still tracked by a newer twin; the NVML
  handle is cached instead of re-initialised every heartbeat.
- Router: one shared prompt-flattening and approximate-tokenisation
  module for the HTTP gateway, gRPC surface, and embeddings (the gRPC
  copy ignored multimodal `content_json`).

## [0.7.0] - 2026-09-10

The "OpenAI parity + truth-fed routing" release. Tool calling,
structured output, and multimodal image inputs now pass through the
full router→agent→engine path verbatim; the prefix index gains a
completion-confirmed feed so KV-overlap scoring reflects what engines
actually cached; the operator grows a reactive SLA planner; multi-node
e2e runs in default CI; and the Helm chart is turnkey.

### Added
- **Tool calling & structured output passthrough**: `tools`,
  `tool_choice`, and `response_format` on `/v1/chat/completions` are
  forwarded verbatim to the engine (vLLM/SGLang implement tool parsing
  and guided decoding), via a new `extensions_json` field on the
  Generate protos. Streaming `delta.tool_calls` fragments flow back
  through the `Token` proto (`tool_calls_json`); buffered responses
  aggregate fragments per index into complete `tool_calls` with a
  proper `finish_reason`. Assistant `tool_calls` and tool-role
  `tool_call_id` messages round-trip. Cascade is bypassed for tool
  requests (tool formats are not portable across cascade models).
- **Multimodal image passthrough**: OpenAI content-parts arrays
  (`type: image_url`, …) are accepted on messages and carried verbatim
  to the engine (`content_json` on the `Message` proto). Prefix
  hashing uses only the text parts, so images don't pollute KV-overlap
  scoring.
- **Confirmed KV prefix feed**: after a generation completes, the
  agent publishes the request's prefix digests as lease-bound etcd
  keys under `/cognitora/kv/<node>/<digest>`; the router watcher
  mirrors PUT/DELETE into the `PrefixIndex`
  (`PrefixIndex::forget_claim`). Claims are confirmed-by-completion,
  die with the node's heartbeat lease, and the agent prunes its oldest
  claims under KV-cache pressure (<5 % free blocks) and beyond a
  4096-key cap. The overlap score now tracks engine truth instead of
  router optimism.
- **SLA planner-lite** (`cgn-operator::planner`): `ModelPool` SLOs
  (`maxQueuePerReplica`, `minReplicas`, `maxReplicas`,
  `scaleCooldownSecs`) drive reactive scaling of `decode_replicas`
  from live queue depths, with cooldown and status reporting
  (`desiredReplicas`, `lastScaleTime`).
- **Multi-node e2e in default CI**: `tests/e2e/multi_node_kv.sh`
  rewritten around a stub OpenAI engine (`tests/e2e/stub_engine.py`):
  4 phases including a real 2-agent prefix-affinity assertion, wired
  into the default GitHub Actions run (`e2e-multinode` job).
- **GPU disaggregation bench harness** (`scripts/bench/disagg/` +
  `bench-disagg.yml` workflow_dispatch): reproducible prefill/decode
  split benchmarks for GPU hosts; numbers to follow once run on GPU
  hardware.
- **Turnkey Helm**: the chart deploys a working cluster out of the box:
  engine sidecar block (`agent.engine.*`), `cluster.etcdEndpoints`,
  mTLS off by default for first contact (`security.require_mtls`),
  `hostNetwork` opt-in, chart README.

### Changed
- `Message` proto gains `content_json`, `tool_calls_json`,
  `tool_call_id`; `Token` gains `tool_calls_json`; `GenerateRequest` /
  `AgentGenerateRequest` gain `extensions_json`, and the agent request
  carries the router's prefix `digests` for confirmed-claim publishing.
  All additions are append-only field numbers, wire-compatible with
  0.6 clients.

## [0.6.0] - 2026-09-10

The "make the routing score true" release. Every term of the KV-aware
routing score is now fed by real signals, the energy-aware autoscaling
loop is closed end-to-end, the SLM→LLM cascade covers streaming
traffic, and TensorRT-LLM joins the spawn-managed engine roster.

### Added
- **Engine telemetry scraper** (`cgn-agent::telemetry`): polls the
  engine's Prometheus `/metrics` (vLLM `num_requests_waiting/running`,
  `gpu_cache_usage_perc`; SGLang `num_queue_reqs/num_running_reqs`,
  `token_usage`) and feeds real `queue_depth` / `free_blocks` /
  `total_blocks` into the etcd heartbeat and the `Agent.Health` RPC.
  The router's `load` and `capacity` score terms were previously inert
  (hardcoded zeros); they now differentiate workers. Engines without a
  metrics endpoint honestly report zeros (`total_blocks == 0` means
  "capacity unknown").
- **Closed autoscaler loop** (`cgn-operator::autoscaler`): the
  operator now consumes the router's energy-aware drain hints from
  `/cognitora/autoscaler/<node>` and translates them into cordon flags,
  which the router's watcher already honors. Cordons set by the
  autoscaler are tagged and never clobber manual `cgn-ctl` cordons;
  capacity is restored automatically when the drain hint clears.
- **Streaming cascade**: `stream_run_cascade` runs early cascade
  steps buffered (confidence gating needs complete output), emits an
  accepted cheap answer as SSE chunks, and streams the final model
  live token-by-token when every early step escalates. The cascade now
  applies to real (streaming) traffic, not just buffered requests.
- **Gateway dispatch retry / failover**: dispatch failures (agent
  unreachable, gRPC setup error) are retried against the next-best
  node with the failed node excluded, up to 3 attempts, strictly
  before the first token so retries are invisible to clients.
  `routing::pick_excluding` supports node exclusion; failed nodes no
  longer accrue optimistic prefix claims.
- **TensorRT-LLM engine driver** (`engine.kind = "tensorrt_llm"`):
  the agent spawns `trtllm-serve <model> --host … --port … --tp_size …`
  and supervises it like any other engine. `[engine.tensorrt_llm]`
  carries binary/host/port/extra_args.
- **Prefix-index truth maintenance**: periodic GC of expired entries
  (30 s), plus pressure-aware pruning: when a node's heartbeat reports
  <5 % free KV blocks, its older optimistic prefix claims are dropped
  (the engine is LRU-evicting, so they are the ones most likely gone).

### Changed
- `Agent.Health` RPC now returns real GPU (NVML) and engine stats and
  the node's loaded models/role instead of zeros.
- Docs honesty pass: IPMI/DCGM, gossip discovery, and RDMA are marked
  as roadmap items rather than shipped features; the Dynamo comparison
  (`docs/architecture/vs-dynamo.md`) is updated to Dynamo 1.x reality
  (etcd/NATS optional there) and to Cognitora's new 0.6 capabilities.

## [0.5.0] - 2026-07-02

The "native inference engine" release. Cognitora previously only
orchestrated external OpenAI-compatible engines (vLLM, SGLang,
llama.cpp, MLX). It now ships its own first-party inference engine,
`cgn-infer`: a seventh binary that loads and runs local GGUF models
directly, making Cognitora self-sufficient rather than purely a
control plane.

### Added

- **`cgn-infer` native engine (preview).** A new Rust service that runs
  quantized GGUF models via [Candle](https://github.com/huggingface/candle):
  - mmap GGUF loader (page-cache resident, no heap copy) with
    embedded-tokenizer reconstruction and embedded Jinja chat-template
    rendering (ChatML fallback).
  - Candle `quantized_llama` runtime behind a `Runtime` trait; CPU
    always available, `metal` / `cuda` behind cargo features
    (`--backend auto|cpu|metal|cuda`).
  - OpenAI-compatible axum server: `POST /v1/chat/completions`,
    `POST /v1/completions` (buffered + SSE streaming), `GET /v1/models`,
    `GET /healthz`.
  - Sampling (temperature, top-k, top-p, repetition penalty) and a
    BLAKE3 block-hash prefix cache aligned with `cgn-kvcached`.
  - CLI: `cgn-infer serve --model <gguf> --host --port --ctx --threads`.
- **Continuous batching.** A scheduler admits multiple sequences per
  forward pass (chunked prefill + batched decode) over a custom
  quantized forward pass with external per-sequence KV and block-based
  preemption.
- **Distributed layer-pipeline inference.** New
  `proto/cognitora/v1/infer.proto` activation-streaming gRPC service,
  coordinator / worker roles (`--role`, `--layers A:B`), workers load
  only their layer slice, f16 activation transport (optional int8),
  mTLS consistent with the rest of the platform. `cgn-agent`
  orchestrates topology from a `[models.*.pipeline]` TOML block;
  workers register non-servable in etcd and the pipeline restarts as a
  unit on member failure.
- **More architectures.** Dispatch on GGUF architecture metadata:
  batched llama / qwen2; sequential qwen3 / gemma3 / phi3 / MoE.
- **Platform integration.** `EngineKind::CgnInfer` (`kind = "cgn_infer"`)
  with `CgnInferEngineConfig` in `cgn-core`, `render_argv` + supervisor
  support in `cgn-agent` (reusing the existing OpenAI HTTP driver and
  `/v1/models` readiness). `kv_offload` is restricted to `none`.
- **Ships in the standard artefacts.** `cgn-infer` is now built into the
  release tarballs, the `install.sh` binary set, and the single
  multi-binary Docker image alongside the original six binaries.
- **Recipe + example.** `recipes/llama3-8b/cgn-infer/single-node/` and
  `examples/local-mac-native/`.
- **Docs.** New `docs/architecture/cgn-infer.md`; engine matrix rows in
  the README and `docs/reference/config.md` (marked preview).

### Notes

- `cgn-infer` is a **preview**: Phase-1 architecture coverage is
  Llama-family GGUF, batching granularity and pipeline decode
  sequentiality carry documented limits inherited from Candle's
  quantized path, and end-to-end generation has not been exercised in
  CI (no model weights available there).

## [0.4.0] - 2026-06-11

The "make the KV layer live" release. The cross-node KV cache design
(tiered store, prefix-overlap routing, peer transfer) existed as
structure but several pipes were not connected: the router's prefix
index was never populated, RAM capacity was tracked but not enforced,
no eviction loop ran, cache stats were hardcoded zeros, and the agent's
`KvHandoff` RPC was a stub. All of these are now wired.

### Added

- **KV-aware routing is live.** The router now records (prefix digest →
  node) in its in-memory `PrefixIndex` after every successful dispatch
  (optimistic insert: the chosen node holds the prefix KV once prefill
  completes), so follow-up requests with shared prefixes route to the
  node that already has the cache. Both nodes of a disaggregated
  prefill/decode pair are recorded. Entries are TTL-bounded and purged
  when a node's etcd lease expires.
- **`cgn-kvcached` background eviction loop** (default 1 Hz):
  - *RAM watermark spill*: when RAM occupancy exceeds
    `kv.ram_high_watermark` (default 0.90), the coldest blocks
    (approximate LRU via touch timestamps) are spilled to SSD in
    batches until under the watermark.
  - *SSD TTL*: blocks not accessed for `kv.ssd_ttl_secs`
    (default 86400, `0` disables) are deleted from disk and the index.
  - New `KvConfig` fields: `ssd_ttl_secs`, `evict_interval_ms`,
    `ram_high_watermark`, all defaulted, so existing configs are unchanged.
- **Real KV cache observability.** The `Kv.Stats` RPC now reports live
  hit / miss / eviction / spill counters and bytes pushed / pulled over
  the QUIC transport (previously hardcoded zeros), plus an accurate
  cold (SSD) block count via a new index scan.
- **`Agent.KvHandoff` implemented.** The agent now bridges handoff
  requests to the host-local `cgn-kvcached` gRPC (`Push`/`Pull` toward
  the peer endpoint) instead of acknowledging and dropping them.
- **`cgn-kv`**: `RamTier` tracks occupancy with an O(1) atomic counter
  (was an O(n) sweep) and exposes `coldest(n)` LRU victim selection;
  `Index::scan` full iteration (RocksDB iterator + in-memory fallback).

### Fixed

- **`Kv.Promote` actually promotes.** A block resident only on SSD is
  now read back into RAM on promote; previously the RPC was a lookup
  no-op.

### Added (MLX / examples)

- **`examples/apple-mlx/download-model.sh`** pre-downloads an MLX-LM model from Hugging Face with a real progress bar so users don't sit silent through `mlx_lm.server`'s lazy first-load.
- **`examples/apple-mlx/demo.sh` and `verify-engine.sh`** now auto-pre-warm the model via `download-model.sh` before hitting the engine. Set `CGN_NO_AUTOPULL=1` to skip when the weights are already cached.

### Fixed

- **`cgn-agent` gRPC server timeout** raised from 120s to 2h so long MLX cold-starts (HF download + first compile) are not cut off mid-`Generate`.
- **Engine subprocess stdio** now **inherits** the agent's stdout/stderr instead of anonymous pipes. Piped stdio with no reader caused `mlx_lm.server` (and other chatty engines) to **block once the pipe buffer filled**, so MLX never bound to `:8090`, `ready` stayed false, and chat hung with no output.
- **`examples/apple-mlx/` model id** corrected from the non-existent `mlx-community/Meta-Llama-3.2-3B-Instruct-4bit` to the real `mlx-community/Llama-3.2-3B-Instruct-4bit` (HF returned 401 for the previous id, blocking pre-download).

## [0.3.1] - 2026-05-08

### Added

- **`engine.kind = "mlx"`**: `cgn-agent` spawns `python3 -m mlx_lm.server` ([mlx-lm](https://github.com/ml-explore/mlx-lm)) on **Apple Silicon** with OpenAI-compatible HTTP. New `[engine.mlx_lm]` config block. Example profile: `examples/apple-mlx/`.

### Fixed

- **`cgn-proto` build script** now reads `CARGO_MANIFEST_DIR` at runtime instead of embedding it with `env!()`, so `protoc` no longer follows a stale absolute path after moving or copying the repository.

## [0.3.0] - 2026-05-07

The "make every plan.md claim runnable end-to-end" release. Five of the
0.3 milestone items shipped over PRs #1, #3, #4, #5, #6: real `cgn-ctl`
control plane, real `/v1/embeddings`, real `cgn-metrics` federation
scraper, single-node installer renderer, soft perf gate in CI, and a
working Kubernetes quickstart. Two follow-up items (Helm chart redesign
and fleshed-out terraform modules) move forward into the 0.3.x patch
window. Versioning bumped from 0.2.1 to 0.3.0 because the public surface
expanded materially (new `/v1/embeddings`, new `/federate`, new
`cgn-ctl install --apply`) and one bug fix (chat completions returning
empty content) is significant enough that pre-0.3 builds should be
treated as broken for chat-template models.

### Fixed
- **`/v1/chat/completions` now returns the model's actual answer
  instead of an empty string.** The agent was sending requests to
  the engine's legacy `/v1/completions` endpoint with a synthesised
  `<role>\n<content>` plain-text prompt, which bypasses the model's
  chat template and produces near-empty output for instruct/chat
  models. The agent now forwards the original `messages` array to
  `/v1/chat/completions` and parses the chat-style SSE
  (`delta.content`) response. The legacy `/v1/completions` plain
  prompt path is preserved as a fallback. Verified end-to-end on
  GKE Autopilot with TinyLlama-1.1B (CPU): sample latency 1.3s
  buffered, streaming SSE deltas working cleanly.

### Added
- **Self-contained Kubernetes quickstart manifest.** New
  `deploy/kubernetes/quickstart/cognitora-cpu.yaml` brings up the
  full Cognitora data plane (etcd + llama.cpp engine + cgn-router +
  cgn-agent + cgn-metrics) in a single Pod with a public
  LoadBalancer, no Helm, no PKI, no operator. Validated on GKE
  Autopilot in ≈ 5 min from `kubectl apply` to a working OpenAI URL.
  See `deploy/kubernetes/quickstart/README.md` and
  `docs/guides/cloud/gcp.md`.
- **`.dockerignore`.** Excludes `target/`, `.git/`, `.temp/`, and
  IDE caches from the docker build context. Cuts the build context
  from ≈ 19 GiB to a few MiB and prevents
  `no space left on device` failures on Docker Desktop.
- **Real `/v1/embeddings`.** `Agent.Embed` is now defined on the proto,
  implemented in `cgn-agent` against the engine's `/v1/embeddings`
  surface, and the router's gateway forwards over gRPC mTLS instead of
  returning synthetic vectors. The handler reuses the same KV-aware
  routing (and cordon-aware filtering) as `/v1/chat/completions`. Empty
  input returns 400; engine 404 (model isn't an embedding model) is
  surfaced as 503 with a clear message.
- **Real `cgn-metrics` federation scraper.** New `[metrics].scrape_targets`
  config field; the scraper fetches every target's `/metrics` body
  on the configured `scrape_interval`, decorates each metric line with
  a `cgn_target = "<name>"` label, and exposes the union under
  `/federate`. Per-target failures increment
  `cgn_metrics_scrape_errors_total`. Five unit tests cover the text
  decorator (HELP/TYPE pass-through, label injection, escape handling,
  blank-line handling, label-less metrics).
- **Single-node installer renderer.** `cgn-ctl install --target
  single-node` now actually generates `cognitora.toml` and
  `compose.yaml` into `--out-dir` (default
  `./cognitora-single-node`). With `--apply` it also runs
  `docker compose up -d`. New flags: `--engine`
  (`vllm`/`sglang`/`llama_cpp`/`openai_compat`), `--hf-repo`, `--tp`,
  `--image`, `--out-dir`, `--apply`. Renderer is pure and unit-tested
  (six tests in `cgn-ctl` covering both file outputs and per-engine
  branches).
- **Soft perf gate workflow.** New `.github/workflows/bench.yml`
  runs `cargo bench -p cgn-perf --bench prefix --bench routing` on
  every PR, uploads the criterion artefacts, and posts a Markdown
  table sticky-comment so reviewers can eyeball regressions. Soft
  by design: the noise floor on shared GitHub runners is ~5–10%, so
  any hard threshold under that is just flake. Hard gating against
  an S3 baseline lands in 0.4.

### Changed
- **Proto:** `EmbedRequest` and `EmbedResponse` moved from
  `router.proto` to `common.proto` so `Agent` and `Router` share the
  same message shape. `EmbedResponse` now carries an optional `model`
  field (set on the agent variant; echoed by the router).
- The router no longer carries the obsolete `embed_via_router_compat`
  extension trait.

## [0.2.1] - 2026-05-07

First release where `cgn-ctl cluster` and `cgn-ctl model` are real
clients instead of placeholders. Also bumps the workspace version
back in line with the git tag history (the v0.2.0 tag shipped from a
0.1.1 source tree; 0.2.1 reunifies them).

### Added
- `cgn-ctl cluster nodes` now reads `/cognitora/nodes/*` from etcd and
  prints a real table of registered agents with role, model, queue
  depth, watts, readiness, and version.
- `cgn-ctl cluster cordon <id>` / `uncordon <id>` writes a flag at
  `/cognitora/cordon/<id>`; the router watcher mirrors it onto
  `NodeEntry.cordoned` and the routing scoring excludes cordoned
  nodes immediately. Inflight requests are not interrupted.
- `cgn-ctl cluster drain <id>` connects to the agent's gRPC endpoint
  (read from its etcd entry) and calls `Agent.Drain`. The agent
  finishes inflight work and exits cleanly.
- `cgn-ctl model load/unload/ls` writes / deletes / lists desired-state
  documents under `/cognitora/models/*`; `ls` shows both desired state
  and live agent reports so operators can see drift.
- New `cgn-ctl -c <path>` global flag for choosing the config file used
  to discover etcd endpoints.
- New `etcd_keys::CORDON = "/cognitora/cordon/"` constant and
  `NodeRegistry::set_cordon` helper.
- `CHANGELOG.md` (this file) and a `## Roadmap` section in `plan.md`.

### Changed
- `cgn-router` now subscribes to the cordon prefix on startup, applies
  any pre-existing cordons from its initial snapshot, and drops them
  on `delete` events.
- Workspace version bumped to `0.2.1` so the published crate versions
  match the git tag.

### Removed
- The `ignored()` placeholder in `cgn-agent::supervisor` and its
  associated `tracing::error` import.

## [0.2.0] - 2026-05-02

### Added
- **SGLang engine support.** Configure with `engine.kind = "sglang"`;
  the agent supervisor renders the right argv shape and the router
  speaks to it over the same OpenAI HTTP surface as vLLM.
- **`engine.kv_offload` knob** with five values (`none | nixl | lmcache
  | hicache | kvbm`). The agent auto-renders the right
  `--kv-transfer-config` JSON for vLLM (with role-aware composition for
  prefill/decode disaggregation) and the right `--enable-hierarchical-cache`
  flag set for SGLang. Invalid pairings (e.g. `hicache` + `vllm`) fail
  fast at startup.
- **Recipes** for one-line bring-up under `recipes/<model>/<engine>/<topology>/`,
  covering Llama 3 8B / 70B, Qwen3 7B, and DeepSeek-V4-Flash, with
  `agg`, `agg-lmcache`, `agg-hicache`, `agg-kvbm`, `disagg`, and
  `disagg-lmcache` topology variants. `cgn-ctl recipe ls/show/up/down`
  drives them.
- **`docs/architecture/kv-strategy.md`**: Cognitora's four-layer KV
  strategy and the engine-side connector matrix.
- **`docs/architecture/vs-dynamo.md`**: detailed comparison with
  NVIDIA Dynamo across 18 concerns.
- **Sequence-chained prefix hashing** (`cgn_core::hash::hash_seq_chunks`)
  and **longest-prefix overlap** (`PrefixIndex::longest_prefix_overlap`)
  so the router scores positional KV reuse correctly. Plain
  per-window hashes are no longer used for routing decisions.
- Recipe integration test (`rust/libraries/cgn-core/tests/recipes.rs`)
  that parses every `recipes/**/*.toml` against the live `Config`
  schema.

### Changed
- README repositioned as "the open-source, datacenter-scale LLM
  inference stack" with a capability matrix, a "When to use Cognitora"
  section, and an expanded comparison-vs-Dynamo table.
- `EngineConfig` now carries `pub kv_offload: KvOffload`. Default is
  `none`, so existing configs keep working.
- `engine::spawn::render_argv` takes a `NodeRoleCfg` so prefill /
  decode workers get the right connector shape in disaggregated mode.

### Fixed
- Release tarballs include all six binaries (`cgn-router`, `cgn-agent`,
  `cgn-kvcached`, `cgn-metrics`, `cgn-ctl`, `cgn-operator`); the 0.1.x
  tarballs shipped only a subset.

## [0.1.1] - 2026-05-01

### Added
- Multi-arch Linux release tarballs (x86_64, aarch64), cosign-signed,
  with sha256 sums attached to every GitHub Release.
- Single multi-binary container image at `ghcr.io/<org>/cognitora`
  (collapsed from the previous six-image matrix).
- Published to crates.io: `cgn-core`, `cgn-router`, `cgn-agent`,
  `cgn-kvcached`, `cgn-ctl`, plus the supporting libraries.
- `examples/docker-ollama/` profile that bridges Cognitora to a local
  Ollama instance over the OpenAI-compat engine.
- Per-crate `README.md` files for every workspace member.

### Changed
- Release workflow drops macOS targets; Linux is the supported
  release target. macOS is fine for development; we just don't ship
  signed binaries for it.

### Fixed
- Router: dropped a handful of unused imports under
  `routing::selector` that triggered `-D warnings`.

## [0.1.0] - initial public release

- All-Rust workspace with six binaries (router, agent, kvcached,
  metrics, ctl, operator).
- vLLM-only engine support over the OpenAI HTTP surface.
- KV-aware routing, RAM/SSD KV tiers, QUIC cross-node transport
  (RDMA gated behind a feature flag).
- Helm chart + bare-metal install script.
- Initial docs tree: ARCHITECTURE, repo layout, routing, KV tiering,
  protocols, OpenAI surface, security model.
