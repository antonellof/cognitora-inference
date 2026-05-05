# cgn-metrics

[![crates.io](https://img.shields.io/crates/v/cgn-metrics.svg)](https://crates.io/crates/cgn-metrics)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Prometheus aggregator and power telemetry collector for
[Cognitora](https://github.com/antonellof/cognitora-inference).

Pulls from:

* `cgn-router :9091/metrics` (request rate, tokens generated, queue depth)
* `cgn-agent :9091/metrics`  (NVML)
* `cgn-kvcached :9091/metrics`
* Redfish chassis power
* NVML per-GPU power

Exposes the union under `:9092/metrics`. The router's `power` score
component subscribes to these gauges and biases requests toward
energy-efficient nodes; the operator's drain logic can use the same
signal to evict scheduling from hot chassis.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/antonellof/cognitora-inference/main/deploy/installer/install.sh | bash
```

Or:

```bash
cargo install cgn-metrics
```

## Run

```bash
cgn-metrics --config /etc/cognitora/cognitora.toml
```

See [`docs/operations/observability.md`](https://github.com/antonellof/cognitora-inference/blob/main/docs/operations/observability.md).

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).
