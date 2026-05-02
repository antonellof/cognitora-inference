#!/usr/bin/env bash
# Bring up vLLM-only engine fleet on the A100 box.
#   :8101  vLLM small (Qwen 0.5B fp16)        prefix-caching ON
#   :8102  vLLM mid   (DeepSeek-R1-Distill-Llama-8B fp16) prefix-caching ON
#   :8103  vLLM small (Qwen 0.5B fp16)        prefix-caching OFF  (KV ablation)
#   :8104  vLLM small (Qwen 0.5B fp16) repB   prefix-caching ON   (kv-aware demo)
set -euo pipefail

LOG_DIR="${LOG_DIR:-/workspace/logs}"
mkdir -p "$LOG_DIR"

export HF_HOME="${HF_HOME:-/workspace/.hf_home}"
export VLLM_USE_V1=1

# 1) vLLM small (cache ON) — :8101
nohup vllm serve "Qwen/Qwen2.5-0.5B-Instruct" \
  --host 127.0.0.1 --port 8101 \
  --served-model-name qwen-05b-vllm cog-qwen-05b-pair \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.10 \
  >"$LOG_DIR/vllm-small.log" 2>&1 &
disown
echo "[1/4] vllm small  (:8101) prefix-caching=ON"

# 2) vLLM small (cache OFF) — :8103
nohup vllm serve "Qwen/Qwen2.5-0.5B-Instruct" \
  --host 127.0.0.1 --port 8103 \
  --served-model-name qwen-05b-vllm-nocache \
  --max-model-len 8192 --no-enable-prefix-caching \
  --gpu-memory-utilization 0.10 \
  >"$LOG_DIR/vllm-small-nocache.log" 2>&1 &
disown
echo "[2/4] vllm small  (:8103) prefix-caching=OFF"

# 3) vLLM small replica B — :8104
nohup vllm serve "Qwen/Qwen2.5-0.5B-Instruct" \
  --host 127.0.0.1 --port 8104 \
  --served-model-name cog-qwen-05b-pair \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.10 \
  >"$LOG_DIR/vllm-small-b.log" 2>&1 &
disown
echo "[3/4] vllm small  (:8104) replica B"

# 4) vLLM mid (DeepSeek-R1-Distill-Llama-8B) — :8102
nohup vllm serve "deepseek-ai/DeepSeek-R1-Distill-Llama-8B" \
  --host 127.0.0.1 --port 8102 \
  --served-model-name deepseek-8b-vllm \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.45 \
  >"$LOG_DIR/vllm-mid.log" 2>&1 &
disown
echo "[4/4] vllm mid    (:8102) DeepSeek-8B"

echo
echo "engines spawning. waiting for /v1/models on each port:"
for p in 8101 8102 8103 8104 ; do
  printf "  :%d ... " "$p"
  for i in $(seq 1 300); do
    curl -fsS -m 1 "http://127.0.0.1:$p/v1/models" >/dev/null 2>&1 && { echo "ready (${i}s)"; break; }
    sleep 1
  done || echo "TIMEOUT"
done

echo
echo "=== nvidia-smi ==="
nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu --format=csv
