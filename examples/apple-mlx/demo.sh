#!/usr/bin/env bash
# examples/apple-mlx/demo.sh
#
# End-to-end smoke test for examples/apple-mlx (MLX-LM engine).
#
# Order of operations:
#   1. cargo build (you do this once)
#   2. bash scripts/install/install-etcd.sh
#   3. bash scripts/run/up.sh examples/apple-mlx
#   4. bash examples/apple-mlx/demo.sh   ← THIS SCRIPT
#
# This script runs a model pre-warm (with progress bar) before hitting the
# router, so the first /v1/chat/completions does not sit silent during a
# multi-GiB Hugging Face download. Set CGN_NO_AUTOPULL=1 to skip the
# pre-warm (you must have the weights cached already).

set -uo pipefail

ROUTER=${ROUTER:-http://127.0.0.1:8080}
ADMIN=${ADMIN:-http://127.0.0.1:9091}
MODEL=${MODEL:-mlx-community/Llama-3.2-3B-Instruct-4bit}
HERE=$(cd "$(dirname "$0")" && pwd)

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

if [[ "${CGN_NO_AUTOPULL:-0}" != "1" ]]; then
  hr "Pre-warm: $MODEL (skip with CGN_NO_AUTOPULL=1)"
  bash "$HERE/download-model.sh" "$MODEL"
fi

hr "router admin /healthz"
curl -fsS "$ADMIN/healthz" && echo

hr "GET /v1/models"
curl -fsS "$ROUTER/v1/models" | python3 -m json.tool

hr "POST /v1/chat/completions ($MODEL)"
bold ">> Say hello in one short sentence."
bold "Note: first request can still take a minute (MLX weight load + first compile)."
bold "Watch progress: tail -f ${WORK:-$HOME/.cache/cognitora/run}/agent-mlx.log"
curl -fsS -m 900 -H 'Content-Type: application/json' \
  "$ROUTER/v1/chat/completions" -d "$(python3 -c '
import json, sys
print(json.dumps({
    "model": sys.argv[1],
    "messages": [{"role": "user", "content": "Say hello in one short sentence."}],
    "max_tokens": 64,
    "temperature": 0.0,
}))' "$MODEL")" \
  | python3 -c '
import sys, json
d = json.load(sys.stdin)
print("==", d.get("model"))
print(d["choices"][0]["message"]["content"].strip())
print("--", d.get("usage", {}))'

bold "demo complete"
