# cgn-router — KV-aware request router

**OpenAI-compatible HTTP gateway** with BLAKE3 prefix scoring, per-tenant admission, optional cascade and disaggregated dispatch, and policy loaded from etcd without blocking the hot path.

## Overview

`cgn-router` is the only binary that speaks HTTP to clients. It authenticates (`cgn-auth`), applies rate limits (`cgn-ratelimit`), hashes the prompt into sequence-chained BLAKE3 digests, scores every healthy agent that hosts the requested model, picks the best node, and opens an `Agent.Generate` gRPC stream to `cgn-agent`. Streaming responses are re-encoded as OpenAI SSE chunks.

For background on the score function and etcd keys, read [Routing architecture](../architecture/routing.md) and the [topology overview](../ARCHITECTURE.md#2-hot-path).

## Features

- Four-term routing score: KV prefix overlap, load, power (from metrics), and capacity; weights hot-reloaded from etcd via lock-free `arc_swap`
- Sequence-chained BLAKE3 digests over ~64-token blocks (positional correctness for interleaved prefixes)
- Per-(model, role) admission with configurable queue caps and TTFT SLO hints
- Optional **cascade** (SLM → larger model on low confidence) and **disaggregated** routing (prefill vs decode pools)
- Cross-cluster federation hooks (forwarding path documented in [Protocols](../architecture/protocols.md))
- API key and OIDC auth; optional mTLS on gRPC admin paths

## Architecture

`Client → :8080 HTTP → auth → rate limit → BLAKE3 prefix hash → score candidates → pick agent → gRPC :7070 (mTLS) → cgn-agent → SSE stream`.

Admin and Prometheus scrape live on **`listen_admin`** (default `127.0.0.1:9091`): `/healthz`, `/readyz`, `/metrics`.

## Configuration (highlights)

Defaults from [`configs/cognitora.toml.example`](../../configs/cognitora.toml.example):

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `[router].listen_http` | string | `0.0.0.0:8080` | OpenAI HTTP API |
| `[router].listen_grpc` | string | `0.0.0.0:7070` | Internal RPC to agents |
| `[router].listen_admin` | string | `127.0.0.1:9091` | Health + Prometheus |
| `[router.score_weights].*` | f64 | kv 0.55, load 0.25, power 0.10, capacity 0.10 | Routing weights |
| `[router.admission].*` | u32 / ms | max_queue, ttft_slo_ms, queue_timeout_ms | Back-pressure |
| `[router.rate_limit].*` | bool / u32 | disabled by default | Per-subject RPS |
| `[router.cascade].*` | bool / f64 | off by default | Multi-model escalation |
| `[router.disagg].*` | bool / u32 | off by default | Prefill/decode split |

`[auth]`, `[cluster]`, and `[models.<name>]` sections are shared with other binaries; see [Configuration reference](../reference/config.md).

## Example

```toml
[cluster]
name     = "demo"
data_dir = "/var/lib/cognitora"
etcd     = ["127.0.0.1:2379"]

[security]
require_mtls = true

[auth]
enabled       = true
api_keys_file = "/etc/cognitora/keys.txt"

[router]
listen_http  = "0.0.0.0:8080"
listen_grpc  = "0.0.0.0:7070"
listen_admin = "127.0.0.1:9091"

[router.score_weights]
kv       = 0.55
load     = 0.25
power    = 0.10
capacity = 0.10

[models."llama3-70b"]
hf_repo = "meta-llama/Meta-Llama-3-70B-Instruct"
tp      = 4
```

## Dependencies

- **etcd** — node registry and routing policy
- **cgn-agent** — gRPC streaming backends
- **cgn-metrics** (optional but recommended) — power term for scoring

## Operational targets

Latency budgets and behaviour are summarized in [SLOs](../operations/slo.md). Symptom-led debugging: [Runbook: Router down](../operations/runbooks/router-down.md).

## Related documentation

- [Routing architecture](../architecture/routing.md)
- [Security model](../architecture/security.md)
- [OpenAI HTTP surface](../api/openai.md)
- [gRPC API](../api/grpc.md)

**Source:** [`rust/services/cgn-router/`](../../rust/services/cgn-router/)
