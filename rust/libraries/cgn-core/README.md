# cgn-core

[![crates.io](https://img.shields.io/crates/v/cgn-core.svg)](https://crates.io/crates/cgn-core)
[![docs.rs](https://docs.rs/cgn-core/badge.svg)](https://docs.rs/cgn-core)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Shared library used by every Cognitora binary: configuration loading, the
workspace `Error` enum, BLAKE3 prefix hashing, and the concurrent prefix
index. Telemetry, TLS, and Kubernetes helpers live in dedicated `cgn-*`
crates so this one stays free of heavy transitive dependencies.

Cognitora is the open-source datacenter-scale LLM inference stack.
`cgn-core` is its smallest internal crate: cross-cutting types and a few
constants that the routing, agent, kvcached, metrics, ctl, and operator
binaries all depend on.

## Use

```toml
[dependencies]
cgn-core = "0.1"
```

```rust
use cgn_core::{config::Config, Error, Result, hash::prefix_chain};

fn load() -> Result<Config> {
    Config::load(cgn_core::DEFAULT_CONFIG_PATH)
}
```

## Modules

| Module     | What it provides                                                        |
|------------|-------------------------------------------------------------------------|
| `config`   | Layered TOML loading with env-var overrides (built on `config-rs`).     |
| `error`    | The workspace-wide `Error` / `Result` and process exit-code mapping.    |
| `hash`     | Sequence-chained BLAKE3 prefix digests used by KV-aware routing.        |
| `prefix`   | Lock-free radix-trie index for the digests above.                       |
| `etcd_keys`| Canonical etcd key prefixes for nodes, models, routing policy, …        |

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
