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

## Where the architecture lives

This file is intentionally short. The detailed architecture, protocol,
deployment, and operations content lives in the docs tree:

| Topic                    | Canonical file                                                            |
|--------------------------|---------------------------------------------------------------------------|
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
