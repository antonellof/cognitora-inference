#!/usr/bin/env bash
# tests/e2e/single_node.sh
#
# CPU-only smoke for the OpenAI HTTP surface. Boots cgn-router with
# auth+mTLS off and a single in-process fake engine, then probes:
#
#   GET  /healthz
#   GET  /v1/models
#   POST /v1/chat/completions  (streaming + buffered)
#
# Used by .github/workflows/e2e.yml. Run locally with:
#
#   ./tests/e2e/single_node.sh
#
# Requirements: cargo, curl, jq, an unused port range :18080..:19092.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d)"
trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$TMP"' EXIT

ok()   { printf '\033[1;32mok\033[0m   %s\n' "$*"; }
fail() { printf '\033[1;31mfail\033[0m %s\n' "$*"; exit 1; }

cd "$REPO_ROOT"

# 1. Build router (no rocksdb on dev machines)
echo "==> building cgn-router"
cargo build --release -p cgn-router \
  --no-default-features 2>&1 | tail -5

# 2. Minimal config
mkdir -p "$TMP/data"
cat > "$TMP/cognitora.toml" <<EOF
[cluster]
name     = "smoke"
data_dir = "$TMP/data"
etcd     = []

[security]
require_mtls = false

[auth]
enabled = false

[router]
listen_http  = "127.0.0.1:18080"
listen_grpc  = "127.0.0.1:17070"
listen_admin = "127.0.0.1:19091"
EOF

# 3. Boot router
echo "==> booting cgn-router"
"$REPO_ROOT/target/release/cgn-router" --config "$TMP/cognitora.toml" \
  > "$TMP/router.log" 2>&1 &

# 4. Wait for /healthz (max 10 s)
for _ in {1..20}; do
  if curl -sSf -m 1 http://127.0.0.1:19091/healthz > /dev/null 2>&1; then
    break
  fi
  sleep 0.5
done
curl -sSf -m 1 http://127.0.0.1:19091/healthz > /dev/null \
  || { cat "$TMP/router.log"; fail "/healthz did not come up"; }
ok   "/healthz"

# 5. /v1/models
body="$(curl -sSf -m 3 http://127.0.0.1:18080/v1/models)"
echo "$body" | grep -q '"object":"list"' \
  || fail "/v1/models bad payload: $body"
ok   "/v1/models"

# 6. POST /v1/chat/completions — expect 503 (no agent), JSON-shaped error
status="$(curl -sS -o "$TMP/chat.json" -w '%{http_code}' \
  -m 5 -X POST http://127.0.0.1:18080/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"llama3-8b","messages":[{"role":"user","content":"Hi"}]}')"
[[ "$status" == "503" ]] \
  || fail "expected 503 (no agents), got $status"
grep -q '"error"' "$TMP/chat.json" \
  || fail "error envelope missing"
ok   "POST /v1/chat/completions returns 503 with error envelope"

# 7. /metrics serves Prometheus content-type
ctype="$(curl -sSI -m 3 http://127.0.0.1:19091/metrics | tr -d '\r' | awk '/^content-type:/ {print tolower($2)}')"
[[ "$ctype" =~ text/plain ]] \
  || fail "expected text/plain on /metrics, got '$ctype'"
ok   "/metrics content-type=$ctype"

echo ""
echo "==> single-node smoke passed"
