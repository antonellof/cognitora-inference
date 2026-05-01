# Multi-LLM example

Cognitora's `cgn-agent` is engine-agnostic: each agent independently chooses
between **vLLM** (GPU) and **llama.cpp** (CPU or GPU offload), or proxies to
an externally-managed OpenAI-compatible server (`openai_compat`). The router
routes by the `model` claim each agent advertises in etcd — it doesn't know
or care which engine produced the tokens.

That gives you four supported topologies out of the same TOML profile:

| Topology              | `agent-qwen` engine | `agent-tiny` engine | When to use |
|-----------------------|---------------------|---------------------|-------------|
| **CPU-only**          | `llama_cpp`         | `llama_cpp`         | Laptop, generic VM, ARM box, CI smoke. |
| **GPU-only**          | `vllm`              | `vllm`              | Production single-host serving. |
| **Mixed (heterogeneous)** | `vllm` (GPU)    | `llama_cpp` (CPU)   | Big model on the GPU box, small fallback model on the CPU box, both behind one router. |
| **External engines**  | `openai_compat`     | `openai_compat`     | Engine lifecycle is owned by systemd / Kubernetes / a sidecar. |

```
                  ┌──────────────────────┐
                  │  cgn-router :8080    │ ◄── OpenAI HTTP clients
                  │  (KV-aware routing)  │
                  └────┬───────────┬─────┘
                  gRPC │           │ gRPC
              ┌────────▼───┐   ┌───▼───────────┐
              │ agent-qwen │   │ agent-tiny    │
              │   :7080    │   │    :7081      │
              └─────┬──────┘   └───────┬───────┘
                    │ HTTP             │ HTTP
              ┌─────▼──────┐     ┌─────▼─────────┐
              │ engine #1  │     │ engine #2     │
              │  :8001     │     │   :8002       │
              │  vLLM /    │     │  vLLM /       │
              │  llama.cpp │     │  llama.cpp    │
              └────────────┘     └───────────────┘
```

This directory ships **two flavors** of each agent config:

```
agent-qwen.toml          # llama.cpp engine (CPU)
agent-qwen-vllm.toml     # vLLM engine (GPU)
agent-tiny.toml          # llama.cpp engine (CPU)
agent-tiny-vllm.toml     # vLLM engine (GPU)
```

`scripts/run/up.sh` picks up any file matching `agent-*.toml`, so you swap
topologies by symlinking or copying the files you want into a profile dir.

## CPU host (laptop / generic VM)

```bash
# 1. host packages + rustup
bash scripts/install/bootstrap-debian.sh

# 2. CPU engine (llama.cpp via llama-cpp-python)
bash scripts/install/install-engine-cpu.sh

# 3. local etcd (skip if you already have one)
bash scripts/install/install-etcd.sh

# 4. fetch two small GGUF models
bash scripts/install/download-model.sh \
  --gguf qwen2.5-0.5b-instruct-q4_k_m.gguf  Qwen/Qwen2.5-0.5B-Instruct-GGUF
bash scripts/install/download-model.sh \
  --gguf tinyllama-1.1b-chat-v1.0.Q4_K_M.gguf  TheBloke/TinyLlama-1.1B-Chat-v1.0-GGUF

# 5. build Cognitora (no rocksdb; not needed for a smoke stack)
cargo build --release \
  -p cgn-router -p cgn-agent -p cgn-kvcached \
  --no-default-features

# 6. up the stack
bash scripts/run/up.sh examples/multi-llm

# 7. demo + bench
bash examples/multi-llm/demo.sh
N=60 C=4 bash examples/multi-llm/bench.sh
```

The `up.sh` script tails `~/.cache/cognitora/run/*.log` for each daemon and
prints the registered models from etcd. Stop everything with:

```bash
bash scripts/run/down.sh
```

## GPU host (vLLM)

