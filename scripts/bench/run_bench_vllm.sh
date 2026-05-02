#!/usr/bin/env bash
# vLLM-only multi-scenario bench (A100 80GB).
#
# Direct engine model names:
#   qwen-05b-vllm           on :8101 (prefix-caching ON)
#   qwen-05b-vllm-nocache   on :8103 (prefix-caching OFF)
#   cog-qwen-05b-pair       on :8104 (replica B, prefix-caching ON)
#   deepseek-8b-vllm        on :8102
#
# Cognitora model names (router :8080):
#   cog-qwen-05b-vllm
#   cog-qwen-05b-vllm-nocache
#   cog-qwen-05b-pair         (advertised by 2 small replicas)
#   cog-deepseek-8b-vllm
set -euo pipefail

OUT="${OUT:-bench-vllm.jsonl}"
N="${N:-20}"
MAX="${MAX:-128}"
PY="${PY:-python3}"
SCRIPT="$(dirname "$0")/bench_client.py"

: >"$OUT"

run() {
  local name="$1" url="$2" model="$3"
  shift 3
  echo "==> $name"
  "$PY" "$SCRIPT" --name "$name" --url "$url" --model "$model" \
    --n "$N" --max-tokens "$MAX" "$@" >>"$OUT"
}

############################################
# A — short-prompt overhead, sequential, both model sizes
############################################
run "A-vllm-direct-small"       "http://127.0.0.1:8101/v1/chat/completions" "qwen-05b-vllm"
run "A-cog-vllm-small"          "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-05b-vllm"
run "A-vllm-direct-mid"         "http://127.0.0.1:8102/v1/chat/completions" "deepseek-8b-vllm"
run "A-cog-vllm-mid"            "http://127.0.0.1:8080/v1/chat/completions" "cog-deepseek-8b-vllm"

############################################
# B — streaming TTFT (proper first-content-token measurement)
############################################
run "B-vllm-direct-small-stream" "http://127.0.0.1:8101/v1/chat/completions" "qwen-05b-vllm"        --stream
run "B-cog-vllm-small-stream"    "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-05b-vllm"    --stream
run "B-vllm-direct-mid-stream"   "http://127.0.0.1:8102/v1/chat/completions" "deepseek-8b-vllm"     --stream
run "B-cog-vllm-mid-stream"      "http://127.0.0.1:8080/v1/chat/completions" "cog-deepseek-8b-vllm" --stream

############################################
# C — input-token sweep with shared-prefix (engine prefix-cache effect)
############################################
for size in 1024 4096 ; do
  run "C-vllm-mid-${size}"     "http://127.0.0.1:8102/v1/chat/completions" "deepseek-8b-vllm"     --prompt-tokens "$size" --shared-prefix
  run "C-cog-vllm-mid-${size}" "http://127.0.0.1:8080/v1/chat/completions" "cog-deepseek-8b-vllm" --prompt-tokens "$size" --shared-prefix
done

############################################
# D — KV / prefix-cache ablation: vLLM small ON vs OFF
############################################
run "D-vllm-small-cacheON-1024"   "http://127.0.0.1:8101/v1/chat/completions" "qwen-05b-vllm"          --prompt-tokens 1024 --shared-prefix
run "D-vllm-small-cacheOFF-1024"  "http://127.0.0.1:8103/v1/chat/completions" "qwen-05b-vllm-nocache"  --prompt-tokens 1024 --shared-prefix
run "D-vllm-small-cacheON-4096"   "http://127.0.0.1:8101/v1/chat/completions" "qwen-05b-vllm"          --prompt-tokens 4096 --shared-prefix
run "D-vllm-small-cacheOFF-4096"  "http://127.0.0.1:8103/v1/chat/completions" "qwen-05b-vllm-nocache"  --prompt-tokens 4096 --shared-prefix

############################################
# E — concurrency: vLLM continuous batching
############################################
for c in 4 16 32 ; do
  run "E-vllm-mid-c${c}"     "http://127.0.0.1:8102/v1/chat/completions" "deepseek-8b-vllm"     --concurrency "$c"
  run "E-cog-vllm-mid-c${c}" "http://127.0.0.1:8080/v1/chat/completions" "cog-deepseek-8b-vllm" --concurrency "$c"
done

############################################
# F — KV-aware routing on 2 vLLM-small replicas (run twice with different policies)
# This block expects $POLICY_TAG to be set externally (e.g. "kv-aware" or "round-robin")
# and will be invoked by the orchestration script that hot-swaps policies.
############################################
if [ "${POLICY_TAG:-}" != "" ] ; then
  run "F-cog-pair-${POLICY_TAG}-1024" "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-05b-pair" --prompt-tokens 1024 --shared-prefix
fi

echo "results -> $OUT"
