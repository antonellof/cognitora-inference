# Observability

Every Cognitora binary emits the same three pillars: **structured
logs** (JSON via `tracing`), **metrics** (Prometheus over an admin
HTTP port), and **traces** (OTLP when an endpoint is configured).
`cgn-telemetry` initialises all three in a single `init(service)`
call so the wire format is identical across binaries.

## Logs

`tracing-subscriber` with the JSON formatter writes one event per
line to stdout. Fields the platform always emits:

| Field         | Type    | Notes                                       |
|---------------|---------|---------------------------------------------|
| `timestamp`   | RFC3339 | UTC                                         |
| `level`       | string  | `INFO`/`WARN`/`ERROR`/`DEBUG`               |
| `fields`      | object  | `message` plus event fields, such as `node` and `overlap` on routing decisions |
| `target`      | string  | tracing target (Rust module path)           |

Override the level with `RUST_LOG=cgn_router=debug,info`.

## Metrics

Each binary serves Prometheus on its admin port:

| Binary          | Port     |
|-----------------|----------|
| `cgn-router`    | `:9091`  |
| `cgn-agent`     | `:9091`  |
| `cgn-kvcached`  | `:9091`  |
| `cgn-metrics`   | `:9092`  |

`cgn-metrics` federates the per-host endpoints and exposes the union
on `:9092`, plus the power gauges from `cgn-power`.

All `/metrics` listeners send `Access-Control-Allow-Origin: *`, so any
browser app can scrape them directly, including the bundled
[standalone cluster dashboard](../../dashboard/README.md), which charts
req/s, tokens/s, latency + TTFT percentiles, per-node status, KV
utilisation, power, and J/token with no Prometheus server required.

The router additionally mirrors its live node registry into
`cgn_cluster_node_*` gauges (`up`, `cordoned`, `queue_depth`,
`kv_free_blocks`, `kv_total_blocks`, `power_watts`, `info`) refreshed
every 5 s, plus `cgn_cluster_nodes_total` and
`cgn_router_prefix_index_digests`. Gateway traffic is covered by
`cgn_router_chat_requests_total`, `cgn_router_chat_completion_tokens_total`,
`cgn_router_chat_latency_seconds`, and `cgn_router_chat_ttft_seconds`
(time from dispatch to first streamed token).

### Core series

| Metric                                    | Type      | Labels                       |
|-------------------------------------------|-----------|------------------------------|
| `cgn_router_chat_requests_total`          | counter   | `model`, `status`            |
| `cgn_router_chat_latency_seconds`         | histogram | `model`, `stream`            |
| `cgn_router_chat_ttft_seconds`            | histogram | `model`                      |
| `cgn_router_chat_completion_tokens_total` | counter   | `model`                      |
| `cgn_router_tenant_tokens_total`          | counter   | `tenant`, `kind`             |
| `cgn_router_token_limit_rejected_total`   | counter   | `limit`                      |
| `cgn_router_rate_limited_total`           | counter   | —                            |
| `cgn_router_admission_inflight`           | gauge     | `model`, `role`              |
| `cgn_router_admission_waiting`            | gauge     | `model`                      |
| `cgn_router_admission_rejected_total`     | counter   | `model`, `reason`            |
| `cgn_router_cascade_confidence_total`     | counter   | `source`                     |
| `cgn_router_tokenizer_fallback_total`     | counter   | `model`                      |
| `cgn_router_federation_forwards_total`    | counter   | `model`, `peer`              |
| `cgn_router_prefix_index_digests`         | gauge     | —                            |
| `cgn_router_prefix_index_pruned_total`    | counter   | `node`, `reason`             |
| `cgn_router_carbon_admission_rejected_total` | counter | —                           |
| `cgn_carbon_intensity_gco2_per_kwh`       | gauge     | `zone`                       |
| `cgn_cluster_nodes_total`                 | gauge     | —                            |
| `cgn_cluster_node_up`, `_cordoned`, `_queue_depth`, `_power_watts`, `_watt_limit`, `_kv_free_blocks`, `_kv_total_blocks`, `_vram_total_mb`, `_info` | gauge | `node`, plus identity labels on `_info` |
| `cgn_cluster_power_watts_total`           | gauge     | —                            |
| `cgn_cluster_tokens_per_watt`             | gauge     | —                            |
| `cgn_power_watts_gpu`, `cgn_power_watts_chassis` | gauge | see `cgn-metrics`         |
| `cgn_metrics_scrape_errors_total`         | counter   | see `cgn-metrics`            |

`cgn-agent` and `cgn-kvcached` export no Prometheus metrics of their own
yet. Node health reaches Prometheus through the router's `cgn_cluster_node_*`
gauges, and the block cache reports through its `Kv.Stats` RPC. No metric
reports the prefix cache hit rate; the router logs `overlap` on each routing
decision instead.

`cgn_cluster_power_watts_total` sums `cgn_cluster_node_power_watts` across
live nodes (refreshed every 5 s). `cgn_cluster_tokens_per_watt` is the
recent completion-token rate divided by fleet power — use it with
`scripts/bench/energy/` or `scripts/bench/proof/` for J/token numbers.

`status` ∈ {`200`, `429`, `5xx`}, where `5xx` also counts client errors
returned by the engine. `reason` ∈ {`queue_full`, `wait_timeout`,
`ttft_violation`}. `limit` ∈ {`tpm`, `budget`}. `source` ∈ {`logprobs`,
`heuristic`}. `kind` ∈ {`prompt`, `completion`}.

## Traces

Set `OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317` and every
binary exports OTLP/gRPC spans from `tracing`. The router's HTTP trace layer
is the main source; there are no custom routing or engine spans yet.

W3C `traceparent` and `tracestate` from the client are forwarded to agents
in the gRPC request, but the agent does not continue the trace yet.

## Dashboards

A starter Grafana dashboard ships at
[`deploy/kubernetes/helm/cognitora/dashboards/cognitora.json`](../../deploy/kubernetes/helm/cognitora/dashboards/cognitora.json).
Set `metrics.dashboards.enabled = true` in the Helm values to mount it
into a `grafana_dashboard=1` ConfigMap that the kube-prometheus-stack
Grafana sidecar picks up automatically. Panels:

- Latency: routing-decision p99, TTFT p50/p95/p99 per model
- Throughput: requests/s and tokens/s per node
- Cache: hit ratio over 5m + per-tier hit count
- Power: watts per chassis vs tokens/s (energy efficiency)
- Health: ready replicas, queue depth, admission rejects

## Alerting

A `PrometheusRule` ships at
[`deploy/kubernetes/helm/cognitora/templates/prometheus-rule.yaml`](../../deploy/kubernetes/helm/cognitora/templates/prometheus-rule.yaml),
enabled with `metrics.prometheusRule.enabled = true`.

**Most of these rules don't work yet.** Only `CognitoraRouterDown` is built
on a metric that exists. The others use `cgn_router_routing_decision_seconds`,
`cgn_router_kv_cache_hits_total`, `cgn_agent_ready` and similar names the code
doesn't export, so they can never fire. Until the chart is fixed, write alerts
on the metrics above. Useful ones:

```promql
cgn_cluster_nodes_total == 0
histogram_quantile(0.99, sum by (le, model) (rate(cgn_router_chat_ttft_seconds_bucket[5m]))) > 1
rate(cgn_router_admission_rejected_total[5m]) > 0
cgn_router_admission_waiting > 0
```
