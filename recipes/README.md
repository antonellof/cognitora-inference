# Cognitora recipes

Production-ready, **one-line bring-up** profiles for popular open
LLMs. Each recipe is a folder of TOML files plus a tiny `up.sh` driver
that brings up a full Cognitora cluster — router, one or more agents,
optional KV-cache daemon, and an embedded etcd — and points it at the
chosen engine (vLLM / SGLang / llama.cpp / OpenAI-compatible).

The recipes mirror the layout NVIDIA Dynamo uses for its own production
recipes (one folder per model × engine × topology) but adapt to
Cognitora's all-Rust, profile-driven runtime: there is no Python
framework, no operator install required, and every recipe is a flat
folder of TOMLs you can read in 30 seconds.

## Quick start

```bash
# Llama-3 8B on a single GPU with vLLM:
bash recipes/llama3-8b/vllm/agg/up.sh

# Same model, prefill/decode disaggregated on two GPUs:
bash recipes/llama3-8b/vllm/disagg-single-node/up.sh

# Same model on SGLang (RadixAttention prefix caching):
bash recipes/llama3-8b/sglang/agg/up.sh

# CPU laptop / dev loop with llama.cpp + a GGUF:
LLAMA_GGUF=/path/to/Meta-Llama-3-8B-Instruct.Q4_K_M.gguf \
  bash recipes/llama3-8b/llama-cpp/cpu/up.sh
```

`up.sh` is a 3-line wrapper: it sources `recipes/_lib/recipe.sh` and
calls `recipe_up`. The shared driver:

1. Builds the Cognitora binaries if they are not already on `PATH`.
2. Verifies the recipe's engine (vLLM / SGLang / llama.cpp) is
   importable and warns if not.
3. Starts an embedded etcd (or honours `ETCD_ENDPOINT=...`).
4. Hands the recipe directory to the existing
   [`scripts/run/up.sh`](../scripts/run/up.sh) profile-runner.
5. Probes `/v1/models` and prints curl one-liners to call the router.

To shut everything down:

```bash
bash scripts/run/down.sh recipes/llama3-8b/vllm/agg
```

## Available recipes

| Model                      | Engine     | Topology                    | GPUs  | Notes                          |
| -------------------------- | ---------- | --------------------------- | ----- | ------------------------------ |
| Llama-3.1 8B               | vLLM       | [`agg`](llama3-8b/vllm/agg)                         | 1     | TP=1, baseline aggregated      |
| Llama-3.1 8B               | vLLM       | [`disagg-single-node`](llama3-8b/vllm/disagg-single-node) | 2 | Prefill/decode split, NIXL handoff |
| Llama-3.1 8B               | SGLang     | [`agg`](llama3-8b/sglang/agg)                       | 1     | RadixAttention prefix cache    |
| Llama-3.1 8B               | llama.cpp  | [`cpu`](llama3-8b/llama-cpp/cpu)                    | 0     | CPU-only via GGUF              |
| Llama-3.3 70B              | vLLM       | [`agg`](llama3-70b/vllm/agg)                        | 4     | TP=4, FP8                      |
| Llama-3.3 70B              | vLLM       | [`disagg-single-node`](llama3-70b/vllm/disagg-single-node) | 8 | TP=4 prefill + TP=4 decode     |
| Qwen-3 7B                  | vLLM       | [`agg`](qwen3-7b/vllm/agg)                          | 1     | TP=1, baseline                 |
| Qwen-3 7B                  | SGLang     | [`agg`](qwen3-7b/sglang/agg)                        | 1     | RadixAttention prefix cache    |

> **What does "engine" mean?** Cognitora is engine-agnostic: the agent
> drives a child process that speaks the OpenAI HTTP surface
> (`/v1/chat/completions`, `/v1/models`, `/health`). vLLM, SGLang, and
> llama.cpp all qualify. Mixing engines in the same cluster is
> supported — the router routes by `model` and doesn't care which
> engine produced the tokens.

## Adding a new recipe

A recipe is just a folder of TOMLs. Copy the closest existing one and
adjust:

```text
recipes/<my-model>/<engine>/<topology>/
  README.md            # what this recipe does, GPU shape, model
  router.toml          # cgn-router config (KV / load / power / capacity weights)
  agent-<name>.toml    # one cgn-agent per file
  kvcached.toml        # optional: cgn-kvcached daemon (RAM/SSD KV tier)
  up.sh                # 3-line wrapper around recipe_up
```

`up.sh` is always the same three lines:

```bash
#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
```

`cgn-ctl recipe up <name>` is an equivalent invocation that does not
require shelling into the repo:

```bash
cgn-ctl recipe up llama3-8b/vllm/agg
cgn-ctl recipe ls
```

## Comparison to Dynamo recipes

| Feature                            | Cognitora recipes              | Dynamo recipes                    |
| ---------------------------------- | ------------------------------ | --------------------------------- |
| Per-model, per-engine, per-topology | yes                            | yes                               |
| Bare-metal / single-node           | first-class (`up.sh`)          | optional                          |
| Kubernetes                         | optional (Helm chart)          | required (operator + CRDs)        |
| Engines                            | vLLM, SGLang, llama.cpp, OpenAI-compat | vLLM, TRT-LLM, SGLang             |
| Format                             | flat TOML                      | `DynamoGraphDeployment` CRD       |
| Bring-up                           | `bash up.sh` or `cgn-ctl recipe up` | `kubectl apply -f`           |
| KV-aware routing                   | router computes longest prefix on sequence-chained BLAKE3 digests | radix tree on chained block hashes |
| Disaggregated prefill/decode       | recipe-level, NIXL/QUIC handoff via cgn-kvcached | recipe-level, NixlConnector |
