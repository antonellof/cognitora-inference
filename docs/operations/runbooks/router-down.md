# Runbook: router not responding

**Symptoms**: external clients receive 502/504, or `curl
http://router:8080/healthz` hangs / errors.

## Triage

1. Check the deployment:
   ```bash
   kubectl -n cognitora get pods -l app.kubernetes.io/component=router
   ```
   - Pod `Running` and `Ready 1/1`? → continue to step 2.
   - Pod `CrashLoopBackOff` → step 4.
   - Pod stuck `Pending` → step 5.

2. Hit the admin port directly:
   ```bash
   kubectl -n cognitora port-forward svc/cognitora-router 9091:9091
   curl localhost:9091/healthz
   curl localhost:9091/metrics | grep cgn_router_admission_inflight
   ```
   - 200 + low inflight → 502 originates further upstream (LB,
     ingress). Check ingress controller and external LB.
   - 200 + inflight at `[router.admission].max_queue` → admission
     queue saturated. Step 6.

3. Tail logs:
   ```bash
   kubectl -n cognitora logs -l app.kubernetes.io/component=router --tail=200 --timestamps
   ```
   Common signals:
   - `etcd watch failure` → step 7.
   - `engine unreachable` (rare; agent-side) → look at the agent.

4. Crash loop. Pull the last log:
   ```bash
   kubectl -n cognitora logs <pod> --previous
   ```
   Mostly config errors at this stage:
   - `require_mtls=true but cert/key/ca not set` → check the `pki`
     Secret.
   - `failed to bind 0.0.0.0:8080` → another container is using the
     host port; restart the node.

5. Pod Pending. Usually a missing PVC or a node that no longer
   matches the nodeSelector. `kubectl describe pod <name>` is the
   single best signal.

6. Admission queue saturated. The router is healthy; downstream
   agents are slow.
   - Increase `[router.admission].max_queue` if it's a transient
     spike.
   - More common cause: an agent is stuck waiting on the engine.
     Check `cgn_agent_engine_ready` per node; restart the agent on
     the offender.

7. etcd watcher failed. The router will keep serving traffic from
   its last-good policy snapshot but won't observe new agents.
   ```bash
   ETCDCTL_API=3 etcdctl --endpoints=$ETCD endpoint health
   ```
   - If etcd is degraded → fix etcd first. The router stays up
     during the outage.
   - If etcd is healthy → check the network policy between the
     router pod and etcd. mTLS material may have rotated.

## Prevention

- Set `router.replicas=2` minimum in production.
- Set a PodDisruptionBudget with `minAvailable: 1`.
- Run a Prometheus alert on `cgn:router_p99_routing_us > 1500` for
  10 min — the routing fast-path is the canary for everything else.
