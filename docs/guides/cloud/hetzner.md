# Hetzner deployment

Hetzner doesn't have a managed Kubernetes offering with first-class
GPU support today, so the reference module at
[`deploy/terraform/hetzner/`](../../../deploy/terraform/hetzner/)
provisions raw cloud servers and runs the Cognitora installer on
each. This is the cheapest production-grade path for self-hosting a
single-region cluster.

## Apply

```bash
cd deploy/terraform/hetzner
terraform init
terraform apply \
  -var="hcloud_token=YOUR_HETZNER_TOKEN" \
  -var="location=fsn1" \
  -var="router_count=1" \
  -var="gpu_count=2"
```

Provisions:

- `cognitora-router-N` cloud servers (default `ccx33`, 8 vCPU/32 GB)
  for the router and `cgn-metrics`.
- `cognitora-gpu-N` GPU servers (placeholder type `ccx53`; replace
  with your account's GPU SKU). The cloud-init runs the Cognitora
  installer end to end.

## After apply

```bash
ssh root@<router-ip>
systemctl status cgn-router

ssh root@<gpu-ip>
systemctl status cgn-agent cgn-kvcached
```

The installer brings up the systemd units; verify with
`curl http://localhost:9091/healthz` on each host.

Edit `/etc/cognitora/cognitora.toml` to point the routers at the
GPU hosts via etcd. A small etcd embedded in one of the router
boxes is fine for clusters < 50 GPUs:

```toml
[cluster]
etcd = ["10.0.0.5:2379"]
```

## Sizing

Hetzner's GPU lineup changes regularly — check
`hcloud server-type list` and pick a SKU with NVIDIA H100, A100, or
L40S. The Cognitora installer auto-detects the driver via NVML and
boots vLLM with the appropriate TP size.

## Networking

Hetzner Cloud's private networks (`hcloud network create`) are the
right place for the etcd, gRPC, and QUIC ports — the public IPs
should only carry the OpenAI HTTP traffic. The Terraform module
binds Cognitora's internal listeners to the private subnet.

## Tear down

```bash
terraform destroy
```
