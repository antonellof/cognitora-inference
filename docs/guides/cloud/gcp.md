# GCP deployment

The reference GKE module is at
[`deploy/terraform/gcp/`](../../../deploy/terraform/gcp/). It
provisions a regional GKE cluster, a GPU node pool (`g2-standard-8`
with 1× L4 by default), and applies the Cognitora chart.

## Apply

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

## Sizing

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
terraform destroy
```
