# DeepSeek-V4-Flash · SGLang · aggregated · 4× GB200 (NVL4, arm64)

Single-host aggregated deployment, **TP=4**, **MXFP4 MoE via
FlashInfer**, **EAGLE MTP 3/4** speculative decoding, on the 4 GB200
GPUs of one **NVL4 tray** (arm64). Mirrors NVIDIA Dynamo's
[`sglang-agg-gb200`](https://github.com/ai-dynamo/dynamo/tree/main/recipes/deepseek-v4/deepseek-v4-flash/sglang/agg-gb200)
recipe.

## GPU shape

| Resource | Required                                                       |
| -------- | -------------------------------------------------------------- |
| GPUs     | 4× GB200 (one NVL4 tray, arm64)                                |
| Host RAM | 512 GiB+                                                       |
| SSD      | 400 GiB cache dir (model is ~160 GiB, leaves headroom for HF metadata) |

## What this configures

The argv is identical to [`sglang/agg-b200`](../agg-b200): `--tp 4`,
`--moe-runner-backend flashinfer_mxfp4`, EAGLE MTP 3/4,
`--chunked-prefill-size 4096`, `--disable-flashinfer-autotune`,
`--reasoning-parser deepseek_v4`, `--tool-call-parser deepseek_v4`. The
arm64 vs x86_64 split shows up in the prebuilt SGLang container image
that the Dynamo recipe pulls (cuda13 vs cuda12); when running this
recipe directly against a `pip install "sglang[all]"` on the host, the
argv stays the same.

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
HF_TOKEN=<your-token> bash recipes/deepseek-v4-flash/sglang/agg-gb200/up.sh
```

First launch: ~60 min. Subsequent launches reuse the cache.

## Tear down

```bash
bash scripts/run/down.sh recipes/deepseek-v4-flash/sglang/agg-gb200
```
