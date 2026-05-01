#!/usr/bin/env bash
# scripts/install/install-engine-cpu.sh
#
# Install the CPU inference engine (llama-cpp-python with the OpenAI server)
# into a Python venv at $VENV (default: ~/venv). Idempotent.
#
# Usage:
#   bash scripts/install/install-engine-cpu.sh
#   VENV=/opt/cognitora/venv bash scripts/install/install-engine-cpu.sh
#
# After this runs, a model can be downloaded with:
#   bash scripts/install/download-model.sh <hf-repo> --gguf <filename>
# and the agent can be started with engine.kind = "llama_cpp".

set -euo pipefail

VENV=${VENV:-$HOME/venv}
LLAMA_CPP_VERSION=${LLAMA_CPP_VERSION:-0.3.4}

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

if [ ! -d "$VENV" ]; then
  log "creating venv at $VENV"
  python3 -m venv "$VENV"
fi
# shellcheck disable=SC1091
. "$VENV/bin/activate"

log "upgrading pip"
pip install --quiet --upgrade pip

log "installing llama-cpp-python[server] $LLAMA_CPP_VERSION + huggingface_hub"
pip install --quiet "llama-cpp-python[server]==$LLAMA_CPP_VERSION" "huggingface_hub>=0.24"

python -c "import llama_cpp; print('llama_cpp', llama_cpp.__version__)"
log "engine (CPU) ready in $VENV"
