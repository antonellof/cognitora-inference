#!/usr/bin/env bash
# scripts/bench/disagg/run.sh
#
# Reproducible disaggregation benchmark: brings up the
# recipes/llama3-8b/vllm/disagg-single-node recipe (prefill + decode
# cgn-agents on 2 GPUs), drives N concurrent streaming chat requests
# through cgn-router with scripts/bench/bench_client.py, records TTFT
# p50/p95 and tokens/s, and tears the stack back down.
#
# With --compare the same load also runs against the aggregated recipe
# (recipes/llama3-8b/vllm/agg) so disagg-vs-agg deltas come from one
# command on the same host.
#
# Usage:
#   bash scripts/bench/disagg/run.sh                 # disagg only
#   bash scripts/bench/disagg/run.sh --compare       # disagg then agg
#   bash scripts/bench/disagg/run.sh --mode agg      # agg only
#   bash scripts/bench/disagg/run.sh --mode disagg-cgn  # CognitoraConnector (preview)
#
# Knobs (env):
#   N              requests per scenario           (default 32)
#   CONC           in-flight concurrency           (default 8)
#   MAX_TOKENS     max_tokens per request          (default 128)
#   PROMPT_TOKENS  shared-prefix length in tokens  (default 512)
#   SHARED_FRAC    fraction of prompts sharing the (default 0.6)
#                  common prefix (rest are unique)
#   OUT_DIR        results directory               (default scripts/bench/disagg/results)
#   MODEL          model id served by the recipes  (default meta-llama/Meta-Llama-3.1-8B-Instruct)
#   ROUTER_URL     router base URL                 (default http://127.0.0.1:8080)
#
# The prompt set is generated once by workload.py (fixed seed, mix of
# shared-prefix and unique prompts) and replayed identically against
# every mode, so the comparison is apples-to-apples.
#
# Outputs (in $OUT_DIR):
#   workload.jsonl  the generated prompt set (identical for all modes)
#   results.jsonl   one JSON record per scenario (bench_client.py output)
#   results.json    combined records + run metadata
#   summary.md      markdown table (TTFT p50/p95, tok/s, deltas)
#
# Prerequisites: 2 GPUs (1 for --mode agg), vLLM with NIXL support,
# etcd (embedded automatically if absent), Cognitora release binaries
# (built automatically if missing). See scripts/bench/disagg/README.md.

set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../../.." && pwd)

