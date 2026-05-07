# Cognitora — Plan

The high-level architecture lives in
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); the reference config in
[`docs/reference/config.md`](docs/reference/config.md). This file
captures the engineering pillars Cognitora is built on, the
capabilities that ship today, the distribution channels, the explicit
non-goals, and an index into the rest of the docs.

## Pillars (no compromises)

1. **One language, one binary per concern.** All Rust. Six binaries.
   No Python, no Go, no JVM in any production container.
2. **mTLS everywhere by default.** External CA or `cgn-ctl pki
   bootstrap` for dev.
3. **etcd is the only coordination service.** Everything else is RAM
   or local disk.
4. **vLLM is just one engine.** The agent ships a stable `Engine`
   trait; SGLang / TRT-LLM / llama.cpp are tracked from day one.
5. **Performance gated in CI.** Each PR runs the perf harness against
   the targets in the README.
6. **Operator is optional.** Bare-metal first; K8s is just one
   deployment target via the Helm chart and operator.

## Capabilities

| Area                    | What ships                                                          |
|-------------------------|---------------------------------------------------------------------|
| OpenAI HTTP surface     | `cgn-router` exposes `/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, `/v1/models`, SSE streaming |
| KV-aware routing        | `cgn-router::routing::score` with policy hot-reload from etcd via `arc_swap` |
| Multi-node clustering   | etcd-backed `NodeRegistry` with lease-based liveness in `cgn-agent::health` |
| KV tiering              | RAM tier in `cgn-kv::RamTier`; SSD tier in `cgn-kv::SsdTier` (file-per-block, atomic rename); RocksDB index |
| Cross-node KV transport | QUIC `Frame` codec in `cgn-kvcached::transport`; RDMA behind `--features rdma` |
| Prefill/decode disagg   | `routing::pick_pair` + `gateway::chat::run_prefill` two-stage path |
| Cascade                 | `cascade::Cascade::run` orchestrates SLM → Mid → LLM with logprob gating |
| Multi-tenant rate limit | `cgn-ratelimit` ships an in-process governor and a `redis-backend` feature (atomic Lua token bucket) |
| OIDC SSO                | `cgn-auth::oidc` with group-claim → tenant-scope mapping            |
| Operator                | `cgn-operator` reconciles `InferenceCluster`, `ModelPool`, `RoutingPolicy` via server-side apply |
| Federation              | `cgn-router::federation` proxies OpenAI requests to peer routers     |
| Energy-aware autoscaler | `cgn-router::autoscaler` writes drain hints to etcd; operator scales replicas |
| SLO admission           | `cgn-router::deadline` rejects fast when the estimated TTFT exceeds the per-tenant deadline |

## Operating principles

* **Frequent commits / push.** Small, well-scoped commits with
  conventional-commit-style headers. PRs are reviewed against the
  performance budget; performance regressions are rejected.
* **No unsafe code in any service.** `#![forbid(unsafe_code)]` is
  enforced at every binary crate root. Library crates that need it
  (`cgn-kv` for `io_uring`) gate the unsafe code behind a clearly named
  module and an internal RFC.
* **Distroless or bust.** Every release pushes multi-arch
  (amd64/arm64) distroless images to GHCR. The agent image is the
  single exception (sits on top of the official vLLM image).
* **One config tree.** `cgn-core::config::Config` is the single source
  of truth. Every section is documented in
  [`configs/cognitora.toml.example`](configs/cognitora.toml.example).

## Distribution channels

* **Source tarballs** — multi-arch (linux x86_64/aarch64, darwin
  x86_64/aarch64), cosign-signed, sha256-summed, attached to every
  GitHub Release by [`.github/workflows/release.yml`](.github/workflows/release.yml).
* **Container images** — distroless, multi-arch (`linux/amd64`,
  `linux/arm64`), pushed to
  `ghcr.io/<org>/{cgn-router,cgn-kvcached,cgn-metrics,cgn-ctl,cgn-operator}:<tag>`.
* **Helm chart** — published as an OCI artifact at
  `oci://ghcr.io/<org>/charts/cognitora`.
* **One-liner installer** — [`deploy/installer/install.sh`](deploy/installer/install.sh)
  is the canonical bootstrap; it verifies cosign signatures before
  it ever runs a downloaded binary.
* **Package manager channels** (Homebrew tap, `apt`/`dnf` repos for
  `cgn-ctl`) are tracked as a post-GA distribution task.

