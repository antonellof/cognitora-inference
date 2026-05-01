#!/usr/bin/env bash
# examples/multi-llm/demo.sh
#
# Hit a running multi-LLM stack with the same demo flow we ran on the
# Cognitora GCP smoke instance:
#
#   1. /v1/models           — list the registered models.
#   2. chat to Qwen 0.5B     — small/fast model.
#   3. chat to TinyLlama     — different family for diversity.
#   4. streaming SSE         — count chunks + watch the [DONE] terminator.
#   5. parallel rate-limit   — fire 5 concurrent requests; expect some 429s
#                              when [router.rate_limit] is tight.

set -uo pipefail

ROUTER=${ROUTER:-http://127.0.0.1:8080}
QWEN=${QWEN:-Qwen/Qwen2.5-0.5B-Instruct}
TINY=${TINY:-TinyLlama/TinyLlama-1.1B-Chat-v1.0}

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
hr()   { printf '\n\033[1;34m──── %s ────\033[0m\n' "$*"; }

hr "GET /v1/models"
curl -fsS "$ROUTER/v1/models" | python3 -m json.tool

chat() {
  local model=$1 prompt=$2 max=${3:-32}
  hr "POST /v1/chat/completions  ($model)"
  bold ">> $prompt"
  curl -fsS -m 120 -H 'Content-Type: application/json' \
    "$ROUTER/v1/chat/completions" -d "{
      \"model\": \"$model\",
      \"messages\": [{\"role\":\"user\",\"content\":$(jq -nc --arg s "$prompt" '$s')}],
      \"max_tokens\": $max,
      \"temperature\": 0.0
    }" \
    | python3 -c '
import sys, json
d = json.load(sys.stdin)
print("==", d["model"])
print(d["choices"][0]["message"]["content"].strip())
print("--", d.get("usage", {}))'
}

chat "$QWEN" "Write a one-sentence haiku about an AI inference router." 24
chat "$TINY" "List three reasons KV-cache reuse matters."                32

hr "STREAMING /v1/chat/completions  ($QWEN)"
curl -sN -m 60 -H 'Content-Type: application/json' \
  "$ROUTER/v1/chat/completions" -d "{
    \"model\": \"$QWEN\",
    \"messages\": [{\"role\":\"user\",\"content\":\"count slowly: 1 2 3 4 5\"}],
    \"max_tokens\": 24,
    \"stream\": true
  }" \
  | awk '/^data:/ { n++; if (n<=4) print "  chunk:", substr($0, 7, 80) } END { print "total chunks:", n }'

hr "RATE LIMIT (5 concurrent)"
codes=()
for _ in 1 2 3 4 5; do
  ( curl -s -o /dev/null -w '%{http_code} ' -m 30 \
      -H 'Content-Type: application/json' \
      "$ROUTER/v1/chat/completions" -d "{
        \"model\":\"$QWEN\",
        \"messages\":[{\"role\":\"user\",\"content\":\"hi\"}],
        \"max_tokens\":4
      }" ) &
done | tr ' ' '\n' | sort | uniq -c
wait
echo
bold "demo complete"
