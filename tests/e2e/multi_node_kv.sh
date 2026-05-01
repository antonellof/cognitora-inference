#!/usr/bin/env bash
# tests/e2e/multi_node_kv.sh
#
# 4-node cluster smoke for KV-aware routing. Brings up 1 router + 4
# fake agents on the loopback interface, replays the
# tests/fixtures/traces/openai_chatlog.jsonl fixture, and verifies the
# cache hit ratio meets the platform SLO (>= 0.55).

set -euo pipefail

if [[ "${CGN_E2E_MULTINODE:-0}" != "1" ]]; then
  cat <<EOF >&2
==> tests/e2e/multi_node_kv.sh requires a running etcd + 4 fake agents.

This harness expects:
  - etcd reachable on \$COGNITORA_ETCD (default 127.0.0.1:2379)
  - tests/fixtures/traces/openai_chatlog.jsonl (replay trace)

Set CGN_E2E_MULTINODE=1 to enable, otherwise the script exits cleanly.
EOF
  exit 0
fi

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO_ROOT"

ETCD_ENDPOINTS="${COGNITORA_ETCD:-127.0.0.1:2379}"
WORK=$(mktemp -d)
trap 'pkill -P $$ 2>/dev/null || true; rm -rf "$WORK"' EXIT

echo "==> Building cgn-router (no-default-features for the dev path)"
cargo build -p cgn-router --no-default-features --quiet

echo "==> Bootstrapping dev PKI"
target/debug/cgn-ctl pki bootstrap --out "$WORK/pki" >/dev/null

echo "==> Seeding 4 fake agent records into etcd"
for i in 0 1 2 3; do
  KEY="/cognitora/nodes/node-${i}"
  VAL=$(cat <<JSON
{"node_id":"node-${i}","address":"https://127.0.0.1:707${i}","role":3,
 "queue_depth":0,"free_blocks":1024,"total_blocks":1024,"power_watts":120,
 "model":"llama3-8b","gpu_index":${i}}
JSON
)
  etcdctl --endpoints="$ETCD_ENDPOINTS" put "$KEY" "$VAL" >/dev/null
done

echo "==> Booting cgn-router"
cat >"$WORK/cognitora.toml" <<EOF
[router]
listen_http  = "0.0.0.0:8080"
listen_grpc  = "0.0.0.0:9090"
listen_admin = "0.0.0.0:9091"
[router.score_weights]
kv = 0.55; load = 0.25; power = 0.10; capacity = 0.10
[cluster]
etcd_endpoints = ["${ETCD_ENDPOINTS}"]
[security]
require_mtls = false
[auth]
required = false
[models.llama3-8b]
EOF

target/debug/cgn-router --config "$WORK/cognitora.toml" >"$WORK/router.log" 2>&1 &

for _ in {1..30}; do
  curl -fsS http://127.0.0.1:9091/healthz >/dev/null 2>&1 && break
  sleep 0.5
done

echo "==> Replaying fixture"
while IFS= read -r line; do
  RESP=$(curl -s -o /dev/null -w '%{http_code}' \
    -H 'content-type: application/json' \
    -d "$line" \
    http://127.0.0.1:8080/v1/chat/completions || true)
  if [[ "$RESP" == "503" ]]; then
    # 503 means the fake agents aren't reachable, which is the
    # expected outcome of this fixture-only smoke; the routing
    # decision is still made and counted in
    # cgn_router_kv_cache_lookups_total.
    :
  fi
done <"${REPO_ROOT}/tests/fixtures/traces/openai_chatlog.jsonl"

HIT_RATIO=$(curl -s http://127.0.0.1:9091/metrics \
  | awk '/^cgn_router_kv_cache_hits_total / {h=$2} /^cgn_router_kv_cache_lookups_total / {l=$2} END { if (l>0) print h/l; else print 0 }')

echo "==> hit ratio: $HIT_RATIO (target >= 0.55)"
awk -v r="$HIT_RATIO" 'BEGIN { exit !(r+0 >= 0.55) }'
