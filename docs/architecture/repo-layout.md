# Repository layout

This document defines the canonical folder structure for the Cognitora monorepo, the rules that govern where new code goes, and the conventions every service and library follows.

## Top-level rules

1. **One language: Rust.** All services and libraries live under [rust/](../../rust/). Cognitora is a single Cargo workspace rooted at `Cargo.toml`.
2. **Services vs libraries.** *Binary* crates go under `rust/services/`; *library* crates go under `rust/libraries/`. Anything that produces a long-running process is a service.
3. **Protos are source of truth.** Wire formats live in [proto/cognitora/v1/](../../proto/cognitora/v1/) and are compiled into Rust code by `cgn-proto/build.rs` (`tonic-build`) at compile time.
4. **Deployment artefacts never mix with source.** Helm charts, systemd units, Terraform modules, Dockerfiles, and the installer all live under [deploy/](../../deploy/). Source code never imports from `deploy/`.
5. **Docs are first-class.** Every architectural decision lands in [docs/architecture/](.); every operational procedure lands in [docs/operations/](../operations/); every API surface lands in [docs/api/](../api/).
6. **Gateway and router are one binary.** OpenAI-compatible HTTP/SSE serving is implemented as a module inside `cgn-router`. There is no separate `cgn-gateway` daemon — eliminating an extra hop, an extra TLS context, and an extra failure mode.

## Tree

```
cognitora/
├── Cargo.toml              Rust workspace root
├── Makefile                build entrypoints
├── buf.yaml                proto lint
├── rust-toolchain.toml     pinned Rust toolchain (1.89+)
├── README.md, LICENSE
│
├── proto/cognitora/v1/     gRPC source of truth
│   ├── common.proto
│   ├── router.proto
│   ├── agent.proto
│   ├── kv.proto
│   ├── control.proto
│   └── metrics.proto
│
├── rust/
│   ├── services/           binary crates
│   │   ├── cgn-router/     gateway + router. submodules:
│   │   │                   src/{gateway,routing,cluster,cascade,disagg,admission}
│   │   ├── cgn-agent/      vLLM supervision, NVML, KvHandoff
│   │   ├── cgn-kvcached/   tiered KV daemon (GPU/RAM/SSD)
│   │   ├── cgn-metrics/    Prometheus aggregator + power telemetry
│   │   ├── cgn-ctl/        admin CLI + installer
│   │   └── cgn-operator/   kube-rs operator (CRDs in deploy/kubernetes/crds/)
│   │
│   └── libraries/          shared crates
│       ├── cgn-proto/      tonic-generated stubs (build.rs)
│       ├── cgn-core/       config, errors, hashing, prefix-trie
│       ├── cgn-tls/        rustls helpers, mTLS bootstrap
│       ├── cgn-telemetry/  tracing + OTLP + Prometheus wiring
│       ├── cgn-kv/         CUDA / io_uring / RDMA bindings
│       ├── cgn-auth/       OIDC + API-key + RBAC
│       ├── cgn-ratelimit/  governor + Redis backend
│       ├── cgn-k8s/        kube-rs helpers (CRD types, watchers)
│       ├── cgn-helm/       wrapper around the helm binary
│       └── cgn-power/      Redfish + IPMI power readers
│
├── deploy/
│   ├── docker/             Dockerfile, Dockerfile.agent (distroless / vllm)
│   ├── systemd/            *.service units for bare-metal install
│   ├── kubernetes/
│   │   ├── crds/           inferencecluster, modelpool, routingpolicy
│   │   └── helm/cognitora/ templates/, values.yaml
│   ├── terraform/
│   │   └── {aws,gcp,azure,hetzner,baremetal}/
│   └── installer/install.sh      public https://inference.cognitora.dev/install → sha256 + cosign verified one-liner
│
├── docs/
│   ├── ARCHITECTURE.md
│   ├── architecture/       repo-layout, security, routing, kv-tiering, protocols
│   ├── guides/             quickstart, kubernetes, baremetal, cloud/{aws,gcp,…}
│   ├── operations/         observability, slo, runbooks/
│   ├── api/                openai.md (HTTP), grpc.md (internal)
│   └── reference/          config, env, exit-codes
│
├── configs/                cognitora.toml.example
├── SECURITY/               cosign.pub for release verification
│
├── examples/
│   ├── local-mac/          Mac/Ollama profile (engine.kind = "openai_compat")
│   └── multi-llm/          Linux/server profile (vLLM or llama-cpp-python)
│
├── tests/
│   ├── e2e/                multi_engine.sh (plugin layer + middleware),
│   │                       single_node.sh, multi_node_kv.sh
│   └── perf/               criterion benches (CI perf gates)
│
├── scripts/
│   ├── install/            bootstrap-debian.sh, install-etcd.sh,
│   │                       install-engine-{cpu,gpu}.sh, download-model.sh
│   ├── run/                up.sh / down.sh / status.sh (profile orchestrator)
│   └── release/            pack.sh (build a local tarball matching release.yml)
│
└── .github/workflows/      ci.yml, release.yml, e2e.yml
```

