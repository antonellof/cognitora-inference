# Runbook: agent stuck / engine not ready

**Symptoms**: `cgn_agent_engine_ready{node_id=...} == 0` for > 2 min,
the router is rejecting traffic for a model with `503 unavailable: no
live node serving model …`.

## Triage

1. Identify the offender:
   ```promql
   cgn_agent_engine_ready == 0
   ```
   then on the host:
   ```bash
   kubectl -n cognitora logs <agent-pod> --tail=500 --timestamps
   ```

2. Common signals:
   - `engine readiness probe timed out` → vLLM is still loading
     weights. Some models take 5+ min on first start. The agent will
     wait for `[agent].ready_timeout` (default 120s) and then
     restart the engine.
   - `engine exited with code N` → vLLM crashed. Look one frame
     deeper — usually OOM or a bad model spec.
   - `nvml: insufficient permissions` → the pod is missing the
     `nvidia.com/gpu` resource request or the agent's user lacks
     `video`/`render` group membership on the host.

3. Force-restart:
   ```bash
   kubectl -n cognitora delete pod <agent-pod>
   ```
   The DaemonSet recreates it; the engine reloads from cache. If
   weights are on object storage, pre-pull a copy to a local PVC to
   shorten future restarts.

4. Drain instead of bouncing:
   ```bash
   cgn-ctl cluster drain <node_id>
   ```
   Drain stops the router from picking the node and lets in-flight
   requests finish. Once the inflight count is 0, the agent is safe
   to restart.

## Prevention

- Pre-pull model weights (the operator's `ModelPool.spec.preload =
  true` does this).
- Set generous `livenessProbe.timeoutSeconds` on the agent for
  multi-tens-of-GB models.
- Run NVML telemetry alongside the engine — `cgn_agent_gpu_mem_used`
  near max for > 1 min is the canary for OOM crashes.
