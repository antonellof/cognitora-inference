# Docker stack (Cognitora + Ollama)

The fastest way to run Cognitora **without building anything**: pull the
published [`ghcr.io/antonellof/cognitora`][image] image, point an agent at
your local Ollama, and serve OpenAI-compatible traffic through the
router.

[image]: https://github.com/antonellof/open-inference/pkgs/container/cognitora

This profile differs from [`local-mac/`](../local-mac/README.md): instead
of running native binaries via `scripts/run/up.sh`, it ships a
`compose.yaml` that boots etcd + router + agent as containers. The TOML
configs are otherwise the same shape.

## What runs

```
                                    Docker network 172.30.0.0/24
                                  ┌──────────────────────────────────┐
   curl :8080 ─────────────────►  │  cognitora-router  172.30.0.3   │
                                  │   :8080 OpenAI HTTP             │
                                  │   :9091 admin (/metrics, /healthz) │
                                  │   :7070 internal gRPC           │
                                  └─────────────┬────────────────────┘
                                                │ watches /cognitora/nodes/
                                                ▼
                                  ┌──────────────────────────────────┐
                                  │  cognitora-etcd    172.30.0.2   │
                                  │   :2379 client                  │
                                  └──────────────────────────────────┘
                                                ▲ heartbeat
                                                │ (15 s lease)
                                  ┌──────────────────────────────────┐
                                  │  cognitora-agent   172.30.0.4   │
                                  │   :7071 gRPC                    │
                                  │   engine.kind = openai_compat   │
                                  └─────────────┬────────────────────┘
                                                │ HTTP /v1/completions
                                                ▼
                                       host.docker.internal:11434
                                       (Ollama on the docker host)
```

## Prereqs

| Thing | Why |
| --- | --- |
| Docker (Desktop or Engine ≥ 20.10) | Compose v2 + `host-gateway` extra-host. |
| Ollama running on the host | `engine.kind = "openai_compat"` proxies to it. |
| `phi3:mini` pulled | Default model used by the demo. |

```bash
ollama serve &              # if it isn't already running
ollama pull phi3:mini
```

## One-shot bring up

```bash
cd examples/docker-ollama
docker compose up -d
```

This boots etcd → router → agent in dependency order and waits for
etcd's healthcheck before starting the rest. First run pulls the GHCR
image (~50 MB compressed; distroless).

Verify everything came up:

```bash
docker compose ps
docker compose logs --tail=20 router agent
```

## Drive it

```bash
bash examples/docker-ollama/demo.sh
```

The script exercises:

| Feature                       | Surface                          |
| ----------------------------- | -------------------------------- |
| Health probes                 | `GET :9091/healthz`, `/readyz`   |
| etcd node registration        | `etcdctl get --prefix /cognitora/nodes/` |
| Model listing                 | `GET /v1/models`                 |
| Chat completion (sync)        | `POST /v1/chat/completions`      |
| Chat completion (SSE stream)  | `stream: true` + `curl -N`       |
| Prometheus metrics            | `GET :9091/metrics`              |

Or fire individual curls:

```bash
curl http://localhost:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{
    "model": "phi3:mini",
    "messages": [{"role":"user","content":"Hello!"}]
  }'
```

## Tear down

```bash
docker compose down
```

This stops and removes every container plus the `cognitora` bridge
network. Volumes aren't used so nothing else needs cleaning up.

## Adding more models

The agent forwards `req.model` straight to Ollama, so any tag from
`ollama list` works. Pull the model, then add a `[models."<tag>"]` block
to **both** `router.toml` *and* `agent.toml`, and restart:

```bash
ollama pull llama3.2
# add `[models."llama3.2:latest"]\ntp = 1` to router.toml + agent.toml
docker compose restart router agent
```

To run **two distinct agents** behind one router (e.g. one per model),
duplicate the `agent` service in `compose.yaml`, give it a different
`container_name`, static IP (e.g. `172.30.0.5`), and host port, and point
it at a separate `agent-XX.toml`. The router will load-balance based on
its score function.

## Pinning a different image

```bash
COGNITORA_VERSION=v0.2.1 docker compose up -d
```

The image tag is templated in `compose.yaml` via `${COGNITORA_VERSION}`
with a `v0.2.0` fallback.

## Why the static IPs?

The agent publishes `agent.listen` to etcd verbatim (it's stored at
`/cognitora/nodes/<node_id>` under the `address` field). The router
reads that address and dials it directly over gRPC. If the agent binds
to `0.0.0.0`, the router will try to dial `0.0.0.0` and fail.

This profile fixes that by giving the agent a stable IP on a custom
subnet (`172.30.0.4`) and configuring `agent.listen = "172.30.0.4:7071"`
in `agent.toml`. The router lives at `172.30.0.3` and can reach the
agent over the bridge network.

## Limitations vs `local-mac/`

| | `local-mac/` (binaries) | `docker-ollama/` (this) |
| --- | --- | --- |
| `cgn-kvcached`           | included          | omitted (KV daemon optional) |
| mTLS / API-key auth      | toggle in TOML    | toggle in TOML (mount cert vol) |
| Multi-agent              | 2 agents wired up | 1 agent (extend `compose.yaml`) |
| Build required           | `cargo build`     | none — pulls GHCR image |
| Reload via etcd watches  | yes               | yes |

Both stacks talk to the same `cgn-router` and the same `cgn-agent`
binaries — only the deployment shape differs.
