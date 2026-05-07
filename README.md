<div align="center">

# Cognitora

**The open-source, datacenter-scale LLM inference stack.**

Run vLLM, SGLang, TensorRT-LLM, or llama.cpp as a coordinated multi-node cluster — KV-aware, disaggregation-ready, energy-aware — on bare metal, Kubernetes, or any major cloud, installed with a single curl line.

[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)
[![CI](https://img.shields.io/badge/ci-passing-brightgreen.svg)](.github/workflows/ci.yml)
[![Domain](https://img.shields.io/badge/cognitora.dev-online-brightgreen.svg)](https://cognitora.dev)

</div>

---

## What is Cognitora?

Cognitora is the **orchestration layer above inference engines**. It does not replace vLLM, SGLang, TensorRT-LLM, or llama.cpp — it turns them into a coordinated cluster. KV-aware routing, prefill/decode disaggregation, multi-tier KV offload, multi-model cascade, and energy-aware admission all work together to maximize throughput and minimize TTFT for production LLM workloads.

Every Cognitora component is a **single statically-linked Rust binary**. No Python control plane, no JVM, no operator install required. The same artifacts run as systemd units on a single host, as a Helm chart on Kubernetes, or as Terraform-deployed VMs on AWS / GCP / Azure / Hetzner.

## When to use Cognitora

- You serve LLMs across **multiple GPUs or nodes** and want them to behave as one cluster.
- You want **KV-aware routing** so repeated prefixes don't get re-prefilled.
- You need to **independently scale prefill and decode** (disaggregated serving).
- You want **engine choice** — vLLM today, SGLang tomorrow, llama.cpp at the edge — without rewriting the platform.
- You operate **bare-metal or hybrid** infrastructure where a Kubernetes-only stack would be overkill.
- You care about **energy / power budgets** and want them surfaced in routing decisions.

For a single model on a single GPU, the inference engine on its own is usually enough.

## Engine support at a glance

|                                | [vLLM](https://github.com/vllm-project/vllm) | [SGLang](https://github.com/sgl-project/sglang) | [llama.cpp](https://github.com/ggerganov/llama.cpp) | [TensorRT-LLM](https://github.com/NVIDIA/TensorRT-LLM) | OpenAI-compat (Ollama, hosted, …) |
|--------------------------------|:----:|:------:|:---------:|:------------:|:-------:|
| **OpenAI HTTP gateway**        | ✅   | ✅     | ✅        | ✅           | ✅      |
| **KV-aware routing**           | ✅   | ✅     | ✅        | ✅           | ✅      |
| **Prefill/decode disaggregate**| ✅ (NIXL) | ✅ (NIXL) | n/a   | ✅ (NIXL)    | n/a     |
| **KV offload — LMCache**       | ✅   | —      | —         | —            | —       |
| **KV offload — HiCache**       | —    | ✅     | —         | —            | —       |
| **KV offload — KVBM (Dynamo)** | ✅   | —      | —         | 🚧           | —       |
| **Multi-tier KV (RAM / SSD)**  | ✅   | ✅     | ✅        | ✅           | ✅      |
| **Multi-model cascade (SLM→LLM)** | ✅ | ✅   | ✅        | ✅           | ✅      |
| **Energy-aware admission**     | ✅   | ✅     | ✅        | ✅           | ✅      |

> ✅ = ships today. 🚧 = wired through the same TOML knob, awaiting an upstream-supported engine version. — = not applicable for that engine.

## Core capabilities

| Capability | What it does | Why it matters |
|------------|--------------|----------------|
| [**KV-aware routing**](docs/architecture/routing.md) | `cgn-router` scores candidate workers using **sequence-chained BLAKE3 digests** + **longest-prefix overlap**, plus load / power / capacity terms. | Eliminates redundant prefill computation. Positionally correct where naive chunk-overlap mis-scores interleaved prefixes. |
| [**Prefill/decode disaggregation**](docs/architecture/routing.md) | Recipe-level split into prefill replicas and decode replicas with `NixlConnector` handoff. | Each phase runs on hardware tuned for its workload; better GPU utilisation. |
| [**Multi-tier KV cache**](docs/architecture/kv-tiering.md) | `cgn-kvcached` = RAM (DashMap) + SSD (RocksDB-indexed file store) + cross-node QUIC peer fetch. | Extends effective KV capacity; hits across the cluster, not just one host. |
| [**Pluggable KV offload**](docs/architecture/kv-strategy.md) | One TOML knob (`engine.kv_offload`) selects `none / nixl / lmcache / hicache / kvbm`; `cgn-agent` auto-renders the right `--kv-transfer-config` JSON or `--enable-hierarchical-cache` flags. | Pick the best community-maintained KV layer per engine without hand-writing connector blobs. |
| [**Engine-agnostic agent**](docs/reference/config.md) | `cgn-agent` drives any process that speaks the OpenAI HTTP surface (`/v1/chat/completions`, `/v1/models`, `/health`). vLLM / SGLang / llama.cpp / OpenAI-compat ship today. | Same control plane, multiple engines. Mix engines in one cluster. |
| [**Multi-model cascade**](docs/architecture/routing.md) | SLM → Mid → LLM gating on the model's own log-probability ("did the cheap model already get this right?"). | Cuts cost on easy queries while preserving worst-case quality. |
| [**Energy-aware scheduling**](docs/operations/observability.md) | `cgn-power` reads Redfish + IPMI + DCGM/NVML; the routing score has a power term and admission can drain hot nodes. | Lower W-per-token, fewer SLA breaches under thermal stress. |
| [**Cross-cluster federation**](docs/architecture/protocols.md) | `cgn-router::federation` forwards across clusters; `cgn-kvcached` peers across QUIC. | Multi-region inference without Kubernetes-of-Kubernetes. |
| [**One-line install**](deploy/installer/install.sh) | Cosign-verified release tarballs, six binaries dropped into `/usr/local/bin`. | Same artifact bare-metal / VM / container / Kubernetes. |
| [**Recipes**](recipes/README.md) | Flat TOML profiles per `<model>/<engine>/<topology>` plus a 3-line `up.sh`. | Reproducible bring-up of a real model in <30 s. |

<p align="center">
  <img src="docs/architecture.svg" alt="Cognitora architecture: an OpenAI SDK client speaks HTTP to cgn-router; cgn-router routes via gRPC mTLS to cgn-agent, which supervises one inference engine per node (vLLM, SGLang, llama.cpp, TensorRT-LLM, or any OpenAI-compatible server). cgn-router watches etcd for cluster state. cgn-agent talks to a colocated cgn-kvcached over UDS; cgn-kvcached owns the RAM and SSD KV tiers, indexes engine-internal GPU residency, and uses QUIC or RDMA to fetch missing blocks from peer nodes." width="90%" />
</p>

## How Cognitora compares to NVIDIA Dynamo

NVIDIA Dynamo is the closest peer in this space. We agree on most fundamentals (KV-aware routing, disaggregated serving, multi-tier KV) and differ on the runtime, deployment surface, and engine coverage.

| Concern | Cognitora | NVIDIA Dynamo |
|---------|-----------|---------------|
| **Positioning** | Engine-agnostic orchestration above vLLM / SGLang / llama.cpp / TRT-LLM | Engine-agnostic orchestration above vLLM / SGLang / TRT-LLM |
| **Runtime artefact** | Six single-file binaries — no Python control plane, JVM, or operator runtime | Rust core + Python frontend / extensibility layer |
| **First-class engines** | vLLM · SGLang · llama.cpp · OpenAI-compat (TRT-LLM via thin driver) | vLLM · SGLang · TRT-LLM |
| **KV routing signal** | Sequence-chained BLAKE3 digests + longest-prefix overlap (positionally correct) | RadixTree on chained block hashes |
| **KV offload backends** | `none / nixl / lmcache / hicache / kvbm` — selected per recipe via one TOML knob, auto-rendered into the engine argv | KVBM (built-in) + LMCache + FlexKV (separate launch scripts per backend) |
| **Multi-tier KV** | RAM + SSD + cross-cluster QUIC peer fetch (cgn-kvcached) | Full G1–G4 (KVBM owns GPU + Host + SSD + remote pools) |
| **Cross-cluster federation** | QUIC peer fetch + cgn-router federation | Single cluster |
| **Disaggregated prefill/decode** | Recipe-level (`vllm/disagg-*`, NIXL) | Recipe-level (1P1D, 2P2D, NIXL) |
| **SLA-driven autoscaling** | `cgn-operator` + energy-aware admission | Planner (TCO-driven) + AIConfigurator |
| **Multi-model cascade (SLM→LLM)** | First-class (logprob gating) | Partial |
| **Multimodal / video** | Not yet | Yes — image E/P/D, FastVideo, SGLang Diffusion |
| **Topology-aware gang scheduling** | Basic (cgn-operator + node selectors) | Grove (NVL72-aware) |
| **Energy / power telemetry** | Yes — Redfish + IPMI + DCGM | No |
| **Service discovery** | etcd (optional), gossip fallback | etcd or NATS (KV routing requires NATS) |
| **Deployment surfaces** | Bare metal (systemd) · Kubernetes (Helm) · Terraform (AWS / GCP / Azure / Hetzner) — same binaries | Kubernetes-first (operator + CRDs); local dev via container |
| **Install surface** | One curl line, six static binaries, no runtime | `pip install ai-dynamo`, container, or operator |
| **External deps** | etcd (optional) | etcd + NATS (when KV routing on) |
| **License** | Apache-2.0 | Apache-2.0 |

The full deep-dive is in [`docs/architecture/vs-dynamo.md`](docs/architecture/vs-dynamo.md).

What we have that Dynamo doesn't: bare-metal-first deployment with one-curl install · llama.cpp + OpenAI-compat as first-class engines · energy-aware scheduling (Redfish + IPMI + DCGM) · positionally-correct KV digests · cross-cluster QUIC peer fetch · multi-model SLM→LLM cascade · single-binary runtime with no Python control plane.

What Dynamo has that we don't yet: multimodal & video pipelines · ModelExpress GPU-to-GPU weight streaming · Grove NVL72 gang scheduling · AIConfigurator deployment search · in-flight request migration · zero-config DGDR deployment.

## The six binaries

All Rust. Built from one workspace.

| Binary          | Role                                                                                  |
| --------------- | ------------------------------------------------------------------------------------- |
| `cgn-router`    | OpenAI-compatible HTTP/SSE **and** KV-aware orchestrator (gateway + router)           |
| `cgn-agent`     | Per-node engine supervisor — vLLM, llama.cpp, or OpenAI-compatible. NVML telemetry, KV handoff |
| `cgn-kvcached`  | GPU(hot)/RAM(warm)/SSD(cold) KV daemon + QUIC/RDMA cross-node fetch                   |
| `cgn-metrics`   | Prometheus aggregator. Surfaces power telemetry from Redfish/IPMI + DCGM              |
| `cgn-ctl`       | Admin CLI: install / cluster / model / pki / bench / key. Embeds `helm` binary        |
| `cgn-operator`  | Kubernetes operator (kube-rs). CRDs in `deploy/kubernetes/crds/`                      |

## Quick start

### One-liner install (Linux x86_64 + aarch64)

Pulls a signed, sha256-verified release tarball from GitHub and drops the
binaries into `/usr/local/bin` (or `~/.cognitora/bin` if not writable):

```bash
curl -fsSL https://raw.githubusercontent.com/antonellof/cognitora-inference/main/deploy/installer/install.sh | sh
```

Pin a version, choose a custom prefix, or point at a fork:

```bash
curl -fsSL .../install.sh | CGN_VERSION=v0.1.0 sh
curl -fsSL .../install.sh | CGN_PREFIX=$HOME/.local sh
curl -fsSL .../install.sh | CGN_REPO=acme/cognitora-fork sh
```

> Cognitora targets Linux for production deployment (bare metal, Kubernetes,
> cloud VMs). macOS is supported as a development platform via the from-source
> path below.

Then bring up a real LLM in <30 s using a **production recipe** —
one folder per model × engine × topology, each with a 3-line `up.sh`
that wires the full router + agent + KV daemon stack:

```bash
# Llama-3.1 8B on a single GPU with vLLM:
bash recipes/llama3-8b/vllm/agg/up.sh

# Same model with prefill/decode disaggregation on two GPUs:
bash recipes/llama3-8b/vllm/disagg-single-node/up.sh

# Same model on SGLang (RadixAttention prefix cache):
bash recipes/llama3-8b/sglang/agg/up.sh

# Same model with LMCache "prefill-once-reuse-everywhere" KV offload:
bash recipes/llama3-8b/vllm/agg-lmcache/up.sh

# Same model on SGLang with HiCache hierarchical KV:
bash recipes/llama3-8b/sglang/agg-hicache/up.sh

# Llama-3.3 70B FP8 on 4×H100, TP=4:
HF_TOKEN=… bash recipes/llama3-70b/vllm/agg/up.sh

# Equivalent invocation via the admin CLI:
cgn-ctl recipe up llama3-8b/vllm/agg
```

The KV offload backend is a single TOML knob (`engine.kv_offload`)
that auto-renders the right `--kv-transfer-config` JSON or HiCache
flags. See [docs/architecture/kv-strategy.md](docs/architecture/kv-strategy.md)
for the full LMCache / HiCache / KVBM matrix.

Recipes mirror the layout NVIDIA Dynamo uses for its own production
deployments (`<model>/<engine>/<topology>/`) but adapt to Cognitora's
profile-driven runtime: every recipe is a flat folder of TOML — no CRD,
no operator install required. See [`recipes/README.md`](recipes/README.md)
and the [recipes guide](docs/guides/recipes.md).

For lower-level dev loops see
[`examples/multi-llm`](examples/multi-llm) (vLLM/llama-cpp on Linux/GPU) or
[`examples/local-mac`](examples/local-mac) (Ollama-backed on macOS).

### From source

```bash
git clone https://github.com/antonellof/cognitora-inference && cd cognitora-inference

cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached \
  -p cgn-metrics -p cgn-ctl -p cgn-operator

./target/release/cgn-ctl --version
```

### Kubernetes

The fastest way to put Cognitora on a real Kubernetes cluster is the
self-contained CPU quickstart manifest — no Helm, no PKI, no GPU,
public OpenAI-compatible URL on port 80:

```bash
kubectl apply -f deploy/kubernetes/quickstart/cognitora-cpu.yaml
kubectl -n cognitora wait --for=condition=ready pod \
  -l app=cognitora --timeout=10m
IP=$(kubectl -n cognitora get svc cognitora-router \
        -o jsonpath='{.status.loadBalancer.ingress[0].ip}')
curl -sS http://$IP/v1/chat/completions -H 'Content-Type: application/json' \
  -d '{"model":"tinyllama","messages":[{"role":"user","content":"hi"}]}'
```

Verified end-to-end on GKE Autopilot in < 5 minutes; the same manifest
works on EKS / AKS / k3d / kind. See
[`docs/guides/cloud/gcp.md`](docs/guides/cloud/gcp.md) for the full
GKE walkthrough.

For production GPU deployments use the Helm chart:

```bash
helm install cognitora ./deploy/kubernetes/helm/cognitora \
    --namespace cognitora --create-namespace \
    --set router.replicas=2 \
    --set models.llama3-70b.tp=4
```

> The OCI Helm chart at `oci://ghcr.io/antonellof/charts/cognitora`
> isn't published yet; pass the local chart path as above. Tracked in
> [`plan.md`](plan.md) under "Roadmap".

### Releases

Tagged builds are produced by [`.github/workflows/release.yml`](.github/workflows/release.yml)
for every `v*.*.*` tag. Each release ships:

- `cognitora-vX.Y.Z-linux-x86_64.tar.gz` and `cognitora-vX.Y.Z-linux-arm64.tar.gz`
  — each tarball carries **all six binaries** (`cgn-router`, `cgn-agent`,
  `cgn-kvcached`, `cgn-metrics`, `cgn-ctl`, `cgn-operator`).
- `<archive>.sha256` per tarball plus an aggregated `SHA256SUMS` manifest.
- One multi-arch container image: `ghcr.io/antonellof/cognitora:vX.Y.Z`
  (and `:latest`) for `linux/amd64` + `linux/arm64`. The image holds all
  six binaries; pods pick one via `command:` (the Helm chart already does this).

To dry-run the full publish flow locally:

```bash
bash scripts/release/pack.sh v0.0.0-dev
( cd dist && python3 -m http.server 8765 ) &
CGN_BASE_URL=http://127.0.0.1:8765 CGN_VERSION=v0.0.0-dev \
  CGN_PREFIX=/tmp/cgn-test \
  sh deploy/installer/install.sh
```

## Repository layout

```
cognitora/
  Cargo.toml                  Rust workspace root
  Makefile                    build entrypoints
  buf.yaml                    proto governance
  rust-toolchain.toml         pinned Rust toolchain (1.89+)

  proto/cognitora/v1/         gRPC source of truth
                              (common · router · agent · kv · control · metrics)

  rust/
    services/                 Binary crates (six)
      cgn-router/             OpenAI gateway + KV-aware router (hot path)
      cgn-agent/              Per-node engine supervisor (vLLM / llama.cpp / OpenAI-compat)
      cgn-kvcached/           Tiered KV daemon
      cgn-metrics/            Prometheus aggregator + power telemetry
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
    docker/                   distroless Dockerfile (one image, six binaries)
    systemd/                  *.service units (single-node / bare metal)
    kubernetes/
      crds/                   InferenceCluster, ModelPool, RoutingPolicy
      helm/cognitora/         Helm chart (values, templates)
    terraform/
      aws/  gcp/  azure/  hetzner/  baremetal/
    installer/                install.sh (cosign-verified one-liner)

  docs/
    ARCHITECTURE.md           top-level architecture
    architecture/             repo-layout, security, protocols, routing, kv-tiering
    guides/                   quickstart, kubernetes, baremetal, cloud/{aws,gcp,azure,hetzner}
    operations/               observability, slo, runbooks/
    api/                      openai (HTTP), grpc (internal)
    reference/                config, env, exit-codes

  configs/                    cognitora.toml.example
  recipes/                    one-line bring-up profiles per model × engine × topology
                              (llama3-8b/{vllm,sglang,llama-cpp}, llama3-70b/vllm,
                              qwen3-7b/{vllm,sglang}, deepseek-v4-flash/{vllm,sglang};
                              mirrors Dynamo's recipes/ layout)
  SECURITY/                   cosign.pub for release verification
  tests/e2e/                  single_node.sh, multi_node_kv.sh
  scripts/                    e2e-gpu.sh + dev/, bench/, release/
  .github/workflows/          ci.yml, release.yml, e2e.yml
```

## Performance targets (CI gates)

| Metric                                      | Target          |
| ------------------------------------------- | --------------- |
| `cgn-router` routing decision p99           | < 500 µs / vCPU |
| `cgn-router` HTTP overhead vs direct engine | < 3 ms p99      |
| `cgn-kvcached` warm tier hit                | < 200 µs        |
| `cgn-kvcached` cold tier hit                | < 5 ms          |
| Cross-node QUIC fetch (1 MiB block, 10 GbE) | < 12 ms         |
| Cache hit ratio (representative trace)      | ≥ 0.55          |
| Energy efficiency vs round-robin baseline   | ≥ 1.4×          |

## Documentation

**Get started**

- [5-minute quickstart](docs/guides/quickstart.md)
- [Recipes — one-line model bring-up](docs/guides/recipes.md) ([recipes/](recipes/))
- [Bare-metal guide](docs/guides/baremetal.md) · [Kubernetes guide](docs/guides/kubernetes.md)
- [Cloud guides](docs/guides/cloud/) — [AWS](docs/guides/cloud/aws.md) · [GCP](docs/guides/cloud/gcp.md) · [Azure](docs/guides/cloud/azure.md) · [Hetzner](docs/guides/cloud/hetzner.md)

**Architecture**

- [Top-level architecture](docs/ARCHITECTURE.md) · [Repo layout](docs/architecture/repo-layout.md)
- Deep dives: [Routing](docs/architecture/routing.md) · [KV tiering](docs/architecture/kv-tiering.md) · [KV strategy (LMCache, HiCache, KVBM, NIXL)](docs/architecture/kv-strategy.md) · [Protocols](docs/architecture/protocols.md)
- [Security model](docs/architecture/security.md)
- [Cognitora vs NVIDIA Dynamo (deep comparison)](docs/architecture/vs-dynamo.md)

**API**

- [OpenAI HTTP surface](docs/api/openai.md) · [Internal gRPC surface](docs/api/grpc.md)

**Operations**

- [Observability](docs/operations/observability.md) · [SLOs](docs/operations/slo.md) · [Runbooks](docs/operations/runbooks/)

**Reference**

- [Configuration](docs/reference/config.md) · [Environment variables](docs/reference/env.md) · [Exit codes](docs/reference/exit-codes.md)

**Project**

- [Project plan & roadmap](plan.md) · [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md) · [Security policy](SECURITY.md)

## Status

Pre-1.0. The full feature set described above ships today; minor
releases may still adjust the configuration surface and the internal
gRPC API. The OpenAI-compatible HTTP surface is stable.

## License

Apache-2.0 — see [LICENSE](LICENSE).
