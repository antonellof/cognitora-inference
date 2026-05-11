# Apple Silicon — MLX-LM (`engine.kind = "mlx"`)

Run Cognitora with **[mlx-lm](https://github.com/ml-explore/mlx-lm)**'s built-in HTTP server (`python -m mlx_lm.server`). The server exposes an **OpenAI-style** `/v1/chat/completions` and `/v1/models` API on the host/port you configure (see upstream [`mlx_lm/SERVER.md`](https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/SERVER.md)).

**Requirements:** macOS on **Apple Silicon**, Python 3 with `pip install mlx-lm`, and network access (or a local checkout) for the Hugging Face repo you pass as `--model`.

This profile mirrors [`../local-mac/`](../local-mac/README.md) (etcd + kvcached + router + one agent) but the agent **spawns** MLX instead of proxying to Ollama.

## Prerequisites

```bash
brew install jq unzip
pip install mlx-lm
```

Pick a small MLX community model (example below uses `mlx-community/Meta-Llama-3.2-3B-Instruct-4bit`).

## Bring up

```bash
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl

bash scripts/install/install-etcd.sh
bash scripts/run/up.sh examples/apple-mlx
bash examples/apple-mlx/demo.sh
```

Tear down:

```bash
bash scripts/run/down.sh examples/apple-mlx
```

## Ports

| Service | Default bind | Notes |
|---------|--------------|--------|
| `cgn-router` HTTP | `127.0.0.1:8080` | Same as other examples |
| `mlx_lm.server` | `127.0.0.1:8090` | Must match `[engine].url` and `[engine.mlx_lm].port` |

## Configuration

See [`agent-mlx.toml`](agent-mlx.toml): `[engine].kind = "mlx"`, `[engine.mlx_lm]` for the Python binary / `--host` / `--port`, and `[models."…"]` for the Hugging Face id (or set `[models."…"].path` to a local MLX weights directory).

`engine.kv_offload` must stay `none` for MLX (no LMCache / NIXL integration on this path).
