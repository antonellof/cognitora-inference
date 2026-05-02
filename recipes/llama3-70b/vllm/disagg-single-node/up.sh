#!/usr/bin/env bash
# Bring up Cognitora with Llama-3.3-70B (vLLM, prefill+decode disaggregated, 8×GPU).
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
