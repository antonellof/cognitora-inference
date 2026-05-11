# Quickstart — 5 minutes from zero

This page walks from "I just heard about Cognitora" to "I'm
streaming tokens through it" without committing to a Kubernetes
cluster. We'll run **everything on one host**, with mTLS off and a
fake engine that returns canned tokens — perfect for a demo, a
laptop test, or a CI smoke check.

## Prerequisites

- **Linux** for prebuilt binaries (`x86_64` or `aarch64`). macOS is
  supported as a dev platform via the from-source path below.
- Rust 1.89+ if building from source.
- ~2 minutes to build.

## 1. Install

Pick one of:

```bash
# Option A — prebuilt binaries (Linux x86_64 / aarch64).
# Pulls a sha256-verified release tarball from GitHub. Override CGN_PREFIX
# to install somewhere other than /usr/local/bin or ~/.cognitora/bin.
# inference.cognitora.dev/install redirects to deploy/installer/install.sh on GitHub.
curl -fsSL https://inference.cognitora.dev/install | sh

# Option B — from source (any platform; required on macOS)
git clone https://github.com/antonellof/cognitora-inference cognitora
cd cognitora
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl
```

If you used option B, the binaries are under `target/release/`.
Either prepend that to your `PATH` or use the full path below.

## 2. Bootstrap dev PKI

```bash
cgn-ctl pki bootstrap --out /tmp/pki --san localhost
```

You'll get four PEM files in `/tmp/pki`. We won't enable mTLS for
this run — but the files prove `cgn-ctl pki` works.

## 3. Issue an API key

```bash
mkdir -p /tmp/cognitora
cgn-ctl key create --file /tmp/cognitora/api-keys --scopes "chat,embed"
# prints: cgn-c782d73a8c914c3da49191626f95737e
export CGN_KEY=cgn-c782...   # paste the printed token
```

## 4. Minimal config

```bash
cat > /tmp/cognitora/cognitora.toml <<'EOF'
[cluster]
name     = "demo"
data_dir = "/tmp/cognitora"
etcd     = []                      # single-node, no etcd

[security]
require_mtls = false

[auth]
enabled       = true
api_keys_file = "/tmp/cognitora/api-keys"

[router]
listen_http  = "127.0.0.1:8080"
listen_grpc  = "127.0.0.1:7070"
listen_admin = "127.0.0.1:9091"

[models.demo]
hf_repo = "fake://demo"            # fake engine for the smoke test
EOF
```

## 5. Boot the router

```bash
cgn-router --config /tmp/cognitora/cognitora.toml &
sleep 2
```

You'll see a JSON log line per listener (HTTP on 8080, gRPC on 7070,
admin on 9091).

## 6. Hit it with the OpenAI SDK

```bash
curl -sSfL http://127.0.0.1:8080/v1/models \
  -H "authorization: bearer $CGN_KEY"
# {"object":"list","data":[{"id":"demo","object":"model","owned_by":"cognitora"}]}
```

A streaming chat completion (returns 503 until you wire up an agent
+ engine):

```bash
curl -N -sS http://127.0.0.1:8080/v1/chat/completions \
  -H "authorization: bearer $CGN_KEY" \
  -H "content-type: application/json" \
  -d '{
    "model": "demo",
    "messages": [{"role":"user","content":"Hello"}],
    "stream": true
  }'
```

## 7. Run a real engine (vLLM, llama.cpp, MLX, or proxy)

The fake engine above is fine for proving the gateway/auth/routing path,
but for an actual model end-to-end use one of the bundled engine drivers:

