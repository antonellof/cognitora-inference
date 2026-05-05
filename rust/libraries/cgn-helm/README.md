# cgn-helm

[![crates.io](https://img.shields.io/crates/v/cgn-helm.svg)](https://crates.io/crates/cgn-helm)
[![docs.rs](https://docs.rs/cgn-helm/badge.svg)](https://docs.rs/cgn-helm)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Thin async wrapper around the `helm` binary used by
[`cgn-ctl`](https://crates.io/crates/cgn-ctl) for Kubernetes installs.

Rust's ecosystem doesn't have a real Helm SDK, but shelling out is fine:
helm is a single static Go binary, Cognitora ships it embedded inside its
release tarballs, and the surface used here (`install`, `upgrade`,
`uninstall`, `version`, `list`) is stable.

The wrapper:

* Resolves the binary via `$CGN_HELM_BIN`, falling back to `which::which`.
* Returns structured results typed against `cgn-core::Result`.
* Streams `stdout` / `stderr` through `tokio::process` so long-running
  installs don't deadlock.

## Use

```toml
[dependencies]
cgn-helm = "0.1"
```

```rust
let v = cgn_helm::version().await?;
println!("helm {v}");
```

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
