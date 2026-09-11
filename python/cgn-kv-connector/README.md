# cgn-kv-connector

Experimental vLLM KV connector that offloads blocks to **`cgn-kvcached`**
(RAM + SSD tiers) instead of LMCache or KVBM.

## Install

```bash
pip install vllm grpcio protobuf
pip install ./python/cgn-kv-connector
```

## Configure vLLM

Use Cognitora's TOML knob (recommended):

```toml
[engine]
kind = "vllm"
kv_offload = "cgn"
```

Or pass directly:

```bash
vllm serve MODEL \
  --kv-transfer-config '{"kv_connector":"CognitoraConnector","kv_role":"kv_both","kv_connector_module_path":"cgn_kv_connector.connector"}'
```

## Environment

| Variable | Default | Purpose |
|----------|---------|---------|
| `CGN_KVCACHED_GRPC` | `127.0.0.1:7090` | gRPC address of local `cgn-kvcached` |
| `CGN_KVCACHED_UDS` | — | Unix socket override (`unix:///path`) |
| `CGN_KV_BLOCK_TOKENS` | `16` | Tokens per matched block in scheduler probe |

## Status

**Preview.** Worker-side tensor save/load is wired to `PutBlock`; full
layer-wise GPU↔host transfer and router digest handoff are still being
hardened. See `recipes/llama3-8b/vllm/agg-cgn/` for the reference stack.

## Regenerate protos

```bash
bash python/cgn-kv-connector/gen_proto.sh
```
