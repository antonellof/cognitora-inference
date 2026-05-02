# Llama-3.1-8B · SGLang · aggregated

Single-GPU bring-up using SGLang's RadixAttention prefix cache. The
agent spawns `python -m sglang.launch_server` and the router treats it
exactly like any other OpenAI-compatible engine — no engine-specific
glue required.

## Why SGLang here?

SGLang's RadixAttention is a per-engine prefix cache that complements
Cognitora's *cross-node* prefix routing: the router still picks the
node with the longest cached prefix, and once routed, SGLang decides
which of *its own* in-process radix-tree nodes to reuse. The two
layers stack cleanly.

## GPU shape

| Resource | Required        |
| -------- | --------------- |
| GPUs     | 1 (any 24 GiB+) |
| Host RAM | 24 GiB+         |
| SSD      | 4 GiB cache dir |

## Bring up

```bash
bash recipes/llama3-8b/sglang/agg/up.sh
```

Equivalent:

```bash
cgn-ctl recipe up llama3-8b/sglang/agg
```

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-8b/sglang/agg
```
