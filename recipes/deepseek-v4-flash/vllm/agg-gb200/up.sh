#!/usr/bin/env bash
# Bring up Cognitora with deepseek-ai/DeepSeek-V4-Flash on vLLM,
# aggregated, TP=4 + Expert Parallel + DeepGEMM mega MoE, 4× GB200
# (single NVL4 tray, arm64).
#
# Mirrors NVIDIA Dynamo's `vllm-agg-gb200` recipe. The DeepSeek-V4 +
# NVLink Sharp env (NVLS multicast, NCCL P2P-NVL, symmetric memory) is
# exported here so the spawned vLLM child inherits it.
set -euo pipefail

# Match the agent's startup probe budget (~60 min first launch).
export VLLM_ENGINE_READY_TIMEOUT_S="${VLLM_ENGINE_READY_TIMEOUT_S:-3600}"

# Skip the P2P check (the NVL4 tray's intra-NVLink is fully connected).
export VLLM_SKIP_P2P_CHECK="${VLLM_SKIP_P2P_CHECK:-1}"

# NVLink Sharp / NVLS multicast for one-shot all-reduce on the tray.
export VLLM_USE_NCCL_SYMM_MEM="${VLLM_USE_NCCL_SYMM_MEM:-1}"
export NCCL_NVLS_ENABLE="${NCCL_NVLS_ENABLE:-1}"
export NCCL_P2P_LEVEL="${NCCL_P2P_LEVEL:-NVL}"
export NCCL_CUMEM_ENABLE="${NCCL_CUMEM_ENABLE:-1}"

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
