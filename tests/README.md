# Tests

Three layers of tests live in this tree:

```
tests/
  unit/            # fast Rust unit tests (`cargo test`)
  integration/     # cross-crate Rust tests (still `cargo test`)
  e2e/             # bash-driven end-to-end against running daemons
  perf/            # criterion benchmarks (`cargo bench`)
```

Most of the day-to-day signal comes from `cargo test --workspace --no-default-features`
(unit + integration). The e2e suite is what you reach for when proving
the binary behaves correctly under realistic process layouts.

## End-to-end smoke (no GPU, no model)

These three scripts only need the release binaries and Python 3 — they
spin up real daemons against stub engines.

| Script                          | What it covers                                                          | Time  |
|---------------------------------|-------------------------------------------------------------------------|-------|
| [`e2e/multi_engine.sh`](e2e/multi_engine.sh)   | Plugin layer (vllm/llama_cpp/openai_compat argv), auth (401/200), rate-limit (429), agent registration. | ~3 s |
| [`e2e/single_node.sh`](e2e/single_node.sh)     | Full single-host bring-up: router + agent + fake engine, OpenAI surface, gRPC handoff. | ~10 s |
| [`e2e/multi_node_kv.sh`](e2e/multi_node_kv.sh) | Cross-node KV transport (QUIC handoff), prefill/decode disagg path. Skips when no second host (`REQUIRE_MULTINODE=0`). | ~15 s |

Run any of them from the repo root:

```bash
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-ctl
./tests/e2e/multi_engine.sh
```

For a tight dev loop, set `CGN_SKIP_BUILD=1` to skip the workspace
re-build between runs (the script will warn if a binary is missing).

## End-to-end with a real LLM

The e2e scripts above use a stub Python HTTP server pretending to be an
OpenAI engine — fast, deterministic, no model weights. To exercise the
same code paths against an *actual* engine, use one of the runnable
profiles in [`examples/`](../examples/):

```bash
# macOS, Ollama-backed (no Python venv, no GGUF download).
bash scripts/run/up.sh examples/local-mac
bash examples/local-mac/demo.sh

# Linux/server, vLLM (GPU) or llama-cpp-python (CPU).
bash scripts/run/up.sh examples/multi-llm
bash examples/multi-llm/demo.sh
bash examples/multi-llm/bench.sh        # latency p50/p95/p99 + tps
```

## Perf

```bash
cargo bench --bench routing_bench       # router scoring micro-bench
cargo bench --bench prefix_cache        # BLAKE3 prefix-trie throughput
```

The criterion HTML reports land under `target/criterion/`.
