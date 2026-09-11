#!/usr/bin/env bash
# Mac / CPU dev validation for bench harnesses (no GPU, no live stack).
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
OUT="$HERE/.validate-local"
mkdir -p "$OUT"

log() { printf '[validate-local] %s\n' "$*"; }

log "disagg workload generation"
python3 "$HERE/disagg/workload.py" --out "$OUT/workload.jsonl" --n 8 --prefix-tokens 64

log "disagg summarize (fixture inputs)"
cat >"$OUT/results.jsonl" <<'EOF'
{"name":"disagg","n":8,"ok":8,"ttft_ms":{"p50":120,"p95":340},"decode_tps":{"p50":42.1},"system_tps":81.9}
{"name":"agg","n":8,"ok":8,"ttft_ms":{"p50":180,"p95":420},"decode_tps":{"p50":38.0},"system_tps":72.5}
EOF
python3 "$HERE/disagg/summarize.py" \
  --in "$OUT/results.jsonl" \
  --out-md "$OUT/disagg-summary.md" \
  --out-json "$OUT/disagg-results.json" \
  --meta "model=test" "n=8"

log "energy summarize (fixture inputs)"
cat >"$OUT/before.json" <<'EOF'
{"t": 1, "power_watts": 200, "completion_tokens": 1000, "tokens_per_watt": 0}
EOF
cat >"$OUT/after.json" <<'EOF'
{"t": 6, "power_watts": 220, "completion_tokens": 1500, "tokens_per_watt": 0.05}
EOF
cat >"$OUT/bench-energy.json" <<'EOF'
{"wall_s": 5, "total_completion_tokens": 500}
EOF
python3 "$HERE/energy/summarize.py" \
  --before "$OUT/before.json" \
  --after "$OUT/after.json" \
  --bench "$OUT/bench-energy.json" \
  --out-md "$OUT/energy-summary.md" \
  --out-json "$OUT/energy-results.json" \
  --meta "model=test" "n=8" "concurrency=2"

log "cgn-kv-connector unit tests"
python3 "$ROOT/python/cgn-kv-connector/tests/test_connector.py"
python3 "$ROOT/python/cgn-kv-connector/tests/test_kv_client.py"

log "ok — harness scripts validated (GPU runs still require a Linux GPU host)"
