# Llama-3.1-8B · llama.cpp · CPU

CPU-only bring-up. The agent spawns `python -m llama_cpp.server`
backed by a quantized GGUF; useful for laptops, CI, and offline
development without a GPU.

## Prerequisites

```bash
pip install "llama-cpp-python[server]"
```

Then either set `LLAMA_GGUF=/abs/path/to/model.gguf` before running
`up.sh`, or point at a local file inside the recipe by editing
`agent-llama3-8b.toml` (`[models.\"…\"].path`).

A small Llama-3 GGUF is plenty:

```bash
huggingface-cli download QuantFactory/Meta-Llama-3.1-8B-Instruct-GGUF \
  Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf \
  --local-dir ~/models
export LLAMA_GGUF=~/models/Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf
```

## Bring up

```bash
LLAMA_GGUF=~/models/Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf \
  bash recipes/llama3-8b/llama-cpp/cpu/up.sh
```

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-8b/llama-cpp/cpu
```
