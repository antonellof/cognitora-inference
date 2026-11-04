# Runbook: cache hit ratio collapsed

**Symptoms**: `cgn:cache_hit_ratio_5m` < 0.30 on a multi-replica
model, p99 TTFT spikes, GPU utilisation low (no prefix reuse).

## Triage

1. Confirm it. No metric reports the hit rate, so read `overlap` from the
   router's routing decisions (`0` to `1`, logged at `info`):
   ```bash
   kubectl -n cognitora logs <router-pod> | jq -r 'select(.fields.overlap != null) | .fields.overlap' | sort -n | uniq -c
   ```
   Was the model recently deployed? Cold starts always look bad for
   the first ~5 min; give it a window and re-check.

2. Check sticky routing:
   ```promql
   cgn_cluster_node_queue_depth
   ```
   If the load is fanned out evenly across nodes for a high-prefix-
   reuse workload, the score weights are off:
   ```bash
   kubectl edit routingpolicy default
   # bump scoreWeights.kv to 0.7 (default 0.55), drop load to 0.15
   ```
   The router picks up the change in < 1 s via etcd.

3. Check kvcached health:
   Query the daemon's `Kv.Stats` RPC for block counts and hit rates per tier.
   - Block count near 0 → the daemon restarted and lost its in-memory
     state. Expected; warms back in a few minutes.
   - High miss rate on RAM with low miss rate on SSD → SSD is
     holding the working set but the eviction policy on RAM is too
     aggressive. Bump `[kv].ram_gib`.

4. Check that prompts actually share prefixes. The most common
   "false miss" is a client adding a per-request system prompt
   timestamp. If `overlap` stays near 0 for repeated prompts, compare the
   start of two prompts from your client.

5. Check tenancy and tokenizers. Tenants don't share prefixes by design, so
   the same prompt from two API keys is a miss unless they share a
   `tenant:` scope or `[router].kv_tenant_isolation` is off. If
   `cgn_router_tokenizer_fallback_total` is rising, configure
   `[models.*].tokenizer` for accurate chunking.

## Knobs to twist (in order of safety)

1. `routingpolicy.spec.scoreWeights.kv` ↑: favours nodes with the
   prefix already cached.
2. `[kv].ram_gib` ↑: bigger warm tier, fewer evictions.
3. `[kv].ssd_gib` ↑: bigger cold tier, fewer cold-disk fetches.
4. `[router.disagg].enabled = true` for prompts > 256 tokens: frees
   up decode-only nodes to retain their KV better.
5. (Last resort) Drop `[router.cascade]` if it's enabled; the
   cascade can fragment prefix-sharing across model tiers.
