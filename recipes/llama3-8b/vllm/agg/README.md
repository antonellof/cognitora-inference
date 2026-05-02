# Llama-3.1-8B · vLLM · aggregated

Single-node bring-up. One `cgn-router`, one `cgn-agent` driving
`vllm serve` with TP=1 and chunked prefill, one `cgn-kvcached` daemon
managing the host-RAM / SSD KV tier.

## GPU shape

| Resource | Required        |
| -------- | --------------- |
| GPUs     | 1 (any 24 GiB+) |
| Host RAM | 24 GiB+         |
| SSD      | 4 GiB cache dir |

## Bring up

```bash
bash recipes/llama3-8b/vllm/agg/up.sh
```

Equivalent:

```bash
cgn-ctl recipe up llama3-8b/vllm/agg
```

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-8b/vllm/agg
```

## Files

| File                  | Role                                                    |
| --------------------- | ------------------------------------------------------- |
| `router.toml`         | KV-aware router (HTTP :8080, gRPC :9090, admin :9091)  |
| `agent-llama3-8b.toml`| Single agent, vLLM driver on `:8001`                   |
| `kvcached.toml`       | Tiered KV daemon (UDS + QUIC peer fetch)               |
| `up.sh`               | 3-line wrapper over `recipes/_lib/recipe.sh`           |
