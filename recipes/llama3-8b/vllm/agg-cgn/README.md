# llama3-8b · vllm · aggregated · cgn-kvcached

Single-GPU vLLM with Cognitora's own **`cgn-kvcached`** tier as the
engine-side offload backend. The `CognitoraConnector` spills KV blocks
from vLLM into the host-local RAM/SSD tiers via gRPC `PutBlock`, and
probes resident prefixes through `BatchLookup` for scheduler-side reuse.

Unlike `agg-lmcache` (community LMCache) or `agg-kvbm` (NVIDIA KVBM),
this recipe keeps the full KV stack inside the Cognitora repo — useful
for benchmarking the router ↔ kvcached ↔ engine loop without external
Python dependencies beyond `cgn-kv-connector`.

## What gets injected

`[engine].kv_offload = "cgn"` makes `cgn-agent` render:

```
--kv-transfer-config '{"kv_connector":"CognitoraConnector","kv_role":"kv_both","kv_connector_module_path":"cgn_kv_connector.connector"}'
```

Install the connector package on the engine host:

```bash
pip install vllm
pip install ./python/cgn-kv-connector
```

Set `CGN_KVCACHED_GRPC=127.0.0.1:7090` (default) to match this recipe's
`kvcached.toml`.

## GPU requirements

* 1 × GPU with ≥ 16 GiB HBM.
* Optional NVMe for the SSD tier (`[kv].ssd_dir` in `kvcached.toml`).

## Quick start

```bash
pip install vllm
pip install ./python/cgn-kv-connector
bash recipes/llama3-8b/vllm/agg-cgn/up.sh
```

Or:

```bash
cgn-ctl recipe up llama3-8b/vllm/agg-cgn
```

## Tear down

```bash
bash scripts/run/down.sh
```
