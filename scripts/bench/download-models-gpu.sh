#!/usr/bin/env bash
# Pre-download models for the GPU bench (no Ollama).
#   * GGUF for llama-cpp-python: qwen2.5-0.5b q4_k_m, qwen2.5-7b q4_k_m
#   * HF for vLLM: Qwen/Qwen2.5-0.5B-Instruct (fp16), Qwen/Qwen2.5-7B-Instruct-AWQ
set -euo pipefail

VENV="${VENV:-/workspace/venv}"
MODEL_DIR="${MODEL_DIR:-/workspace/models}"
mkdir -p "$MODEL_DIR/hf"

. "$VENV/bin/activate"

echo "=== GGUFs (for llama-cpp-python) ==="
hf download Qwen/Qwen2.5-0.5B-Instruct-GGUF \
  qwen2.5-0.5b-instruct-q4_k_m.gguf --local-dir "$MODEL_DIR"
hf download Qwen/Qwen2.5-7B-Instruct-GGUF \
  qwen2.5-7b-instruct-q4_k_m.gguf --local-dir "$MODEL_DIR"

echo "=== HF weights for vLLM ==="
HF_HOME="$MODEL_DIR/hf" hf download Qwen/Qwen2.5-0.5B-Instruct \
  --exclude "*.bin" --exclude "*.pt"
HF_HOME="$MODEL_DIR/hf" hf download Qwen/Qwen2.5-7B-Instruct-AWQ \
  --exclude "*.bin" --exclude "*.pt"

echo
echo "=== sizes ==="
du -sh "$MODEL_DIR" "$MODEL_DIR/hf" 2>/dev/null
