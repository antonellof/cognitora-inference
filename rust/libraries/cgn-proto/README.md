# cgn-proto

[![crates.io](https://img.shields.io/crates/v/cgn-proto.svg)](https://crates.io/crates/cgn-proto)
[![docs.rs](https://docs.rs/cgn-proto/badge.svg)](https://docs.rs/cgn-proto)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Generated tonic / prost stubs for the Cognitora gRPC services.

The `*.proto` files under `proto/cognitora/v1/` are bundled inside the crate
and compiled by `tonic-build` at build time. Both server and client stubs
are generated. Anyone consuming this crate gets the full gRPC surface
(`Router`, `Agent`, `Kv`, `Control`, `Metrics`) without needing `protoc`
to be invoked at the consumer side — it is invoked while building this
crate.

Build dependency: `protoc` must be available in `PATH` while compiling.
Cognitora's [installer](https://github.com/antonellof/cognitora-inference/blob/main/deploy/installer/install.sh)
takes care of this; on dev machines `brew install protobuf` (macOS) or
`apt install protobuf-compiler libprotobuf-dev` (Debian / Ubuntu) is enough.

## Use

```toml
[dependencies]
cgn-proto = "0.1"
```

```rust
use cgn_proto::v1::router_service_client::RouterServiceClient;

let mut client = RouterServiceClient::connect("https://router:9090").await?;
```

## Services

| Proto file       | Service                  | Used by                        |
|------------------|--------------------------|--------------------------------|
| `common.proto`   | (shared message types)   | every service                  |
| `router.proto`   | `RouterService`          | `cgn-router` admin / federation|
| `agent.proto`    | `AgentService`           | router → agent generation calls|
| `kv.proto`       | `KvService`              | agent → kvcached, peer fetch   |
| `control.proto`  | `ControlService`         | `cgn-ctl` admin RPCs           |
| `metrics.proto`  | `MetricsService`         | `cgn-metrics` aggregation      |

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
