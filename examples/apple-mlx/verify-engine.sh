#!/usr/bin/env bash
# examples/apple-mlx/verify-engine.sh
#
# Confirms mlx_lm.server on :8090 (not the router on :8080).
# Run after: bash scripts/run/up.sh examples/apple-mlx

set -euo pipefail

MLX_URL=${MLX_URL:-http://127.0.0.1:8090}
MODEL=${MODEL:-mlx-community/Llama-3.2-3B-Instruct-4bit}
HERE=$(cd "$(dirname "$0")" && pwd)

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

if [[ "${CGN_NO_AUTOPULL:-0}" != "1" ]]; then
  hr "Pre-warm: $MODEL (skip with CGN_NO_AUTOPULL=1)"
  bash "$HERE/download-model.sh" "$MODEL"
fi

hr "GET $MLX_URL/v1/models (proves MLX HTTP is up)"
bold "Note: an empty data:[] is NORMAL — mlx_lm only lists models in the HF cache,"
bold "      and only appends --model when it is a LOCAL path (not an HF repo id)."
curl -fsS -m 30 "$MLX_URL/v1/models" | python3 -m json.tool

hr "POST $MLX_URL/v1/chat/completions (streaming)"
bold "First POST after start triggers HF download + model load + first compile."
bold "That phase is SILENT — no data: lines until generation begins."
bold "Watch progress in another terminal: tail -f ~/.cache/cognitora/run/agent-mlx.log"
bold "Do not press Ctrl+Z (suspends the script). Use Ctrl+C to abort."
echo
echo "(curl will block until tokens start; up to ~15 min on first run)"
curl -N --max-time 1800 -sS "$MLX_URL/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -d "$(python3 -c '
import json, sys
print(json.dumps({
    "model": sys.argv[1],
    "stream": True,
    "messages": [{"role": "user", "content": "Say hi in three words."}],
    "max_tokens": 32,
    "temperature": 0.0,
}))' "$MODEL")" | head -n 40

echo
bold "verify-engine: tokens received — MLX is generating."
