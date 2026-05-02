# llama3-8b · vllm · aggregated · KVBM

Single-GPU vLLM with [KVBM](https://github.com/ai-dynamo/dynamo) — NVIDIA
Dynamo's KV Block Manager — as the offload backend. KVBM tiers blocks
across GPU HBM (G1) → pinned host RAM (G2) → NVMe/SSD (G3) → object
storage (G4) using NIXL underneath, and is the offload story Dynamo
ships in its own reference recipes.

This recipe exists primarily for **head-to-head benchmarking against
Dynamo**. For most production workloads `agg-lmcache` is simpler and
gives comparable results.

## What gets injected

`[engine].kv_offload = "kvbm"` makes `cgn-agent` render:

```
--kv-transfer-config '{"kv_connector":"DynamoConnector","kv_role":"kv_both","kv_connector_module_path":"kvbm.vllm_integration.connector"}'
```

into the vLLM argv. KVBM expects an etcd reachable at
`localhost:2379` (the recipe's embedded etcd is fine) and the `kvbm`
Python package on the engine host:

```bash
pip install kvbm vllm
```

## GPU requirements

* 1 × GPU with ≥ 16 GiB HBM.
* Optional NVMe SSD for the G3 tier — KVBM falls back to host RAM only
  if absent.

## Quick start

```bash
pip install vllm kvbm
bash recipes/llama3-8b/vllm/agg-kvbm/up.sh
```

Or:

```bash
cgn-ctl recipe up llama3-8b/vllm/agg-kvbm
```

## Tear down

```bash
bash scripts/run/down.sh
```
