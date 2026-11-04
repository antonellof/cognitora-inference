# cgn-router: KV-aware request router

**OpenAI-compatible HTTP gateway** with BLAKE3 prefix scoring, per-tenant admission, optional cascade and disaggregated dispatch, and policy loaded from etcd without blocking the hot path.

## Overview

`cgn-router` is the only binary that speaks HTTP to clients. It authenticates (`cgn-auth`), applies rate limits (`cgn-ratelimit`), hashes the prompt into sequence-chained BLAKE3 digests, scores every healthy agent that hosts the requested model, picks the best node, and opens an `Agent.Generate` gRPC stream to `cgn-agent`. Streaming responses are re-encoded as OpenAI SSE chunks.

For background on the score function and etcd keys, read [Routing architecture](../architecture/routing.md) and the [topology overview](../ARCHITECTURE.md#2-hot-path).

## Features

- Four-term routing score: KV prefix overlap, load, power (from metrics), and capacity; weights hot-reloaded from etcd via lock-free `arc_swap`
- Sequence-chained BLAKE3 digests over 32-token chunks of real tokenizer ids, salted per tenant
- Live per-node in-flight counts and session affinity on top of the score
- Per-(model, role) admission with per-tenant wait queues, and TTFT SLO hints
- Retry on another node until the first token; drain on SIGTERM
- Virtual keys: scopes for models, tenants, tokens per minute and budgets
- Optional **cascade** (SLM → larger model on low confidence) and **disaggregated** routing (prefill vs decode pools)
- Cross-cluster federation hooks (forwarding path documented in [Protocols](../architecture/protocols.md))
- API key and OIDC auth; optional mTLS on gRPC paths

## Architecture

`Client → :8080 HTTP → auth → rate limit → scopes + limits → tokenize → admission → prefix digests → score candidates → agent gRPC → first token → SSE stream`.

Admin and Prometheus scrape live on **`listen_admin`** (default `0.0.0.0:9091`): `/healthz`, `/readyz`, `/metrics`. `/readyz` returns 503 while draining, or when discovery is configured and no node is ready.

## Configuration (highlights)

Binary defaults (the [example config](../../configs/cognitora.toml.example) sets some of these differently):

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `[router].listen_http` | string | `0.0.0.0:8080` | OpenAI HTTP API |
| `[router].listen_grpc` | string | `0.0.0.0:9090` | Router gRPC surface |
| `[router].listen_admin` | string | `0.0.0.0:9091` | Health + Prometheus |
| `[router].drain_timeout` | duration | `30s` | In-flight time after SIGTERM |
| `[router].first_token_timeout` | duration | `300s` | Retry elsewhere after this silence |
| `[router].max_body_mb` | u32 | `32` | HTTP body limit |
| `[router].kv_tenant_isolation` | bool | `true` | Salt prefix digests per tenant |
| `[router.score_weights].*` | f32 | kv 0.55, load 0.25, power 0.10, capacity 0.10, session 0.25 | Routing weights |
| `[router.admission].*` | mixed | max_queue 1024, ttft_slo 800ms, wait_queue_per_tenant 64, wait_timeout 10s | Back-pressure |
| `[router.rate_limit].*` | mixed | rps 50, burst 200, no trusted proxies | Per-principal requests per second; always on |
| `[router.cascade].*` | bool / f64 | off by default | Multi-model escalation |
| `[router.disagg].*` | bool / u32 | off by default | Prefill/decode split |

`[auth]`, `[cluster]`, and `[models.<name>]` sections are shared with other binaries; see [Configuration reference](../reference/config.md).

## Example

```toml
[cluster]
name           = "demo"
etcd_endpoints = ["http://127.0.0.1:2379"]

[security]
require_mtls = true

[auth]
enabled       = true
api_keys_file = "/etc/cognitora/keys.txt"

[router]
listen_http  = "0.0.0.0:8080"
listen_grpc  = "0.0.0.0:9090"
listen_admin = "127.0.0.1:9091"

[router.score_weights]
kv       = 0.55
load     = 0.25
power    = 0.10
capacity = 0.10

# vLLM treats the model name as the Hugging Face repo id.
[models."meta-llama/Meta-Llama-3-70B-Instruct"]
tp        = 4
tokenizer = "/models/llama3-70b/tokenizer.json"
```

## Dependencies

- **etcd**: node registry and routing policy
- **cgn-agent**: gRPC streaming backends
- **cgn-metrics** (optional but recommended): power term for scoring

## Operational targets

Latency budgets and behaviour are summarized in [SLOs](../operations/slo.md). Symptom-led debugging: [Runbook: Router down](../operations/runbooks/router-down.md).

## Related documentation

- [Routing architecture](../architecture/routing.md)
- [Security model](../architecture/security.md)
- [OpenAI HTTP surface](../api/openai.md)
- [gRPC API](../api/grpc.md)

**Source:** [`rust/services/cgn-router/`](../../rust/services/cgn-router/)
