# cgn-kvcached

[![crates.io](https://img.shields.io/crates/v/cgn-kvcached.svg)](https://crates.io/crates/cgn-kvcached)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

Multi-tier KV cache daemon for a [Cognitora](https://github.com/antonellof/cognitora-inference)
cluster.

Three tiers:

* **GPU** (hot, optional pinned pool — when CUDA paths are enabled).
* **RAM** (warm, `DashMap`).
* **SSD** (cold, file-per-block under a configurable root,
  RocksDB-indexed).

Three listeners:

* UDS gRPC (low-latency same-host calls from `cgn-agent`).
* TCP gRPC + mTLS for cross-host control RPCs (`Lookup`, `Push`, …).
* QUIC `:7072` for cross-host KV transfer.

## Install

Pre-built binary (recommended):

```bash
curl -fsSL https://raw.githubusercontent.com/antonellof/cognitora-inference/main/deploy/installer/install.sh | bash
```

From source:

```bash
cargo install cgn-kvcached
```

> Building from source compiles RocksDB's C++ tree. macOS 15+ SDKs sometimes
> trip on the bundled rocksdb 8.10 source — use the pre-built tarball or a
> Linux build host on those systems.

## Run

```bash
cgn-kvcached --config /etc/cognitora/cognitora.toml
```

See [`docs/architecture/kv-tiering.md`](https://github.com/antonellof/cognitora-inference/blob/main/docs/architecture/kv-tiering.md).

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).
