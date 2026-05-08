# cgn-agent — Engine supervisor

**Per-GPU-host daemon** that exposes `Agent.Generate` over gRPC, drives a pluggable inference engine, reports **NodeHealth** to etcd, and coordinates KV residency with `cgn-kvcached`.

## Overview

`cgn-agent` sits between `cgn-router` and the engine process. The `Engine` trait abstracts **vLLM**, **SGLang**, **llama.cpp**, and **openai_compat** (proxy-only — no subprocess). It renders engine argv from `[engine.*]` and `[models.*]`, including KV offload connectors selected by `engine.kv_offload`.

## Features

- Spawns or proxies an OpenAI HTTP-compatible engine (`/v1/chat/completions`, `/v1/models`, health)
- Configurable **prefill** / **decode** / **both** roles for disaggregated serving + NIXL handoff
- Heartbeats to etcd on `[agent].heartbeat` (default 5s) with load and capacity signals
- KV offload knob: `none`, `nixl`, `lmcache`, `hicache`, `kvbm` — see [KV strategy](../architecture/kv-strategy.md)
- Admin + Prometheus on `[agent].listen_admin` (see example config)
- NVML-backed metrics for utilization (surfaced on `:9091/metrics` when enabled)

## Architecture

`cgn-router → gRPC :7071 → cgn-agent → Engine → HTTP engine subprocess (or proxy)`.

KV events flow to **`cgn-kvcached`** over the Unix socket configured in your profile (`kv_uds` / shared `[kv]` paths — see examples under [`examples/`](../../examples/)).

## Configuration (highlights)

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `[agent].listen_grpc` | string | `0.0.0.0:7071` | Agent gRPC listen address |
| `[agent].listen_admin` | string | `127.0.0.1:9091` | Admin + `/metrics` |
| `[agent].heartbeat` | duration | `5s` | etcd lease refresh |
| `[agent].ready_probe_url` | string | engine `/v1/models` | Readiness gate |
| `[engine].kind` | enum | `vllm` | `vllm`, `sglang`, `llama_cpp`, `openai_compat` |
| `[engine].url` | string | `http://127.0.0.1:8000` | Engine HTTP base |
| `[engine].kv_offload` | enum | `none` | Engine-side KV connector |

Full engine tables (vLLM/SGLang/llama.cpp flags) live in [Configuration reference](../reference/config.md#engine--pluggable-inference-engine).

## Example

```toml
[cluster]
name = "prod"
etcd = ["http://etcd:2379"]

[security]
require_mtls = true

[agent]
listen_grpc = "0.0.0.0:7071"
listen_admin = "127.0.0.1:9091"
heartbeat = "5s"

[engine]
kind       = "vllm"
url        = "http://127.0.0.1:8000"
kv_offload = "nixl"

[models."llama3-70b"]
hf_repo = "meta-llama/Meta-Llama-3-70B-Instruct"
tp      = 4
```

## Dependencies

- **etcd** — registration and health
- **Inference engine** subprocess or sidecar (unless `openai_compat`)
- **cgn-kvcached** (optional but required for cross-node KV routing signals)

## Related documentation

- [KV strategy](../architecture/kv-strategy.md)
- [Recipes (GPU bring-up)](../guides/recipes.md)
- [Runbook: Agent stuck](../operations/runbooks/agent-stuck.md)

**Source:** [`rust/services/cgn-agent/`](../../rust/services/cgn-agent/)
