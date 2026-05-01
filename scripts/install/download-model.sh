#!/usr/bin/env bash
# scripts/install/download-model.sh
#
# Download a model from the Hugging Face hub. Two modes:
#
#   * --gguf <file>   — single GGUF file for the llama.cpp engine.
#   * --repo          — full HF repo snapshot for the vLLM engine.
#
# Outputs go into $MODELS_DIR (default: ~/models). The destination path is
# printed on stdout so callers can plug it straight into [models.X.path].
#
# Usage:
#   bash scripts/install/download-model.sh \
#     --gguf qwen2.5-0.5b-instruct-q4_k_m.gguf \
#     Qwen/Qwen2.5-0.5B-Instruct-GGUF
#
#   bash scripts/install/download-model.sh \
#     --repo Qwen/Qwen2.5-0.5B-Instruct

set -euo pipefail

MODELS_DIR=${MODELS_DIR:-$HOME/models}
VENV=${VENV:-$HOME/venv}

mode=""
file=""
repo=""

while [ $# -gt 0 ]; do
  case "$1" in
    --gguf)
      mode="gguf"; file="$2"; shift 2 ;;
    --repo)
      mode="repo"; shift ;;
    -h|--help)
      sed -n '2,18p' "$0"; exit 0 ;;
    *)
      repo="$1"; shift ;;
  esac
done

if [ -z "$mode" ] || [ -z "$repo" ]; then
  echo "usage: $0 (--gguf FILENAME | --repo) <hf-repo-id>" >&2
  exit 64
fi
if [ "$mode" = "gguf" ] && [ -z "$file" ]; then
  echo "--gguf requires a filename" >&2
  exit 64
fi

# shellcheck disable=SC1091
. "$VENV/bin/activate" 2>/dev/null || true

mkdir -p "$MODELS_DIR"

if [ "$mode" = "gguf" ]; then
  python - <<EOF >&2
from huggingface_hub import hf_hub_download
p = hf_hub_download(
    repo_id="$repo",
    filename="$file",
    local_dir="$MODELS_DIR",
)
print("downloaded:", p)
EOF
  echo "$MODELS_DIR/$file"
else
  out="$MODELS_DIR/${repo//\//__}"
  python - <<EOF >&2
from huggingface_hub import snapshot_download
p = snapshot_download(repo_id="$repo", local_dir="$out")
print("downloaded:", p)
EOF
  echo "$out"
fi
