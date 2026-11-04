# cgn-agent: Engine supervisor

**Per-GPU-host daemon** that exposes `Agent.Generate` over gRPC, drives a pluggable inference engine, reports **NodeHealth** to etcd, and coordinates KV residency with `cgn-kvcached`.

## Overview

`cgn-agent` sits between `cgn-router` and the engine process. The `Engine` trait abstracts **vLLM**, **SGLang**, **llama.cpp**, and **openai_compat** (proxy-only, no subprocess). It renders engine argv from `[engine.*]` and `[models.*]`, including KV offload connectors selected by `engine.kv_offload`.

## Features

- Spawns or proxies an OpenAI HTTP-compatible engine (`/v1/chat/completions`, `/v1/models`, health)
- Configurable **prefill** / **decode** / **both** roles for disaggregated serving + NIXL handoff
- Heartbeats to etcd on `[agent].heartbeat` (default 5s) with load and capacity signals
- KV offload knob: `none`, `nixl`, `lmcache`, `hicache`, `kvbm`; see [KV strategy](../architecture/kv-strategy.md)
- Admin + Prometheus on `[agent].listen_admin` (see example config)
- NVML-backed metrics for utilization (surfaced on `:9091/metrics` when enabled)

## Architecture

`cgn-router → gRPC :7071 → cgn-agent → Engine → HTTP engine subprocess (or proxy)`.

KV events flow to **`cgn-kvcached`** over the Unix socket configured in your profile (`kv_uds` / shared `[kv]` paths; see examples under [`examples/`](../../examples/)).

## Configuration (highlights)

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `[agent].listen` | string | `0.0.0.0:7070` | Agent gRPC listen address |
| `[agent].role` | enum | `both` | `prefill`, `decode` or `both` |
| `[agent].drain_timeout` | duration | `30s` | In-flight time after SIGTERM |
| `[agent].watt_limit` | f32 | none | Soft power cap published in heartbeats |
| `[engine].kind` | enum | `vllm` | `vllm`, `sglang`, `llama_cpp`, `mlx`, `tensorrt_llm`, `cgn_infer`, `openai_compat` |
| `[engine].url` | string | `http://127.0.0.1:8000` | Engine HTTP base |
| `[engine].kv_offload` | enum | `none` | Engine-side KV connector |

Full engine tables (vLLM/SGLang/llama.cpp flags) live in [Configuration reference](../reference/config.md#engine--pluggable-inference-engine).

## Example

```toml
[cluster]
name = "prod"
etcd_endpoints = ["http://etcd:2379"]

[security]
require_mtls = true

[agent]
listen = "0.0.0.0:7070"

[engine]
kind       = "vllm"
url        = "http://127.0.0.1:8000"
kv_offload = "nixl"

# vLLM treats the model name as the Hugging Face repo id.
[models."meta-llama/Meta-Llama-3-70B-Instruct"]
tp      = 4
```

## Dependencies

- **etcd**: registration and health
- **Inference engine** subprocess or sidecar (unless `openai_compat`)
- **cgn-kvcached** (optional but required for cross-node KV routing signals)

## Related documentation

- [KV strategy](../architecture/kv-strategy.md)
- [Recipes (GPU bring-up)](../guides/recipes.md)
- [Runbook: Agent stuck](../operations/runbooks/agent-stuck.md)

**Source:** [`rust/services/cgn-agent/`](../../rust/services/cgn-agent/)

## Shutdown

On SIGTERM the agent publishes `draining` in its heartbeat so routers stop
sending work, waits up to `drain_timeout` for running generations, then
stops the engine. Set the Kubernetes termination grace period above
`drain_timeout`; the Helm chart uses 45 s.
