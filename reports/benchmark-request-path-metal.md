# Cognitora request-path benchmark (metal)

Generated 2026-11-04 13:55 UTC by `scripts/bench/proof/proof.py metal`.
Host: macOS-26.7.2-arm64-arm-64bit, arm64.

**Real inference on an Apple GPU (Metal), not NVIDIA and not vLLM.** Two `llama-server`
processes share one laptop GPU with the load generator, so absolute numbers are low
and don't predict a datacenter. Prefix reuse here is llama.cpp's own slot cache.

GPU evidence (`llama-server --list-devices`, offload `-ngl 99`):
```
Available devices:
  MTL0: Apple M2 Pro (25559 MiB, 25558 MiB free)
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
  "legacy_completion": " located in which region"
}
```

## Overhead, 1 client

30 samples per path, so p99 is close to the maximum. An earlier run of the same
command measured 0.90 ms p50 and 1.44 ms p99 added, so read this as roughly
1 to 2 ms p50 and 1.5 to 4 ms p99. It doesn't reliably meet the README's 3 ms p99
target. Both paths used plain HTTP on loopback with no TLS.

| | Direct | Via router | Added |
|---|---|---|---|
| TTFT p50 | 21.04 ms | 22.99 ms | 1.96 ms |
| TTFT p99 | 23.09 ms | 26.85 ms | 3.75 ms |

## KV-aware routing vs load-only routing

Each column starts with empty caches. p50 is the follow-up turns, which reuse a
cached prefix. p95 is similar in both because every session's first turn is a
cold prefill either way. 32 requests is a small sample; the earlier run agreed
(200 ms vs 1887 ms p50).

Workload: {"requests": 32, "concurrency": 2, "sessions": 8, "turns": 4, "model": "Llama-3.2-3B-Instruct-Q4_K_M.gguf", "engines": 2, "slots_per_engine": 4}

| | KV-aware | Load-only |
|---|---|---|
| TTFT p50 | 184.3 ms | 1482.9 ms |
| TTFT p95 | 3801.7 ms | 3936.6 ms |
| Elapsed | 20.9 s | 36.7 s |
| Completion tokens | 253 | 253 |
| J/token (CPU+GPU) | not measured (powermetrics needs root) | not measured (powermetrics needs root) |
