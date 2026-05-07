# GCP deployment

Three paths, in order of how cheap and fast they are to validate:

1. **Quickstart on GKE Autopilot** — single-pod, CPU-only, < 5 min to a
   public OpenAI-compatible URL. Best for "does this thing actually
   run on Kubernetes?". Verified end-to-end on
   `gke_cognitora_us-central1_cognitora-test`.
2. **Helm chart on GKE Standard with GPU** — production shape; one
   router replica + agent DaemonSet on a GPU node pool, your choice of
   engine.
3. **Terraform module** — same as (2) but described declaratively.
   Currently a single GKE Standard regional cluster + GPU pool; future
   work will fold the chart install in too.

## 1. Quickstart on GKE Autopilot (recommended for first-time
demos)

Costs roughly **$0.10 / hour** while running and tears down to zero.
No GPU required, no quotas, no Helm. The model — TinyLlama 1.1B —
gets downloaded by an init container on first boot.

```bash
# 0. One-time per project: enable APIs.
gcloud services enable container.googleapis.com compute.googleapis.com \
  --project YOUR_PROJECT

# 1. Create the cheapest possible Autopilot cluster (regional, no
#    per-cluster fee). Takes ~10 minutes.
gcloud container clusters create-auto cognitora-test \
  --project YOUR_PROJECT \
  --region us-central1 \
  --release-channel regular

# 2. Deploy the self-contained quickstart manifest. Brings up etcd +
#    llama.cpp engine + cgn-router + cgn-agent + cgn-metrics in one
#    Pod, fronted by a LoadBalancer Service.
kubectl apply -f deploy/kubernetes/quickstart/cognitora-cpu.yaml

# 3. Wait for the public IP (~30-90s) and the Pod to come up
#    (~3-5 min on first apply because of the model download).
kubectl -n cognitora wait --for=condition=ready pod \
  -l app=cognitora --timeout=10m
IP=$(kubectl -n cognitora get svc cognitora-router \
        -o jsonpath='{.status.loadBalancer.ingress[0].ip}')
echo "router: http://$IP"

# 4. Talk to it.
curl -sS http://$IP/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"tinyllama","messages":[{"role":"user","content":"What is 2+2?"}]}' \
  | jq

# 5. Stream the response.
curl -sN http://$IP/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"tinyllama","messages":[{"role":"user","content":"Hi"}],"stream":true}'

# 6. Tear down.
kubectl delete -f deploy/kubernetes/quickstart/cognitora-cpu.yaml
gcloud container clusters delete cognitora-test --region us-central1 --quiet
```

What the quickstart proves out:

- The full data plane: HTTP gateway → router → agent → engine over
  loopback, all in one Pod.
- KV-aware routing on a single node (degenerate but exercises the
  scoring path).
- `/v1/chat/completions` (buffered + streaming SSE) and `/v1/models`.
- `cgn-metrics` federation: `/federate` returns every router metric
  with a `cgn_target="router"` label injected.
- Prometheus scrape at `/metrics` and `/healthz` / `/readyz`.

What it does **not** do:

- No GPU, no vLLM / SGLang. The engine is `llama.cpp` running CPU.
  Swap the `engine` container in the manifest for a vLLM image and
  add a GPU node selector when you want to test GPU paths.
- No mTLS. `[security].require_mtls = false` to keep the bring-up
  trivial. For production use the Helm chart or terraform paths
  below.
- Single Pod, so disaggregation and cross-node KV transport are
  exercised but degenerate (prefill and decode resolve to the same
  agent).

> **Pin the image.** The manifest references
> `ghcr.io/antonellof/cognitora:latest`. The chat-template fix
> required for buffered chat completions to return non-empty content
> shipped in **v0.3.0**; pin to `:v0.3.0` (or newer) if you want a
> reproducible install.

## 2. Helm chart on GKE Standard with GPU

Production shape. One Router Deployment + Agent DaemonSet on a GPU
node pool.

