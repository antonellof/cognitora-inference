#!/usr/bin/env bash
# scripts/install/install-engine-gpu.sh
#
# Install the GPU inference engine (vLLM) into a Python venv at $VENV
# (default: ~/venv). Idempotent. Requires a working CUDA toolkit on the
# host; vLLM bundles its own torch wheel.
#
# Usage:
#   bash scripts/install/install-engine-gpu.sh
#   VENV=/opt/cognitora/venv VLLM_VERSION=0.9.1 bash scripts/install/install-engine-gpu.sh
#
# After this runs, the agent can be started with engine.kind = "vllm".

set -euo pipefail

VENV=${VENV:-$HOME/venv}
VLLM_VERSION=${VLLM_VERSION:-0.9.1}

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33mwarn\033[0m %s\n' "$*"; }

if ! command -v nvidia-smi >/dev/null 2>&1; then
  warn "nvidia-smi not found — vLLM expects an NVIDIA GPU; aborting."
  warn "Use scripts/install/install-engine-cpu.sh for CPU-only nodes."
  exit 1
fi

if [ ! -d "$VENV" ]; then
  log "creating venv at $VENV"
  python3 -m venv "$VENV"
fi
# shellcheck disable=SC1091
. "$VENV/bin/activate"

log "upgrading pip"
pip install --quiet --upgrade pip

log "installing vLLM $VLLM_VERSION + huggingface_hub"
pip install --quiet "vllm==$VLLM_VERSION" "huggingface_hub>=0.24"

python -c "import vllm; print('vllm', vllm.__version__)"
log "engine (GPU) ready in $VENV"
