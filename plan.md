# Cognitora — Plan

This is the canonical engineering plan. The high-level architecture
lives in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md); the reference
config in [`docs/reference/config.md`](docs/reference/config.md). This
file tracks **what** ships **when**, in what order, and what each
milestone is gated on.

## Pillars (no compromises)

1. **One language, one binary per concern.** All Rust. Six binaries.
   No Python, no Go, no JVM in any production container.
2. **mTLS everywhere by default.** External CA or `cgn-ctl pki
   bootstrap` for dev.
3. **etcd is the only coordination service.** Everything else is RAM
   or local disk.
4. **vLLM is just one engine.** The agent ships a stable `Engine`
   trait; SGLang / TRT-LLM / llama.cpp are tracked from day one.
5. **Performance gated in CI.** Each PR runs the perf harness against
   the targets in the README.
6. **Operator is optional.** Bare-metal first; K8s is just one
   deployment target via the Helm chart and operator.

## Milestones

### M1 — Single-node end-to-end (alpha) — ✅

* `cgn-router` (gateway + KV-aware routing for one node).
* `cgn-agent` (vLLM supervisor + NVML).
* `cgn-kvcached` (RAM tier + RocksDB index).
* `cgn-ctl pki bootstrap`, `install single-node`, `key create`.
* OpenAI HTTP/SSE for chat + completions.
* `cargo check --workspace` clean. CI fmt+clippy+build+test green.

**Exit gate**: `tests/e2e/single_node.sh` exercises the OpenAI surface
against a real vLLM instance and probes `/v1/models`, streaming chat,
and a buffered chat round-trip.

### M2 — Multi-node KV-aware routing (alpha → beta) — ✅

* etcd-backed `NodeRegistry` with TTL-based liveness via `lease_grant`
  + `lease_keep_alive` in `cgn-agent`'s health emitter.
* `arc_swap`-based hot reload of `RoutingPolicy` from etcd.
* RAM tier promotion across nodes via gRPC `Push/Pull`, backed by the
  QUIC frame codec in `cgn-kvcached::transport`.
* Helm chart, CRDs, operator reconciler.

**Exit gate**: 4-node cluster demo (`tests/e2e/multi_node_kv.sh`)
shows ≥ 0.55 cache hit ratio on a representative trace.

### M3 — Cross-node KV transport — ✅

* QUIC `Frame` codec end-to-end (header → block → ack) in
  `cgn-kvcached::transport`.
* RDMA transport gated behind `--features rdma` (Linux + ibverbs).
* SSD tier as file-per-block with atomic temp-file rename
  (`cgn_kv::SsdTier`); `io_uring` direct I/O is a future Linux-only
  optimisation tracked under M3 follow-ups.
* Prefill/decode disaggregation wired into `gateway::chat`: the router
  picks a `(prefill, decode)` pair via `routing::pick_pair`, runs the
  prefill phase against the prefill node, then proxies the decode
  phase with the resulting block list.

**Exit gate**: < 12 ms p99 1 MiB block fetch on a 10 GbE LAN.

### M4 — Cascade + multi-tenancy — ✅

* `cascade::Cascade::run` orchestrates SLM → Mid → LLM through
  `chat::completions`; falls back to single-shot when no cascade is
  configured.
* `cgn-ratelimit` ships an in-process governor by default and a
  `redis-backend` feature for multi-replica deployments (atomic Lua
  token bucket, one round-trip per request).
* OIDC verifier supports a group-claim → tenant-scope mapping
  (`OidcVerifier::with_group_scope_map`); accepts both array and
  space-separated string shapes for the groups claim.

**Exit gate**: 3-tenant fairness benchmark with strict-SLA +
best-effort classes; no SLA violations under contention.

### M5 — Operator GA + federation — ✅

* `cgn-operator` reconciles `InferenceCluster` (renders Deployment +
  DaemonSet + Service via server-side apply), `ModelPool` (writes a
  `cognitora-models-<name>` ConfigMap), and `RoutingPolicy` (publishes
  to `/cognitora/routing/policy` in etcd).
* Federation forwarder in `cgn-router::federation` picks the first
  reachable peer router and proxies the OpenAI request unchanged.

**Exit gate**: `helm upgrade` produces zero downtime in the e2e suite.

### M6 — Energy-aware autoscaler — ✅

* `autoscaler::spawn` runs a 30 s control loop. When the cluster's
  average load drops below `idle_util_pct` and a node's watts exceed
  `high_watt_threshold`, the loop writes `{drain: true}` into etcd at
  `/cognitora/autoscaler/<node_id>`; `cgn-operator` picks it up and
  scales the node's replica down. When traffic returns the same loop
  flips the hint back.
* `deadline::check` enforces per-tenant SLO admission: every request
  carries a deadline (explicit or derived from `[router.admission]
  ttft_slo` × `max_tokens`); requests whose estimated TTFT exceeds the
  deadline are rejected fast instead of starving behind a noisy queue.

**Exit gate**: ≥ 1.4× energy efficiency vs round-robin baseline on the
fixture trace.

## Operating principles

* **Frequent commits / push.** Small, well-scoped commits with
  conventional-commit-style headers. PRs are reviewed against the
  performance budget; performance regressions are rejected.
* **No unsafe code in any service.** `#![forbid(unsafe_code)]` is
  enforced at every binary crate root. Library crates that need it
  (`cgn-kv` for `io_uring`) gate the unsafe code behind a clearly named
  module and an internal RFC.
* **Distroless or bust.** Every release pushes multi-arch
  (amd64/arm64) distroless images to GHCR. The agent image is the
  single exception (sits on top of the official vLLM image).
* **One config tree.** `cgn-core::config::Config` is the single source
  of truth. Every section is documented in
  [`configs/cognitora.toml.example`](configs/cognitora.toml.example).