## Out of scope

The following are explicit non-goals for the 0.x line. They are not
"missing features"; they belong to adjacent products or future tiers.

* **Training and fine-tuning.** Cognitora is an inference platform.
  Use any trainer you like (HF, NeMo, axolotl) and hand us the weights.
* **Model-weight distribution.** We *cache* weights in `cgn-kvcached`
  and on local SSD; we don't host a model registry. Point the agent
  at HuggingFace, S3, GCS, or a private OCI registry.
* **Multi-tenant GPU partitioning beyond MIG passthrough.** We expose
  MIG slices to vLLM through the engine config; MPS, time-slicing,
  and finer-grained virtualisation are deferred.
* **FIPS / confidential computing.** Cryptographic FIPS-140 modules,
  TDX/SEV attestation, and confidential VMs are tracked under a future
  "regulated" tier and are not in the 0.x scope.

## Roadmap

Snapshot view of what we plan to ship next, grouped by milestone.
Items move out of "next" only when they're tracked by an issue and a
PR is in flight. The detailed deltas vs NVIDIA Dynamo live in
[`docs/architecture/vs-dynamo.md`](docs/architecture/vs-dynamo.md);
the engine-side KV strategy lives in
[`docs/architecture/kv-strategy.md`](docs/architecture/kv-strategy.md).

### 0.3 — close the credibility gaps

Targeted at making every `plan.md` capability claim runnable end to
end on bare metal.

* ✓ **`cgn-ctl` is a real client, not a logger.** Shipped in 0.2.1.
  `cluster nodes / cordon / uncordon / drain` and `model load / unload
  / ls` read and write the same etcd key prefixes the router watches.
* ✓ **First-class `/v1/embeddings`.** `Agent.Embed` is on the proto,
  implemented in `cgn-agent` against the engine's `/v1/embeddings`,
  and the router's gateway forwards over gRPC mTLS instead of
  synthesising vectors.
* ✓ **Real `cgn-metrics` federation scraper.** `[metrics].scrape_targets`
  is honoured; the union is exposed under `/federate` with a
  `cgn_target` label injected on every line.
* ✓ **Single-node installer renderer.** `cgn-ctl install --target
  single-node` writes `cognitora.toml` and `compose.yaml` into
  `--out-dir` (defaulting to `./cognitora-single-node`). With
  `--apply` it also runs `docker compose up -d`; without, it stays a
  pure-text dry run for review. Engine, image tag, tensor parallelism,
  and HF repo are CLI flags.
* **Fleshed-out terraform modules.** At least one cloud module
  (`aws` or `hetzner`) drives a runnable end-to-end install; the
  others import from it. Today every cloud module ships as a
  near-empty `main.tf`. (Partial credit: GKE has a verified
  CPU-only quickstart at
  `deploy/kubernetes/quickstart/cognitora-cpu.yaml` that runs the
  full data plane on Autopilot for ~$0.10/hr — see
  `docs/guides/cloud/gcp.md`. The GPU + terraform path is the work
  remaining here.)
* **Helm chart redesign.** Today
  `deploy/kubernetes/helm/cognitora/` requires `[security].require_mtls
  = true` with no PKI bootstrap and has no engine sidecar option, so
  `helm install` alone won't produce a working stack. Goal: optional
  engine sidecar (vLLM / SGLang / llama.cpp / openai-compat),
  `require_mtls = false` default for dev, conditional pki secret
  mount, and an OCI chart push from the release workflow so
  `helm install cognitora oci://ghcr.io/antonellof/charts/cognitora`
  matches the README claim.
* ✓ **Soft perf gate in CI.** A new `bench` workflow runs
  `cargo bench -p cgn-perf --bench prefix --bench routing`, uploads
  the criterion artefacts, and sticks a Markdown table on every PR.
  It's intentionally non-blocking — the noise floor on shared runners
  is too high for hard gating; a hard gate against an S3 baseline
  arrives in 0.4.

### 0.4 — beat Dynamo on the routing path

The `vs-dynamo.md` deltas where we want to lead, not match.

