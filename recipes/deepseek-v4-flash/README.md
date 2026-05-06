# DeepSeek-V4-Flash recipes

Production bring-up profiles for `deepseek-ai/DeepSeek-V4-Flash` — a
**284B-total / 13B-active MoE** with a hybrid CSA + HCA attention stack
(Blackwell FP4 indexer cache) and mixed FP4 (expert weights) + FP8
(attention, norm, router) checkpoints. All recipes are single-replica
**aggregated** (no prefill/decode disaggregation) decode-only
deployments using **4 GPUs** of one host.

These recipes mirror the layout NVIDIA Dynamo uses for its own
[DeepSeek-V4-Flash](https://github.com/ai-dynamo/dynamo/tree/main/recipes/deepseek-v4/deepseek-v4-flash)
recipe, but adapt to Cognitora's profile-driven, single-binary runtime:
flat TOML, no CRD, no operator install required. Compared to the Dynamo
manifests, the only Dynamo-isms dropped are the ones that don't apply
(the `DynamoGraphDeployment` CRD, the prebuilt NGC images, the PVC /
download Job — Cognitora pulls weights through the engine's own HF
cache on first launch).

| Variant                                   | Engine | Hardware  | Topology                                         | Notes                                              |
| ----------------------------------------- | ------ | --------- | ------------------------------------------------ | -------------------------------------------------- |
| [`vllm/agg-b200`](vllm/agg-b200)           | vLLM   | 4× B200   | DP=4 + Expert Parallel, TP=1                     | Mirrors Dynamo's `vllm-agg-b200`                   |
| [`vllm/agg-gb200`](vllm/agg-gb200)         | vLLM   | 4× GB200  | TP=4 + Expert Parallel, `deep_gemm_mega_moe`     | Mirrors Dynamo's `vllm-agg-gb200` (NVL4 tray)      |
| [`sglang/agg-b200`](sglang/agg-b200)       | SGLang | 4× B200   | TP=4, MXFP4 MoE via FlashInfer, EAGLE MTP 3/4    | Mirrors Dynamo's `sglang-agg`                      |
| [`sglang/agg-gb200`](sglang/agg-gb200)     | SGLang | 4× GB200  | TP=4, MXFP4 MoE via FlashInfer, EAGLE MTP 3/4    | Mirrors Dynamo's `sglang-agg-gb200` (arm64 NVL4)   |

Status: **Experimental** (Day-0). Modality: text only.

## Prerequisites

1. **Cognitora binaries** — `cgn-router`, `cgn-agent`, `cgn-kvcached`
   on `PATH` or under `target/release/` (the recipe's `up.sh` builds
   them on first run).
2. **Engine** — `pip install vllm` (vLLM variants) or
   `pip install "sglang[all]"` (SGLang variants), pinned to a build
   that supports DeepSeek-V4-Flash.
3. **GPUs** — 4× Blackwell GPUs of the matching arch on one host:
   - **B200 variants** (x86_64): 4 of the node's 8 B200s are enough.
   - **GB200 variants** (arm64): the 4 GPUs of one NVL4 tray.
4. **HuggingFace token** with access to `deepseek-ai/DeepSeek-V4-Flash`:
   ```bash
   export HF_TOKEN=<your-token>
   ```
5. **Disk** — `deepseek-ai/DeepSeek-V4-Flash` is ~160 GiB on disk
   (46 safetensors shards in mixed FP4 + FP8 form). Plan ~400 GiB free
   in `$HF_HOME` for the cache + headroom.

## Bring up

```bash
# vLLM, 4× B200
HF_TOKEN=<…> bash recipes/deepseek-v4-flash/vllm/agg-b200/up.sh

# vLLM, 4× GB200 NVL4 tray
HF_TOKEN=<…> bash recipes/deepseek-v4-flash/vllm/agg-gb200/up.sh

# SGLang, 4× B200
HF_TOKEN=<…> bash recipes/deepseek-v4-flash/sglang/agg-b200/up.sh

# SGLang, 4× GB200 NVL4 tray
HF_TOKEN=<…> bash recipes/deepseek-v4-flash/sglang/agg-gb200/up.sh
```

The first launch of any variant takes **up to ~60 minutes**: weights
load, FlashInfer / DeepGEMM autotune, and CUDA-graph warmup all run
back-to-back. Subsequent launches are <5 minutes once the engine's
warm-up cache is on disk.

`cgn-ctl recipe up <name>` is an equivalent invocation:

```bash
cgn-ctl recipe up deepseek-v4-flash/vllm/agg-b200
```

## Test the deployment

The OpenAI-compatible HTTP surface lives on the router (port 8080):

```bash
curl -fsS http://127.0.0.1:8080/v1/models | jq

curl -fsS http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "deepseek-ai/DeepSeek-V4-Flash",
    "messages": [{"role":"user","content":"Hello!"}],
    "max_tokens": 100
  }'
```

### Verifying reasoning

DeepSeek-V4-Flash emits chain-of-thought wrapped in `<think>…</think>`.
The engine's reasoning parser (`--reasoning-parser deepseek_v4` on
both vLLM and SGLang) extracts it into
`choices[0].message.reasoning_content` and leaves only the final answer
in `choices[0].message.content`:

