# Kubernetes quickstart manifests

Self-contained, copy-pastable manifests that bring Cognitora up on
any Kubernetes cluster — no Helm chart, no PKI, no operator. They're
deliberately scoped to "prove the data plane works", not "run in
production".

## `cognitora-cpu.yaml`

Single Pod, CPU-only, llama.cpp + TinyLlama 1.1B. About **$0.10/hr**
on GKE Autopilot.

Containers in the Pod:

| Container | Image                                         | Listens                                |
|-----------|-----------------------------------------------|----------------------------------------|
| `etcd`    | `quay.io/coreos/etcd:v3.5.15`                 | `2379` (cluster client)                |
| `engine`  | `ghcr.io/ggml-org/llama.cpp:server`           | `8000` (OpenAI HTTP)                   |
| `router`  | `ghcr.io/antonellof/cognitora:latest`         | `8080` (HTTP), `9090` (gRPC), `9091` (admin) |
| `agent`   | `ghcr.io/antonellof/cognitora:latest`         | `127.0.0.1:7070` (gRPC, loopback only) |
| `metrics` | `ghcr.io/antonellof/cognitora:latest`         | `9092` (`/federate`)                   |

Plus an `initContainer` (`curlimages/curl`) that downloads the GGUF
into a shared `emptyDir` on first boot.

A `LoadBalancer` Service exposes the router's HTTP surface on port
`80`; a `ClusterIP` Service exposes the router's `/metrics` and the
metrics aggregator's `/federate`.

### Use it

```bash
kubectl apply -f deploy/kubernetes/quickstart/cognitora-cpu.yaml

# wait for everything to be Ready (model download + warmup ≈ 3-5 min)
kubectl -n cognitora wait --for=condition=ready pod \
  -l app=cognitora --timeout=10m

# get the public IP (give your cloud provider ~30-90s to allocate one)
IP=$(kubectl -n cognitora get svc cognitora-router \
       -o jsonpath='{.status.loadBalancer.ingress[0].ip}')
echo "router: http://$IP"

# OpenAI-compatible chat completion
curl -sS http://$IP/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"tinyllama","messages":[{"role":"user","content":"hi"}]}' | jq

# streaming SSE
curl -sN http://$IP/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"tinyllama","messages":[{"role":"user","content":"hi"}],"stream":true}'

# /v1/models
curl -sS http://$IP/v1/models | jq

# Prometheus federation endpoint
kubectl -n cognitora port-forward svc/cognitora-metrics 9092:9092 &
curl -sS http://127.0.0.1:9092/federate
```

### Tear down

```bash
kubectl delete -f deploy/kubernetes/quickstart/cognitora-cpu.yaml
```

### Verified on

- **GKE Autopilot** (`us-central1`, regular release channel) — boots
  in 3-5 min from `kubectl apply` to a working LoadBalancer IP.
- Should also work on EKS, AKS, k3d, kind, and Docker Desktop. On
  local clusters the LoadBalancer Service stays `Pending`; use
  `kubectl -n cognitora port-forward svc/cognitora-router 8080:80`
  instead.

### Promoting to production

This manifest is the absolute minimum. To go to a real deployment:

1. Replace the engine container with vLLM / SGLang and add a GPU
   node selector + `nvidia.com/gpu` resource request.
2. Move the `agent` to its own DaemonSet on the GPU pool, and the
   `router` to its own Deployment with `replicas: 2+`.
3. Set `[security].require_mtls = true` and either bring your own
   PKI material or use `cgn-ctl pki bootstrap` + `cert-manager`.
4. Front the router with an Ingress + ManagedCertificate (GKE) or
   ALB / TLS termination from your provider, instead of a raw
   LoadBalancer.
5. Switch from a single etcd container to a managed etcd cluster
   (e.g. `etcd-operator` or hosted etcd).

The Helm chart at `deploy/kubernetes/helm/cognitora/` is the path
once you outgrow this manifest; the chart redesign that folds an
optional engine sidecar in is tracked in `plan.md`.

## Pinning the image

The manifest references `ghcr.io/antonellof/cognitora:latest` for
ergonomics. For reproducible installs pin to a specific release —
the chat-template fix that makes buffered chat completions return
non-empty content shipped in **v0.3.0**, so anything older won't
work for `/v1/chat/completions`:

```bash
sed -i.bak 's|cognitora:latest|cognitora:v0.3.0|g' \
  deploy/kubernetes/quickstart/cognitora-cpu.yaml
```
