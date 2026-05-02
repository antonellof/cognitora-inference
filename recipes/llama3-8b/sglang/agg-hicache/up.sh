#!/usr/bin/env bash
# Bring up Cognitora with Llama-3.1-8B (SGLang, aggregated, HiCache offload).
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
