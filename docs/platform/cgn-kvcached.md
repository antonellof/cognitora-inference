# cgn-kvcached: Multi-tier KV cache daemon

**Host-local KV block service** with RAM + SSD tiers and cross-node fetch over QUIC (optional RDMA). Feeds prefix-overlap data so `cgn-router` can schedule onto GPUs that already hold prompt state.

## Overview

`cgn-kvcached` implements Cognitora’s **tier-3 / tier-4** cache story from [KV tiering](../architecture/kv-tiering.md): GPU-resident KV stays inside the engine; evicted blocks land in RAM and SSD; misses can pull from peers. Block IDs use BLAKE3-derived addresses (see tiering doc).

## Features

- gRPC **`cognitora.v1.Kv`** for router/agent queries
- **QUIC** listener for peer fetch (`[kv].quic_listen`, default `0.0.0.0:7072`), authenticated with mTLS
- RocksDB-backed metadata + SSD layout under `[kv].ssd_dir` / `[kv].index_dir`
- RAM cap via `[kv].ram_gib`, SSD cap via `[kv].ssd_gib`
- Tunable eviction and optional mirror behaviour (see example config and [KV tiering](../architecture/kv-tiering.md))

## Architecture

`Engine eviction → agent UDS/gRPC → cgn-kvcached (RAM → SSD) ⟷ QUIC peers`. The router **does not** read KV bytes directly; it consumes overlap hints computed from agent + cache metadata.

## Configuration (highlights)

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `[kv].listen` | string | `0.0.0.0:7071` | Kv gRPC service |
| `[kv].quic_listen` | string | `0.0.0.0:7072` | Peer fetch transport |
| `[kv].allow_insecure_quic` | bool | `false` | Allow plain QUIC on a non-loopback listener |
| `[kv].quic_server_name` | string | `cognitora` | SAN every peer cert needs under mTLS |
| `[kv].ram_gib` | u32 | `32` | RAM tier budget |
| `[kv].ssd_gib` | u32 | `1024` | SSD tier budget |
| `[kv].ssd_dir` | path | `/var/lib/cognitora/kv` | Cold tier files |
| `[kv].index_dir` | path | `/var/lib/cognitora/index` | RocksDB index |

Without `[security].require_mtls = true`, the daemon refuses to start on a
non-loopback `quic_listen` unless `allow_insecure_quic` is set. With mTLS,
issue certificates with the SAN above: `cgn-ctl pki bootstrap` includes it
by default, and `--san` replaces the defaults, so list it yourself there.

## Example

```toml
[cluster]
name = "prod"
etcd_endpoints = ["http://etcd:2379"]

[security]
require_mtls = true

[kv]
listen      = "0.0.0.0:7071"
quic_listen = "0.0.0.0:7072"
ram_gib     = 16
ssd_gib     = 512
ssd_dir     = "/var/lib/cognitora/kv/ssd"
index_dir   = "/var/lib/cognitora/kv/index"
```

## Dependencies

- **cgn-agent**: publishes residency / eviction events
- **Peer `cgn-kvcached` instances**: for cross-node fills

## Operational targets

See latency buckets in [KV tiering](../architecture/kv-tiering.md) and [SLOs](../operations/slo.md). Cold-cache playbook: [Runbook: Cache cold](../operations/runbooks/cache-cold.md).

## Related documentation

- [KV strategy](../architecture/kv-strategy.md)
- [Protocols (QUIC / federation)](../architecture/protocols.md)

**Source:** [`rust/services/cgn-kvcached/`](../../rust/services/cgn-kvcached/)
