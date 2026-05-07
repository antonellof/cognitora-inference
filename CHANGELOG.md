# Changelog

All notable changes to Cognitora are documented here.

The format is loosely based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Pre-1.0 releases may make small breaking changes between minor versions;
each one is called out under **Breaking** below.

## [Unreleased]

### Added
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

## [0.2.1] — 2026-05-07

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

## [0.2.0] — 2026-05-02

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
- **`docs/architecture/kv-strategy.md`** — Cognitora's four-layer KV
  strategy and the engine-side connector matrix.
- **`docs/architecture/vs-dynamo.md`** — detailed comparison with
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
  `cgn-kvcached`, `cgn-metrics`, `cgn-ctl`, `cgn-operator`) — the 0.1.x
  tarballs shipped only a subset.

## [0.1.1] — 2026-05-01

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
- Release workflow drops macOS targets — Linux is the supported
  release target. macOS is fine for development; we just don't ship
  signed binaries for it.

### Fixed
- Router: dropped a handful of unused imports under
  `routing::selector` that triggered `-D warnings`.

## [0.1.0] — initial public release

- All-Rust workspace with six binaries (router, agent, kvcached,
  metrics, ctl, operator).
- vLLM-only engine support over the OpenAI HTTP surface.
- KV-aware routing, RAM/SSD KV tiers, QUIC cross-node transport
  (RDMA gated behind a feature flag).
- Helm chart + bare-metal install script.
- Initial docs tree: ARCHITECTURE, repo layout, routing, KV tiering,
  protocols, OpenAI surface, security model.
