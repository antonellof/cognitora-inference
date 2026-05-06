# DeepSeek-V4-Flash · vLLM · aggregated · 4× GB200 (NVL4)

Single-host aggregated deployment, **TP=4 + Expert Parallel** with the
**`deep_gemm_mega_moe`** kernel, on the 4 GB200 GPUs of one **NVL4
tray** (arm64). Mirrors NVIDIA Dynamo's
[`vllm-agg-gb200`](https://github.com/ai-dynamo/dynamo/tree/main/recipes/deepseek-v4/deepseek-v4-flash/vllm/agg_gb200)
recipe.

## GPU shape

| Resource | Required                                                       |
| -------- | -------------------------------------------------------------- |
| GPUs     | 4× GB200 (one NVL4 tray, arm64)                                |
| Host RAM | 512 GiB+                                                       |
| SSD      | 400 GiB cache dir (model is ~160 GiB, leaves headroom for HF metadata) |

## What this configures

| Flag                                       | Purpose                                                              |
| ------------------------------------------ | -------------------------------------------------------------------- |
| `--tensor-parallel-size 4 --enable-expert-parallel` | **TP=4 + EP** across the 4 GPUs of the NVL4 tray (DP dropped — the tray's intra-NVLink makes TP attractive at this size class) |
| `--moe-backend deep_gemm_mega_moe`         | DeepGEMM "mega MoE" kernel — the optimized FP8 MoE path for V4 expert routing on Blackwell |
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
| `VLLM_USE_NCCL_SYMM_MEM`          | `1`   | NVLink Sharp (NVLS) symmetric-memory allreduce                |
| `VLLM_SKIP_P2P_CHECK`             | `1`   | Skip the P2P check (matches the DeepSeek-R1 recipe)           |
| `NCCL_CUMEM_ENABLE`               | `1`   | Required for V4 NCCL collectives on Blackwell                 |
| `NCCL_NVLS_ENABLE`                | `1`   | Enable NVLink Sharp (NVLS) multicast for one-shot all-reduce  |
| `NCCL_P2P_LEVEL`                  | `NVL` | Restrict P2P to NVLink (the NVL4 tray)                        |

These are exported by `up.sh` before the agent starts.

> **FlashInfer TRT-LLM allreduce on GB200.** You may see a non-fatal
> startup warning `Failed to initialize FlashInfer Allreduce norm fusion
> workspace … Flashinfer allreduce-norm fusion will be disabled`. vLLM
> falls back to a non-fused allreduce + RMSNorm; correctness is
> unaffected. To enable the fused kernel, swap the compilation config
> for: `{"mode":3,"cudagraph_mode":"FULL_AND_PIECEWISE","custom_ops":["all"],"pass_config":{"fuse_allreduce_rms":true}}`.

## Bring up

```bash
HF_TOKEN=<your-token> bash recipes/deepseek-v4-flash/vllm/agg-gb200/up.sh
```

First launch: ~60 min (weights load + DeepGEMM warmup + CUDA-graph
compilation). Subsequent launches reuse the cache.

## Tear down

```bash
bash scripts/run/down.sh recipes/deepseek-v4-flash/vllm/agg-gb200
```
