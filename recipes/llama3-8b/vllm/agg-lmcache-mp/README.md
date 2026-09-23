# llama3-8b · vllm · aggregated · LMCache MP

Single-GPU vLLM with [LMCache](https://docs.lmcache.ai/getting_started/quickstart.html)
running as its own process. `cgn-agent` starts `lmcache server`, waits for
`/healthcheck`, then launches vLLM with `LMCacheMPConnector`. KV tensors
move over CUDA IPC; the server keeps the L1 cache in CPU RAM.

This is the same shape as the [TensorMesh Dynamo integration](https://docs.tensormesh.ai/integrations/dynamo):
one cache server per node, workers attach with the MP connector, and
`PYTHONHASHSEED=0` so both processes hash tokens the same way. The
in-process alternative is [`agg-lmcache`](../agg-lmcache).

## What gets injected

```
lmcache server --host localhost --port 5555 --http-port 8080 \
  --l1-size-gb 20 --eviction-policy LRU --chunk-size 256

vllm serve ... --kv-transfer-config \
  '{"kv_connector":"LMCacheMPConnector","kv_role":"kv_both","kv_connector_extra_config":{"lmcache.mp.host":"localhost","lmcache.mp.port":5555}}'
```

`PYTHONHASHSEED=0` is set on both processes when the environment does
not already pin it.

On vLLM 0.20 or newer you can opt into LMCache's own connector:

```toml
[engine.lmcache]
connector_module_path = "lmcache.integration.vllm.lmcache_mp_connector"
```

Set `disable_prefix_caching = true` while checking that a repeated long
prompt reports `cached_tokens` from LMCache. Prompts shorter than
`chunk_size` (256) are not stored.

## GPU requirements

* 1 × GPU with ≥ 16 GiB HBM (Llama-3.1-8B FP16, `max_model_len = 8192`).
* About 20 GiB of host RAM for the L1 cache (`l1_size_gb`).

## Quick start

```bash
pip install vllm lmcache
bash recipes/llama3-8b/vllm/agg-lmcache-mp/up.sh
```

Confirm the server saw a store and a retrieve:

```bash
curl -s localhost:8080/metrics | grep '^lmcache_mp_'
```

## Kubernetes

The Helm chart can run the server as a DaemonSet and mount the connector
JSON into the vLLM sidecar. With `isolatedIPC: false` the worker pod
gets `hostIPC: true` and runs as root, which is what the CUDA IPC
handshake needs. See the chart README.
