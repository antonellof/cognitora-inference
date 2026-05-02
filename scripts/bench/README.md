# Single-host benchmark harness

This directory contains the harness used to produce
[`reports/benchmark-v0.1.1-cpu.md`](../../reports/benchmark-v0.1.1-cpu.md).

It compares latency and throughput of:

* Ollama (direct, on `:11434`)
* `llama-cpp-python` (direct, on `:8001`)
* Cognitora → Ollama (via `cgn-router` on `:8080`)
* Cognitora → `llama-cpp-python` (via `cgn-router` on `:8080`)

across three workload patterns: sequential / streaming / shared-prefix.

> **vLLM is not included** — vLLM requires NVIDIA GPUs and its CPU mode is not
> usable in 0.20.x. **Multi-node and KV-distributed scenarios are not exercised
> by this harness** because they need ≥ 2 nodes; see `§7` of the report for the
> measurement plan.

## Layout

```
scripts/bench/
├── README.md           # this file
├── bench_client.py     # workload generator (TTFT / latency / tok-s / pXX)
├── run_bench.sh        # driver — runs all 12 scenarios and writes JSONL
├── up-bench.sh         # brings up etcd + kvcached + 2 agents + router
├── down-bench.sh       # tears them back down
└── configs/
    ├── router.toml
    ├── agent-ollama.toml
    ├── agent-llamacpp.toml
    └── kvcached.toml
```

## Prerequisites

1. Cognitora binaries on `PATH` (e.g. installed via the `curl | sh` installer).
2. Ollama installed and `qwen2.5:0.5b` pulled.
3. `llama-cpp-python` 0.3.19 installed in a venv, with the Qwen 0.5B Q4_K_M GGUF.
4. `etcd` available on `PATH`.

See `§9` of the report for the exact apt / pip commands.

## Running

```bash
# 1) Start engines (independent of Cognitora):
ollama serve &                        # :11434
. ~/venv/bin/activate
python -m llama_cpp.server \
  --host 127.0.0.1 --port 8001 \
  --model ~/models/qwen2.5-0.5b-instruct-q4_k_m.gguf \
  --model_alias "Qwen/Qwen2.5-0.5B-Instruct" \
  --n_ctx 2048 --n_threads 4 --n_gpu_layers 0 &

# 2) Bring up the Cognitora stack pointed at both engines:
bash scripts/bench/up-bench.sh

# 3) Run the bench (12 scenarios, ~10 minutes on c2-standard-4):
N=20 MAX=64 OUT=bench-results.jsonl bash scripts/bench/run_bench.sh

# 4) Tear it down:
bash scripts/bench/down-bench.sh
```

`bench-results.jsonl` is one JSON record per scenario; the existing report's
raw data is kept under `reports/raw-full.jsonl` and `reports/raw-conc.jsonl`.

## Knobs

* `N`             — requests per scenario (default 20)
* `CONC`          — in-flight concurrency (default 1)
* `MAX`           — `max_tokens` per request (default 64)
* `OUT`           — output JSONL path

## Measurement caveat

The current `bench_client.py` measures TTFT as **time to first SSE `data:`
line**. Cognitora's router emits the SSE preamble immediately on accept; the
honest "time to first content token" needs a smarter parser that waits for
non-empty `choices[0].delta.content`. The report flags this caveat and the
direct-engine TTFTs (which do measure first-content-token) are unaffected.
