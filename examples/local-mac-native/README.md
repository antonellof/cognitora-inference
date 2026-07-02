# Local Mac stack (Cognitora + cgn-infer, fully native)

Runs the whole platform — router, agent, KV cache, **and the inference
engine** — as first-party Rust binaries from this workspace. No Python,
no Ollama, no llama.cpp install: the agent spawns `cgn-infer serve`
directly on a local GGUF file.

## What runs

```
       cgn-router (HTTP :8080 / gRPC :9090 / admin :9091)
            │
            ▼
   ┌─────────────────────┐
   │ cgn-agent  :7080    │
   └─────────┬───────────┘
             │ spawns + supervises
             ▼
   cgn-infer serve (127.0.0.1:8001, OpenAI HTTP + SSE)
             │
             └─ mmap'd GGUF (llama-3.2-3b-instruct)

   cgn-kvcached  (UDS /tmp/cognitora-mac-native-kv.sock, QUIC :7091)
   etcd          (127.0.0.1:2379)
```

The agent uses `engine.kind = "cgn_infer"` and polls the engine's
`/v1/models` until it is ready, exactly like the other engines.
`engine.kv_offload` must stay `"none"` for cgn-infer.

## Prereqs

```bash
# 1. Build the Cognitora binaries, including the native engine.
cargo build --release -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl -p cgn-infer
export PATH="$PWD/target/release:$PATH"

# 2. Install a pinned local etcd into ~/.local/cognitora/etcd.
bash scripts/install/install-etcd.sh

# 3. Grab a small Llama-3 GGUF.
huggingface-cli download bartowski/Llama-3.2-3B-Instruct-GGUF \
  Llama-3.2-3B-Instruct-Q4_K_M.gguf --local-dir ~/models
export LLAMA_GGUF=~/models/Llama-3.2-3B-Instruct-Q4_K_M.gguf
```

## Bring up

`agent-native.toml` references `${LLAMA_GGUF}`; expand it before starting
(TOML has no native env substitution):

```bash
sed -i '' "s|\${LLAMA_GGUF}|$LLAMA_GGUF|" examples/local-mac-native/agent-native.toml
bash scripts/run/up.sh examples/local-mac-native
bash scripts/run/status.sh examples/local-mac-native
```

## Drive it

```bash
bash examples/local-mac-native/demo.sh
```

Exercises model listing (`/v1/models`), chat completion, SSE streaming,
and Prometheus metrics — all served end-to-end by first-party binaries.

## Tear down

```bash
bash scripts/run/down.sh examples/local-mac-native
```

## Notes

- cgn-infer currently serves requests sequentially (continuous batching
  lands in a later phase), so keep concurrency expectations modest.
- To swap models, point `path` at any Llama-family GGUF and update the
  model name in both `router.toml` and `agent-native.toml`.
