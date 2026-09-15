#!/usr/bin/env bash
# scripts/bench/energy/run.sh
#
# Measure fleet energy efficiency while driving chat completions through
# cgn-router. Assumes a running stack (recipe or manual); samples router
# /metrics before and after bench_client.py and writes tokens/W and J/token.
#
# Usage:
#   bash scripts/bench/energy/run.sh
#
# Knobs (env):
#   N, CONC, MAX_TOKENS, MODEL, ROUTER_URL, ADMIN_URL, OUT_DIR

set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)

N=${N:-32}
CONC=${CONC:-8}
MAX_TOKENS=${MAX_TOKENS:-128}
MODEL=${MODEL:-meta-llama/Meta-Llama-3.1-8B-Instruct}
ROUTER_URL=${ROUTER_URL:-http://127.0.0.1:8080}
ADMIN_URL=${ADMIN_URL:-http://127.0.0.1:9091}
OUT_DIR=${OUT_DIR:-$HERE/results}

log()  { printf '\033[1;34m[energy-bench]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[energy-bench] fail:\033[0m %s\n' "$*" >&2; exit 1; }

require_metrics_endpoint() {
  local url="$ADMIN_URL/metrics"
  log "checking metrics endpoint at $url"
  if ! curl -fsS -m 5 "$url" >/dev/null 2>&1; then
    fail "router metrics not reachable at $url — start a stack first (e.g. bash recipes/llama3-8b/vllm/agg/up.sh). See scripts/bench/energy/README.md"
  fi
}

require_router_endpoint() {
  local url="$ROUTER_URL/v1/models"
  log "checking router at $url"
  if ! curl -fsS -m 5 "$url" >/dev/null 2>&1; then
    fail "router not reachable at $url — start a stack first (e.g. bash recipes/llama3-8b/vllm/agg/up.sh). See scripts/bench/energy/README.md"
  fi
}

command -v python3 >/dev/null 2>&1 || fail "python3 not found"
command -v curl >/dev/null 2>&1 || fail "curl not found"
mkdir -p "$OUT_DIR"
require_metrics_endpoint
require_router_endpoint

BENCH="$ROOT/scripts/bench/bench_client.py"
[ -f "$BENCH" ] || fail "missing $BENCH"

sample() {
  PYTHONPATH="$HERE" python3 - "$1" <<'PY'
import json, sys, time
from metrics import sample
print(json.dumps({"t": time.time(), **sample(sys.argv[1])}))
PY
}

log "sampling metrics before load ($ADMIN_URL/metrics)"
BEFORE="$OUT_DIR/before.json"
sample "$ADMIN_URL/metrics" >"$BEFORE"

log "running bench_client n=$N conc=$CONC"
BENCH_OUT="$OUT_DIR/bench.json"
set +e
python3 "$BENCH" \
  --url "$ROUTER_URL/v1/chat/completions" \
  --model "$MODEL" \
  --n "$N" \
  --concurrency "$CONC" \
  --max-tokens "$MAX_TOKENS" \
  --stream \
  --name energy \
  >"$BENCH_OUT"
rc=$?
set -e
if [ "$rc" -eq 2 ]; then
  fail "bench client recorded zero completion tokens — engine likely down or misconfigured"
elif [ "$rc" -ne 0 ]; then
  fail "bench client failed (exit $rc — no successful samples)"
fi

log "sampling metrics after load"
AFTER="$OUT_DIR/after.json"
sample "$ADMIN_URL/metrics" >"$AFTER"

python3 "$HERE/summarize.py" \
  --before "$BEFORE" \
  --after "$AFTER" \
  --bench "$BENCH_OUT" \
  --out-json "$OUT_DIR/results.json" \
  --out-md "$OUT_DIR/summary.md" \
  --meta "model=$MODEL" "n=$N" "concurrency=$CONC" "max_tokens=$MAX_TOKENS"

log "wrote $OUT_DIR/summary.md"
cat "$OUT_DIR/summary.md"
