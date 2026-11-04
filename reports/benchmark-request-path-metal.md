# Cognitora request-path benchmark (metal)

Generated 2026-11-04 14:42 UTC by `scripts/bench/proof/proof.py metal`.
Host: macOS-26.7.2-arm64-arm-64bit, arm64.

**Real inference on an Apple GPU (Metal), not NVIDIA and not vLLM.** Two `llama-server`
processes share one laptop GPU with the load generator, so absolute numbers are low
and don't predict a datacenter. Prefix reuse here is llama.cpp's own slot cache.
Energy comes from IOReport (`mac_energy.c`), is whole-machine, and includes idle draw,
the router, the agents and the load generator.

GPU evidence:
```

Offload: -ngl 99
```

Real-engine checks:
```json
{
  "client_logprobs_returned": true,
  "real_usage": {
    "prompt_tokens": 39,
    "completion_tokens": 3,
    "total_tokens": 42
  },
  "cascade_status": 200,
  "cascade_used_real_logprobs": true,
  "cascade_used_heuristic": false,
  "legacy_completion": " Paris, which is"
}
```

## Overhead, 1 client

30 samples per path, so p99 is close to the maximum and noisy. Across four runs the
router added 0.9 to 2.0 ms at p50 and 0.05 to 3.75 ms at p99, so it usually but not
always meets the README's 3 ms p99 target. Plain HTTP on loopback, no TLS.

| | Direct | Via router | Added |
|---|---|---|---|
| TTFT p50 | 21.09 ms | 23.09 ms | 2.00 ms |
| TTFT p99 | 26.65 ms | 26.71 ms | 0.05 ms |

## KV-aware routing vs load-only routing

Each column starts with empty caches and serves the same 32 requests, each capped at
64 tokens. p50 is the follow-up turns, which reuse a cached prefix. p95 is close in
both because every session's first turn is a cold prefill either way. A second run
agreed: 177 vs 1346 ms p50 and 0.98 vs 1.41 J/token.

The saving comes from avoided prefill, so it depends on this workload: long shared
prefixes and short answers. Workloads with less prefix reuse will save less.

Workload: {"requests": 32, "concurrency": 2, "sessions": 8, "turns": 4, "model": "Llama-3.2-3B-Instruct-Q4_K_M.gguf", "engines": 2, "slots_per_engine": 4}

| | KV-aware | Load-only |
|---|---|---|
| TTFT p50 | 173.5 ms | 1338.6 ms |
| TTFT p95 | 3370.5 ms | 3834.0 ms |
| Elapsed | 40.9 s | 57.9 s |
| Completion tokens | 1425 | 1425 |
| Energy, CPU+GPU+DRAM+ANE | 1338 J | 1923 J |
| Mean power | 32.7 W | 33.2 W |
| J per completion token | 0.94 | 1.35 |
