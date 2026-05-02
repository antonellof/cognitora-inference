# Llama-3.1-8B · vLLM · disaggregated · single-node

Prefill and decode run as separate `cgn-agent`s on the same host, each
pinned to one GPU. The router routes prefill and decode independently
and hands KV blocks across the colocated `cgn-kvcached` daemon (UDS for
local fetch, QUIC for cross-node — unused here but configured for
symmetry with the multi-node variant).

## GPU shape

| Resource | Required        |
| -------- | --------------- |
| GPUs     | 2 (any 24 GiB+) |
| Host RAM | 48 GiB+         |
| SSD      | 16 GiB cache dir |

## Why disaggregate?

Prefill is compute-bound and bursty; decode is memory-bound and
steady. Running them on different GPUs lets each batch on its own
schedule, which usually trades a small per-token latency increase for
a 1.3-1.7× throughput win once the prompt mix has both short and long
sequences. See [`docs/architecture/routing.md`](../../../../docs/architecture/routing.md).

## Bring up

```bash
bash recipes/llama3-8b/vllm/disagg-single-node/up.sh
```

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-8b/vllm/disagg-single-node
```
