# AWS deployment

A reference EKS deployment with GPU nodes lives at
[`deploy/terraform/aws/`](../../../deploy/terraform/aws/). The module
provisions a VPC, an EKS control plane, a single managed nodegroup
of `g5.2xlarge` (or whatever you set for `instance_type`), and
applies the Cognitora Helm chart on top.

## What you need

- AWS account and credentials (`aws sso login` or static keys).
- Terraform 1.5+, kubectl, helm.
- A model artifact bucket — defaults to a HuggingFace Hub pull, but
  S3 (`s3://your-bucket/llama3-8b/`) cuts cold start to seconds.

## Apply

```bash
cd deploy/terraform/aws
terraform init
terraform apply \
  -var="region=us-east-1" \
  -var="cluster_name=cognitora-prod" \
  -var="instance_type=g5.2xlarge" \
  -var="node_count=2"
```

After ~12 minutes you'll have:

- An EKS cluster named `cognitora-prod` in the configured region.
- A managed nodegroup of `g5.2xlarge` (1× A10G per node) with the
  `nvidia.com/gpu.present=true` label and a `nvidia.com/gpu` taint.
- The Cognitora chart installed in the `cognitora` namespace.

## Sizing

| Workload                   | Suggested instance | Notes                                |
|----------------------------|--------------------|--------------------------------------|
| 7-13 B models, dev / staging | `g5.2xlarge`     | 1× A10G, 24 GB VRAM                  |
| 30-70 B models             | `g5.12xlarge`      | 4× A10G, TP=4                        |
| 100 B+ models              | `p4d.24xlarge`     | 8× A100, TP=8 (multi-instance pod)   |
| Long-context (>16k)        | `p5.48xlarge`      | H100, NVLink for KV transfer         |

The router scales separately — `router.replicas: 2` on
`m5.large` is plenty for tens of thousands of QPS because the routing
fast-path is sub-ms.

## Storage

`cgn-kvcached` SSD tier wants fast NVMe. On the GPU instance types
above, the local NVMe is exposed at `/dev/nvme1n1` — mount it at
`/var/lib/cognitora/kv/ssd` via a `local-storage`
`StorageClassName` in `values.yaml`:

```yaml
kvcached:
  ssdGib: 800             # 80% of the local NVMe on g5.2xlarge
  storageClassName: local-storage
```

For multi-AZ deployments use EBS gp3 (cheaper than io2; the SSD
tier doesn't need µs latency, the RAM tier handles that).

## Ingress

The Helm chart ships the `Ingress` resource opt-in. To expose the
OpenAI API behind an AWS ALB:

```yaml
router:
  service:
    type: ClusterIP
  ingress:
    enabled: true
    className: alb
    host: api.cognitora.example.com
    annotations:
      alb.ingress.kubernetes.io/scheme: internet-facing
      alb.ingress.kubernetes.io/target-type: ip
      alb.ingress.kubernetes.io/listen-ports: '[{"HTTPS":443}]'
      alb.ingress.kubernetes.io/certificate-arn: arn:aws:acm:...
```

ALB terminates TLS; the chart's mTLS still applies for
router↔agent traffic.

## IAM for the agent

If your model weights live in S3, add an IRSA-bound IAM role to the
agent ServiceAccount:

```yaml
agent:
  serviceAccountAnnotations:
    eks.amazonaws.com/role-arn: arn:aws:iam::123:role/cognitora-agent
```

The role needs `s3:GetObject` on your model bucket. Nothing else.

## Tear down

```bash
terraform destroy
```

This drops the chart first (Helm release as a Terraform resource),
then the cluster. EBS volumes detach but persist for one billing
cycle — `aws ec2 delete-volume` once you're sure.
