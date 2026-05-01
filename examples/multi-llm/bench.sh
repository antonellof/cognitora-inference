#!/usr/bin/env bash
# examples/multi-llm/bench.sh
#
# Drive a running Cognitora gateway with N concurrent OpenAI-style chat
# requests and report end-to-end latency / throughput. Useful as a smoke
# benchmark when comparing two engine kinds (vLLM vs llama.cpp) or two
# router configurations (rate-limit, mTLS, KV tiering).
#
# Output: a JSON object with mean, p50, p95, p99, and tokens-per-second.
#
# Usage:
#   bash examples/multi-llm/bench.sh                # 30 reqs, concurrency 4
#   N=120 C=8 MAX_TOKENS=64 bash examples/multi-llm/bench.sh

set -uo pipefail

ROUTER=${ROUTER:-http://127.0.0.1:8080}
MODEL=${MODEL:-Qwen/Qwen2.5-0.5B-Instruct}
N=${N:-30}
C=${C:-4}
MAX_TOKENS=${MAX_TOKENS:-32}
PROMPT=${PROMPT:-"Write a single sentence about KV cache reuse."}

OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

oneshot() {
  local i=$1
  local t0 t1
  t0=$(date +%s.%N)
  local body
  body=$(curl -fsS -m 180 -H 'Content-Type: application/json' \
    "$ROUTER/v1/chat/completions" -d "{
      \"model\": \"$MODEL\",
      \"messages\": [{\"role\":\"user\",\"content\":$(jq -nc --arg s "$PROMPT" '$s')}],
      \"max_tokens\": $MAX_TOKENS,
      \"temperature\": 0.0
    }" 2>/dev/null) || return 0
  t1=$(date +%s.%N)
  local secs tok
  secs=$(awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.6f", b - a }')
  tok=$(printf '%s' "$body" | python3 -c '
import sys, json
try:
    d = json.load(sys.stdin)
    print(d.get("usage", {}).get("completion_tokens", 0))
except Exception:
    print(0)
')
  printf '%s %s\n' "$secs" "$tok" >> "$OUT/lat"
}

start=$(date +%s.%N)
i=0
while [ "$i" -lt "$N" ]; do
  parallel=0
  while [ "$parallel" -lt "$C" ] && [ "$i" -lt "$N" ]; do
    oneshot "$i" &
    i=$((i + 1))
    parallel=$((parallel + 1))
  done
  wait
done
end=$(date +%s.%N)

python3 - <<EOF
import json, statistics, sys
rows = []
with open("$OUT/lat") as f:
    for line in f:
        a, b = line.split()
        rows.append((float(a), int(b)))
if not rows:
    sys.exit("no successful requests")
lat = sorted(r[0] for r in rows)
tok = sum(r[1] for r in rows)
wall = $end - $start
print(json.dumps({
  "model": "$MODEL",
  "router": "$ROUTER",
  "n_requests": len(rows),
  "concurrency": $C,
  "max_tokens": $MAX_TOKENS,
  "wallclock_s": round(wall, 3),
  "latency_s": {
    "mean":  round(statistics.fmean(lat), 3),
    "p50":   round(lat[len(lat)//2], 3),
    "p95":   round(lat[max(int(0.95*len(lat))-1, 0)], 3),
    "p99":   round(lat[max(int(0.99*len(lat))-1, 0)], 3),
    "max":   round(lat[-1], 3),
  },
  "tokens": {
    "completion_total": tok,
    "tokens_per_s_overall": round(tok / wall, 2) if wall else None,
  },
}, indent=2))
EOF
