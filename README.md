<div align="center">

# Cognitora

**The open distributed inference platform for vLLM.**

KV-aware routing · Prefill/decode disaggregation · GPU/RAM/SSD KV tiering · Tokens/joule optimisation · One-line installer for bare metal, Kubernetes, AWS, GCP, Azure, Hetzner.

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![Status](https://img.shields.io/badge/status-alpha-orange.svg)](#)
[![Domain](https://img.shields.io/badge/cognitora.dev-online-brightgreen.svg)](https://cognitora.dev)

</div>

---

## What is Cognitora?

Cognitora is a low-overhead, open-source orchestration layer that turns one or many vLLM workers into a production-grade inference cluster. Every binary is **a single statically-linked Rust executable** — no Python, Go, or JVM runtime in any container. vLLM itself is left untouched and runs as a child process per node.

```
┌─────────────┐    ┌────────────────────────────────┐    ┌──────────┐
│   Client    │───▶│           cgn-router           │───▶│ cgn-agent│──▶ vLLM
│ OpenAI SDK  │HTTP│ OpenAI-compat + KV-aware route │gRPC│ (Rust)   │HTTP
└─────────────┘    │   gateway · score · cascade    │    └─────┬────┘
                   └───────────────┬────────────────┘          │
                                   │                            ▼
                                   │                    ┌──────────────┐
                                   ▼                    │ cgn-kvcached │
                              ┌──────────┐ UDS          │ GPU/RAM/SSD  │
                              │   etcd   │              └──────┬───────┘
                              └──────────┘                     │ QUIC/RDMA
                                                               ▼
                                                       (cross-node KV)
```

## Why Cognitora?

| Capability                     | Cognitora            | vLLM alone | NVIDIA Dynamo | KServe |
| ------------------------------ | -------------------- | ---------- | ------------- | ------ |
| KV-aware prefix routing        | yes (BLAKE3 trie)    | local only | yes           | basic  |
| Prefill/decode disaggregate    | yes (QUIC handoff)   | no         | yes           | no     |
| GPU/RAM/SSD KV tiering         | yes (RocksDB index)  | host only  | partial       | no     |
| Tokens/joule SLO               | yes (Redfish + IPMI) | no         | no            | no     |
| Single static executable / svc | yes (all Rust)       | n/a        | no            | no     |
| Bare-metal first-class         | yes (systemd units)  | no         | k8s-only      | k8s    |
| Apache-2.0, OSS-only           | yes                  | yes        | yes           | yes    |

## The six binaries

All Rust. Built from one workspace.

| Binary          | Role                                                                              |
| --------------- | --------------------------------------------------------------------------------- |
| `cgn-router`    | OpenAI-compatible HTTP/SSE **and** KV-aware orchestrator (gateway + router)       |
| `cgn-agent`     | Per-node sidecar to vLLM. NVML telemetry, KV handoff, model load                  |
| `cgn-kvcached` | GPU(hot)/RAM(warm)/SSD(cold) KV daemon + QUIC/RDMA cross-node fetch               |
| `cgn-metrics`   | Prometheus aggregator. Derives `cgn_tokens_per_joule` from Redfish/IPMI + DCGM    |
| `cgn-ctl`       | Admin CLI: install / cluster / model / pki / bench / key. Embeds `helm` binary    |
| `cgn-operator`  | Kubernetes operator (kube-rs). CRDs in `deploy/kubernetes/crds/`                  |

## Quick start

### One-liner (bare metal or any of AWS/GCP/Azure/Hetzner)

```bash
curl -fsSL https://get.cognitora.dev | sh -s -- --target single-node --model llama3-8b
```

### From source (Linux, NVIDIA GPU)

```bash
git clone https://github.com/cognitora/cognitora && cd cognitora

cargo build --release --workspace

./target/release/cgn-ctl install --target single-node --model llama3-8b
```

### Kubernetes

```bash
helm install cognitora oci://ghcr.io/cognitora/charts/cognitora \
    --set router.replicas=2 \
    --set models.llama3-70b.tp=4
```

## Repository layout

```
cognitora/
  Cargo.toml                  Rust workspace root
  Makefile                    build entrypoints
  buf.yaml                    proto governance
  rust-toolchain.toml         pinned Rust toolchain (1.79+)

  proto/cognitora/v1/         gRPC source of truth
                              (common · router · agent · kv · control · metrics)

  rust/
    services/                 Binary crates (six)
      cgn-router/             OpenAI gateway + KV-aware router (hot path)
      cgn-agent/              Per-node vLLM sidecar
      cgn-kvcached/           Tiered KV daemon
      cgn-metrics/            Prometheus + tokens/joule
      cgn-ctl/                Admin CLI + installer
      cgn-operator/           Kubernetes operator (kube-rs)

    libraries/                Shared library crates
      cgn-proto/              tonic-generated stubs
      cgn-core/               config, errors, hashing, prefix-tree
      cgn-tls/                rustls / mTLS bootstrap
      cgn-telemetry/          tracing + OTLP + Prometheus
      cgn-kv/                 CUDA / io_uring / RDMA bindings
      cgn-auth/               OIDC + API-key + RBAC
      cgn-ratelimit/          token-bucket + Redis backend
      cgn-k8s/                kube-rs helpers (CRD types, watchers)
      cgn-helm/               wrapper around the helm binary
      cgn-power/              Redfish + IPMI power readers

  deploy/
    docker/                   distroless Dockerfile per binary
    systemd/                  *.service units (single-node / bare metal)
    kubernetes/
      crds/                   InferenceCluster, ModelPool, RoutingPolicy
      helm/cognitora/         Helm chart (values, templates, dashboards)
      operator-manifests/     standalone CRD + RBAC bundle
    terraform/
      modules/                aws/, gcp/, azure/, hetzner/, baremetal/
      examples/               single-node/, multi-node-eks/, …
    installer/                install.sh (cosign-verified one-liner)

  docs/
    ARCHITECTURE.md           top-level architecture
    architecture/             deep dives (protocols, kv-tiering, routing, …)
    guides/                   quickstart, kubernetes, bare-metal, cloud/{aws,gcp,…}
    operations/               observability, SLO, runbooks/
    api/                      OpenAI surface, gRPC surface
    reference/                config reference, env vars, exit codes

  ci/                         pipeline scripts and fixtures
  .github/workflows/          ci.yml, release.yml, e2e.yml

  tests/
    e2e/  integration/  perf/ fixtures/{configs,models}/

  scripts/                    dev/, bench/, release/
  examples/                   single-node/, k8s-multi-node/, bench/
```

## Performance targets (CI gates)

| Metric                                      | Target          |
| ------------------------------------------- | --------------- |
| `cgn-router` routing decision p99           | < 500 µs / vCPU |
| `cgn-router` HTTP overhead vs direct vLLM   | < 3 ms p99      |
| `cgn-kvcached` warm tier hit                | < 200 µs        |
| `cgn-kvcached` cold tier hit                | < 5 ms          |
| Cross-node QUIC fetch (1 MiB block, 10 GbE) | < 12 ms         |
| Cache hit ratio (representative trace)      | ≥ 0.55          |
| Tokens/joule vs round-robin baseline        | ≥ 1.4×          |

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Repo layout](docs/architecture/repo-layout.md)
- [Configuration reference](docs/reference/config.md)
- [Security model](docs/architecture/security.md)
- [Kubernetes guide](docs/guides/kubernetes.md)
- [Bare-metal guide](docs/guides/baremetal.md)

## Status

Alpha (M1–M2 of the [phased rollout](docs/ARCHITECTURE.md#15-phased-rollout)). APIs may change in minor releases until 1.0.

## License

Apache-2.0 — see [LICENSE](LICENSE).
