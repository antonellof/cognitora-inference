# Bare-metal install (no Terraform)

For physical servers, the canonical install path is:

```bash
curl -sSfL https://get.cognitora.dev | sh
cgn-ctl pki bootstrap                       # dev PKI
cgn-ctl install baremetal                   # systemd units + config
systemctl enable --now cognitora.target
```

If you want declarative provisioning, the same flow works under
[Tinkerbell](https://tinkerbell.org/), Ironic/Metal3, MAAS, or
configuration-management tools (Ansible, Puppet, Chef). Sample Ansible
roles will land in `deploy/ansible/` after the v0.1 release.

## Topology guidance

| Role                       | Where it runs                         | Replicas               |
|----------------------------|---------------------------------------|------------------------|
| `cgn-router`               | Co-located with edge / load balancer  | ≥ 2 for HA             |
| `cgn-agent` + `cgn-kvcached` | Every GPU host                      | one of each per host   |
| `cgn-metrics`              | Anywhere reachable from BMC + Prom    | 1 per region           |
| `cgn-operator`             | Kubernetes-only                       | n/a for bare metal     |

## Requirements

* Linux 5.15+ kernel; `io_uring` and `O_DIRECT` enabled.
* NVIDIA driver 535+ on every GPU host. CUDA toolkit not required at the
  host level (vLLM ships its own).
* etcd reachable from every host (`/etc/cognitora/cognitora.toml`
  `[cluster] etcd = [...]`).
* Outbound to your model artifact store (HuggingFace, S3, OCI registry).
