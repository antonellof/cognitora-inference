#!/usr/bin/env bash
# GPU multi-engine bench driver (vLLM + llama-cpp-python; Ollama omitted).
#
# Endpoints used:
#   :8001   llama-cpp-python (small)
#   :8002   llama-cpp-python (mid)
#   :8101   vLLM (small) prefix-caching ON
#   :8102   vLLM (mid)   prefix-caching ON  (AWQ)
#   :8103   vLLM (small) prefix-caching OFF       (KV-cache ablation)
#   :8104   vLLM (small) prefix-caching ON, replica B (for KV-aware-routing demo)
#   :8080   cgn-router (multiplexes all of the above)
#
# Cognitora-side model names:
#   cog-qwen-05b-llamacpp / cog-qwen-7b-llamacpp
#   cog-qwen-05b-vllm     / cog-qwen-7b-vllm
#   cog-qwen-05b-vllm-nocache
#   cog-qwen-05b-pair       (advertised by 2 vLLM-small agents, A on :8101, B on :8104)
#
# Direct engine model names:
#   llama-cpp:  qwen-05b-llamacpp / qwen-7b-llamacpp
#   vLLM:       qwen-05b-vllm / qwen-7b-vllm / qwen-05b-vllm-nocache / cog-qwen-05b-pair

set -euo pipefail

OUT="${OUT:-bench-gpu.jsonl}"
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
# Block A — short-prompt overhead, sequential
# Goal: per-request overhead of cognitora vs direct, per engine, per size.
############################################
run "A-llamacpp-direct-small"  "http://127.0.0.1:8001/v1/chat/completions" "qwen-05b-llamacpp"
run "A-cog-llamacpp-small"     "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-05b-llamacpp"
run "A-vllm-direct-small"      "http://127.0.0.1:8101/v1/chat/completions" "qwen-05b-vllm"
run "A-cog-vllm-small"         "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-05b-vllm"

run "A-llamacpp-direct-mid"    "http://127.0.0.1:8002/v1/chat/completions" "qwen-7b-llamacpp"
run "A-cog-llamacpp-mid"       "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-7b-llamacpp"
run "A-vllm-direct-mid"        "http://127.0.0.1:8102/v1/chat/completions" "qwen-7b-vllm"
run "A-cog-vllm-mid"           "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-7b-vllm"

############################################
# Block B — streaming TTFT, fixed by counting first non-empty delta.content
############################################
run "B-llamacpp-direct-mid-stream" "http://127.0.0.1:8002/v1/chat/completions" "qwen-7b-llamacpp"     --stream
run "B-cog-llamacpp-mid-stream"    "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-7b-llamacpp" --stream
run "B-vllm-direct-mid-stream"     "http://127.0.0.1:8102/v1/chat/completions" "qwen-7b-vllm"         --stream
run "B-cog-vllm-mid-stream"        "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-7b-vllm"     --stream

############################################
# Block C — input-token sweep (1k, 4k) with shared-prefix
#   exposes prefill cost and engine prefix-cache benefit.
############################################
for size in 1024 4096 ; do
  run "C-vllm-mid-${size}"      "http://127.0.0.1:8102/v1/chat/completions" "qwen-7b-vllm"     --prompt-tokens "$size" --shared-prefix
  run "C-cog-vllm-mid-${size}"  "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-7b-vllm" --prompt-tokens "$size" --shared-prefix
  run "C-llamacpp-mid-${size}"  "http://127.0.0.1:8002/v1/chat/completions" "qwen-7b-llamacpp" --prompt-tokens "$size" --shared-prefix
done

############################################
# Block D — KV / prefix-cache ablation: vLLM ON vs OFF, same prompts
############################################
run "D-vllm-small-cacheON-1024"  "http://127.0.0.1:8101/v1/chat/completions" "qwen-05b-vllm"          --prompt-tokens 1024 --shared-prefix
run "D-vllm-small-cacheOFF-1024" "http://127.0.0.1:8103/v1/chat/completions" "qwen-05b-vllm-nocache"  --prompt-tokens 1024 --shared-prefix
run "D-vllm-small-cacheON-4096"  "http://127.0.0.1:8101/v1/chat/completions" "qwen-05b-vllm"          --prompt-tokens 4096 --shared-prefix
run "D-vllm-small-cacheOFF-4096" "http://127.0.0.1:8103/v1/chat/completions" "qwen-05b-vllm-nocache"  --prompt-tokens 4096 --shared-prefix

############################################
# Block E — concurrency: vLLM continuous batching vs llama.cpp
############################################
for c in 4 16 ; do
  run "E-llamacpp-mid-c${c}" "http://127.0.0.1:8002/v1/chat/completions" "qwen-7b-llamacpp"  --concurrency "$c"
  run "E-vllm-mid-c${c}"     "http://127.0.0.1:8102/v1/chat/completions" "qwen-7b-vllm"      --concurrency "$c"
  run "E-cog-vllm-mid-c${c}" "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-7b-vllm"  --concurrency "$c"
done

############################################
# Block F — Cognitora KV-aware routing on TWO vLLM-small replicas (same model)
# Run twice: once with kv=0.55 (default), once with kv=0.0 hot-swapped via etcd.
# Only the F-block name is set here; the *driver* (run-block-F.sh) handles the
# policy swap between the two invocations.
############################################
run "F-cog-pair-1024"          "http://127.0.0.1:8080/v1/chat/completions" "cog-qwen-05b-pair"   --prompt-tokens 1024 --shared-prefix

echo "results -> $OUT"
