# Kubernetes guide

Cognitora ships as a Helm chart at
[`deploy/kubernetes/helm/cognitora/`](../../deploy/kubernetes/helm/cognitora/).

## Prerequisites

* Kubernetes 1.28+ with the [NVIDIA GPU operator](https://github.com/NVIDIA/gpu-operator)
  installed on every GPU node.
* Helm 3.13+.
* etcd reachable from the cluster (you can run it as a StatefulSet
  alongside Cognitora; future versions will package it).

## Install the CRDs

The CRDs are versioned independently of the chart so you can upgrade
the operator without re-running `helm install`:

```bash
kubectl apply -f deploy/kubernetes/crds/
```

## Install the chart

Pin the version in production. From the repo:

```bash
helm install cognitora ./deploy/kubernetes/helm/cognitora \
  --namespace cognitora --create-namespace \
  --set router.replicas=2 \
  --set agent.resources.limits."nvidia\.com/gpu"=1 \
  --set kvcached.ramGib=16
```

Or from the GHCR OCI repository:

```bash
helm install cognitora oci://ghcr.io/cognitora/charts/cognitora \
  --namespace cognitora --create-namespace --version 0.1.0
```

## Declarative model loading

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
  cascade:
    - llama3-8b           # try the SLM first
    - llama3-70b          # escalate when confidence drops
```

The operator translates the spec into `Agent.LoadModel` RPCs against
the agents that match `nodeSelector`.

## Routing policy

```yaml
apiVersion: cognitora.dev/v1alpha1
kind: RoutingPolicy
metadata:
  name: default
  namespace: cognitora
spec:
  scoreWeights: { kv: 0.7, load: 0.2, power: 0.05, capacity: 0.05 }
  admission: { maxQueue: 8192, ttftSloMs: 600 }
  cascade:   { enabled: true, confidenceThreshold: -1.2 }
```

Editing this resource updates the live router without restart — the
operator publishes the JSON to etcd at `/cognitora/routing/policy` and
`cgn-router`'s `arc_swap` watcher picks up the new weights inside a
second.

## Exposing the OpenAI surface

The chart's `router.service.type` defaults to `ClusterIP`. To expose
publicly:

```bash
helm upgrade cognitora ./deploy/kubernetes/helm/cognitora \
  --reuse-values \
  --set router.service.type=LoadBalancer
```

Or wire your existing IngressController:

```yaml
router:
  ingress:
    enabled: true
    className: nginx
    host: api.example.com
```

## Observability

Every binary exposes Prometheus on its admin port (`:9091` for
router/agent/kvcached, `:9092` for metrics). The chart ships a
`PodMonitor` (in `templates/podmonitor.yaml`, generated when the Prom
operator CRDs are present).
