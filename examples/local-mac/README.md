# Local Mac stack (Cognitora + Ollama)

The fastest way to exercise *every* Cognitora feature on a laptop. Uses
**Ollama** as the inference engine — no Python venv, no GGUF download
script, no GPU required.

## What runs

```
       cgn-router (HTTP :8080  / gRPC :9090 / admin :9091)
            │
            │  gRPC mTLS-capable (off by default in this profile)
            ▼
   ┌───────────────────┐         ┌───────────────────┐
   │ cgn-agent phi3    │         │ cgn-agent llama32 │
   │  :7080            │         │  :7081            │
   └─────────┬─────────┘         └─────────┬─────────┘
             │                              │
             └────── HTTP /v1/chat/completions ───────┐
                                                      ▼
                                       Ollama (127.0.0.1:11434)
                                       ├─ phi3:mini
                                       └─ llama3.2:latest

   cgn-kvcached  (UDS /tmp/cognitora-mac-kv.sock, QUIC :7091)
   etcd          (127.0.0.1:2379)
```

Each agent is in `engine.kind = "openai_compat"` mode, so it does **not**
fork a child process — it just proxies to the Ollama HTTP surface. That's
why the same code paths drive a real GPU stack on a server: only the
`[engine]` block changes.

## Prereqs

```bash
brew install jq unzip
ollama serve &                        # if it isn't already running
ollama pull phi3:mini
ollama pull llama3.2
```

## One-shot bring up

```bash
# 1. Build the Cognitora binaries (~2 min cold).
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl

# 2. Install a pinned local etcd into ~/.local/cognitora/etcd.
bash scripts/install/install-etcd.sh

# 3. Bring the stack up.
bash scripts/run/up.sh examples/local-mac
```

`up.sh` will start, in order: `etcd` → `cgn-kvcached` → both `cgn-agent`
processes → `cgn-router`. Each daemon writes its log to
`~/.cache/cognitora/run/<name>.log` and its pid to `<name>.pid`.

Verify everything registered:

```bash
bash scripts/run/status.sh examples/local-mac
```

## Drive it

```bash
bash examples/local-mac/demo.sh
```

This exercises:

| Feature                               | Surface                          |
| ------------------------------------- | -------------------------------- |
| Model listing                         | `GET /v1/models`                 |
| Multi-LLM routing                     | `POST /v1/chat/completions` × 2  |
| Streaming SSE                         | `stream: true` + `curl -N`       |
| Prometheus metrics                    | `GET :9091/metrics`              |
| Rate-limiting middleware              | 5 concurrent requests            |

To exercise mTLS or API-key auth, edit `router.toml` (set `[auth].enabled = true`,
add `api_keys_file = "..."`) — same flags as the production cloud profile.

## Tear down

```bash
bash scripts/run/down.sh examples/local-mac
```

`down.sh` is idempotent: it stops the router, both agents, kvcached, and
etcd in reverse-dependency order.

## Other smoke tests on this stack

```bash
# 1. Plugin layer + middleware (no LLM required, ~30 s).
./tests/e2e/multi_engine.sh

# 2. Multi-node KV transport (real-LLM optional).
./tests/e2e/multi_node_kv.sh

# 3. End-to-end benchmark (latency p50/p95/p99 + tokens/s).
ROUTER=http://127.0.0.1:8080 MODEL=phi3:mini \
  bash examples/multi-llm/bench.sh
```

## Swap in different Ollama models

The router and agents only key off the model name — change the strings in
`router.toml` + the matching `agent-*.toml` to anything `ollama list`
prints, restart, and you're done. No rebuild needed.
