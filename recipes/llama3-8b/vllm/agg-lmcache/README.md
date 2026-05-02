# llama3-8b · vllm · aggregated · LMCache

Single-GPU vLLM with [LMCache](https://github.com/LMCache/LMCache) as the
KV-offload backend. The vLLM worker spills cold KV blocks to host RAM (and
optionally disk / Redis / Mooncake) via `LMCacheConnectorV1`, so reuse
spans the entire prefix tree, not just the GPU's resident pages.

This is the recipe to pick when you want **prefill-once, reuse-everywhere**
behaviour on a single GPU and don't yet need disaggregation.

## What gets injected

The recipe sets `[engine].kv_offload = "lmcache"`. At spawn time
`cgn-agent` auto-renders:

```
--kv-transfer-config '{"kv_connector":"LMCacheConnectorV1","kv_role":"kv_both"}'
```

into the vLLM argv. Customise LMCache itself (chunk size, backends,
storage limits) via [LMCache's environment
variables](https://docs.lmcache.ai/api_reference/configurations.html) —
the most common ones are `LMCACHE_CHUNK_SIZE` and
`LMCACHE_MAX_LOCAL_CPU_SIZE`.

## GPU requirements

* 1 × GPU with ≥ 16 GiB HBM (Llama-3.1-8B FP16, `max_model_len = 8192`).
* Bumping `max_model_len` to 16k+ requires ~24 GiB.

## Quick start

```bash
pip install vllm lmcache                    # one-time
bash recipes/llama3-8b/vllm/agg-lmcache/up.sh
```

Or via the admin CLI:

```bash
cgn-ctl recipe up llama3-8b/vllm/agg-lmcache
```

## Tear down

```bash
bash scripts/run/down.sh
```

## Comparison

| Variant                    | Engine | Topology         | KV offload | Best for                                     |
|----------------------------|--------|------------------|------------|----------------------------------------------|
| `vllm/agg`                 | vLLM   | aggregated       | none       | Smallest possible footprint, no Python deps  |
| `vllm/agg-lmcache` (this)  | vLLM   | aggregated       | LMCache    | Long sessions, RAG, multi-turn chat          |
| `vllm/agg-kvbm`            | vLLM   | aggregated       | KVBM       | Parity with NVIDIA Dynamo's reference stack  |
| `vllm/disagg-lmcache`      | vLLM   | prefill ⇆ decode | LMCache+NIXL | Highest TTFT improvement, 2× GPU available |
| `sglang/agg`               | SGLang | aggregated       | none       | RadixAttention prefix cache only             |
| `sglang/agg-hicache`       | SGLang | aggregated       | HiCache    | SGLang shops needing GPU/Host tiering        |
