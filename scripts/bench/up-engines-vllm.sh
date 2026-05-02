#!/usr/bin/env bash
# Bring up vLLM engines SEQUENTIALLY (waits for each to be ready before
# starting the next). Avoids the "all 4 race for GPU memory at startup" failure.
#
# A100 80GB layout (engine memory budgets sum well under 80GB):
#   :8101 small Qwen 0.5B  prefix-caching ON         12% util ≈ 9 GB
#   :8103 small Qwen 0.5B  prefix-caching OFF        12% util ≈ 9 GB
#   :8104 small Qwen 0.5B  prefix-caching ON, repB   12% util ≈ 9 GB
#   :8102 mid   DeepSeek-R1-Distill-Llama-8B fp16    50% util ≈ 40 GB
set -euo pipefail

LOG_DIR="${LOG_DIR:-/workspace/logs}"
mkdir -p "$LOG_DIR"
export HF_HOME="${HF_HOME:-/workspace/.hf_home}"

wait_ready() {
  local port="$1" timeout_s="${2:-300}"
  printf "  :%s ... " "$port"
  for i in $(seq 1 "$timeout_s") ; do
    curl -fsS -m 1 "http://127.0.0.1:$port/v1/models" >/dev/null 2>&1 && {
      echo "ready (${i}s)"
      return 0
    }
    sleep 1
  done
  echo "TIMEOUT"
  return 1
}

# --- 1) vLLM small (cache ON) :8101
nohup vllm serve "Qwen/Qwen2.5-0.5B-Instruct" \
  --host 127.0.0.1 --port 8101 \
  --served-model-name qwen-05b-vllm cog-qwen-05b-pair \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.12 \
  >"$LOG_DIR/vllm-small.log" 2>&1 &
disown
echo "[1/4] launched vllm small (:8101)"
wait_ready 8101 || exit 1

# --- 2) vLLM small (cache OFF) :8103
nohup vllm serve "Qwen/Qwen2.5-0.5B-Instruct" \
  --host 127.0.0.1 --port 8103 \
  --served-model-name qwen-05b-vllm-nocache \
  --max-model-len 8192 --no-enable-prefix-caching \
  --gpu-memory-utilization 0.12 \
  >"$LOG_DIR/vllm-small-nocache.log" 2>&1 &
disown
echo "[2/4] launched vllm small nocache (:8103)"
wait_ready 8103 || exit 1

# --- 3) vLLM small replica B :8104
nohup vllm serve "Qwen/Qwen2.5-0.5B-Instruct" \
  --host 127.0.0.1 --port 8104 \
  --served-model-name cog-qwen-05b-pair \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.12 \
  >"$LOG_DIR/vllm-small-b.log" 2>&1 &
disown
echo "[3/4] launched vllm small replica B (:8104)"
wait_ready 8104 || exit 1

# --- 4) vLLM mid :8102
nohup vllm serve "deepseek-ai/DeepSeek-R1-Distill-Llama-8B" \
  --host 127.0.0.1 --port 8102 \
  --served-model-name deepseek-8b-vllm \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.50 \
  >"$LOG_DIR/vllm-mid.log" 2>&1 &
disown
echo "[4/4] launched vllm mid (:8102)"
wait_ready 8102 600 || exit 1

echo
echo "=== nvidia-smi ==="
nvidia-smi --query-gpu=memory.used,memory.free,utilization.gpu --format=csv
