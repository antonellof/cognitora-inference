#!/usr/bin/env bash
# Bring up Cognitora with deepseek-ai/DeepSeek-V4-Flash on vLLM,
# aggregated, DP=4 + Expert Parallel, 4× B200.
#
# Mirrors NVIDIA Dynamo's `vllm-agg-b200` recipe. The DeepSeek-V4-
# specific env (FP4 indexer cache, DP dummy-input stabilizers, NCCL)
# is exported here so the spawned vLLM child inherits it.
set -euo pipefail

# Stabilize DP dummy inputs and skip the P2P check (matches the
# DeepSeek-R1 vLLM recipe).
export VLLM_RANDOMIZE_DP_DUMMY_INPUTS="${VLLM_RANDOMIZE_DP_DUMMY_INPUTS:-1}"
export VLLM_SKIP_P2P_CHECK="${VLLM_SKIP_P2P_CHECK:-1}"

# Match the agent's startup probe budget (~60 min first launch).
export VLLM_ENGINE_READY_TIMEOUT_S="${VLLM_ENGINE_READY_TIMEOUT_S:-3600}"

# Required for V4 NCCL collectives on Blackwell.
export NCCL_CUMEM_ENABLE="${NCCL_CUMEM_ENABLE:-1}"

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
