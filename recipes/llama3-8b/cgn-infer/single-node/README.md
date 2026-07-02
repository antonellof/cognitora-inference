# Llama-3.1-8B · cgn-infer · single node

Single-node bring-up on **cgn-infer**, Cognitora's first-party native
inference engine. The agent spawns `cgn-infer serve --model <gguf>`;
no Python, vLLM, or llama.cpp install required — everything is a Rust
binary from this workspace.

## Prerequisites

Build the engine (once):

```bash
cargo build --release -p cgn-infer
export PATH="$PWD/target/release:$PATH"
```

Then either set `LLAMA_GGUF=/abs/path/to/model.gguf` before running
`up.sh`, or point at a local file inside the recipe by editing
`agent.toml` (`[models.\"…\"].path`).

```bash
huggingface-cli download QuantFactory/Meta-Llama-3.1-8B-Instruct-GGUF \
  Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf \
  --local-dir ~/models
export LLAMA_GGUF=~/models/Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf
```

## Bring up

```bash
LLAMA_GGUF=~/models/Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf \
  bash recipes/llama3-8b/cgn-infer/single-node/up.sh
```

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-8b/cgn-infer/single-node
```

## Notes

- `cgn-infer` speaks the same OpenAI HTTP surface as the llama.cpp
  server (`/v1/chat/completions`, `/v1/completions`, `/v1/models`,
  `/healthz`), so the router and agent treat it like any other engine.
- `engine.kv_offload` must stay `"none"` for this engine (the default).
