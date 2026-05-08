# cgn-kvcached — Multi-tier KV cache daemon

**Host-local KV block service** with RAM + SSD tiers and cross-node fetch over QUIC (optional RDMA). Feeds prefix-overlap data so `cgn-router` can schedule onto GPUs that already hold prompt state.

## Overview

`cgn-kvcached` implements Cognitora’s **tier-3 / tier-4** cache story from [KV tiering](../architecture/kv-tiering.md): GPU-resident KV stays inside the engine; evicted blocks land in RAM and SSD; misses can pull from peers. Block IDs use BLAKE3-derived addresses (see tiering doc).

## Features

- gRPC **`cognitora.v1.Kv`** for router/agent queries
- **QUIC** listener for peer fetch (`[kv].listen_quic`, default `0.0.0.0:7073`)
- RocksDB-backed metadata + SSD layout under `[kv].ssd_dir` / `[kv].index_dir`
- RAM cap via `[kv].ram_gib`, SSD cap via `[kv].ssd_gib`
- Tunable eviction and optional mirror behaviour (see example config and [KV tiering](../architecture/kv-tiering.md))

## Architecture

`Engine eviction → agent UDS/gRPC → cgn-kvcached (RAM → SSD) ⟷ QUIC peers`. The router **does not** read KV bytes directly; it consumes overlap hints computed from agent + cache metadata.

## Configuration (highlights)

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `[kv].listen_grpc` | string | `0.0.0.0:7072` | Kv gRPC service |
| `[kv].listen_quic` | string | `0.0.0.0:7073` | Peer fetch transport |
| `[kv].ram_gib` | u32 | `8` | RAM tier budget |
| `[kv].ssd_gib` | u32 | `256` | SSD tier budget |
| `[kv].ssd_dir` | path | `/var/lib/cognitora/kv/ssd` | Cold tier files |
| `[kv].index_dir` | path | `/var/lib/cognitora/kv/index` | RocksDB index |

## Example

```toml
[cluster]
name = "prod"
etcd = ["http://etcd:2379"]

[security]
require_mtls = true

[kv]
listen_grpc = "0.0.0.0:7072"
listen_quic = "0.0.0.0:7073"
ram_gib     = 16
ssd_gib     = 512
ssd_dir     = "/var/lib/cognitora/kv/ssd"
index_dir   = "/var/lib/cognitora/kv/index"
```

## Dependencies

- **cgn-agent** — publishes residency / eviction events
- **Peer `cgn-kvcached` instances** — for cross-node fills

## Operational targets

See latency buckets in [KV tiering](../architecture/kv-tiering.md) and [SLOs](../operations/slo.md). Cold-cache playbook: [Runbook: Cache cold](../operations/runbooks/cache-cold.md).

## Related documentation

- [KV strategy](../architecture/kv-strategy.md)
- [Protocols (QUIC / federation)](../architecture/protocols.md)

**Source:** [`rust/services/cgn-kvcached/`](../../rust/services/cgn-kvcached/)
