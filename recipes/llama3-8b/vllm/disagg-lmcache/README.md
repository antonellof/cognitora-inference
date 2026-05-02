# llama3-8b · vllm · disaggregated · LMCache

Single-node disaggregated topology (1× prefill GPU + 1× decode GPU) with
[LMCache](https://github.com/LMCache/LMCache) on the prefill worker.

* Prefill worker: `MultiConnector(LMCacheConnectorV1 + NixlConnector)`.
  LMCache handles cross-request reuse (long contexts, RAG, multi-turn),
  NIXL streams the freshly-produced KV from prefill GPU to decode GPU.
* Decode worker: `NixlConnector` only. It receives KV from prefill;
  there is nothing to offload.

This is the highest-throughput recipe in the tree: prefill amortises
across requests via LMCache, and decode runs unblocked on a dedicated
GPU.

## What gets injected

The recipe sets `kv_offload = "lmcache"` on both agents and lets the
`role` field drive what `cgn-agent` renders:

```text
agent-prefill.toml  →  --kv-transfer-config '{"kv_connector":"PdConnector","kv_role":"kv_both","kv_connector_extra_config":{"connectors":[{"kv_connector":"LMCacheConnectorV1","kv_role":"kv_both"},{"kv_connector":"NixlConnector","kv_role":"kv_both"}]}}'
agent-decode.toml   →  --kv-transfer-config '{"kv_connector":"NixlConnector","kv_role":"kv_both"}'
```

Mirrors NVIDIA Dynamo's [LMCache disaggregated
recipe](https://github.com/ai-dynamo/dynamo/blob/main/examples/backends/vllm/launch/disagg_lmcache.sh).

## GPU requirements

* 2 × GPU with ≥ 16 GiB HBM each (one for prefill, one for decode).
* `CUDA_VISIBLE_DEVICES=0,1` — recipe pins prefill to GPU 0, decode to
  GPU 1 via `[agent].gpu_index`.

## Quick start

```bash
pip install vllm lmcache
bash recipes/llama3-8b/vllm/disagg-lmcache/up.sh
```

Or:

```bash
cgn-ctl recipe up llama3-8b/vllm/disagg-lmcache
```

## Tear down

```bash
bash scripts/run/down.sh
```

See `agg-lmcache/README.md` for the recipe matrix.
