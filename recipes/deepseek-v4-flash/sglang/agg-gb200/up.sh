#!/usr/bin/env bash
# Bring up Cognitora with deepseek-ai/DeepSeek-V4-Flash on SGLang,
# aggregated, TP=4 + MXFP4 MoE + EAGLE MTP 3/4, 4× GB200 (single NVL4
# tray, arm64).
#
# Mirrors NVIDIA Dynamo's `sglang-agg-gb200` recipe.
set -euo pipefail

# Skip the slow precompile and use the fast warmup path.
export SGLANG_JIT_DEEPGEMM_PRECOMPILE="${SGLANG_JIT_DEEPGEMM_PRECOMPILE:-0}"
export SGLANG_JIT_DEEPGEMM_FAST_WARMUP="${SGLANG_JIT_DEEPGEMM_FAST_WARMUP:-1}"

# Required for V4 NCCL collectives on Blackwell.
export NCCL_CUMEM_ENABLE="${NCCL_CUMEM_ENABLE:-1}"

# Pin Gloo to the standard interface (matches the dynamo recipe).
export GLOO_SOCKET_IFNAME="${GLOO_SOCKET_IFNAME:-eth0}"

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
