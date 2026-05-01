# SLOs

Cognitora defines two SLO tiers: **platform-SLOs** that apply to the
control plane (the routing decision, the admission queue, the KV
cache) and **workload-SLOs** that the operator owns (TTFT and TPS for
each model).

## Platform SLOs (CI-gated)

These are the same numbers exposed in the README's perf table. Each
PR runs the perf harness in `tests/perf/` (TODO) and a regression
> 5% of any of them blocks merge.

| SLI                                          | Target          | Source                              |
|----------------------------------------------|-----------------|-------------------------------------|
| `cgn_router_routing_decision_seconds` p99    | < 500 µs / vCPU | `cgn-router::routing::score`        |
| HTTP overhead (router p99 − engine p99)      | < 3 ms          | `tests/perf/router_overhead.rs`     |
| `cgn_kvcached_lookup_seconds` p99 (warm)     | < 200 µs        | `cgn-kvcached` RAM tier             |
| `cgn_kvcached_lookup_seconds` p99 (cold)     | < 5 ms          | `cgn-kvcached` SSD tier             |
| Cross-node 1 MiB block fetch p99 (10 GbE)    | < 12 ms         | QUIC transport                      |
| Cache hit ratio (representative trace)       | ≥ 0.55          | `cgn:cache_hit_ratio_5m`            |
| Energy efficiency vs round-robin baseline    | ≥ 1.4×          | tokens/s ÷ Σ watts                  |

## Workload SLOs

Per-model, owned by whoever runs the deployment. Example targets we
ship as defaults:

| Model class | TTFT p95 | Tokens/s p50 (per stream) |
|-------------|----------|---------------------------|
| Small (≤8B) | 200 ms   | 60                        |
| Mid (≤30B)  | 500 ms   | 35                        |
| Large       | 1.0 s    | 18                        |

The router's admission control rejects requests once a node would
violate the configured `[router.admission].ttft_slo_ms` for the
model. That `ttft_slo_ms` value should be set per-cluster from the
matching tier above.

## Error budgets

A 30-day rolling window. Every SLO above gets a 99.5% target by
default — that's 3.6 hours of error budget per month. The Helm
chart's PrometheusRules emit:

- `cgn:slo_burn_rate_5m{model=...}` — fast burn detector (1h, 14.4×)
- `cgn:slo_burn_rate_1h{model=...}` — slow burn detector (6h, 6×)

Alerts fire when both windows are burning above their multiplier.

## How we keep them honest

1. **Every PR runs the perf harness.** It uses synthetic prompts and
   pre-warmed KV blocks so the numbers are reproducible. CI fails if
   p99 regresses > 5%.
2. **Release builds publish the numbers** to the GitHub Release page
   so every tag has a reproducible baseline.
3. **Production scrapes the same series.** The dashboards alert on
   real traffic; we don't lean on synthetic-only.
