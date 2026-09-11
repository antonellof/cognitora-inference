# Benchmarks

Reproducible harnesses for TTFT/throughput and energy efficiency.
**Publishable numbers require a Linux GPU host** — the scripts below
validate on Mac/CI without GPUs via fixture data only.

## Local validation (Mac / CI, no GPU)

```bash
bash scripts/bench/validate-local.sh
```

Checks workload generation, summarize math, energy formulas, and
`cgn-kv-connector` unit tests. CI runs this on every push (`bench-validate`
job in `.github/workflows/ci.yml`).

## GPU host runbook

### Prerequisites

* Linux host with 1–2 NVIDIA GPUs (24 GiB+ each for llama3-8b recipes)
* vLLM with NIXL support (disagg recipes)
* `python3`, `curl`, etcd (embedded by recipes if absent)
* Cognitora binaries on `PATH` or built by recipe `up.sh`

### Disaggregation TTFT (disagg vs agg)

```bash
# 2 GPUs: disagg-single-node, then agg (apples-to-apples prompt set)
bash scripts/bench/disagg/run.sh --compare

# Cognitora connector variant (preview)
bash scripts/bench/disagg/run.sh --mode disagg-cgn

# Manual workflow on self-hosted runner
gh workflow run bench-disagg.yml
```

Outputs land in `scripts/bench/disagg/results/`:
`summary.md`, `results.json`, `workload.jsonl`.

### Energy efficiency (J/token, tokens/W)

Requires agents reporting power (NVML / Redfish / rocm-smi):

```bash
# Stack must already be running (any agg recipe)
bash scripts/bench/energy/run.sh
```

Outputs: `scripts/bench/energy/results/summary.md`.

### Connector smoke (preview)

```bash
pip install vllm
pip install ./python/cgn-kv-connector
bash recipes/llama3-8b/vllm/agg-cgn/up.sh
# or 2-GPU:
bash recipes/llama3-8b/vllm/disagg-cgn/up.sh
```

## Published results

Fill this table after a GPU run (do not invent numbers):

| Date | Hardware | Recipe / mode | TTFT p50 (ms) | TTFT p95 (ms) | tok/s | J/token | tokens/W |
|------|----------|---------------|---------------|---------------|-------|---------|----------|
| — | — | — | — | — | — | — | — |

Copy `summary.md` from the harness output into a PR when publishing.
