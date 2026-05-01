# Routing — KV-aware scoring deep dive

The router's job is to pick the best node for each request in
sub-millisecond p99. "Best" is a four-term linear combination of
signals scraped from every live agent.

## The score

For every healthy node `n` that hosts the requested model, we
compute:

```
score(n) = w_kv  · overlap(prefix, n)
         + w_load · (1 − util(n))
         + w_pwr  · (1 − norm_watts(n))
         + w_cap  · capacity(n)
```

Each term is normalised to `[0, 1]`; the four weights are read live
from etcd at `/cognitora/routing/policy` (defaults `0.55, 0.25, 0.10,
0.10`) and applied via an `arc_swap::ArcSwap` so policy changes are
zero-cost on the hot path.

### `overlap(prefix, n)`

The router hashes the prompt's first ~64 tokens to a chain of
BLAKE3 digests (one per 16-token block). For each candidate node it
asks `cgn-kvcached` (over the local UDS) how many leading blocks are
present in the warm tier. The fraction of matched blocks is the
overlap signal. This is the single most-important term — it's why
KV-aware routing exists.

### `util(n)`

Each `cgn-agent` publishes its in-flight request count and total
GPU SM utilisation every `[agent].heartbeat` (default 5 s). The
router computes
`util = max(in_flight / max_concurrent, sm_pct)`
and uses `(1 − util)` so a busy node loses score linearly.

### `norm_watts(n)`

`cgn-metrics` exports `cgn_power_watts{component=...}` per host.
The router fetches a 30 s rolling average and normalises by the
fleet's max so an idle node with low watts wins, all else equal.
This is the only term whose default weight is small (`0.10`); turn
it up for energy-aware clusters.

### `capacity(n)`

Static metadata: TP size, dtype, VRAM, free model slots. Mostly
breaks ties when two nodes are equally busy and equally cached.

## Tie-break

When two nodes score within ε (`0.01` default) the router falls back
to a stable hash of `(node_id, prefix_hash)`. That keeps a
prefix-bound request hitting the same node across retries — the
stickiness is what makes KV-aware routing worth it under burst
traffic.

## Admission

After scoring, the router calls `Admission::try_admit(model, role)`.
The admission counter is per-(model, role) and bounded by
`[router.admission].max_queue`. A `Permit` is held for the lifetime
of the request and decrements on drop (RAII). When the queue is
full the router returns `503` immediately without calling the agent.

We deliberately don't queue — queueing inflates TTFT and the
client's deadline budget is more useful at the source. The queue
parameter is therefore a **hard cap on concurrency**, not a buffer.

## Cascade

Optional. When `[router.cascade].enabled = true` and the request's
model has a `cascade` chain configured, the router runs the request
against the smallest model first. After the response comes back, the
mean log-probability across emitted tokens is compared against
`[router.cascade].confidence_threshold`. If it's below threshold the
router escalates to the next model in the chain.

The score function above runs once per cascade step, so each step
goes to the best-suited node for *that* model. The cascade FSM lives
in `cgn-router/src/cascade.rs`; the gateway invokes it as a wrapper
around `routing::pick`.

## Disaggregation

Optional. When `[router.disagg].enabled = true` and the prompt
exceeds `colocate_below_tokens`, the router asks for **two** nodes:
a prefill agent that runs the first forward pass and emits KV blocks
into `cgn-kvcached`, and a decode agent that picks up those blocks
(via QUIC/RDMA) and streams tokens. The handshake adds one round
trip but pays for itself on long-prompt / short-completion workloads
because decode-only nodes can be smaller / cheaper.

The QUIC transport is the default; RDMA is available behind the
`rdma` feature flag on Linux hosts with ibverbs.

## Where the code lives

| File                                                  | Role                                  |
|-------------------------------------------------------|---------------------------------------|
| `rust/services/cgn-router/src/routing/score.rs`       | the score function above              |
| `rust/services/cgn-router/src/routing/selector.rs`    | `pick`: scoring + admission + tie     |
| `rust/services/cgn-router/src/admission.rs`           | per-(model,role) queue + RAII permit  |
| `rust/services/cgn-router/src/cascade.rs`             | cascade FSM                           |
| `rust/services/cgn-router/src/disagg.rs`              | prefill/decode plan                   |
| `rust/services/cgn-router/src/cluster/{registry,watcher}.rs` | etcd-backed node registry      |
