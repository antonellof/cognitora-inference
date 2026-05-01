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
| `service`     | string  | `cgn-router` / `cgn-agent` / …              |
| `version`     | string  | `Cargo.toml` version                        |
| `node_id`     | string  | from `[cluster].node_id` (12-char prefix)   |
| `request_id`  | string  | propagated through `x-request-id`           |
| `subject`     | string  | from `cgn-auth` (`key:<id>` / `oidc:<sub>`) |
| `target`      | string  | tracing target (Rust module path)           |
| `message`     | string  | free text                                   |

Override the level with `RUST_LOG=cgn_router=debug,info` or
`COGNITORA__LOG_LEVEL=debug`.

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

### Core series

| Metric                                    | Type      | Labels                       |
|-------------------------------------------|-----------|------------------------------|
| `cgn_router_requests_total`               | counter   | `model`, `subject`, `outcome`|
| `cgn_router_routing_decision_seconds`     | histogram | `model`                      |
| `cgn_router_admission_inflight`           | gauge     | `model`, `role`              |
| `cgn_router_admission_rejected_total`     | counter   | `model`, `reason`            |
| `cgn_router_cache_hit_ratio`              | gauge     | `model`                      |
| `cgn_agent_engine_ready`                  | gauge     | `model`                      |
| `cgn_agent_generate_seconds`              | histogram | `model`                      |
| `cgn_agent_kv_blocks_total`               | counter   | `tier`, `op`                 |
| `cgn_kvcached_lookup_seconds`             | histogram | `tier`, `outcome`            |
| `cgn_kvcached_blocks`                     | gauge     | `tier`                       |
| `cgn_kvcached_bytes`                      | gauge     | `tier`                       |
| `cgn_power_watts`                         | gauge     | `component`                  |

`outcome` ∈ {`ok`, `error`, `rate_limited`, `admission_rejected`}.
`reason` ∈ {`queue_full`, `ttft_violation`, `unavailable`}.
`component` ∈ {`chassis`, `psu0`, `gpu0`, …}.

### Recording rules

The Helm chart ships a Prometheus rules ConfigMap with these
recording rules so dashboards stay cheap:

```yaml
- record: cgn:router_p99_routing_us
  expr:  histogram_quantile(0.99,
           sum by (le, model) (rate(cgn_router_routing_decision_seconds_bucket[5m]))) * 1e6
- record: cgn:cache_hit_ratio_5m
  expr:  avg_over_time(cgn_router_cache_hit_ratio[5m])
- record: cgn:tokens_per_watt_5m
  expr:  sum(rate(cgn_agent_tokens_total[5m]))
       / sum(cgn_power_watts{component=~"gpu.*|chassis"})
```

## Traces

Set `OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4317` and every
binary will export OTLP/gRPC spans. The default sample rate is the
parent-based ratio sampler at 1% (override with
`OTEL_TRACES_SAMPLER_ARG=0.1` for 10%).

Span structure:

- **Root span** at the gateway: `gateway.chat` (or `gateway.embed`).
  Attributes: `model`, `subject`, `prompt_tokens`, `cache_hit`.
- **Routing**: child span `router.pick` with `score`, `node_id`,
  `kv_overlap`, `load`, `power`.
- **Agent**: child span `agent.generate` propagated from the router
  via gRPC metadata.
- **Engine**: child span `engine.forward` emitted by the engine
  driver (vLLM today writes the OTLP span itself when launched with
  `--otlp-endpoint=$OTEL_EXPORTER_OTLP_ENDPOINT`).

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
[`deploy/kubernetes/helm/cognitora/templates/prometheus-rule.yaml`](../../deploy/kubernetes/helm/cognitora/templates/prometheus-rule.yaml).
Enable with `metrics.prometheusRule.enabled = true` (requires the
prometheus-operator CRDs). The included rules fire when:

- TTFT p99 > `[router.admission].ttft_slo_ms` for 5 min
- `cgn:cache_hit_ratio_5m` < 0.30 for 15 min on a multi-replica model
- `cgn_agent_engine_ready == 0` for 2 min on any node
- `cgn_router_admission_rejected_total{reason="queue_full"}` rate
  > 0 for 5 min
