# Cognitora Helm chart

Deploys the Cognitora inference stack on Kubernetes: the `cgn-router`
OpenAI-compatible gateway, a `cgn-agent` DaemonSet (with an optional
inference-engine sidecar), the KV cache daemon, the metrics aggregator,
and the CRD operator.

## Install

The chart's defaults are dev-safe: no mTLS, no auth, CPU-friendly
resources, agent scheduled on every node. A plain install works on any
cluster:

```sh
helm install cognitora deploy/kubernetes/helm/cognitora -n cognitora --create-namespace
```

By default no engine runs — the agent proxies to `agent.engine.url`
(`http://127.0.0.1:8000`). For a working end-to-end stack either enable
the engine sidecar (below) or point `agent.engine.url` at an
OpenAI-compatible engine you already run.

For the cheapest possible smoke test of the full data plane, see the
single-pod manifest at `deploy/kubernetes/quickstart/cognitora-cpu.yaml`
(no Helm required).

## Engine sidecar

Set `agent.engine.enabled=true` to run the inference engine as a
sidecar container inside every agent pod. The sidecar owns the engine
process; the rendered config always pins `[engine] kind =
"openai_compat"` and `url = http://127.0.0.1:<agent.engine.port>`, so
the agent proxies over localhost and never spawns anything.

### llama.cpp server on CPU

```yaml
# values-llamacpp.yaml
agent:
  engine:
    enabled: true
    image: ghcr.io/ggml-org/llama.cpp:server
    command: ["/app/llama-server"]
    args:
      - "-m"
      - "/models/model.gguf"
      - "--host"
      - "0.0.0.0"
      - "--port"
      - "8000"
      - "-c"
      - "4096"
      - "--alias"
      - "mymodel"
    port: 8000
    resources:
      requests: { cpu: "2", memory: "4Gi" }
      limits:   { cpu: "4", memory: "8Gi" }
```

```sh
helm install cognitora deploy/kubernetes/helm/cognitora \
  -n cognitora --create-namespace -f values-llamacpp.yaml
```

The GGUF must be present at the path given in `args` — bake it into an
image, mount a PVC, or add an init container (the quickstart manifest
shows a curl-based downloader).

### vLLM on GPU

```yaml
# values-vllm.yaml
agent:
  nodeSelector:
    nvidia.com/gpu.present: "true"
  tolerations:
    - key: nvidia.com/gpu
      operator: Exists
      effect: NoSchedule
  engine:
    enabled: true
    image: vllm/vllm-openai:latest
    args:
      - "--model"
      - "meta-llama/Meta-Llama-3-8B-Instruct"
      - "--port"
      - "8000"
      - "--tensor-parallel-size"
      - "1"
    port: 8000
    env:
      - name: HUGGING_FACE_HUB_TOKEN
        valueFrom:
          secretKeyRef: { name: hf-token, key: token }
    resources:
      requests: { cpu: "4", memory: "24Gi", nvidia.com/gpu: 1 }
      limits:   { memory: "32Gi", nvidia.com/gpu: 1 }
```

```sh
helm install cognitora deploy/kubernetes/helm/cognitora \
  -n cognitora --create-namespace -f values-vllm.yaml
```

### External engine (no sidecar)

Leave `agent.engine.enabled=false` and set the URL:

```sh
helm install cognitora deploy/kubernetes/helm/cognitora \
  -n cognitora --create-namespace \
  --set agent.engine.url=http://my-vllm.inference.svc:8000
```

## Enabling mTLS

`security.require_mtls` defaults to `false` so a fresh install never
waits on PKI material. To turn it on:

```sh
# 1. Mint a CA and leaf certs
cgn-ctl pki bootstrap --out ./pki

# 2. Ship them as a secret
kubectl -n cognitora create secret generic cognitora-pki \
  --from-file=ca.crt=./pki/ca.crt \
  --from-file=leaf.crt=./pki/leaf.crt \
  --from-file=leaf.key=./pki/leaf.key

# 3. Install (or upgrade) with mTLS on
helm upgrade --install cognitora deploy/kubernetes/helm/cognitora \
  -n cognitora \
  --set security.require_mtls=true \
  --set pki.existingSecret=cognitora-pki
```

When `require_mtls` is true the chart mounts the PKI secret into the
router and agent pods and renders the `ca_file` / `cert_file` /
`key_file` paths into `[security]` in `cognitora.toml`.

## Notable values

| Key | Default | Meaning |
|-----|---------|---------|
| `security.require_mtls` | `false` | mTLS between components; needs `pki.existingSecret` when true. |
| `pki.existingSecret` | `""` | Secret with `ca.crt`, `leaf.crt`, `leaf.key` (see `cgn-ctl pki bootstrap`). |
| `cluster.etcdEndpoints` | `[]` | etcd for cluster state; empty = single-router dev mode. |
| `agent.engine.enabled` | `false` | Render the engine sidecar in the agent DaemonSet. |
| `agent.engine.image` | `""` | Sidecar image; required when the sidecar is enabled. |
| `agent.engine.port` | `8000` | Sidecar port; agent proxies to `http://127.0.0.1:<port>`. |
| `agent.engine.kind` / `agent.engine.url` | `openai_compat` / `http://127.0.0.1:8000` | Used only when the sidecar is disabled. |
| `agent.hostNetwork` | `false` | Host networking for the agent pod (KV QUIC transport). |
| `router.service.type` | `ClusterIP` | Set `LoadBalancer` for a public OpenAI endpoint. |

Auth for the public OpenAI surface (`auth.enabled`, OIDC / API keys) and
observability extras (`metrics.dashboards`, `metrics.prometheusRule`)
are documented inline in `values.yaml`.
