#!/usr/bin/env bash
# Bring up vLLM and llama-cpp-python engines on dedicated ports.
# Models must already be downloaded via download-models-gpu.sh.
set -euo pipefail

VENV="${VENV:-/workspace/venv}"
LOG_DIR="${LOG_DIR:-/workspace/logs}"
MODEL_DIR="${MODEL_DIR:-/workspace/models}"
mkdir -p "$LOG_DIR"

. "$VENV/bin/activate"

# 1) llama-cpp-python — small (port 8001)
nohup python -m llama_cpp.server \
  --host 127.0.0.1 --port 8001 \
  --model "$MODEL_DIR/qwen2.5-0.5b-instruct-q4_k_m.gguf" \
  --model_alias "qwen-05b-llamacpp" \
  --n_ctx 8192 --n_gpu_layers 999 --n_threads 8 \
  >"$LOG_DIR/llamacpp-small.log" 2>&1 &
disown
echo "[1/6] llama-cpp-python small (:8001)"

# 2) llama-cpp-python — mid (port 8002)
nohup python -m llama_cpp.server \
  --host 127.0.0.1 --port 8002 \
  --model "$MODEL_DIR/qwen2.5-7b-instruct-q4_k_m.gguf" \
  --model_alias "qwen-7b-llamacpp" \
  --n_ctx 8192 --n_gpu_layers 999 --n_threads 8 \
  >"$LOG_DIR/llamacpp-mid.log" 2>&1 &
disown
echo "[2/6] llama-cpp-python mid  (:8002)"

# 3) vLLM small (prefix caching ON) — :8101
nohup python -m vllm.entrypoints.openai.api_server \
  --host 127.0.0.1 --port 8101 \
  --model "Qwen/Qwen2.5-0.5B-Instruct" \
  --served-model-name qwen-05b-vllm \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.10 \
  --download-dir "$MODEL_DIR/hf" \
  >"$LOG_DIR/vllm-small.log" 2>&1 &
disown
echo "[3/6] vllm small (:8101) prefix-caching=ON"

# 4) vLLM small (prefix caching OFF) — :8103
nohup python -m vllm.entrypoints.openai.api_server \
  --host 127.0.0.1 --port 8103 \
  --model "Qwen/Qwen2.5-0.5B-Instruct" \
  --served-model-name qwen-05b-vllm-nocache \
  --max-model-len 8192 --no-enable-prefix-caching \
  --gpu-memory-utilization 0.10 \
  --download-dir "$MODEL_DIR/hf" \
  >"$LOG_DIR/vllm-small-nocache.log" 2>&1 &
disown
echo "[4/6] vllm small (:8103) prefix-caching=OFF"

# 5) vLLM mid (AWQ Q4) — :8102
nohup python -m vllm.entrypoints.openai.api_server \
  --host 127.0.0.1 --port 8102 \
  --model "Qwen/Qwen2.5-7B-Instruct-AWQ" \
  --served-model-name qwen-7b-vllm \
  --max-model-len 8192 --enable-prefix-caching \
  --quantization awq \
  --gpu-memory-utilization 0.45 \
  --download-dir "$MODEL_DIR/hf" \
  >"$LOG_DIR/vllm-mid.log" 2>&1 &
disown
echo "[5/6] vllm mid  (:8102) prefix-caching=ON, AWQ"

# 6) vLLM small replica B — :8104
nohup python -m vllm.entrypoints.openai.api_server \
  --host 127.0.0.1 --port 8104 \
  --model "Qwen/Qwen2.5-0.5B-Instruct" \
  --served-model-name cog-qwen-05b-pair \
  --max-model-len 8192 --enable-prefix-caching \
  --gpu-memory-utilization 0.10 \
  --download-dir "$MODEL_DIR/hf" \
  >"$LOG_DIR/vllm-small-b.log" 2>&1 &
disown
echo "[6/6] vllm small replica B (:8104) for kv-routing demo"

echo
echo "engines spawned. each takes 30-90s to load. waiting for /v1/models on each port:"
for p in 8001 8002 8101 8102 8103 8104 ; do
  printf "  :%d ... " "$p"
  for i in $(seq 1 240); do
    curl -fsS -m 1 "http://127.0.0.1:$p/v1/models" >/dev/null 2>&1 && { echo "ready (${i}s)"; break; }
    sleep 1
  done || echo "TIMEOUT"
done

echo
echo "=== nvidia-smi ==="
nvidia-smi --query-gpu=memory.used,memory.total,utilization.gpu --format=csv
