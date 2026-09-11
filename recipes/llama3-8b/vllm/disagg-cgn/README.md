# llama3-8b · vllm · disaggregated · cgn-kvcached

Single-node disaggregated topology (1× prefill GPU + 1× decode GPU) with
Cognitora's **`cgn-kvcached`** connector on the prefill worker.

* Prefill worker: `PdConnector(CognitoraConnector + NixlConnector)`.
  CognitoraConnector spills/reuses KV in host-local RAM/SSD tiers;
  NIXL streams freshly-produced blocks to the decode GPU.
* Decode worker: `NixlConnector` only.

## What gets injected

The recipe sets `kv_offload = "cgn"` and uses `[agent].role`:

```text
agent-prefill.toml  →  PdConnector(CognitoraConnector + NixlConnector)
agent-decode.toml   →  NixlConnector
```

Install the connector package:

```bash
pip install vllm
pip install ./python/cgn-kv-connector
```

## GPU requirements

* 2 × GPU with ≥ 16 GiB HBM (prefill on GPU 0, decode on GPU 1).
* vLLM with NIXL support for cross-GPU handoff.

## Quick start

```bash
pip install vllm
pip install ./python/cgn-kv-connector
bash recipes/llama3-8b/vllm/disagg-cgn/up.sh
```

Or:

```bash
cgn-ctl recipe up llama3-8b/vllm/disagg-cgn
```

## Tear down

```bash
bash scripts/run/down.sh
```

See also `agg-cgn/` (single-GPU) and `disagg-lmcache/` (LMCache variant).