```bash
curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "deepseek-ai/DeepSeek-V4-Flash",
    "messages": [{"role":"user","content":"What is 2+2? Answer briefly."}],
    "max_tokens": 200
  }' | python3 -m json.tool
```

If `reasoning_content` is `null` and `</think>` shows up in `content`,
the reasoning parser isn't wired up — confirm `--reasoning-parser`
appears in the agent's engine extra_args.

### Verifying tool calling

The tool-call parser (`--tool-call-parser deepseek_v4` +
`--enable-auto-tool-choice` on vLLM, `--tool-call-parser deepseek_v4`
on SGLang) emits OpenAI-compatible structured `tool_calls`:

```bash
curl -s http://127.0.0.1:8080/v1/chat/completions \
  -H 'Content-Type: application/json' \
  -d '{
    "model": "deepseek-ai/DeepSeek-V4-Flash",
    "messages": [{"role":"user","content":"What is the weather in San Francisco?"}],
    "tools": [{
      "type": "function",
      "function": {
        "name": "get_weather",
        "description": "Get the current weather for a location",
        "parameters": {
          "type": "object",
          "properties": {
            "location": {"type": "string", "description": "City name"}
          },
          "required": ["location"]
        }
      }
    }],
    "max_tokens": 300
  }' | python3 -m json.tool
```

`choices[0].message.tool_calls` should be a structured array and
`finish_reason` should be `"tool_calls"`.

## Tear down

```bash
bash scripts/run/down.sh recipes/deepseek-v4-flash/<engine>/<variant>
```

## Notes

- **Aggregated, decode-only.** All four variants run a single replica
  that does both prefill and decode (`role = "both"` in the agent
  TOML). The Dynamo recipe labels its sole worker
  `subComponentType: decode` because the operator otherwise reserves
  the slot for prefill in DGD-typed deployments — Cognitora's agent
  has no equivalent reservation.
- **No external KV-offload backend.** Both engines use their native
  KV manager for this model (FP8 KV cache + 256-block on vLLM, MXFP4
  MoE + EAGLE MTP on SGLang). `engine.kv_offload` is `none`;
  `cgn-kvcached` provides a separate cluster-level RAM/SSD KV tier
  above the engine's GPU cache.
- **First launch is slow.** ~60 min for weight load + FlashInfer or
  DeepGEMM warmup + CUDA-graph compilation. Watch `~/.cache/cognitora/run/agent-*.log`
  to track progress.
- **Sibling recipe.** [DeepSeek-V4-Pro](https://github.com/ai-dynamo/dynamo/tree/main/recipes/deepseek-v4/deepseek-v4-pro)
  is the larger sibling (1.6T / 49B active, 1M context, 8× B200) and
  uses the same dsv4 vLLM and SGLang flag set with TP=8 / DP=8.