N=${N:-32}
CONC=${CONC:-8}
MAX_TOKENS=${MAX_TOKENS:-128}
PROMPT_TOKENS=${PROMPT_TOKENS:-512}
SHARED_FRAC=${SHARED_FRAC:-0.6}
OUT_DIR=${OUT_DIR:-$HERE/results}
MODEL=${MODEL:-meta-llama/Meta-Llama-3.1-8B-Instruct}
ROUTER_URL=${ROUTER_URL:-http://127.0.0.1:8080}

DISAGG_RECIPE="$ROOT/recipes/llama3-8b/vllm/disagg-single-node"
DISAGG_CGN_RECIPE="$ROOT/recipes/llama3-8b/vllm/disagg-cgn"
AGG_RECIPE="$ROOT/recipes/llama3-8b/vllm/agg"

COMPARE=0
MODES=(disagg)
while [ $# -gt 0 ]; do
  case "$1" in
    --compare) COMPARE=1; MODES=(disagg agg); shift ;;
    --mode)
      [ $# -ge 2 ] || { echo "--mode needs an argument (disagg|agg|disagg-cgn)" >&2; exit 64; }
      MODES=("$2"); shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1 (see --help)" >&2; exit 64 ;;
  esac
done

log()  { printf '\033[1;34m[disagg-bench]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[disagg-bench] fail:\033[0m %s\n' "$*" >&2; exit 1; }

# shellcheck disable=SC1091
. "$ROOT/scripts/run/lib.sh"

command -v python3 >/dev/null 2>&1 || fail "python3 not found"
command -v curl >/dev/null 2>&1 || fail "curl not found"

require_gpu_host() {
  if [ "${CGN_BENCH_FORCE:-0}" = "1" ]; then
    warn "CGN_BENCH_FORCE=1 — skipping vllm/GPU preflight"
    return 0
  fi
  command -v vllm >/dev/null 2>&1 \
    || fail "vllm not found in PATH (pip install vllm). Mac/CPU smoke: bash scripts/bench/validate-local.sh"
}

require_agents_up() {
  local down=""
  for f in "$WORK"/agent-*.pid; do
    [ -f "$f" ] || continue
    local name pid
    name=$(basename "$f" .pid)
    pid=$(cat "$f")
    if ! kill -0 "$pid" 2>/dev/null; then
      down="$down $name"
    fi
  done
  if [ -n "$down" ]; then
    fail "agent(s) exited:$down — check $WORK/agent-*.log (usually missing vllm or GPU)"
  fi
}

require_model_ready() {
  local url="$ROUTER_URL/v1/models"
  log "waiting for model $MODEL at $url (up to 300s — first bring-up loads weights)"
  for _ in $(seq 1 300); do
    if curl -fsS -m 2 "$url" 2>/dev/null | python3 -c "
import json, sys
want = sys.argv[1]
try:
    data = json.load(sys.stdin).get('data') or []
except Exception:
    sys.exit(1)
sys.exit(0 if any(m.get('id') == want for m in data) else 1)
" "$MODEL"; then
      return 0
    fi
    sleep 1
  done
  return 1
}

require_gpu_host
mkdir -p "$OUT_DIR"
RESULTS="$OUT_DIR/results.jsonl"
: >"$RESULTS"

# Fixed-seed workload: every mode replays the exact same prompt set
# (mix of shared-prefix and unique prompts, see workload.py).
WORKLOAD="$OUT_DIR/workload.jsonl"
log "generating workload: n=$N shared_frac=$SHARED_FRAC prefix_tokens=$PROMPT_TOKENS"
python3 "$HERE/workload.py" \
  --n "$N" \
  --shared-frac "$SHARED_FRAC" \
  --prefix-tokens "$PROMPT_TOKENS" \
  --seed 0 \
  --out "$WORKLOAD" \
  || fail "workload generation failed"

CURRENT_RECIPE=""
teardown() {
  if [ -n "$CURRENT_RECIPE" ]; then
    log "tearing down $(basename "$CURRENT_RECIPE")"
    bash "$ROOT/scripts/run/down.sh" "$CURRENT_RECIPE" || true
    CURRENT_RECIPE=""
  fi
}
trap teardown EXIT INT TERM

wait_for_router() {
  local url="$ROUTER_URL/v1/models"
  log "waiting for router at $url (up to 300s — first bring-up loads weights)"
  for _ in $(seq 1 300); do
    curl -fsS -m 2 "$url" >/dev/null 2>&1 && return 0
    sleep 1
  done
  return 1
}

run_mode() {
  local mode=$1 recipe
  case "$mode" in
    disagg) recipe=$DISAGG_RECIPE ;;
    disagg-cgn) recipe=$DISAGG_CGN_RECIPE ;;
    agg) recipe=$AGG_RECIPE ;;
    *) fail "unknown mode: $mode (want disagg|disagg-cgn|agg)" ;;
  esac

  log "=== mode: $mode — bringing up $(basename "$recipe") ==="
  CURRENT_RECIPE=$recipe
  CGN_SKIP_PROBE=1 bash "$recipe/up.sh"
  require_agents_up
  require_model_ready || fail "model $MODEL never registered for mode=$mode (see $WORK/router.log agent-*.log)"

  log "load phase: n=$N conc=$CONC max_tokens=$MAX_TOKENS prompt_tokens=$PROMPT_TOKENS"
  local client_args=(
    --name "$mode"
    --url "$ROUTER_URL/v1/chat/completions"
    --model "$MODEL"
    --n "$N"
    --concurrency "$CONC"
    --max-tokens "$MAX_TOKENS"
    --stream
    --warmup 2
    --prompts-file "$WORKLOAD"
  )
  set +e
  python3 "$ROOT/scripts/bench/bench_client.py" "${client_args[@]}" >>"$RESULTS"
  rc=$?
  set -e
  if [ "$rc" -eq 2 ]; then
    fail "bench client recorded zero completion tokens for mode=$mode — engine likely down or misconfigured (see $WORK/agent-*.log)"
  elif [ "$rc" -ne 0 ]; then
    fail "bench client failed for mode=$mode (exit $rc — no successful samples)"
  fi

  teardown
  # Give engine subprocesses a moment to release the GPUs before the
  # next bring-up claims them.
  sleep 5
}

for mode in "${MODES[@]}"; do
  run_mode "$mode"
done

log "writing results.json + summary.md"
python3 "$HERE/summarize.py" \
  --in "$RESULTS" \
  --out-json "$OUT_DIR/results.json" \
  --out-md "$OUT_DIR/summary.md" \
  --meta "model=$MODEL" "n=$N" "concurrency=$CONC" "max_tokens=$MAX_TOKENS" \
         "prompt_tokens=$PROMPT_TOKENS" "shared_frac=$SHARED_FRAC" "compare=$COMPARE"

echo ""
cat "$OUT_DIR/summary.md"
log "raw records: $RESULTS"
log "combined:    $OUT_DIR/results.json"
