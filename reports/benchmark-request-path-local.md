# Cognitora request-path benchmark (local)

Generated 2026-11-04 13:41 UTC by `scripts/bench/proof/proof.py local`.
Host: macOS-26.7.2-arm64-arm-64bit, arm64.

**No model ran.** Engines are `tests/e2e/stub_engine.py`, a Python stub on the same
host. This measures the router and agent, not inference. The KV table uses a *simulated*
prefix cache, so it shows routing decisions, not real TTFT.

## Overhead, 1 client

| | Direct | Via router | Added |
|---|---|---|---|
| TTFT p50 | 0.40 ms | 0.98 ms | 0.59 ms |
| TTFT p99 | 1.17 ms | 1.54 ms | 0.37 ms |

## Overhead, 8 clients

| | Direct | Via router | Added |
|---|---|---|---|
| TTFT p50 | 1.77 ms | 3.78 ms | 2.01 ms |
| TTFT p99 | 2.62 ms | 5.11 ms | 2.49 ms |

## KV-aware routing vs load-only routing

Workload: {"requests": 128, "sim_cache": 120, "sim_ms_per_chunk": 2.0, "concurrency": 4, "sessions": 16, "turns": 8}

| | KV-aware | Load-only |
|---|---|---|
| Prefix chunks served from cache | 78.4% | 10.4% |
| TTFT p50 | 6.2 ms | 61.1 ms |
| TTFT p95 | 63.4 ms | 67.1 ms |
| Requests per node | [24, 32, 32, 40] | [24, 28, 34, 42] |
