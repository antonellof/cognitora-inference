# SLOs

Cognitora defines two SLO tiers: **platform-SLOs** that apply to the
control plane (the routing decision, the admission queue, the KV
cache) and **workload-SLOs** that the operator owns (TTFT and TPS for
each model).

## Platform SLOs

These are the same numbers exposed in the README's perf table. Each
PR runs the perf harness in [`tests/perf/`](../../tests/perf) and a
regression is reported on the PR, but the gate is soft and doesn't block
merge. Results so far are in [`reports/`](../../reports/).

| SLI                                          | Target          | Source                              |
|----------------------------------------------|-----------------|-------------------------------------|
| Routing decision p99                         | < 500 µs / vCPU | `cgn-router::routing::score`        |
| HTTP overhead (router p99 − engine p99)      | < 3 ms          | `tests/perf/router_overhead.rs`     |
| KV lookup p99 (warm)                         | < 200 µs        | `cgn-kvcached` RAM tier             |
| KV lookup p99 (cold)                         | < 5 ms          | `cgn-kvcached` SSD tier             |
| Cross-node 1 MiB block fetch p99 (10 GbE)    | < 12 ms         | QUIC transport                      |
| Cache hit ratio (representative trace)       | ≥ 0.55          | `scripts/bench/proof/proof.py`      |
| Energy efficiency vs round-robin baseline    | ≥ 1.4×          | tokens/s ÷ Σ watts                  |

## Workload SLOs

Per-model, owned by whoever runs the deployment. Example targets we
ship as defaults:

| Model class | TTFT p95 | Tokens/s p50 (per stream) |
|-------------|----------|---------------------------|
| Small (≤8B) | 200 ms   | 60                        |
| Mid (≤30B)  | 500 ms   | 35                        |
| Large       | 1.0 s    | 18                        |

With `[router.autoscaler].deadline_admission = true`, the router rejects
requests once a node would miss their deadline: `x-cgn-deadline-ms`, else
`[router.admission].ttft_slo`. Set `ttft_slo` per cluster from the matching
tier above.

## Error budgets

A 30-day rolling window. Every SLO above gets a 99.5% target by
default; that's 3.6 hours of error budget per month. The Helm
chart's PrometheusRules emit:

- `cgn:slo_burn_rate_5m{model=...}`: fast burn detector (1h, 14.4×)
- `cgn:slo_burn_rate_1h{model=...}`: slow burn detector (6h, 6×)

Alerts fire when both windows are burning above their multiplier.

## How we verify them

1. **Every PR runs the perf harness.** It uses synthetic prompts and
   pre-warmed KV blocks so the numbers are reproducible. CI fails if
   p99 regresses > 5%.
2. **Release builds publish the numbers** to the GitHub Release page
   so every tag has a reproducible baseline.
3. **Production scrapes the same series.** The dashboards alert on
   real traffic; we don't lean on synthetic-only.
