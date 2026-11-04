# Routing: KV-aware scoring deep dive

The router's job is to pick the best node for each request in
sub-millisecond p99. "Best" is a four-term linear combination of
signals from every live agent.

## The score

For every ready node `n` serving the requested model or adapter, we
compute:

```
score(n) = w_kv   · overlap(prefix, n)
         + w_load · load(n)
         + w_pwr  · power(n)
         + w_cap  · capacity(n)
         + w_session        (only on the session's preferred node)
```

The four main terms are normalised to `[0, 1]`. Weights are read live
from etcd at `/cognitora/routing/policy` (defaults `0.55, 0.25, 0.10,
0.10`, session `0.25`) and applied via an `arc_swap::ArcSwap`, so policy
changes are zero-cost on the hot path.

Before scoring, candidates are filtered: nodes that are cordoned,
draining, not ready, or excluded by a failed attempt are removed, as are
nodes failing `min_vram_mb` / `require_gpu`. If any candidate is under
its `watt_limit`, nodes over theirs are removed too.

### Tokens and digests

The router tokenizes the prompt with the model's `tokenizer.json`
(`[models.*].tokenizer` or `tokenizer_repo`). Without one it estimates:
one token per CJK character, otherwise one per four characters of each
word. `cgn_router_tokenizer_fallback_total{model}` counts estimates.

Tokens are hashed into a chain of BLAKE3 digests, one per 32-token
chunk, over the whole prompt. The salt is the model, plus the tenant
when `[router].kv_tenant_isolation` is on, so tenants never match each
other's prefixes. A LoRA adapter is requested as its own model, so its
digests stay separate from the base model's. Image parts contribute a
digest of their URL, so prompts differing only by image don't match.

The hashed text is the joined messages, not the model's chat template,
so chunk boundaries approximate what the engine caches rather than
matching it exactly.

### `overlap(prefix, n)`

For each candidate the router walks the in-memory prefix index to find
the longest contiguous cached prefix, as a fraction of the prompt's
chunks. The index is fed by confirmed-KV etcd keys, optimistic
post-dispatch inserts, and agent-reported `kv_epoch` / eviction-burst
pruning. This is the single most-important term; it's why KV-aware
routing exists.

**This is local scoring only.** The router does not call `cgn-kvcached`
on the hot path, and it does not consume engine block-hash events.

### `load(n)`

`1 − queue / 64`, clamped to `[0, 1]`. `queue` is the larger of the
engine queue depth from the last heartbeat and this router's in-flight
count for the node. Heartbeats arrive every 5 s, so the live count is
what spreads a burst. Other routers' traffic is visible only through
the heartbeat.

### `power(n)`

`1 − watts / max_watts`, using the node power reported in heartbeats and
the highest value among the candidates. An idle low-power node wins, all
else equal. Turn the weight up for energy-aware clusters.

### `capacity(n)`

`free_blocks / total_blocks` from engine telemetry, or 0 for engines
that don't report it.

## Session affinity

`x-session-id`, or the `user` field, names a conversation. The candidate
with the highest `H(session, node)` (rendezvous hashing) gets the
`session` bonus, so removing a node only moves the sessions it owned.
It is a bonus rather than a pin: a busy node can still lose.

## Tie-break

The first highest scorer wins. Ties are rare in practice because the
in-flight count separates identical nodes as soon as one takes work.

## Admission

`Admission::admit` takes a slot on the HTTP and gRPC paths before
routing. Slots are per-(model, role), capped by
`[router.admission].max_queue`, and held by an RAII `Permit` for the
life of the request.

At the cap, requests park in a per-tenant queue
(`wait_queue_per_tenant`) for up to `wait_timeout`. A released slot goes
to the next tenant in rotation, so a tenant with a large backlog can't
starve the others. A full queue or an expired wait returns `429`.
Rotation counts requests, not tokens, so tenants sending long prompts
still get more compute.

When `[router.autoscaler].deadline_admission = true`, requests whose
estimated TTFT exceeds their deadline are rejected after routing. The
deadline comes from `x-cgn-deadline-ms`, else the configured SLO.

## Retries

The gateway waits for the first token before committing to a response.
Until then it makes up to three attempts, excluding each failed node.
It retries connection failures, engine errors, 429 and 503 from the
engine, empty streams, and silence past `first_token_timeout`. Client
errors such as a 400 from the engine are returned as they are.

After the first token, nothing retries: the client has already seen
output. Mid-stream migration is not implemented.

## Cascade

Optional. When `[router.cascade].enabled = true` and the request's
model has a `cascade` chain configured, the router runs the request
against the smallest model first and asks the engine for logprobs. The
mean logprob of the answer is compared against
`[router.cascade].confidence_threshold`; below it, or on failure, the
router escalates to the next model.

Early steps always run buffered, because confidence needs the whole
answer, so a cascade adds their full latency to TTFT. Only the last step
streams live. Engines that report no logprobs fall back to a length
heuristic, shown by `cgn_router_cascade_confidence_total{source}`.
Requests with `tools`, `tool_choice` or `response_format` skip the
cascade, since they must run on the model the client named.

The score function above runs once per cascade step, so each step
goes to the best-suited node for *that* model.

## Disaggregation

Optional. When `[router.disagg].enabled = true` and the prompt
exceeds `colocate_below_tokens`, the router asks for **two** nodes:
a prefill agent and a decode agent. Moving KV between them is the
engine's job, through its NIXL or LMCache connector, configured by
`[engine].kv_offload`.

**This path is unproven.** `cgn-agent` does not act on `prefill_only`
yet, and no benchmark covers it. See [`plan.md`](../../plan.md).

## Where the code lives

| File                                                  | Role                                  |
|-------------------------------------------------------|---------------------------------------|
| `rust/services/cgn-router/src/routing/score.rs`       | the score function above              |
| `rust/services/cgn-router/src/routing/selector.rs`    | `pick`: filtering, scoring, sessions  |
| `rust/services/cgn-router/src/routing/tokenize.rs`    | per-model tokenizers                  |
| `rust/services/cgn-router/src/gateway/generate.rs`    | admission, dispatch, retry, streaming |
| `rust/services/cgn-router/src/admission.rs`           | slots, tenant wait queues, RAII permit |
| `rust/services/cgn-router/src/cascade.rs`             | cascade FSM                           |
| `rust/services/cgn-router/src/disagg.rs`              | prefill/decode plan                   |
| `rust/services/cgn-router/src/cluster/{registry,watcher}.rs` | etcd-backed node registry      |
