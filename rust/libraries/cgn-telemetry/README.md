# cgn-telemetry

[![crates.io](https://img.shields.io/crates/v/cgn-telemetry.svg)](https://crates.io/crates/cgn-telemetry)
[![docs.rs](https://docs.rs/cgn-telemetry/badge.svg)](https://docs.rs/cgn-telemetry)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Tracing, OTLP export, and Prometheus exposition shared by every Cognitora
binary.

`init` wires a `tracing-subscriber` with `EnvFilter` (driven by
`RUST_LOG`), optionally enables OTLP-over-tonic export when an endpoint is
configured, and registers a process-wide Prometheus registry. The
`admin_router` returns an axum `Router` that serves `/metrics`,
`/healthz`, and `/readyz` for use by the orchestrator and load balancers.

## Use

```toml
[dependencies]
cgn-telemetry = "0.1"
```

```rust
use cgn_telemetry::{init, admin_router, registry};
use prometheus::IntCounter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init("cgn-router")?;
    let counter = IntCounter::new("requests_total", "incoming requests")?;
    registry().register(Box::new(counter.clone()))?;

    let admin = admin_router();
    // ... serve admin on :9091, traffic on :8080 ...
    Ok(())
}
```

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
