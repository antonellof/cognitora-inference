# DeepSeek-V4-Flash · SGLang · aggregated · 4× B200

Single-host aggregated deployment, **TP=4**, **MXFP4 MoE via
FlashInfer**, **EAGLE MTP 3/4** speculative decoding, on 4 of 8 B200
GPUs of one x86_64 node. Mirrors NVIDIA Dynamo's
[`sglang-agg`](https://github.com/ai-dynamo/dynamo/tree/main/recipes/deepseek-v4/deepseek-v4-flash/sglang/agg)
recipe.

## GPU shape

| Resource | Required               |
| -------- | ---------------------- |
| GPUs     | 4× B200 (180 GiB HBM3e)|
| Host RAM | 512 GiB+               |
| SSD      | 400 GiB cache dir (model is ~160 GiB, leaves headroom for HF metadata) |

## What this configures

| Flag                                                                                         | Purpose                                                                                |
| -------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| `--tp 4`                                                                                     | Tensor-parallel across the 4 B200s                                                     |
| `--trust-remote-code`                                                                        | Required for the V4 architecture's custom modeling code                                |
| `--moe-runner-backend flashinfer_mxfp4`                                                      | MXFP4 MoE kernel via FlashInfer for the V4 expert weights                              |
| `--speculative-algo EAGLE` + `--speculative-num-steps 3` + `--speculative-eagle-topk 1` + `--speculative-num-draft-tokens 4` | EAGLE MTP speculative decoding (3 draft steps, top-1 over the EAGLE head, 4 draft tokens per step) |
| `--chunked-prefill-size 4096`                                                                | Chunk long prompts at 4 k tokens for steady-state decode interleaving                  |
| `--disable-flashinfer-autotune`                                                              | Skip per-shape autotuning at startup; the dsv4 base ships pre-tuned defaults           |
| `--reasoning-parser deepseek_v4`                                                             | Extracts chain-of-thought into `message.reasoning_content`                             |
| `--tool-call-parser deepseek_v4`                                                             | Emits OpenAI-compatible structured `tool_calls`                                        |

## Engine env

| Variable                              | Value | Purpose                                                       |
| ------------------------------------- | ----- | ------------------------------------------------------------- |
| `SGLANG_JIT_DEEPGEMM_PRECOMPILE`      | `0`   | Skip the slow precompile path                                 |
| `SGLANG_JIT_DEEPGEMM_FAST_WARMUP`     | `1`   | Use the fast DeepGEMM warmup path                             |
| `NCCL_CUMEM_ENABLE`                   | `1`   | Required for V4 NCCL collectives on Blackwell                 |
| `GLOO_SOCKET_IFNAME`                  | `eth0`| Pin Gloo to the standard interface                            |

These are exported by `up.sh` before the agent starts.

## Bring up

```bash
HF_TOKEN=<your-token> bash recipes/deepseek-v4-flash/sglang/agg-b200/up.sh
```

First launch: ~60 min (weight load + DeepGEMM warmup + cudagraph
warmup). Subsequent launches reuse the cache.

## Tear down

```bash
bash scripts/run/down.sh recipes/deepseek-v4-flash/sglang/agg-b200
```
