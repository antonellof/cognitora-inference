# Apple Silicon — MLX-LM (`engine.kind = "mlx"`)

Run Cognitora with **[mlx-lm](https://github.com/ml-explore/mlx-lm)**'s built-in HTTP server (`python -m mlx_lm.server`). The server exposes an **OpenAI-style** `/v1/chat/completions` and `/v1/models` API on the host/port you configure (see upstream [`mlx_lm/SERVER.md`](https://github.com/ml-explore/mlx-lm/blob/main/mlx_lm/SERVER.md)).

**Requirements:** macOS on **Apple Silicon**, Python 3 with `pip install mlx-lm`, and network access (or a local checkout) for the Hugging Face repo you pass as `--model`.

**Important:** `GET http://127.0.0.1:8080/v1/models` on the **router** is built from **static config + etcd registrations** — it does **not** call MLX. A 200 there does **not** prove `mlx_lm.server` is healthy. Always verify **`http://127.0.0.1:8090`** (engine port) or run `bash examples/apple-mlx/verify-engine.sh`.

This profile mirrors [`../local-mac/`](../local-mac/README.md) (etcd + kvcached + router + one agent) but the agent **spawns** MLX instead of proxying to Ollama.

## Prerequisites

```bash
brew install jq unzip
pip install mlx-lm
```

Pick a small MLX community model. The example default is `mlx-community/Llama-3.2-3B-Instruct-4bit` (~1.8 GB). For a faster first smoke test use the 1B variant (~600 MB):

```text
mlx-community/Llama-3.2-1B-Instruct-4bit
```

## End-to-end (recommended)

The bundled scripts pre-download the weights for you (with a progress bar) before they touch the engine, so you don't sit through a silent multi-GiB pull on the first chat:

```bash
# 1. Build the Rust binaries (once)
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl

# 2. Local etcd (once per machine)
bash scripts/install/install-etcd.sh

# 3. Bring up etcd + kvcached + agent + router
bash scripts/run/up.sh examples/apple-mlx

# 4. Pre-warm + smoke-test (downloads model with progress, then chats)
bash examples/apple-mlx/demo.sh

# Optional: poke the engine on :8090 directly
bash examples/apple-mlx/verify-engine.sh
```

Both `demo.sh` and `verify-engine.sh` shell out to `download-model.sh` first (idempotent — instant if the weights are already cached). To skip that pre-warm because you know the model is cached, set `CGN_NO_AUTOPULL=1`:

```bash
CGN_NO_AUTOPULL=1 bash examples/apple-mlx/demo.sh
```

To use a smaller / different model:

```bash
MODEL=mlx-community/Llama-3.2-1B-Instruct-4bit bash examples/apple-mlx/demo.sh
```

Then update the `[models."…"]` keys in [`agent-mlx.toml`](agent-mlx.toml) and [`router.toml`](router.toml) to match `MODEL` and restart with `down.sh` + `up.sh`.

## Manual: pre-download only

```bash
bash examples/apple-mlx/download-model.sh                                            # default 3B
bash examples/apple-mlx/download-model.sh mlx-community/Llama-3.2-1B-Instruct-4bit   # tiny
HF_TOKEN=... bash examples/apple-mlx/download-model.sh <gated-repo>                  # gated
```

The helper picks `hf download` (new HF CLI), falls back to legacy `huggingface-cli`, then to `python3 -c "snapshot_download(...)"`. All three render real tqdm progress bars.

## Tear down

```bash
bash scripts/run/down.sh examples/apple-mlx
```

## Troubleshooting

**`GET :8080/v1/models` works but chat never returns**

That only proves the **router** knows the model name. Confirm MLX is listening and answering on **8090**:

```bash
bash examples/apple-mlx/verify-engine.sh
```

**`/v1/models` returns `{"data": []}` on `:8090`**

Expected. `mlx_lm.server`'s `/v1/models` only lists repos already in the HF cache that look like MLX models, **plus** the CLI `--model` only when it is a **local path**. With an HF id (`mlx-community/...`) and a cold cache, the list is empty. This does **not** mean chat is broken.

**`curl` / `demo.sh` hangs on chat**

`mlx_lm.server` **lazily loads** the model on the first request: HF download → weight load → first compile. That phase is **silent** (no `data:` SSE lines) and can last **5–15 min** on a fresh cache. Pre-warm with the helper (shows a real progress bar):

```bash
bash examples/apple-mlx/download-model.sh
# or:
bash examples/apple-mlx/download-model.sh mlx-community/Llama-3.2-1B-Instruct-4bit
```

The helper uses `huggingface-cli download` (or `snapshot_download` as a fallback) so you see per-file MB/s progress instead of a silent terminal.

Watch what mlx-lm is actually doing — add `--log-level=DEBUG` to `extra_args`:

```toml
[engine.mlx_lm]
binary     = "python3"
host       = "127.0.0.1"
port       = 8090
extra_args = ["--log-level", "DEBUG"]
```

Then:

```bash
tail -f ~/.cache/cognitora/run/agent-mlx.log
```

**Do not press Ctrl+Z to abort `curl` / `demo.sh`** — that *suspends* bash; the request keeps running in the background but you see nothing. Use **Ctrl+C**.

Probe MLX directly (bypasses the router):

```bash
curl -fsS -m 10 http://127.0.0.1:8090/v1/models | jq .
```

To see tokens as they arrive (after weights are loaded):

```bash
curl -N --max-time 1800 -sS http://127.0.0.1:8080/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{"model":"mlx-community/Llama-3.2-3B-Instruct-4bit","stream":true,"messages":[{"role":"user","content":"Hi"}],"max_tokens":32,"temperature":0}' | head -c 2000
```

**`ready=False`**

- **Forever, nothing on `:8090`:** older builds spawned the engine with **piped** stdout/stderr and never read them; `mlx_lm.server` then **blocked** once the pipe buffer filled. Fixed by inheriting stdio (logs go to `agent-mlx.log`). Rebuild `cgn-agent`, then `down` + `up`.
- **Right after `up` only:** often a short race while MLX binds. Re-run `bash scripts/run/status.sh examples/apple-mlx` after a minute or watch `agent-mlx.log`.

**`ModuleNotFoundError: mlx_lm` in `agent-mlx.log`**

`cgn-agent` runs `python3` from the environment that was active when you started `up.sh`. Install into that interpreter, or set an absolute path in [`agent-mlx.toml`](agent-mlx.toml) under `[engine.mlx_lm]`:

```toml
binary = "/full/path/to/python3"
```

## Ports

| Service | Default bind | Notes |
|---------|--------------|--------|
| `cgn-router` HTTP | `127.0.0.1:8080` | Same as other examples |
| `mlx_lm.server` | `127.0.0.1:8090` | Must match `[engine].url` and `[engine.mlx_lm].port` |

## Configuration

See [`agent-mlx.toml`](agent-mlx.toml): `[engine].kind = "mlx"`, `[engine.mlx_lm]` for the Python binary / `--host` / `--port`, and `[models."…"]` for the Hugging Face id (or set `[models."…"].path` to a local MLX weights directory).

`engine.kv_offload` must stay `none` for MLX (no LMCache / NIXL integration on this path).
