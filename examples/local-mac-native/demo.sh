#!/usr/bin/env bash
# examples/local-mac-native/demo.sh
#
# End-to-end exercise of a Mac-local Cognitora stack running the native
# cgn-infer engine. Assumes the stack is already up
# (see ../../scripts/run/up.sh examples/local-mac-native).

set -uo pipefail

ROUTER=${ROUTER:-http://127.0.0.1:8080}
ADMIN=${ADMIN:-http://127.0.0.1:9091}
MODEL=${MODEL:-llama-3.2-3b-instruct}

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

hr "router admin /healthz"
curl -fsS "$ADMIN/healthz" && echo

hr "GET /v1/models"
curl -fsS "$ROUTER/v1/models" | python3 -m json.tool

hr "POST /v1/chat/completions  ($MODEL)"
bold ">> Write a one-sentence haiku about a native inference engine."
curl -fsS -m 300 -H 'Content-Type: application/json' \
  "$ROUTER/v1/chat/completions" -d "{
    \"model\": \"$MODEL\",
    \"messages\": [{\"role\":\"user\",\"content\":\"Write a one-sentence haiku about a native inference engine.\"}],
    \"max_tokens\": 48,
    \"temperature\": 0.0
  }" | python3 -c '
import sys, json
d = json.load(sys.stdin)
print("==", d["model"])
print(d["choices"][0]["message"]["content"].strip())
print("--", d.get("usage", {}))'

hr "STREAMING /v1/chat/completions  ($MODEL)"
curl -sN -m 300 -H 'Content-Type: application/json' \
  "$ROUTER/v1/chat/completions" -d "{
    \"model\": \"$MODEL\",
    \"messages\": [{\"role\":\"user\",\"content\":\"count slowly: 1 2 3 4 5\"}],
    \"max_tokens\": 24,
    \"stream\": true
  }" \
  | awk '/^data:/ { n++; if (n<=4) print "  chunk:", substr($0, 7, 120) } END { print "total chunks:", n }'

hr "Prometheus metrics (top 5 cgn_*)"
curl -fsS "$ADMIN/metrics" | grep -E '^cgn_' | head -5 || echo "(no cgn_* metrics yet — run a few more requests first)"

echo
bold "demo complete"