* **WSPT prefill scheduling** in `cgn-router::admission`. The
  longest-prefix-overlap signal already exists; the queue restructure
  (Smith's-rule weighted shortest predicted task) is what's missing.
* **Federated peer-fetch policy.** A new routing knob,
  `[router.federation].prefer_local_cache_hit_over_remote_lmcache`,
  bounds egress when peer-fetching from another cluster.
* **Smarter federation peer scoring.** `federation::pick_peer` admits
  to "first healthy wins"; replace it with geo / RTT / cache-overlap
  weighted scoring.
* **First-class TOML for SGLang HiCache backend.**
  `[engine.sglang].hicache_storage_backend = "nixl|mooncake|nvme|s3"`
  instead of `extra_args` overrides.
* **`kv_offload = "flexkv"`.** Tencent's
  [FlexKV](https://github.com/taco-project/FlexKV) connector. Same
  rendering shape as LMCache; we just need a renderer branch and a
  recipe.
* **Streaming cascade.** Incremental logprob gating on SSE responses;
  today only buffered responses pass through `Cascade::run`.

### 0.5+ — research and infra

* **Hard perf gate in CI.** Fail PRs whose criterion baseline
  regresses > N% on key benches; baseline lives in S3 keyed by
  `main`'s tip.
* **RDMA verbs path.** Finish the `rdma` feature gate so the
  cross-host KV transport has a real ibverbs implementation behind
  it; QUIC stays the default.
* **GPU-backed nightly e2e.** Run `scripts/e2e-gpu.sh` on a schedule
  even when no PR is labelled `run-gpu-e2e`.
* **Helm values schema.** Ship `values.schema.json` so `helm install`
  rejects bad inputs locally; `helm lint` is the only check today.

### Out of scope (not on any milestone)

* **Multimodal text+image E/P/D.** Track once vLLM and SGLang ship
  stable disaggregated multimodal hooks. Until then this is a vendor
  gap, not a roadmap item.
* **Native G1 GPU pool inside `cgn-kvcached`.** L2 backends
  (LMCache / KVBM / HiCache) cover every workload we've benchmarked.
  Only landing a native G1 if a workload genuinely doesn't fit.
* **Kubernetes-only deployment paths.** Bare metal stays first-class.
* **Python control plane.** New control-plane logic stays in Rust.
* **CRD-as-config.** Recipes stay flat TOML, not custom resources.

The non-goals already listed in the [Out of scope](#out-of-scope)
section above (training, model registry, FIPS, MPS time-slicing) still
apply.

## Where the architecture lives

This file is intentionally short. The detailed architecture, protocol,
deployment, and operations content lives in the docs tree:

| Topic                    | Canonical file                                                            |
|--------------------------|---------------------------------------------------------------------------|
| Release notes            | [`CHANGELOG.md`](CHANGELOG.md)                                            |
| One-page architecture    | [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)                            |
| Repo + crate layout      | [`docs/architecture/repo-layout.md`](docs/architecture/repo-layout.md)    |
| Routing + score function | [`docs/architecture/routing.md`](docs/architecture/routing.md)            |
| KV tiering + transports  | [`docs/architecture/kv-tiering.md`](docs/architecture/kv-tiering.md)      |
| Wire protocols           | [`docs/architecture/protocols.md`](docs/architecture/protocols.md), [`docs/api/grpc.md`](docs/api/grpc.md) |
| OpenAI HTTP surface      | [`docs/api/openai.md`](docs/api/openai.md)                                |
| Configuration reference  | [`docs/reference/config.md`](docs/reference/config.md), [`docs/reference/env.md`](docs/reference/env.md) |
| Exit codes               | [`docs/reference/exit-codes.md`](docs/reference/exit-codes.md)            |
| Security model           | [`SECURITY.md`](SECURITY.md), [`docs/architecture/security.md`](docs/architecture/security.md) |
| Observability + alerts   | [`docs/operations/observability.md`](docs/operations/observability.md)    |
| SLOs + perf targets      | [`docs/operations/slo.md`](docs/operations/slo.md)                        |
| Runbooks                 | [`docs/operations/runbooks/`](docs/operations/runbooks/)                  |
| Quickstart               | [`docs/guides/quickstart.md`](docs/guides/quickstart.md)                  |
| Bare-metal install       | [`docs/guides/baremetal.md`](docs/guides/baremetal.md)                    |
| Kubernetes install       | [`docs/guides/kubernetes.md`](docs/guides/kubernetes.md)                  |
| Cloud installs           | [`docs/guides/cloud/`](docs/guides/cloud/)                                |
