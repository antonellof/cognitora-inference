# Cognitora architecture

> One-page technical overview. For repo and crate layout see
> [`architecture/repo-layout.md`](architecture/repo-layout.md). For the
> full configuration reference see [`reference/config.md`](reference/config.md).

## 1. Topology

<p align="center">
  <img src="architecture.svg" alt="Cognitora architecture diagram" width="100%" />
</p>

Every box is a single statically-linked Rust binary. `cgn-metrics`,
`cgn-ctl`, and `cgn-operator` are shown elsewhere; they sit alongside
the four hot-path daemons above.

## 2. Hot path

A request travels:

1. **HTTP ingress** — `cgn-router` accepts an OpenAI-compatible request
   on `:8080`. Auth is checked by `cgn-auth` (API key in
   `Authorization: Bearer ...` or OIDC bearer with JWKS rotation).
   Rate limiting is applied by `cgn-ratelimit` keyed on the
   authenticated subject.
2. **Approximation** — the gateway hashes the prompt prefix to a
   BLAKE3 digest tree; this digest is the input to the routing score.
3. **Score** — for every healthy node that hosts the requested model,
   `routing::score` computes
   `total = w_kv·overlap + w_load·load + w_pwr·power + w_cap·capacity`
   using the policy from etcd (live-updated via `arc_swap`). The router
   selects the highest-scoring node; ties break on prefix hash.
4. **Admission** — `admission::try_admit` increments an inflight
   counter for the (model, role) pair; the request is rejected with
   429 if the global queue is full.
5. **Forward** — the gateway opens an `Agent.Generate` gRPC stream over
   mTLS. `cgn-agent` proxies the stream to the engine subprocess
   (`vLLM` today; pluggable through the `Engine` trait).
6. **Stream out** — tokens flow back through the gRPC stream and are
   re-encoded as OpenAI SSE chunks by `gateway::sse`.

For long prompts, the optional **disagg** path replaces step 5 with a
two-stage flow: a prefill agent runs the first forward pass and emits
KV blocks into `cgn-kvcached`; the blocks are pushed (QUIC/RDMA) to a
decode agent which streams the rest of the response.

## 3. Cluster state

`etcd` holds:

* `/cognitora/nodes/<node_id>` — `NodeHealth` JSON written by every
  `cgn-agent` every `[agent].heartbeat` (default 5 s). Stale entries
  are filtered out by `routing::pick`.
* `/cognitora/routing/policy` — score weights + admission tunables,
  written by `cgn-operator` from a `RoutingPolicy` CRD or by
  `cgn-ctl cluster set-policy` for non-K8s deploys.

There is no PostgreSQL, MySQL, or KV-store-of-the-week dependency:
etcd is the *only* coordination service.

## 4. KV cache

`cgn-kvcached` runs once per GPU host and exposes
`cognitora.v1.Kv` over gRPC + a QUIC transport. It owns three tiers:

* **GPU (hot)** — pinned by the agent in vLLM's KV pool. Lookups happen
  in-process; only the index lives in `cgn-kvcached`.
* **RAM (warm)** — a `RamTier` keyed by `BlockAddress` (model digest +
  layer). Eviction is approximate-LRU.
* **SSD (cold)** — block files under `/var/lib/cognitora/kv/ssd/`. The
  metadata index is RocksDB by default; an in-memory fallback exists
  for dev builds (see the `persistent-index` feature).

Cross-host fetches use QUIC (1-RTT, 0-RTT for re-tries on the same
peer). The wire format is binary `Frame { addr, model, layer, bytes }`
with bincode-encoded headers.

## 5. Power and energy

`cgn-power` reads:

* **Redfish** out-of-band power (chassis + per-PSU draw),
* **IPMI** as a fallback,
* **NVML** per-GPU power.

`cgn-metrics` exports those readings as `cgn_power_watts{component=...}`
gauges. The router subscribes to the metrics endpoint and incorporates
the result into the `power` term of the routing score, biasing requests
toward energy-efficient nodes when the operator turns the weight up.

## 6. Security

* **mTLS everywhere** by default. `cgn-tls` provides one helper for
  loading PEM material and another for generating dev PKI material on
  install (rcgen).
* **API auth**: API keys (sha256-hashed, hot-reloaded from disk) and
  OpenID Connect bearer tokens (validated against JWKS).
* **Distroless images** with `nonroot` UID; no shell in production
  containers.
* All commits land on a release branch only after `clippy -D warnings`,
  `cargo test`, and `helm lint` pass in CI.

## 7. Phased rollout

| Milestone | Scope                                                                |
|-----------|----------------------------------------------------------------------|
| **M1**    | Single node end-to-end: router, agent, kvcached, vLLM, OpenAI HTTP   |
| **M2**    | Multi-node KV-aware routing + RAM/SSD tiers + Helm chart             |
| **M3**    | QUIC/RDMA cross-node fetch + prefill/decode disagg                   |
| **M4**    | Cascade (SLM→Mid→LLM) + multi-tenant rate limit + OIDC SSO           |
| **M5**    | `cgn-operator` GA + multi-cluster federation                         |
| **M6**    | Energy-aware autoscaler + per-tenant SLO admission                   |

Today: M1 + M2 implementations land; M3-M6 are skeletons with TODO
markers. The `tests/perf/` harness gates every PR against the
performance targets in the README.