```bash
bash scripts/install/bootstrap-debian.sh
bash scripts/install/install-engine-gpu.sh        # vLLM venv
bash scripts/install/install-etcd.sh
cargo build --release -p cgn-router -p cgn-agent --no-default-features

# Use the vLLM-flavored agent configs in place of the llama.cpp ones.
mkdir -p /tmp/cgn-profile-gpu
cp examples/multi-llm/router.toml             /tmp/cgn-profile-gpu/
cp examples/multi-llm/agent-qwen-vllm.toml    /tmp/cgn-profile-gpu/agent-qwen.toml
cp examples/multi-llm/agent-tiny-vllm.toml    /tmp/cgn-profile-gpu/agent-tiny.toml
bash scripts/run/up.sh /tmp/cgn-profile-gpu
```

vLLM downloads the weights from HuggingFace on first launch — no
`download-model.sh` step needed.

## Mixed cluster (vLLM + llama.cpp behind one router)

Run a big model on the GPU host and a smaller fallback on a CPU box, both
registered with the same router. Cognitora picks the right agent per
request based on the `model` field in the OpenAI request body.

On the **GPU host**:

```bash
mkdir -p /tmp/cgn-profile
cp examples/multi-llm/router.toml             /tmp/cgn-profile/
cp examples/multi-llm/agent-qwen-vllm.toml    /tmp/cgn-profile/agent-qwen.toml
ETCD_ENDPOINT=10.0.0.10:2379 bash scripts/run/up.sh /tmp/cgn-profile
```

On the **CPU host** (etcd already running on `10.0.0.10`):

```bash
mkdir -p /tmp/cgn-profile
cp examples/multi-llm/agent-tiny.toml /tmp/cgn-profile/
# (no router.toml here — single router on the GPU host)
ETCD_ENDPOINT=10.0.0.10:2379 bash scripts/run/up.sh /tmp/cgn-profile
```

`bash scripts/run/status.sh` on either host will show both agents
registered with their respective engines:

```
agent-qwen   running  …   model=Qwen/Qwen2.5-0.5B-Instruct          ready=true
agent-tiny   running  …   model=TinyLlama/TinyLlama-1.1B-Chat-v1.0  ready=true
```

A single curl from anywhere with line-of-sight to the router will hit the
GPU agent for `Qwen/...` and the CPU agent for `TinyLlama/...`. Different
engines, same client API.

## Externally managed engine (`openai_compat`)

If you already run vLLM, llama.cpp, or another OpenAI-compatible server
under systemd or Kubernetes, point the agent at it without spawning
anything:

```toml
[engine]
kind = "openai_compat"
url  = "http://10.0.5.1:8000"
```

The agent will only proxy; it will not fork a child process.

## What the demo proves

| Feature             | demo.sh step      | bench.sh column                           |
| ------------------- | ----------------- | ----------------------------------------- |
| Multi-model routing | chat() per model  | N/A                                       |
| Streaming SSE       | "STREAMING …"     | `wallclock_s` reflects streamed tokens    |
| Rate limit          | "RATE LIMIT (5)"  | observe 429s when `[router.rate_limit]` is tight |
| Latency / TPS       | timing in chat()  | `latency_s.{mean,p50,p95,p99}` + `tokens.tokens_per_s_overall` |

## Tuning

- `n_threads` / `n_ctx` in `agent-*.toml` → llama.cpp engine sizing.
- `[router.rate_limit] rps,burst` in `router.toml` → request shaping.
- `[router.score_weights] kv,load,power,capacity` → routing decisions.
- `[router.disagg].enabled = true` to test prefill/decode splitting (needs
  multiple agents per model, one with `role = "prefill"` and another with
  `role = "decode"`).

## Reference benchmark numbers

These are the numbers from a live `c2-standard-4` Spot VM in `us-central1`
with the configuration above (CPU only, 2 threads per engine, Q4_K_M):

| Model                 | N=30, C=4    | tokens/s overall |
| --------------------- | ------------ | ---------------- |
| Qwen2.5-0.5B (Q4_K_M) | p50 ≈ 0.6 s  | ≈ 28             |
| TinyLlama-1.1B (Q4_K_M) | p50 ≈ 1.5 s | ≈ 18             |

These are intentionally small models — the exercise is the routing /
streaming / rate-limit pipeline, not raw throughput. Swap to vLLM on a GPU
host for production-grade performance.