```bash
gcloud container clusters create cognitora \
  --region us-central1 \
  --release-channel stable \
  --num-nodes 1 \
  --machine-type e2-standard-4

gcloud container node-pools create gpu-pool \
  --cluster cognitora --region us-central1 \
  --machine-type g2-standard-8 \
  --accelerator type=nvidia-l4,count=1 \
  --num-nodes 1 \
  --node-taints nvidia.com/gpu=true:NoSchedule \
  --node-labels nvidia.com/gpu.present=true

# Apply NVIDIA driver installer on GPU nodes.
kubectl apply -f https://raw.githubusercontent.com/GoogleCloudPlatform/container-engine-accelerators/master/nvidia-driver-installer/cos/daemonset-preloaded.yaml

helm install cognitora ./deploy/kubernetes/helm/cognitora \
  --namespace cognitora --create-namespace \
  --set router.replicas=1 \
  --set models.llama3-8b.tp=1
```

> **Caveat.** The Helm chart in `deploy/kubernetes/helm/cognitora/`
> currently expects you to bring your own engine container (e.g. via a
> sidecar Deployment) and has `[security].require_mtls = true` by
> default. We're tracking a chart redesign in plan.md to fold an
> optional engine sidecar in and disable mTLS by default for dev
> installs; until then the quickstart manifest above is the closest
> thing to a "one-command working install".

## 3. Terraform module

```bash
cd deploy/terraform/gcp
terraform init
terraform apply \
  -var="project=YOUR_GCP_PROJECT" \
  -var="region=us-central1" \
  -var="machine_type=g2-standard-8" \
  -var="gpu_type=nvidia-l4" \
  -var="node_count=2"
```

After ~10 minutes:

- GKE Standard cluster (`cognitora` by default), regional.
- GPU node pool with `nvidia.com/gpu.present=true` label and a
  `nvidia.com/gpu` taint.
- The Cognitora chart installed in the `cognitora` namespace.

The terraform module is intentionally minimal today — same caveat as
the Helm path: bring your own engine container.

## Sizing for GPU paths

| Workload      | Machine type        | GPU         | Notes                       |
|---------------|---------------------|-------------|-----------------------------|
| 7-13 B dev    | `g2-standard-8`     | 1× L4 24 GB | cheapest GPU on GKE         |
| 30-70 B       | `g2-standard-32`    | 4× L4       | TP=4                        |
| 100 B+        | `a2-ultragpu-1g`    | 1× A100 80G | bigger context, less TP     |
| Long-context  | `a3-highgpu-8g`     | 8× H100     | NVLink, fastest KV transfer |

L4 is currently the cheapest GPU SKU on GKE and a good fit for
serving 7-13 B models behind Cognitora.

## Storage

GKE provides ephemeral local SSD on the GPU node types. Configure
it as a `LocalSSDProvisioner` `StorageClass` and point
`kvcached.storageClassName` at it. Or use Filestore for a durable
cluster-wide SSD tier (slower than local NVMe but persistent).

## Ingress

GKE's GCE Ingress controller integrates well with the Helm chart:

```yaml
router:
  ingress:
    enabled: true
    className: gce
    host: api.cognitora.example.com
    annotations:
      networking.gke.io/managed-certificates: cognitora-cert
      kubernetes.io/ingress.global-static-ip-name: cognitora-ip
```

Pair it with a `ManagedCertificate` resource for free Let's Encrypt
TLS.

## Workload Identity

For agents that pull weights from GCS:

```yaml
agent:
  serviceAccountAnnotations:
    iam.gke.io/gcp-service-account: cognitora-agent@YOUR_PROJECT.iam.gserviceaccount.com
```

Grant that GSA `roles/storage.objectViewer` on the model bucket.

## Tear down

```bash
# Quickstart path:
kubectl delete -f deploy/kubernetes/quickstart/cognitora-cpu.yaml
gcloud container clusters delete cognitora-test --region us-central1 --quiet

# Helm / terraform paths:
helm uninstall cognitora -n cognitora
terraform destroy   # if you used the terraform module
```
