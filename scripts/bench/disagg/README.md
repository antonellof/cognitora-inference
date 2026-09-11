# Disaggregation benchmark harness

Reproducible TTFT / throughput comparison of the two llama3-8b vLLM
topologies:

* **disagg** — [`recipes/llama3-8b/vllm/disagg-single-node`](../../../recipes/llama3-8b/vllm/disagg-single-node):
  prefill and decode as separate `cgn-agent`s, one GPU each, KV blocks
  handed across `cgn-kvcached`.
* **agg** — [`recipes/llama3-8b/vllm/agg`](../../../recipes/llama3-8b/vllm/agg):
  one agent, one GPU, prefill + decode colocated.

The load generator is the existing
[`scripts/bench/bench_client.py`](../bench_client.py): N concurrent
streaming chat requests against `cgn-router`, measuring TTFT (time to
first non-empty `delta.content` SSE chunk — comparable across
topologies), p50/p95/p99, decode tokens/s, and system tokens/s.

The prompt set is produced once per run by
[`workload.py`](workload.py) with a fixed seed and replayed
byte-identically against every mode: by default 60% of prompts share a
~512-token common prefix (where prefix-cache reuse and disaggregated
prefill pay off) and 40% are fully unique (no reuse), interleaved so
every concurrency window contains both kinds.

## Local validation (no GPU)

On a Mac or CI host without GPUs, smoke-test the harness scripts only:

```bash
bash scripts/bench/validate-local.sh
```

This checks workload generation, summarize paths, energy math, and
`cgn-kv-connector` unit tests. It does **not** produce publishable TTFT
numbers — those require the GPU run below.

## Prerequisites (GPU host)

* **2 GPUs** (24 GiB+ each) for the disagg recipe; agg needs 1.
* **vLLM with NIXL support** — the disagg recipe passes
  `--kv-transfer-config` with the NIXL connector, so the vLLM install
  must have the NIXL extra available (`pip install vllm` plus the NIXL
  package for your CUDA version).
* Access to `meta-llama/Meta-Llama-3.1-8B-Instruct` (accepted HF
  license + `HF_TOKEN` exported) or a local checkpoint.
* Rust toolchain (the recipe builds `cgn-router`/`cgn-agent`/
  `cgn-kvcached` release binaries automatically if missing; set
  `CGN_PREBUILT=1` to skip).
* `etcd` — started automatically by the recipe if none is reachable on
  `127.0.0.1:2379` (`scripts/install/install-etcd.sh` installs it).
* `python3`, `curl`.

## Running

One command produces the disagg-vs-agg comparison:

```bash
bash scripts/bench/disagg/run.sh --compare
```

Single topology:

```bash
bash scripts/bench/disagg/run.sh                # disagg only
bash scripts/bench/disagg/run.sh --mode agg     # agg only
```

Knobs (env): `N` (requests, default 32), `CONC` (concurrency, default
8), `MAX_TOKENS` (default 128), `PROMPT_TOKENS` (shared-prefix length
in tokens, default 512 — long prefixes are where disaggregation
matters), `SHARED_FRAC` (fraction of prompts sharing the prefix,
default 0.6), `OUT_DIR`, `MODEL`, `ROUTER_URL`.

Each mode is brought up, loaded, and torn down
(`scripts/run/down.sh <recipe>`) before the next starts, so both runs
see the same idle host.

## Outputs

Written to `scripts/bench/disagg/results/` (override with `OUT_DIR`):

| File             | Contents                                                    |
|------------------|-------------------------------------------------------------|
| `workload.jsonl` | The generated prompt set (identical for all modes)          |
| `results.jsonl`  | Raw bench-client record per scenario                        |
| `results.json`   | Combined records + run metadata (host, knobs, timestamp)    |
| `summary.md`     | Markdown table: TTFT p50/p95, tok/s, disagg-vs-agg deltas   |

There is also a manual-only GitHub Actions workflow
([`.github/workflows/bench-disagg.yml`](../../../.github/workflows/bench-disagg.yml))
that runs this harness on a self-hosted GPU runner via
`workflow_dispatch` and uploads the JSON results as an artifact. It is
never triggered by pushes or PRs.

## Published results

**Pending first GPU run** — no numbers have been recorded yet. This
table is filled in only from an actual `run.sh --compare` on the
reference hardware; do not populate it by hand.

| date | hardware | mode | TTFT p50 (ms) | TTFT p95 (ms) | system tok/s |
|------|----------|------|---------------|---------------|--------------|
| —    | —        | —    | —             | —             | —            |
