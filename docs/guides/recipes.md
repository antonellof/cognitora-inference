# Recipes — one-line cluster bring-up

A **recipe** is a folder of TOML profiles plus a 3-line `up.sh` driver
that brings up a complete Cognitora cluster — router, agents, optional
KV daemon, and an embedded etcd — pointed at a specific model, engine,
and topology. Recipes are inspired by NVIDIA Dynamo's per-model
production folders, but adapt to Cognitora's all-Rust profile-driven
runtime: there is no Python framework and no operator install required.

The in-tree set is rooted at [`recipes/`](../../recipes/).

## TL;DR

```bash
# Llama-3.1-8B on a single GPU with vLLM:
bash recipes/llama3-8b/vllm/agg/up.sh

# Same model, prefill/decode disaggregated on two GPUs:
bash recipes/llama3-8b/vllm/disagg-single-node/up.sh

# Same model on SGLang with RadixAttention prefix caching:
bash recipes/llama3-8b/sglang/agg/up.sh

# CPU-only laptop / dev loop with llama.cpp:
LLAMA_GGUF=~/models/Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf \
  bash recipes/llama3-8b/llama-cpp/cpu/up.sh

# 70B FP8 on 4 H100s, TP=4:
HF_TOKEN=… bash recipes/llama3-70b/vllm/agg/up.sh
```

Or, via the admin CLI:

```bash
cgn-ctl recipe ls                         # list every recipe
cgn-ctl recipe show llama3-8b/vllm/agg    # show README + files
cgn-ctl recipe up   llama3-8b/vllm/agg    # bring up
cgn-ctl recipe down llama3-8b/vllm/agg    # tear down
```

## What `up.sh` does

Every recipe's `up.sh` is the same three lines:

```bash
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
```

The shared driver in [`recipes/_lib/recipe.sh`](../../recipes/_lib/recipe.sh):

1. Builds Cognitora's six Rust binaries (release) on first run; reuses
   the build cache thereafter. Set `CGN_PREBUILT=1` to skip the build
   preflight.
2. Verifies that the recipe's engine binary is importable (vLLM,
   SGLang, llama.cpp). Warns if not.
3. Hands the recipe directory off to
   [`scripts/run/up.sh`](../../scripts/run/up.sh), which:
   - starts (or reuses) a local etcd,
   - starts `cgn-kvcached` if `kvcached.toml` is present,
   - starts one `cgn-agent` per `agent-*.toml`,
   - starts `cgn-router` from `router.toml`.
4. Probes `/v1/models` on the router.
5. Prints the curl one-liners for `/v1/models` and
   `/v1/chat/completions`.

## Layout

A recipe is a flat folder. The shared driver consumes:

```text
recipes/<model>/<engine>/<topology>/
  README.md                 # human description, GPU shape, knobs
  router.toml               # cgn-router config (one file)
  agent-<name>.toml         # one file per agent (1..N)
  kvcached.toml             # optional cgn-kvcached daemon
  up.sh                     # 3-line wrapper
```

`agent-*.toml` is the marker the runner uses to pick up agents — each
file becomes one `cgn-agent` process. Naming is arbitrary; we use
`agent-prefill.toml` / `agent-decode.toml` for disaggregated
deployments and `agent-<model>.toml` for aggregated ones.

## Mapping to NVIDIA Dynamo

| Dynamo                                     | Cognitora                              |
| ------------------------------------------ | -------------------------------------- |
| `recipes/<model>/<engine>/<topology>/deploy.yaml` | `recipes/<model>/<engine>/<topology>/{router,agent-*,kvcached}.toml` |
| `kubectl apply -f deploy.yaml`             | `bash up.sh` or `cgn-ctl recipe up …`  |
| `DynamoGraphDeployment` CRD                | flat TOML profile (no CRD)             |
| Frontend service                           | `cgn-router` (one binary, no Python)   |
| `VllmPrefillWorker` / `VllmDecodeWorker`   | `agent-prefill.toml` / `agent-decode.toml` |
| `kv-transfer-config` on the worker         | passed through `[engine.vllm].extra_args` |
| RadixTree KV router                        | `cgn-router` longest-prefix routing on sequence-chained BLAKE3 digests |

## Authoring a new recipe

```bash
mkdir -p recipes/<model>/<engine>/<topology>
cd       recipes/<model>/<engine>/<topology>
cp ../../../llama3-8b/vllm/agg/{router,agent-*,kvcached}.toml .
cp ../../../llama3-8b/vllm/agg/up.sh                          .
$EDITOR README.md router.toml agent-*.toml
```

Cognitora's [`scripts/run/up.sh`](../../scripts/run/up.sh) discovers
agents by globbing `agent-*.toml` — there is no separate registry, so
new agents drop in automatically.

The router's `[router.score_weights]` block is the main knob worth
tuning per recipe:

| Topology       | KV   | Load | Power | Capacity |
| -------------- | ---- | ---- | ----- | -------- |
| Aggregated     | 0.55 | 0.25 | 0.10  | 0.10     |
| Disaggregated  | 0.50 | 0.30 | 0.10  | 0.10     |

The four weights must sum to 1.0; see
[`docs/architecture/routing.md`](../architecture/routing.md) for the
mathematics.

## Tearing down

```bash
bash scripts/run/down.sh           # kills everything launched by up.sh
# or:
cgn-ctl recipe down <recipe>
```

Both invocations are idempotent and clean up child engine processes
that escape their agent's process group (vLLM, SGLang, and
llama-cpp-python all do this in some configurations).
