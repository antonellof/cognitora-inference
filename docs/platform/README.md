# Platform components

Cognitora ships **six** statically linked binaries. Four sit on the inference hot path (router, agent, KV cache daemon, metrics); **`cgn-ctl`** is the admin CLI; **`cgn-operator`** reconciles Kubernetes CRDs into etcd state the daemons already consume.

Each page follows the same layout as the [marketing site platform section](https://cognitora.dev): overview, features, architecture, configuration highlights, an example, dependencies, and links deeper into this repo.

| Binary | Role |
|--------|------|
| [`cgn-router`](cgn-router.md) | OpenAI-compatible HTTP gateway, KV-aware routing, admission, federation |
| [`cgn-agent`](cgn-agent.md) | Per-node engine supervisor (vLLM / SGLang / llama.cpp / OpenAI-compat proxy) |
| [`cgn-kvcached`](cgn-kvcached.md) | Multi-tier KV block store + cross-node QUIC fetch |
| [`cgn-metrics`](cgn-metrics.md) | Prometheus scrape fan-in + power telemetry for routing scores |
| [`cgn-ctl`](cgn-ctl.md) | Cluster ops, PKI, keys, recipes, install/render helpers |
| [`cgn-operator`](cgn-operator.md) | kube-rs controller for InferenceCluster, ModelPool, RoutingPolicy |

Authoritative TOML schema: [`configs/cognitora.toml.example`](../../configs/cognitora.toml.example) and [`docs/reference/config.md`](../reference/config.md). Source: [`rust/services/`](../../rust/services/).

## See also

- [Architecture overview](../ARCHITECTURE.md)
- [Repo layout](../architecture/repo-layout.md)
- [Configuration reference](../reference/config.md)
