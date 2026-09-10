# Cognitora Cluster Dashboard

A standalone, zero-dependency web dashboard for monitoring a Cognitora
inference cluster: live request/token throughput, latency and TTFT
percentiles, per-node status, KV-cache utilization, queue depth, power
draw, and estimated energy per token.

## How it works

The dashboard is a single static HTML file. It polls a Prometheus
`/metrics` endpoint over HTTP (CORS is enabled on all Cognitora metric
listeners), parses the exposition text in the browser, and keeps ~30
minutes of in-memory time series to render charts. There is no backend,
no build step, and nothing to install.

Metric sources:

| Endpoint | Scope |
| --- | --- |
| `cgn-router` admin listener (default `:9091/metrics`) | one cluster: gateway traffic + node registry state |
| `cgn-metrics` federation endpoint | whole fleet (aggregated across clusters) |

## Run it

Any static file server works:

```bash
python3 -m http.server 8088 -d dashboard
# open http://localhost:8088 and point it at http://<router>:9091/metrics
```

Opening `index.html` directly from `file://` also works.

The endpoint and poll interval are saved to `localStorage`.

## Charts

- **requests / s** and **tokens / s** — rates derived from
  `cgn_router_chat_requests_total` / `cgn_router_chat_completion_tokens_total`.
- **latency p50 / p95** — interpolated from `cgn_router_chat_latency_seconds`
  histogram bucket deltas between scrapes (recent traffic, not lifetime).
- **TTFT p95** — same technique on `cgn_router_chat_ttft_seconds`
  (time from dispatch to first streamed token).
- **queue depth** — sum of `cgn_cluster_node_queue_depth` across nodes.
- **power** — sum of `cgn_cluster_node_power_watts`.
- **KV cache used %** — from `cgn_cluster_node_kv_free_blocks` /
  `cgn_cluster_node_kv_total_blocks`.
- **energy (J / token)** — average watts over the scrape window × window
  seconds ÷ tokens generated in the window.

The node table is driven by `cgn_cluster_node_info` plus the per-node
gauges, refreshed every scrape; nodes that leave the cluster disappear
automatically because the router resets the gauge vectors each pass.

All of these series are also available to Grafana/Prometheus directly —
this dashboard is just the batteries-included view.
