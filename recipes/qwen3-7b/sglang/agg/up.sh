#!/usr/bin/env bash
# Bring up Cognitora with Qwen-3-7B (SGLang, aggregated, 1×GPU).
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
. "$HERE/../../../_lib/recipe.sh"
recipe_up "$HERE"
