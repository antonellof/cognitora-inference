# cgn-metrics — Telemetry and power aggregator

**Prometheus scrape fan-in** plus chassis / GPU power probes. Exposes a single **`/metrics`** endpoint (default `:9092`) and optional **`/federate`** for upstream Prometheus; feeds the router’s **power** scoring term.

## Overview

`cgn-metrics` collects:

- Scraped targets (router, agent, `cgn-kvcached` `/metrics` endpoints) declared in `[metrics].scrape_targets`
- Redfish / IPMI style power inputs when configured
- NVML / DCGM signals from agent scrapes

It merges everything with labels so dashboards stay consistent. Details: [Observability](../operations/observability.md).

## Features

- Configurable **`scrape_interval`** (default `10s` in the example config)
- **`scrape_targets`** list with per-target `name` + `url`; lines are tagged `cgn_target=...` at `/federate`
- **`listen_admin`** binds the HTTP server (default `0.0.0.0:9092`)
- Redfish credentials via `[metrics].redfish_*` fields

## Architecture

`Downstream /metrics → cgn-metrics → :9092/metrics (+ /federate) → Prometheus`. The router consumes normalized power gauges when scoring.

## Configuration (highlights)

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `[metrics].listen_admin` | string | `0.0.0.0:9092` | HTTP listen |
| `[metrics].scrape_interval` | duration | `10s` | Poll cadence |
| `[metrics].scrape_targets` | array | `[]` | `{ name, url }` entries |
| `[metrics].redfish_url` | string | `""` | Optional BMC REST |

## Example

```toml
[cluster]
name = "prod"
etcd = ["http://etcd:2379"]

[metrics]
listen_admin    = "0.0.0.0:9092"
scrape_interval = "10s"

scrape_targets = [
  { name = "router", url = "http://127.0.0.1:9091/metrics" },
  { name = "agent",  url = "http://127.0.0.1:9191/metrics" },
]
```

## Dependencies

- **Scrape targets** reachable from the metrics pod/host
- **Prometheus** (or compatible) for long-term storage — optional but typical

## Related documentation

- [Observability](../operations/observability.md)
- [SLOs](../operations/slo.md)

**Source:** [`rust/services/cgn-metrics/`](../../rust/services/cgn-metrics/)
