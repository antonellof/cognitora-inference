#!/usr/bin/env bash
# examples/apple-mlx/demo.sh
#
# Smoke test for examples/apple-mlx (MLX-LM engine). Assumes:
#   bash scripts/run/up.sh examples/apple-mlx
#   pip install mlx-lm

set -uo pipefail

ROUTER=${ROUTER:-http://127.0.0.1:8080}
ADMIN=${ADMIN:-http://127.0.0.1:9091}
MODEL=${MODEL:-mlx-community/Meta-Llama-3.2-3B-Instruct-4bit}

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

hr "router admin /healthz"
curl -fsS "$ADMIN/healthz" && echo

hr "GET /v1/models"
curl -fsS "$ROUTER/v1/models" | python3 -m json.tool

hr "POST /v1/chat/completions ($MODEL)"
bold ">> Say hello in one short sentence."
curl -fsS -m 180 -H 'Content-Type: application/json' \
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
