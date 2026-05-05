# cgn-kv

[![crates.io](https://img.shields.io/crates/v/cgn-kv.svg)](https://crates.io/crates/cgn-kv)
[![docs.rs](https://docs.rs/cgn-kv/badge.svg)](https://docs.rs/cgn-kv)
[![license](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE)

KV cache primitives: block addressing, RocksDB index, multi-tier storage,
and the QUIC transfer wire format.

Used by the [`cgn-kvcached`](https://crates.io/crates/cgn-kvcached) daemon
and (for prefix accounting) by `cgn-router`. The storage backends here
are the safe Rust ones; CUDA and RDMA paths live behind feature gates so
the crate builds on a plain Linux box without a GPU.

## Use

```toml
[dependencies]
cgn-kv = "0.1"
```

## Features

| Feature              | Default | What it adds                                          |
|----------------------|---------|-------------------------------------------------------|
| `persistent-index`   | yes     | Persistent RocksDB-backed block index.                |
| `rdma`               | no      | RDMA fast-path (Linux + ibverbs). QUIC handles the    |
|                      |         | transfer when this is off.                            |

To build without the RocksDB C++ tree (useful on macOS 15 SDKs that
struggle with the bundled rocksdb 8.10 source):

```toml
cgn-kv = { version = "0.1", default-features = false }
```

## Modules

| Module       | What it provides                                                  |
|--------------|-------------------------------------------------------------------|
| `block`      | `BlockAddress`, `BlockHandle`, `BlockMeta` value types.           |
| `index`      | RocksDB-backed persistent index (feature-gated).                  |
| `tier`       | `Tier` / `TierKind` abstraction over RAM, SSD, GPU pools.         |
| `ssd`        | SSD tier: file-per-block under a configurable root.               |
| `transport`  | QUIC peer-to-peer KV transfer wire format.                        |
| `rdma`       | ibverbs RDMA path (feature-gated).                                |

## License

Apache-2.0. See [LICENSE](https://github.com/antonellof/cognitora-inference/blob/main/LICENSE).

Part of [Cognitora](https://github.com/antonellof/cognitora-inference).