## Conventions

### Rust

- Every crate name is `cgn-<role>`. Library crates expose a single `lib.rs`; binary crates expose `main.rs` and submodules under `src/<feature>/mod.rs`.
- Service crates **must** depend on `cgn-core` (config + errors), `cgn-proto` (wire types), and `cgn-telemetry` (logging + metrics).
- Inter-crate references go through the workspace `[workspace.dependencies]` table — never hard-code a relative path inside a leaf crate.
- All public types are `Debug`. Public types crossing thread boundaries are `Send + Sync` unless explicitly justified.
- `unsafe` is allowed only inside `cgn-kv`; everywhere else `#![forbid(unsafe_code)]` is the default.
- One async runtime: tokio. One TLS stack: rustls. One serialization: serde + bincode for on-the-wire blobs.

### `cgn-router` internal layout

`cgn-router` is the largest crate. Its top-level submodules each have a single responsibility:

| Module        | Responsibility                                                                            |
| ------------- | ----------------------------------------------------------------------------------------- |
| `gateway/`    | Axum HTTP server. OpenAI-compatible `/v1/chat/completions` and `/v1/embeddings`. SSE.     |
| `routing/`    | Score function, candidate selection, prefix-overlap weighting, KV-aware tie-breaking.     |
| `cluster/`    | etcd watcher, gossip, node registry, health.                                              |
| `cascade/`    | SLM → mid → LLM cascade FSM and confidence thresholding.                                  |
| `disagg/`     | Prefill/decode disaggregation: KV handoff handshake with `cgn-agent` and `cgn-kvcached`.  |
| `admission/`  | Token-bucket admission control, queue depth, rejection codes.                             |

Submodules communicate only via well-defined types in `cgn-router::types`; they never share mutable state directly.

### Protos

- All RPCs live under `proto/cognitora/v1/`. Breaking changes require a new package version (`v2`) and a deprecation window.
- `common.proto` holds shared messages; per-service files hold service definitions plus service-specific request/response types.
- `buf lint` and `buf breaking` run in CI. `tonic-build` regenerates Rust stubs whenever `proto/` changes.

### Deployment

- Helm chart values are the canonical surface for cluster configuration. Anything that needs to differ between environments lives in a values override, not in templated logic.
- systemd units enforce `User=cognitora`, `NoNewPrivileges=true`, `ProtectSystem=strict`, `ProtectHome=true`, `MemoryMax=` per role.
- Terraform modules emit a uniform `cluster.json` (or kubeconfig) that `cgn-ctl install` consumes.
- The release pipeline embeds a tested `helm` binary into `cgn-ctl` (via `include_bytes!`) so installs work without external tooling.

### Docs

- One file per concept. Long files are split into `docs/<area>/<topic>/index.md` plus children.
- Every public RPC and every CLI command is documented; the binary's `--help` output is checked against the docs in CI.

## Adding a new component

1. **Service or library?** Decide; place under the right `services/` or `libraries/` subtree.
2. **Add to the workspace.** Append the new crate to the `members` list in `Cargo.toml` and (if it's a library) to `[workspace.dependencies]`.
3. **Add to CI.** The CI matrix walks `rust/{services,libraries}/*` automatically; no hand-edits unless the new component has special requirements (e.g., GPU runners).
4. **Document it.** Add a one-line entry to this file and a dedicated page under `docs/architecture/` if the component introduces new concepts.
