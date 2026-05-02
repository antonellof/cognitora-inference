#!/usr/bin/env bash
# examples/docker-ollama/demo.sh
#
# Smoke-test the docker-ollama compose stack. Assumes:
#   - `docker compose up -d` has been run from this directory.
#   - Ollama is running on the docker host (default 127.0.0.1:11434)
#     with `phi3:mini` pulled.

set -uo pipefail

ROUTER=${ROUTER:-http://127.0.0.1:8080}
ADMIN=${ADMIN:-http://127.0.0.1:9091}
MODEL=${MODEL:-phi3:mini}

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

hr "router /healthz + /readyz"
curl -fsS "$ADMIN/healthz" && echo
curl -fsS "$ADMIN/readyz"  && echo

hr "etcd registration"
docker exec cognitora-etcd etcdctl get --prefix /cognitora/nodes/ \
  || echo "(no nodes registered — agent may still be starting)"

hr "GET /v1/models"
curl -fsS "$ROUTER/v1/models" | python3 -m json.tool

hr "POST /v1/chat/completions  ($MODEL)"
bold ">> Say hi in three words."
curl -fsS -m 120 -H 'content-type: application/json' \
  "$ROUTER/v1/chat/completions" \
  -d "{
    \"model\": \"$MODEL\",
    \"messages\": [{\"role\":\"user\",\"content\":\"Say hi in three words.\"}],
    \"max_tokens\": 20,
    \"temperature\": 0.0
  }" | python3 -c '
import sys, json
d = json.load(sys.stdin)
print("==", d["model"])
print(d["choices"][0]["message"]["content"].strip())
print("--", d.get("usage", {}))'

hr "STREAMING /v1/chat/completions  ($MODEL)"
curl -sN -m 120 -H 'content-type: application/json' \
  "$ROUTER/v1/chat/completions" \
  -d "{
    \"model\": \"$MODEL\",
    \"messages\": [{\"role\":\"user\",\"content\":\"count slowly: 1 2 3 4 5\"}],
    \"max_tokens\": 24,
    \"stream\": true
  }" \
  | awk '/^data:/ { n++; if (n<=4) print "  chunk:", substr($0, 7, 120) } END { print "total chunks:", n }'

hr "Prometheus metrics (top cgn_*)"
curl -fsS "$ADMIN/metrics" | grep -E '^cgn_' | head -5 \
  || echo "(no cgn_* metrics yet — fire a few more requests first)"

echo
bold "demo complete"
