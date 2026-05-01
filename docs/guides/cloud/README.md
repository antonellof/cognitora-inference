# Cloud guides

Reference deployments for each major cloud, paired with the
matching Terraform module under
[`deploy/terraform/<cloud>/`](../../../deploy/terraform/).

| Cloud   | Managed K8s? | Recommended GPU SKU       | Guide                  |
|---------|--------------|---------------------------|------------------------|
| AWS     | EKS          | `g5.2xlarge` (A10G 24G)   | [aws.md](aws.md)       |
| GCP     | GKE          | `g2-standard-8` (L4 24G)  | [gcp.md](gcp.md)       |
| Azure   | AKS          | `Standard_NC8as_T4_v3`    | [azure.md](azure.md)   |
| Hetzner | no (raw VMs) | varies (CCX series)       | [hetzner.md](hetzner.md) |

Bare metal is covered separately in
[../baremetal.md](../baremetal.md).
