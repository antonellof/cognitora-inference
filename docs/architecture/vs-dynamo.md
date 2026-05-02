# Cognitora vs NVIDIA Dynamo

This page is an honest, axis-by-axis comparison between Cognitora and
[NVIDIA Dynamo](https://github.com/ai-dynamo/dynamo) — the closest peer
in the open-source distributed-inference space.

It is **not** a marketing pitch: where Dynamo is ahead, we say so;
where we are ahead, we explain why; where the projects converge, we
say that too. The goal is to help an operator decide which stack
matches their constraints.

For the lower-level "how does Cognitora compose engines, offload
backends, and the cross-cluster index" deep dive, see
[`kv-strategy.md`](kv-strategy.md). For the routing math, see
[`routing.md`](routing.md). For Cognitora's own architecture, see
[`../ARCHITECTURE.md`](../ARCHITECTURE.md).

## TL;DR

* **Same shape.** Both projects are orchestration layers above
  inference engines. Both ship KV-aware routing, disaggregated
  prefill/decode, and multi-tier KV caching. Both are Apache-2.0.
* **Different runtime.** Dynamo is Rust core + Python frontend, designed
  Kubernetes-first with an operator and CRDs. Cognitora is six static
  binaries, designed bare-metal-first with the same artifacts running
  under systemd, Helm, or Terraform.
* **Different engine breadth.** Dynamo treats vLLM / SGLang / TRT-LLM
  as the universe. Cognitora adds llama.cpp and any OpenAI-compatible
  process (Ollama, hosted endpoints, sidecars) as first-class drivers.
* **Different KV story.** Dynamo ships KVBM as the in-house tiered
  block manager. Cognitora doesn't ship its own — it integrates
  LMCache, SGLang HiCache, and KVBM as alternatives behind one TOML
  knob, plus its own `cgn-kvcached` cross-cluster index on top.
* **Pick Dynamo if** you want NVIDIA-aligned reference deployments,
  multimodal / video pipelines, or NVL72 gang scheduling.
* **Pick Cognitora if** you want pure-binary deployment, bare-metal /
  hybrid topologies, energy-aware admission, llama.cpp at the edge,
  or cross-cluster federation.

## Side-by-side: capabilities

The vertical groupings are *what an operator typically asks about*,
not the internal module names of either project.

### Routing

| Capability | Cognitora | Dynamo |
|------------|-----------|--------|
| KV-aware prefix routing | yes | yes |
| Hashing scheme | **Sequence-chained BLAKE3** — each chunk's hash covers all preceding tokens, so identical chunks at different positions never collide | RadixTree of chained block hashes |
| Scoring metric | **Longest-prefix overlap** + `load` + `power` + `capacity`, weights live in etcd, hot-reloaded via `arc_swap` | Overlap + load |
| Power / energy term | yes (Redfish + IPMI + DCGM) | no |
| SLO / deadline propagation | yes (`cgn-router::deadline`) | yes (Planner SLA targets) |
| Admission control | per-(model, role) inflight counters; queue caps; rate limiting | similar |
| Cascade (SLM → Mid → LLM) | yes (logprob gating) | partial |

### Engines

| Engine | Cognitora | Dynamo |
|--------|-----------|--------|
| vLLM | first-class | first-class |
| SGLang | first-class | first-class |
| TensorRT-LLM | thin driver via the same `Engine` trait — community-supported | first-class |
| llama.cpp (CPU + GPU offload) | first-class | not supported |
| OpenAI-compatible (Ollama, hosted, sidecars) | first-class (`engine.kind = "openai_compat"`) | not supported |
| Mixing engines in one cluster | yes (router routes by `model`, not engine) | partial |

### KV cache

This is the area where the projects differ the most. Both stacks
recognise the same four layers; they own and integrate them
differently.

| Layer | Cognitora | Dynamo |
|-------|-----------|--------|
| **L1 — Engine-internal KV** (GPU HBM) | left to engine | left to engine |
| **L2 — Engine-side offload connector** | `none / nixl / lmcache / hicache / kvbm` selectable per recipe via one TOML knob (`engine.kv_offload`); `cgn-agent` auto-renders the right `--kv-transfer-config` JSON or HiCache flags | KVBM (built-in), LMCache, FlexKV — separate launch scripts per backend |
| **L3 — Cross-worker transfer (disagg)** | `NixlConnector`, optionally stacked with LMCache/KVBM via `PdConnector` MultiConnector (auto-composed from `[agent].role`) | `NixlConnector`, optionally with KVBM/LMCache |
| **L4 — Cross-cluster KV-aware routing** | **`cgn-kvcached`** — RAM (DashMap) + SSD (RocksDB-indexed file store) + QUIC peer fetch (federation across clusters) | `kv-router` (single cluster, NATS-based event plane) |
| Engine-internal G1 GPU pool | not owned by Cognitora | KVBM owns G1 |
| Pinned host pool (G2) | RAM tier in cgn-kvcached | KVBM Host Pool |
| NVMe / SSD (G3) | SSD tier in cgn-kvcached (RocksDB index) | KVBM Disk Pool |
| Object / S3 / Mooncake (G4) | passthrough via LMCache or HiCache config | KVBM Remote (NIXL plug-ins) |

The pragmatic upshot: **Dynamo owns the offload data plane via KVBM;
Cognitora delegates that to the engine's preferred connector
(LMCache, HiCache, KVBM) and focuses on the cross-cluster index that
sits above all of them**.

### Disaggregated serving

| Capability | Cognitora | Dynamo |
|------------|-----------|--------|
| Recipe-level prefill/decode split | yes (`vllm/disagg-*`, `sglang/disagg`) | yes (1P1D, 2P2D, …) |
| Auto-rendered KV transfer config | `engine.kv_offload` × `[agent].role` | per-script connector config |
| KV transport | NIXL today; QUIC peer fetch in `cgn-kvcached` | NIXL |
| Variants shipped today | aggregated, disagg, agg+lmcache, agg+kvbm, agg+hicache, disagg+lmcache | aggregated, disagg, agg+kvbm, disagg+kvbm, disagg+kvbm 2p2d, agg+lmcache, disagg+lmcache, agg+flexkv |

### Operator / control plane

| Concern | Cognitora | Dynamo |
|---------|-----------|--------|
| Runtime artefact | Six single-file binaries — no Python control plane, JVM, or operator runtime | Rust core + Python frontend / extensibility |
| Service discovery | etcd (optional) + gossip fallback | etcd or NATS (KV routing requires NATS) |
| Coordination plane | etcd only — `nodes`, `routing/policy` keys | etcd + NATS — KV events, prefix coordination |
| External hard dependencies | etcd | etcd + NATS (when KV routing on) |
| Kubernetes | optional Helm chart (`deploy/kubernetes/helm/cognitora`) | first-class operator + CRDs |
| Bare metal | first-class systemd units (`deploy/systemd/`) | not the focus |
| Cloud Terraform | `deploy/terraform/{aws,gcp,azure,hetzner}` | not shipped |
| Install surface | one curl line, six binaries, no runtime | `pip install ai-dynamo`, container, or operator |
| Runtime artefact | one container image with six binaries; `command:` selects | many Python + Rust components |

### Autoscaling and topology

| Capability | Cognitora | Dynamo |
|------------|-----------|--------|
| SLA / TCO-driven autoscaler | `cgn-operator` + energy-aware admission | Planner |
| Workload simulator | not yet | AIConfigurator (search 10K configs) |
| Topology-aware gang scheduling | basic (cgn-operator + node selectors) | Grove (NVL72-aware) |
| Federation (cross-cluster) | `cgn-router::federation` + `cgn-kvcached` QUIC peer fetch | not shipped |
| Multi-tenancy | OIDC SSO + group → scope mapping; in-process and Redis rate-limit | similar |
| Energy / power telemetry | yes (Redfish + IPMI + DCGM) | no |

### Modalities

| Modality | Cognitora | Dynamo |
|----------|-----------|--------|
| Text LLM | yes | yes |
| Tool calling | yes (passthrough through the engine) | yes (built-in agent toolkit) |
| Multimodal (images, audio) | not yet | yes (E/P/D pipeline + embedding cache) |
| Video generation | not yet | yes (FastVideo, SGLang Diffusion) |
| Speculative decoding | yes (engine-side, passthrough) | yes (engine-side, passthrough) |
| LoRA / adapters | yes (engine-side, passthrough) | yes (engine-side + multi-LoRA scheduling) |

### Fault tolerance

| Capability | Cognitora | Dynamo |
|------------|-----------|--------|
| Health checks (router + agent) | yes | yes |
| In-flight request migration | not yet | yes (canary + migration on worker failure) |
| Drain / cordon | yes (etcd `drain` flag, picked up by router scoring) | yes |
| Idempotent retries | yes | yes |

### Observability

| Concern | Cognitora | Dynamo |
|---------|-----------|--------|
| Prometheus metrics | yes | yes |
| OpenTelemetry traces | yes | yes |
| Per-tier KV metrics | yes (`cgn_kvcached_*`) | yes (KVBM + planner) |
| Power metrics | yes (`cgn_power_watts`) | no |
| LMCache / HiCache passthrough metrics | yes (engine `/metrics` proxied) | yes |

## What we have that Dynamo doesn't

Differentiators where Cognitora is currently ahead:

1. **Engine breadth.** llama.cpp + OpenAI-compatible engines are
   first-class drivers, not adapters. This means the same control
   plane runs on a laptop, a CPU edge box, an NVIDIA H100 cluster,
   and an Ollama-backed dev sandbox.
2. **Bare-metal-first deployment.** `deploy/systemd/` units, a
   one-line installer with cosign-verified release tarballs, and
   Terraform recipes for the four major clouds — without requiring
   Kubernetes.
3. **Pure-binary runtime.** Six static binaries, no Python control
   plane, no JVM, no operator install required. The same artifacts
   work under systemd, Helm, or `kubectl run`.
4. **Sequence-chained KV digests.** Routing scores are computed on
   prefixes that encode position, so two requests with identical chunks
   in different orders never share routing fate.
5. **Cross-cluster federation.** `cgn-router::federation` forwards
   across clusters; `cgn-kvcached` peers across QUIC. Multi-region
   inference doesn't need a Kubernetes-of-Kubernetes.
6. **Energy-aware admission.** `cgn-power` reads Redfish + IPMI +
   DCGM and feeds into the router scoring weight; admission can drain
   nodes that hit thermal or power caps.
7. **Multi-model SLM → LLM cascade.** `cascade::Cascade::run` runs
   the cheap model first and only escalates when the log-probability
   of the cheap answer falls below threshold.
8. **Single TOML knob for KV offload.** `engine.kv_offload` swaps
   `none / nixl / lmcache / hicache / kvbm` without editing the
   engine argv yourself.

## What Dynamo has that we don't yet

Areas where Dynamo is currently ahead:

1. **Multimodal & video pipelines.** Dynamo ships disaggregated
   encode/prefill/decode for images and native FastVideo + SGLang
   Diffusion integration. We are text-only today.
2. **KVBM as a built-in tiered block manager.** Dynamo's KVBM owns
   GPU + Host + Disk + Remote pools natively. We integrate KVBM as
   one of several offload backends, but we don't ship our own
   GPU-tier block pool.
3. **ModelExpress.** GPU-to-GPU weight streaming across NIXL/NVLink
   for fast cold-starts. Cognitora cold-starts use disk-bound
   `from_pretrained` paths today.
4. **Grove gang scheduling.** Topology-aware NVL72 placement. Our
   `cgn-operator` does basic node selection but isn't fabric-aware.
5. **AIConfigurator.** Workload simulator that searches thousands of
   deployment configs to find the optimal one. We have benchmark
   harnesses (`scripts/bench/`) but no auto-search yet.
6. **In-flight request migration.** When a Dynamo worker dies,
   in-flight requests can migrate to a healthy replica. Cognitora
   restarts the agent and retries idempotent requests; mid-stream
   migration is future work.
7. **Zero-config deploy (DGDR).** Specify model + SLA in one YAML and
   Dynamo profiles + plans + deploys. Our equivalent is the
   `recipes/` tree — pre-baked, not generated.
8. **Tool-calling toolkit.** Dynamo ships a NeMo Agent Toolkit
   integration. Cognitora passes tool calls through to the engine but
   adds nothing on top.

## Where the projects converge

These are areas where the two stacks made similar choices and the
operator-visible behaviour is comparable:

* OpenAI-compatible HTTP gateway as the public API.
* gRPC / TCP for internal request fan-out.
* etcd for cluster state.
* `NixlConnector` for prefill→decode KV handoff.
* LMCache support as an engine-side offload backend.
* `<model>/<engine>/<topology>/` recipe layout.
* Apache-2.0 license, OSS-first development model.

## When to pick which

| Situation | Pick |
|-----------|------|
| You're on NVIDIA hardware, Kubernetes-only, and want vendor-aligned reference deployments | **Dynamo** |
| You need multimodal or video pipelines today | **Dynamo** |
| You need NVL72 topology-aware gang scheduling | **Dynamo** |
| You want bare-metal or hybrid (some bare-metal, some cloud) topologies | **Cognitora** |
| You want to mix engines in one cluster (e.g. SGLang for chat, llama.cpp at the edge, OpenAI passthrough for fallback) | **Cognitora** |
| You want a single static binary install with no Python control plane | **Cognitora** |
| You care about energy / power as a routing dimension | **Cognitora** |
| You need cross-cluster federation with KV peer fetch | **Cognitora** |
| You're benchmarking different KV offload strategies (LMCache, HiCache, KVBM) without rewriting deployment YAML | **Cognitora** (one TOML knob) |
| You're operating at NVIDIA InferenceX scale on GB200 / GB300 NVL72 | **Dynamo** (today) |

## Migration / interop

Both stacks consume the same `--kv-transfer-config` JSON shapes for
LMCache, KVBM, and NIXL — Cognitora's auto-renderer was modelled on
the same patterns. A vLLM container that Dynamo can launch in agg or
disagg mode is the same container Cognitora launches with
`engine.kv_offload = "lmcache"` (or `kvbm`).

The recipe layouts are also similar: a Dynamo
`recipes/<model>/<engine>/<topology>/` folder maps to a Cognitora
`recipes/<model>/<engine>/<topology>/` folder of TOML profiles.
Porting between the two is mostly mechanical.

## Roadmap impact

Items on the Cognitora roadmap (`plan.md`) that close the deltas above:

* **Multimodal text+image E/P/D** — track once vLLM and SGLang ship
  stable disaggregated multimodal hooks.
* **Native G1 GPU pool** in `cgn-kvcached` — optional, only if a
  workload doesn't fit any of the L2 backends.
* **WSPT prefill scheduling** — Dynamo's "weighted shortest predicted
  task" admission. The KV-overlap signal needed for it already exists
  in our router; the queue restructure is what's missing.
* **Federated peer-fetch policy** — bound egress when peer-fetching
  from another cluster.
* **Workload simulator** — `cgn-ctl bench plan` that searches the
  recipe matrix.

Items where we deliberately don't intend to converge:

* **No Kubernetes-only path.** Bare metal stays first-class.
* **No Python control plane.** New control-plane logic stays in Rust.
* **No CRD-as-config.** Recipes stay flat TOML.
