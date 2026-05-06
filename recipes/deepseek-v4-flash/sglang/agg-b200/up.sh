#!/usr/bin/env bash
# Bring up Cognitora with deepseek-ai/DeepSeek-V4-Flash on SGLang,
# aggregated, TP=4 + MXFP4 MoE + EAGLE MTP 3/4, 4× B200.
#
# Mirrors NVIDIA Dynamo's `sglang-agg` recipe. The DeepSeek-V4 +
# DeepGEMM + Gloo env is exported here so the spawned SGLang child
# inherits it.
set -euo pipefail

# Skip the slow precompile and use the fast warmup path. This and
# `--disable-flashinfer-autotune` keep first-launch time bounded to ~60
# min on 4× B200.
export SGLANG_JIT_DEEPGEMM_PRECOMPILE="${SGLANG_JIT_DEEPGEMM_PRECOMPILE:-0}"
export SGLANG_JIT_DEEPGEMM_FAST_WARMUP="${SGLANG_JIT_DEEPGEMM_FAST_WARMUP:-1}"

# Required for V4 NCCL collectives on Blackwell.
export NCCL_CUMEM_ENABLE="${NCCL_CUMEM_ENABLE:-1}"

# Pin Gloo to the standard interface (matches the dynamo recipe).
export GLOO_SOCKET_IFNAME="${GLOO_SOCKET_IFNAME:-eth0}"

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
