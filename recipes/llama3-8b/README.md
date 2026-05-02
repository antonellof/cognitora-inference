# Llama-3.1 8B recipes

Production-ready bring-up profiles for `meta-llama/Meta-Llama-3.1-8B-Instruct`
across vLLM, SGLang, and llama.cpp.

| Topology                                            | GPUs | Engine    | Notes                                                       |
| --------------------------------------------------- | ---- | --------- | ----------------------------------------------------------- |
| [vllm/agg](vllm/agg)                                | 1    | vLLM      | Single-node, TP=1, chunked prefill                          |
| [vllm/disagg-single-node](vllm/disagg-single-node)  | 2    | vLLM      | Prefill on GPU 0, decode on GPU 1, NIXL/KV handoff          |
| [sglang/agg](sglang/agg)                            | 1    | SGLang    | RadixAttention prefix cache, single GPU                     |
| [llama-cpp/cpu](llama-cpp/cpu)                      | 0    | llama.cpp | CPU-only fallback for laptops / dev loops                   |

## Prerequisites

- Linux (or macOS for the `llama-cpp/cpu` recipe)
- For vLLM:    `pip install vllm`
- For SGLang:  `pip install "sglang[all]"` (and a CUDA-capable GPU)
- For llama.cpp: `pip install "llama-cpp-python[server]"` and a GGUF file
- `huggingface-cli login` for gated repos (Llama-3 requires a HF token)

## One-liner

```bash
bash recipes/llama3-8b/vllm/agg/up.sh
```

The driver builds Cognitora's six Rust binaries (release) on first run,
starts an embedded etcd, brings up router + agent + KV daemon, and
probes `/v1/models`. Subsequent runs reuse the build cache.

## Test it

```bash
curl -fsS http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "meta-llama/Meta-Llama-3.1-8B-Instruct",
    "messages": [{"role":"user","content":"Hello!"}],
    "max_tokens": 32
  }'
```

## Tear down

```bash
bash scripts/run/down.sh recipes/llama3-8b/vllm/agg
```
