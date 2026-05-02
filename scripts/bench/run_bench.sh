#!/usr/bin/env bash
# Driver: runs the four scenarios + KV-reuse pair and emits a JSONL result file.
#
# Endpoints assumed:
#   * Ollama  direct      :11434  (model: qwen2.5:0.5b)
#   * llama.cpp direct    :8001   (model: Qwen/Qwen2.5-0.5B-Instruct)
#   * Cognitora router    :8080   (both models registered)
set -euo pipefail

OUT="${OUT:-bench-results.jsonl}"
N="${N:-20}"
CONC="${CONC:-1}"
MAX="${MAX:-64}"
PY="${PY:-python3}"
SCRIPT="$(dirname "$0")/bench_client.py"

: >"$OUT"

run() {
  local name="$1" url="$2" model="$3"
  shift 3
  echo "==> $name"
  "$PY" "$SCRIPT" --name "$name" --url "$url" --model "$model" \
    --n "$N" --concurrency "$CONC" --max-tokens "$MAX" "$@" >>"$OUT"
}

# ---- Non-streaming, varied prompts (cold cache pattern) ----
run "ollama-direct"        "http://127.0.0.1:11434/v1/chat/completions"   "qwen2.5:0.5b"
run "cognitora-ollama"     "http://127.0.0.1:8080/v1/chat/completions"    "qwen2.5:0.5b"
run "llamacpp-direct"      "http://127.0.0.1:8001/v1/chat/completions"    "Qwen/Qwen2.5-0.5B-Instruct"
run "cognitora-llamacpp"   "http://127.0.0.1:8080/v1/chat/completions"    "Qwen/Qwen2.5-0.5B-Instruct"

# ---- Streaming TTFT ----
run "ollama-direct-stream"      "http://127.0.0.1:11434/v1/chat/completions"   "qwen2.5:0.5b"                 --stream
run "cognitora-ollama-stream"   "http://127.0.0.1:8080/v1/chat/completions"    "qwen2.5:0.5b"                 --stream
run "llamacpp-direct-stream"    "http://127.0.0.1:8001/v1/chat/completions"    "Qwen/Qwen2.5-0.5B-Instruct"   --stream
run "cognitora-llamacpp-stream" "http://127.0.0.1:8080/v1/chat/completions"    "Qwen/Qwen2.5-0.5B-Instruct"   --stream

# ---- Shared-prefix (engine-local prefix-cache effect) ----
run "ollama-direct-shared"      "http://127.0.0.1:11434/v1/chat/completions"   "qwen2.5:0.5b"                 --shared-prefix
run "cognitora-ollama-shared"   "http://127.0.0.1:8080/v1/chat/completions"    "qwen2.5:0.5b"                 --shared-prefix
run "llamacpp-direct-shared"    "http://127.0.0.1:8001/v1/chat/completions"    "Qwen/Qwen2.5-0.5B-Instruct"   --shared-prefix
run "cognitora-llamacpp-shared" "http://127.0.0.1:8080/v1/chat/completions"    "Qwen/Qwen2.5-0.5B-Instruct"   --shared-prefix

echo "results written to $OUT"
