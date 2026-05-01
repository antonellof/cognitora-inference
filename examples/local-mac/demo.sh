#!/usr/bin/env bash
# examples/local-mac/demo.sh
#
# End-to-end exercise of a Mac-local Cognitora stack. Assumes:
#   - The stack is already up (see ../../scripts/run/up.sh examples/local-mac).
#   - Ollama is serving phi3:mini and llama3.2 on 127.0.0.1:11434.
#
# Steps mirror examples/multi-llm/demo.sh but use Ollama-native model names.

set -uo pipefail

ROUTER=${ROUTER:-http://127.0.0.1:8080}
ADMIN=${ADMIN:-http://127.0.0.1:9091}
PHI3=${PHI3:-phi3:mini}
LLAMA=${LLAMA:-llama3.2:latest}

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

hr "router admin /healthz"
curl -fsS "$ADMIN/healthz" && echo

hr "GET /v1/models"
curl -fsS "$ROUTER/v1/models" | python3 -m json.tool

chat() {
  local model=$1 prompt=$2 max=${3:-48}
  hr "POST /v1/chat/completions  ($model)"
  bold ">> $prompt"
  curl -fsS -m 120 -H 'Content-Type: application/json' \
    "$ROUTER/v1/chat/completions" -d "$(python3 -c '
import json, sys
print(json.dumps({
    "model": sys.argv[1],
    "messages": [{"role": "user", "content": sys.argv[2]}],
    "max_tokens": int(sys.argv[3]),
    "temperature": 0.0,
}))' "$model" "$prompt" "$max")" \
    | python3 -c '
import sys, json
d = json.load(sys.stdin)
print("==", d["model"])
print(d["choices"][0]["message"]["content"].strip())
print("--", d.get("usage", {}))'
}

chat "$PHI3"  "Write a one-sentence haiku about an AI inference router." 32
chat "$LLAMA" "List three reasons KV-cache reuse matters."                64

hr "STREAMING /v1/chat/completions  ($PHI3)"
curl -sN -m 60 -H 'Content-Type: application/json' \
  "$ROUTER/v1/chat/completions" -d "{
    \"model\": \"$PHI3\",
    \"messages\": [{\"role\":\"user\",\"content\":\"count slowly: 1 2 3 4 5\"}],
    \"max_tokens\": 24,
    \"stream\": true
  }" \
  | awk '/^data:/ { n++; if (n<=4) print "  chunk:", substr($0, 7, 120) } END { print "total chunks:", n }'

hr "Prometheus metrics (top 5 cgn_*)"
curl -fsS "$ADMIN/metrics" | grep -E '^cgn_' | head -5 || echo "(no cgn_* metrics yet — run a few more requests first)"

hr "RATE LIMIT (5 concurrent)"
for _ in 1 2 3 4 5; do
  ( curl -s -o /dev/null -w '%{http_code} ' -m 30 \
      -H 'Content-Type: application/json' \
      "$ROUTER/v1/chat/completions" -d "{
        \"model\":\"$PHI3\",
        \"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],
        \"max_tokens\":4
      }" ) &
done | tr ' ' '\n' | sort | uniq -c
wait
echo
bold "demo complete"
