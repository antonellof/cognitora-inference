# cgn-operator — Kubernetes controller

**kube-rs reconciler** that watches Cognitora CRDs and mirrors desired state into etcd (`/cognitora/*`) so `cgn-router` and `cgn-agent` behave the same as in bare-metal profiles — no second control plane.

## Overview

The operator translates declarative Kubernetes objects into the **same** routing policies and node expectations that `scripts/run/up.sh` profiles use. Helm installs the Deployment; CRDs ship under [`deploy/kubernetes/crds/`](../../deploy/kubernetes/crds/).

## CRDs

| CRD | File | Purpose |
|-----|------|---------|
| `InferenceCluster` | [`inferencecluster.yaml`](../../deploy/kubernetes/crds/inferencecluster.yaml) | Cluster identity, etcd endpoints, TLS/auth bootstrap hints |
| `ModelPool` | [`modelpool.yaml`](../../deploy/kubernetes/crds/modelpool.yaml) | Model deployments (engine, tensor parallelism, resources) |
| `RoutingPolicy` | [`routingpolicy.yaml`](../../deploy/kubernetes/crds/routingpolicy.yaml) | Score weights, admission, cascade / disagg toggles |

YAML shapes mirror the examples in [Kubernetes guide](../guides/kubernetes.md).

## Features

- Single reconciliation loop per CRD kind — edits propagate to etcd for live router reload
- Works alongside the packaged [**Helm chart**](../../deploy/kubernetes/helm/cognitora/) (`helm install … ./deploy/kubernetes/helm/cognitora` or published OCI chart)
- No Go runtime — pure Rust/Kubernetes dependency chain via **kube-rs**

## Architecture

`kubectl apply → Kubernetes API → cgn-operator watch → etcd (/cognitora/routing/policy, …) → cgn-router watches`.

## Example (`ModelPool` + `RoutingPolicy`)

```yaml
apiVersion: cognitora.dev/v1alpha1
kind: ModelPool
metadata:
  name: llama3-70b
  namespace: cognitora
spec:
  model: meta-llama/Meta-Llama-3-70B-Instruct
  tensorParallel: 4
  dtype: bfloat16
  replicas: 2
---
apiVersion: cognitora.dev/v1alpha1
kind: RoutingPolicy
metadata:
  name: default
  namespace: cognitora
spec:
  scoreWeights:
    kv: 0.55
    load: 0.25
    power: 0.10
    capacity: 0.10
  admission:
    maxQueue: 8192
    ttftSloMs: 600
```

Field names follow [`deploy/kubernetes/crds/`](../../deploy/kubernetes/crds/); use `kubectl explain modelpool.spec` after `kubectl apply -f deploy/kubernetes/crds/`.

## Dependencies

- **Kubernetes 1.28+**
- **etcd** reachable from the cluster (same requirement as bare metal)
- **cgn-router / cgn-agent** DaemonSets or Deployments charted to talk to that etcd

## Related documentation

- [Kubernetes guide](../guides/kubernetes.md)
- [Configuration reference](../reference/config.md)
- [Security architecture](../architecture/security.md)

**Source:** [`rust/services/cgn-operator/`](../../rust/services/cgn-operator/)
