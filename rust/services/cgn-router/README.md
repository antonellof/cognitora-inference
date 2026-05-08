# cgn-router

[![crates.io](https://img.shields.io/crates/v/cgn-router.svg)](https://crates.io/crates/cgn-router)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

OpenAI-compatible HTTP/SSE gateway **and** KV-aware orchestrator —
the front door of a [Cognitora](https://github.com/antonellof/cognitora-inference)
cluster.

One binary, three listeners:

* `:8080` — HTTP (OpenAI surface, SSE streaming).
* `:9090` — gRPC (admin / control / federation, mTLS).
* `:9091` — plain HTTP admin (`/metrics`, `/healthz`, `/readyz`).

The router scores candidate workers using
**sequence-chained BLAKE3 prefix digests** + **longest-prefix overlap**
plus load, power, and capacity terms, then forwards via gRPC mTLS to the
right `cgn-agent`. It also handles admission (deadline, quota, rate
limit), the multi-model SLM→LLM cascade, prefill/decode disaggregation,
and cross-cluster federation.

## Install

Most users install the whole Cognitora suite via the one-liner:

```bash
curl -fsSL https://inference.cognitora.dev/install | bash
```

To get just this binary from crates.io:

```bash
cargo install cgn-router
```

## Run

```bash
cgn-router --config /etc/cognitora/cognitora.toml
```

See [`docs/reference/config.md`](https://github.com/antonellof/cognitora-inference/blob/main/docs/reference/config.md)
for the full TOML schema and
[`docs/architecture/routing.md`](https://github.com/antonellof/cognitora-inference/blob/main/docs/architecture/routing.md)
for the routing model.

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).
