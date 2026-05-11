#!/usr/bin/env bash
# examples/apple-mlx/download-model.sh
#
# Pre-download an MLX-LM model from Hugging Face into the local HF cache so
# the first /v1/chat/completions call to mlx_lm.server doesn't sit silent
# during a multi-GiB download.
#
# Usage:
#   bash examples/apple-mlx/download-model.sh                       # default model
#   bash examples/apple-mlx/download-model.sh <model_id>            # any HF repo id
#   MODEL=<model_id> bash examples/apple-mlx/download-model.sh
#
# Requires: huggingface_hub (installed transitively by `pip install mlx-lm`).
# Set HF_TOKEN in the environment for gated repos.

set -euo pipefail

MODEL=${1:-${MODEL:-mlx-community/Llama-3.2-3B-Instruct-4bit}}

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

hr "Pre-downloading $MODEL"
bold "Cache:  ${HF_HOME:-$HOME/.cache/huggingface}/hub"
bold "Tip:    set HF_TOKEN=<token> for gated repos."
echo

# Prefer the new `hf` CLI (huggingface_hub >= 0.27 renamed `huggingface-cli`).
# Fall back to legacy `huggingface-cli`, then to Python snapshot_download.
# All three render tqdm progress bars and resolve to the same HF cache.
if command -v hf >/dev/null 2>&1; then
  hf download "$MODEL"
elif command -v huggingface-cli >/dev/null 2>&1; then
  huggingface-cli download "$MODEL"
else
  bold "no hf / huggingface-cli on PATH — using snapshot_download."
  python3 - "$MODEL" <<'PY'
import sys
from huggingface_hub import snapshot_download
path = snapshot_download(sys.argv[1])
print(f"\nDownloaded to: {path}")
PY
fi

hr "Done"
bold "Verify size:    du -sh \"\$HOME/.cache/huggingface/hub/models--${MODEL//\//--}\""
bold "Use in stack:   set [models.\"$MODEL\"] in examples/apple-mlx/agent-mlx.toml"
