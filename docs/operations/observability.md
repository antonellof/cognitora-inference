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
| `cgn_router_routing_decision_seconds`     | histogram | `model`                      |
| `cgn_router_prefix_overlap_ratio`         | histogram | `model`                      |
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
gauges, and the block cache reports through its `Kv.Stats` RPC.
`cgn_cluster_node_up` is 1 for a node that is ready and not draining.

`cgn_router_prefix_overlap_ratio` is what the router believes a node holds,
not a measured engine cache hit rate.

`cgn_cluster_power_watts_total` sums `cgn_cluster_node_power_watts` across
live nodes (refreshed every 5 s). `cgn_cluster_tokens_per_watt` is the
recent completion-token rate divided by fleet power — use it with
`scripts/bench/energy/` or `scripts/bench/proof/` for J/token numbers.

`status` is `200`, a client or overload code such as `400` or `429`, or
`5xx` for every server fault. `reason` ∈ {`queue_full`, `wait_timeout`,
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

- Latency: routing-decision p99, TTFT p99 per model
- Prefix overlap, requests by status, in-flight by model
- Power per node

## Alerting

The chart ships both halves of alerting, off by default. Both need the
prometheus-operator CRDs:

```bash
helm upgrade cognitora ... \
  --set metrics.serviceMonitor.enabled=true \
  --set metrics.prometheusRule.enabled=true
```

`serviceMonitor` adds a ClusterIP Service for the router's admin port and
scrapes it. If you scrape another way, set `metrics.prometheusRule.routerJob`
to your `job` label, or the router-down alerts will look for the wrong job.
Your Prometheus must also select these objects, which usually means copying
its selector into `metrics.serviceMonitor.labels` and
`metrics.prometheusRule.labels`.

| Alert | Severity | Fires when |
|-------|----------|------------|
| `CognitoraRouterDown` | page | A router scrape fails for 1 min |
| `CognitoraRouterNotScraped` | page | No router targets exist for 5 min, which would silence everything below |
| `CognitoraNoNodes` | page | The router sees no nodes for 2 min |
| `CognitoraHighErrorRatio` | page | Server errors exceed `thresholds.errorRatio` (5%) for 5 min |
| `CognitoraNodeLost` | warn | Fewer nodes than at any point in the last hour; clears an hour after a scale-down |
| `CognitoraAgentNotReady` | warn | A node is unready or draining for 5 min on every router |
| `CognitoraTtftSlo` | warn | TTFT p99 exceeds `thresholds.ttftP99Seconds` (2.5 s) for 10 min |
| `CognitoraRoutingDecisionSlo` | warn | Routing decision p99 exceeds 500 µs for 10 min |
| `CognitoraAdmissionRejecting` | warn | Requests are rejected at admission for 5 min |
| `CognitoraWaitQueueBacklog` | warn | Requests stay parked in wait queues for 10 min |
| `CognitoraNodeOverPowerCap` | warn | A node stays above its `watt_limit` for 15 min |
| `CognitoraLowPrefixOverlap` | info | Mean overlap is below `thresholds.minPrefixOverlap` (0.1) for 30 min |
| `CognitoraTokenizerFallback` | info | A model has been using the estimate instead of a tokenizer for 30 min |

Set the TTFT threshold from your tier in [SLOs](slo.md); the histogram's top
bucket is 10 s, so it can't see beyond that. Overload (429) and client errors
(400) deliberately don't count towards the error ratio.

CI keeps this honest: `scripts/check-helm-metrics.py` fails if the chart uses a
metric the code doesn't export, and `tests/helm/alerts_test.yaml` runs the rules
through `promtool` to prove the main alerts fire and stay quiet when they should.