| Engine kind     | When to pick it                                              |
| --------------- | ------------------------------------------------------------ |
| `vllm`          | NVIDIA GPU node. The agent spawns `vllm serve <model> …`.    |
| `sglang`        | NVIDIA GPU node; RadixAttention and SGLang features.          |
| `llama_cpp`     | CPU or GPU offload via `n_gpu_layers` (GGUF on disk).       |
| `mlx`           | **Apple Silicon only.** Agent spawns `python3 -m mlx_lm.server` ([mlx-lm](https://github.com/ml-explore/mlx-lm)). |
| `openai_compat` | The engine is managed by systemd / k8s; the agent only proxies. |

Ready-to-run profiles under [`examples/`](../../examples/):

| Profile                                                | Engine                       | Best for |
|--------------------------------------------------------|------------------------------|----------|
| [`examples/local-mac`](../../examples/local-mac)       | `openai_compat` → Ollama     | macOS laptop; `ollama pull` only — no GGUF build. |
| [`examples/apple-mlx`](../../examples/apple-mlx)        | `mlx` → `mlx_lm.server`      | macOS Apple Silicon; `pip install mlx-lm`. |
| [`examples/multi-llm`](../../examples/multi-llm)       | `vllm` (GPU) or `llama_cpp` (CPU) | Linux box, server, or CI. |

### macOS (Ollama — fastest)

```bash
brew install jq unzip
ollama serve &
ollama pull phi3:mini
ollama pull llama3.2

cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl
bash scripts/install/install-etcd.sh
bash scripts/run/up.sh examples/local-mac
bash examples/local-mac/demo.sh
```

### macOS (MLX — Apple Silicon)

```bash
brew install jq unzip
pip install mlx-lm

cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl
bash scripts/install/install-etcd.sh
bash scripts/run/up.sh examples/apple-mlx
bash examples/apple-mlx/demo.sh
```

### macOS or Linux (llama.cpp + GGUF)

```bash
brew install jq unzip

cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl
bash scripts/install/install-engine-cpu.sh      # builds llama.cpp with Metal on Mac
bash scripts/install/install-etcd.sh
bash scripts/install/download-model.sh \
  --gguf qwen2.5-0.5b-instruct-q4_k_m.gguf  Qwen/Qwen2.5-0.5B-Instruct-GGUF
bash scripts/run/up.sh examples/multi-llm
bash examples/multi-llm/demo.sh
```

### Linux (vLLM or llama.cpp)

```bash
bash scripts/install/bootstrap-debian.sh        # apt + rustup
bash scripts/install/install-engine-cpu.sh      # or install-engine-gpu.sh
bash scripts/install/install-etcd.sh
bash scripts/install/download-model.sh \
  --gguf qwen2.5-0.5b-instruct-q4_k_m.gguf  Qwen/Qwen2.5-0.5B-Instruct-GGUF
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl
bash scripts/run/up.sh examples/multi-llm
bash examples/multi-llm/demo.sh
```

The same TOML profile boots a vLLM stack on a GPU host — only the
`[engine]` block in `agent-*.toml` changes.

## 8. Run the smoke tests

These tests need only the binaries and Python 3 — no models, no GPUs:

```bash
# Engine-plugin layer + auth + rate-limit middleware. ~3 s.
./tests/e2e/multi_engine.sh

# Multi-node KV transport. Skips with REQUIRE_MULTINODE=0 if the second
# host isn't available; runs full QUIC handoff when it is.
./tests/e2e/multi_node_kv.sh
```

For a tighter dev loop, drop `CGN_SKIP_BUILD=1` in front of the test
once your `target/release` is warm.

## 9. What's next

- **Understand the route** → [routing deep dive](../architecture/routing.md).
- **Cluster of one becomes cluster of N** → point `etcd_endpoints` at a
  real etcd, start more agents on more hosts, watch them register.
- **Skim the OpenAI surface** → [API reference](../api/openai.md).
- **Production install** → [bare-metal guide](baremetal.md) or
  [Kubernetes guide](kubernetes.md).

## 10. Tear down

```bash
kill %1                                # the backgrounded router
rm -rf /tmp/cognitora /tmp/pki

# or, if you used scripts/run/up.sh with a profile:
bash scripts/run/down.sh examples/local-mac      # or examples/multi-llm / examples/apple-mlx
```
