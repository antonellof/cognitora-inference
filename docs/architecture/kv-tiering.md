# KV cache tiering

A KV cache lookup sees three tiers in order: GPU (hot), RAM (warm),
SSD (cold). When all three miss, the router schedules a cross-node
fetch over QUIC or RDMA. This page describes the data structures,
the addressing scheme, and the eviction strategy.

## Block addressing

Every KV block is identified by `BlockAddress { digest, layer }`:

- `digest` — BLAKE3-256 of the (model, dtype, prefix tokens) tuple.
  Stable across nodes, so a block hashed on host A has the same
  address on host B.
- `layer` — the transformer layer index. We cache one block per
  layer so partial replays still benefit.

The address is 36 bytes; we pack it into a 32-byte RocksDB key by
hashing `layer` into the digest (zero collisions for any realistic
model depth).

## The three tiers

### GPU (hot)

Lives inside vLLM's KV pool, **not** owned by `cgn-kvcached`. The
agent reports a window of pinned addresses to `cgn-kvcached` so the
router's overlap query can answer "is this prefix on this GPU
right now?" without traversing the engine. When vLLM evicts a
block the agent emits a `block_evicted` event over the local UDS.

Lookup latency: <30 µs (in-process pointer table).

### RAM (warm)

`RamTier` in `cgn-kv`. A `DashMap<BlockAddress, Bytes>` plus an
approximate-LRU eviction list. Blocks land here when:

1. vLLM evicts from GPU and the agent calls `cgn-kvcached::Push`.
2. A peer node pulls from us and we keep a copy locally (mirror
   policy controlled by `[kv].mirror_pulls`).

Capacity is configured by `[kv].ram_gib`. When full we evict the
LRU block; the metadata stays in the index so later requests can
re-fetch from SSD or a peer.

Lookup latency: <200 µs target (see [SLOs](../operations/slo.md)).

### SSD (cold)

One file per block at `<[kv].ssd_dir>/<short(digest)>-<layer>.kvb`.
The naming embeds a 16-char hex prefix of the digest so even with
millions of blocks `ls` doesn't blow up.

Reads use `O_DIRECT` + `io_uring` for zero-copy DMA into a pinned
RAM buffer when the block is requested back to the GPU. The
`io_uring` plumbing lives behind `unsafe` code in `cgn-kv` and is
the **only** unsafe surface in the platform — it's gated behind a
named module, not generic helpers.

Lookup latency: <5 ms target.

## The index

A RocksDB column-family (`cf=kv`) maps `BlockAddress` →
`BlockMeta { model, layer, bytes, created_unix, last_seen_unix,
tier }`. The index is the source of truth for the warm and cold
tiers — RAM is just the cached bytes.

The index lives at `[kv].index_dir` and survives restarts. On boot,
`Store::open` walks the SSD tier and reconciles missing/stale
entries (a torn write on the previous shutdown might leave a block
file with no index entry; we discard those).

For dev hosts that can't compile rocksdb (macOS 15 SDK), the
`persistent-index` feature is off and the index is an in-memory
`DashMap`. Production builds always set it on (Linux containers).

## Cross-node fetch

When all three local tiers miss but the index says a peer has the
block, `cgn-kvcached` opens (or reuses) a QUIC connection to the
peer and asks for `Pull(addr)`. The wire format is a `Frame { addr,
model, layer, bytes }` with a bincode header, raw bytes payload.

QUIC features we lean on:

- **0-RTT for repeats** — a peer we've talked to in the last 30 s
  is a single round trip away.
- **Multi-stream multiplexing** — one connection, many in-flight
  block requests; head-of-line blocking is per-block, not per-peer.
- **mTLS-rooted peer auth** — the peer cert is verified against
  the same cluster CA used for gRPC.

When `--features rdma` is built, the same `Frame` codec runs over
GPUDirect RDMA (`ibv_post_send` / verbs). The transport choice
falls out of the agent's hardware report (`has_rdma_nic = true` →
prefer RDMA; otherwise QUIC). Both paths share the same higher-level
state machine.

## Eviction policy

| Tier | Policy            | Trigger                                 |
|------|-------------------|-----------------------------------------|
| GPU  | LRU (vLLM-owned)  | vLLM's pool reclaim; we don't override  |
| RAM  | approximate-LRU   | RAM tier reaches `[kv].ram_gib`         |
| SSD  | TTL + capacity    | block age > `[kv].ssd_ttl` or > `ssd_gib` |

Eviction is opportunistic — we **never** block a write to make room.
The LRU walks happen in a background tokio task at 1 Hz.

## Observability

| Metric                                    | Tier   |
|-------------------------------------------|--------|
| `cgn_kvcached_blocks{tier=ram\|ssd}`      | gauge  |
| `cgn_kvcached_bytes{tier=ram\|ssd}`       | gauge  |
| `cgn_kvcached_lookup_seconds{tier,outcome}` | histogram |
| `cgn_kvcached_evictions_total{tier}`      | counter |
| `cgn_kvcached_quic_pulls_total{outcome}`  | counter |

`cgn-router` joins these with its own `cgn_router_cache_hit_ratio`
to drive routing decisions.
