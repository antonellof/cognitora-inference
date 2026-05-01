# Azure deployment

AKS module: [`deploy/terraform/azure/`](../../../deploy/terraform/azure/).
Provisions an AKS cluster with a GPU node pool
(`Standard_NC8as_T4_v3` by default) and applies the Helm chart.

## Apply

```bash
cd deploy/terraform/azure
terraform init
terraform apply \
  -var="subscription_id=YOUR_AZURE_SUBSCRIPTION_ID" \
  -var="location=eastus" \
  -var="cluster_name=cognitora" \
  -var="vm_size=Standard_NC8as_T4_v3" \
  -var="node_count=2"
```

After ~12 minutes:

- AKS cluster `cognitora` in the configured location.
- GPU node pool with the `nvidia.com/gpu.present=true` label and
  the `nvidia.com/gpu=true:NoSchedule` taint.
- Cognitora chart installed.

## Sizing

| Workload        | VM size                        | GPU                | Notes        |
|-----------------|--------------------------------|--------------------|--------------|
| 7-13 B dev      | `Standard_NC8as_T4_v3`         | 1× T4 16 GB        | cheap GPU    |
| 30-70 B         | `Standard_NC24ads_A100_v4`     | 1× A100 80G        | TP=1         |
| 100 B+          | `Standard_ND96asr_v4`          | 8× A100 40G        | TP=8         |
| Long-context    | `Standard_ND96isr_H100_v5`     | 8× H100            | NVLink       |

You'll need to request quota for the H100 / A100 SKUs; T4 is
usually available immediately.

## Ingress

AKS pairs naturally with Application Gateway:

```yaml
router:
  ingress:
    enabled: true
    className: azure-application-gateway
    host: api.cognitora.example.com
    annotations:
      appgw.ingress.kubernetes.io/ssl-redirect: "true"
      appgw.ingress.kubernetes.io/use-private-ip: "false"
```

Or use NGINX ingress controller if you prefer cloud-portable config.

## Identity

Azure AD Workload Identity for agents pulling from Azure Blob
Storage:

```yaml
agent:
  serviceAccountAnnotations:
    azure.workload.identity/client-id: <UAMI client id>
  podLabels:
    azure.workload.identity/use: "true"
```

Federate the UAMI with the AKS OIDC issuer; grant it
`Storage Blob Data Reader` on the model container.

## Tear down

```bash
terraform destroy
```
