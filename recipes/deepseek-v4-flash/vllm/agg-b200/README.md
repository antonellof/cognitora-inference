# DeepSeek-V4-Flash · vLLM · aggregated · 4× B200

Single-host aggregated deployment, **DP=4 + Expert Parallel, TP=1**, on
4 of 8 B200 GPUs of one x86_64 node. Mirrors NVIDIA Dynamo's
[`vllm-agg-b200`](https://github.com/ai-dynamo/dynamo/tree/main/recipes/deepseek-v4/deepseek-v4-flash/vllm/agg_b200)
recipe.

## GPU shape

| Resource | Required               |
| -------- | ---------------------- |
| GPUs     | 4× B200 (180 GiB HBM3e)|
| Host RAM | 512 GiB+               |
| SSD      | 400 GiB cache dir (model is ~160 GiB, leaves headroom for HF metadata) |

## What this configures

| Flag                                       | Purpose                                                              |
| ------------------------------------------ | -------------------------------------------------------------------- |
| `--data-parallel-size 4 --enable-expert-parallel` | DP=4 + EP across the 4 B200s (TP=1)                                  |
| `--tokenizer-mode deepseek_v4`             | Selects the DeepSeek-V4 tokenizer                                    |
| `--reasoning-parser deepseek_v4`           | Extracts chain-of-thought into `message.reasoning_content`           |
| `--tool-call-parser deepseek_v4` + `--enable-auto-tool-choice` | Emits OpenAI-compatible structured `tool_calls`                |
| `--attention-config '{"use_fp4_indexer_cache":true}'` | Blackwell FP4 indexer cache for CSA + HCA attention                  |
| `--kv-cache-dtype fp8` + `--block-size 256` | FP8 KV cache; block size matches the upstream recipe                 |
| `--compilation-config '{"cudagraph_mode":"FULL_AND_PIECEWISE","custom_ops":["all"]}'` | Single-node DEP compilation config from the upstream recipe          |
| `--max-num-seqs 256`                       | Concurrency cap                                                      |

## Engine env

| Variable                          | Value | Purpose                                                       |
| --------------------------------- | ----- | ------------------------------------------------------------- |
| `VLLM_ENGINE_READY_TIMEOUT_S`     | `3600`| Match the ~60 min first-launch budget                         |
| `VLLM_RANDOMIZE_DP_DUMMY_INPUTS`  | `1`   | Stabilize DP dummy inputs (matches the DeepSeek-R1 recipe)    |
| `VLLM_SKIP_P2P_CHECK`             | `1`   | Skip the P2P check (matches the DeepSeek-R1 recipe)           |
| `NCCL_CUMEM_ENABLE`               | `1`   | Required for V4 NCCL collectives on Blackwell                 |

These are exported by `up.sh` before the agent starts.

## Bring up

```bash
HF_TOKEN=<your-token> bash recipes/deepseek-v4-flash/vllm/agg-b200/up.sh
```

First launch downloads `deepseek-ai/DeepSeek-V4-Flash` (~160 GiB) into
`$HF_HOME/hub` and runs FlashInfer autotune + CUDA-graph warmup; budget
**~60 min**. Subsequent launches reuse the cache.

Probe the router once it's up:

```bash
curl -fsS http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{"model":"deepseek-ai/DeepSeek-V4-Flash","messages":[{"role":"user","content":"hello"}]}'
```

## Tear down

```bash
bash scripts/run/down.sh recipes/deepseek-v4-flash/vllm/agg-b200
```
