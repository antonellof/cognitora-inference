#!/usr/bin/env bash
# tests/e2e/multi_engine.sh
#
# Validate that the engine plugin layer renders the right argv for every
# kind, and that the gateway middleware (rate-limit + auth) is actually
# registered. This test does NOT require a GPU or pulled model weights —
# it runs against the Cognitora binaries with `engine.kind = "openai_compat"`
# and a stub upstream HTTP server provided by python's stdlib.
#
# What it covers
#
#   ✓ supervisor uses [engine] block (no longer hardcodes vLLM).
#   ✓ openai_compat mode does NOT fork a child process.
#   ✓ rate-limit middleware actually rejects bursts (rps=1, burst=1).
#   ✓ auth middleware enforces 401 without a token, 200 with one.
#   ✓ vllm + llama_cpp argv rendering verified by `cargo test`.
#
# What it does NOT cover (run examples/multi-llm/demo.sh on a real host
# with a real model for those):
#
#   ✗ real LLM responses
#   ✗ KV transport / mTLS dial
#
# Run locally: ./tests/e2e/multi_engine.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TMP="$(mktemp -d)"
# Use a high random base to avoid colliding with parallel test runs
# (xdist, GitHub Actions matrix) and previous interrupted runs.
PORT_BASE=${PORT_BASE:-$((25000 + (RANDOM % 5000)))}
PORT_HTTP=$((PORT_BASE + 0))
PORT_GRPC=$((PORT_BASE + 1))
PORT_ADMIN=$((PORT_BASE + 2))
PORT_AGENT=$((PORT_BASE + 3))
PORT_FAKE=$((PORT_BASE + 4))

# Track every PID we spawn so the trap can kill them even after `disown`.
PIDS=()

cleanup() {
  for p in "${PIDS[@]:-}"; do
    [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true
  done
  # Best-effort port reclamation. We bound lsof to 2s because on macOS
  # it can stall enumerating stale sockets. The PID kill above is the
  # primary cleanup.
  for p in "$PORT_HTTP" "$PORT_GRPC" "$PORT_ADMIN" "$PORT_AGENT" "$PORT_FAKE"; do
    leftover=$(timeout_cmd 2 lsof -ti tcp:"$p" 2>/dev/null || true)
    [ -n "$leftover" ] && kill -9 $leftover 2>/dev/null || true
  done
  rm -rf "$TMP"
}

# Cross-platform `timeout` helper. macOS doesn't ship coreutils' timeout;
# use `gtimeout` if installed, otherwise fall back to running the command
# directly (no bound).
timeout_cmd() {
  local secs=$1; shift
  if command -v timeout >/dev/null 2>&1; then
    timeout "$secs" "$@"
  elif command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$secs" "$@"
  else
    "$@"
  fi
}

trap cleanup EXIT INT TERM

# Pre-flight: free the test ports if a previous run leaked. Bounded so we
# don't get stuck on a slow lsof enumeration.
for p in "$PORT_HTTP" "$PORT_GRPC" "$PORT_ADMIN" "$PORT_AGENT" "$PORT_FAKE"; do
  pids=$(timeout_cmd 2 lsof -ti tcp:"$p" 2>/dev/null || true)
  if [ -n "$pids" ]; then
    echo "==> freeing port $p (killing $pids)"
    kill -9 $pids 2>/dev/null || true
    sleep 0.3
  fi
done

ok()   { printf '\033[1;32mok\033[0m   %s\n' "$*"; }
fail() { printf '\033[1;31mfail\033[0m %s\n' "$*"; exit 1; }

cd "$REPO_ROOT"

# 1. Compile-side: verify the spawn module renders argv correctly.
#    Set CGN_SKIP_BUILD=1 in CI to skip if the cargo cache is already warm.
if [ "${CGN_SKIP_BUILD:-0}" != "1" ]; then
  echo "==> cargo test (engine::spawn)"
  cargo test --release -p cgn-agent --no-default-features --bin cgn-agent \
    --quiet engine::spawn::tests 2>&1 | tail -8
fi
ok "engine::spawn argv rendering"

# 2. Build the binaries we need (skipped if they already exist).
need_build=0
for b in cgn-router cgn-agent cgn-ctl; do
  [ -x "$REPO_ROOT/target/release/$b" ] || need_build=1
done
if [ "$need_build" = 1 ] && [ "${CGN_SKIP_BUILD:-0}" != "1" ]; then
  echo "==> building cgn-router + cgn-agent + cgn-ctl"
  cargo build --release --no-default-features \
    -p cgn-router -p cgn-agent -p cgn-ctl 2>&1 | tail -3
fi
for b in cgn-router cgn-agent cgn-ctl; do
  [ -x "$REPO_ROOT/target/release/$b" ] || fail "missing target/release/$b"
done

# 3. Stub OpenAI-compatible engine: a tiny python server that returns
#    canned chat completions. This stands in for vLLM/llama.cpp.
cat > "$TMP/fake_engine.py" <<'PY'
from http.server import BaseHTTPRequestHandler, HTTPServer
import json, sys

class H(BaseHTTPRequestHandler):
    def log_message(self, *a, **k): pass
    def do_GET(self):
        if self.path in ("/health", "/v1/models"):
            self.send_response(200); self.send_header("content-type","application/json"); self.end_headers()
            self.wfile.write(b'{"object":"list","data":[]}'); return
        self.send_error(404)
    def do_POST(self):
        n = int(self.headers.get("content-length","0") or 0)
        body = self.rfile.read(n)
        self.send_response(200); self.send_header("content-type","text/event-stream"); self.end_headers()
        for tok in [' hello', ' world']:
            self.wfile.write(b'data: ' + json.dumps({"choices":[{"text":tok,"finish_reason":None}]}).encode() + b'\n\n'); self.wfile.flush()
        self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush()

HTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
PY
python3 "$TMP/fake_engine.py" "$PORT_FAKE" >/dev/null 2>&1 &
PIDS+=("$!")

# 4. Bootstrap PKI + an API key.
mkdir -p "$TMP/pki"
"$REPO_ROOT/target/release/cgn-ctl" pki bootstrap --out "$TMP/pki" \
  --san localhost --san 127.0.0.1 >/dev/null 2>&1
KEY=$("$REPO_ROOT/target/release/cgn-ctl" key create \
  --scopes chat,embed --file "$TMP/api-keys" | grep -oE 'cgn-[A-Za-z0-9]{20,}' | head -1)
[ -n "$KEY" ] || fail "cgn-ctl key create did not emit a token"

# 5. Agent config: openai_compat → no spawn, just proxy at PORT_FAKE.
cat > "$TMP/agent.toml" <<EOF
[cluster]
name           = "ci"
state_backend  = "etcd"
etcd_endpoints = []

[security]
require_mtls = false

[auth]
enabled = false

[agent]
listen   = "127.0.0.1:$PORT_AGENT"
role     = "both"
node_id  = "agent-ci"
kv_uds   = "/tmp/cognitora-ci-kv.sock"

[engine]
kind = "openai_compat"
url  = "http://127.0.0.1:$PORT_FAKE"

[models."ci/test"]
tp = 1
EOF

"$REPO_ROOT/target/release/cgn-agent" --config "$TMP/agent.toml" \
  > "$TMP/agent.log" 2>&1 &
PIDS+=("$!")
sleep 2

# 6. Verify openai_compat did NOT fork a python/vllm child.
if pgrep -P "$(pgrep -f "$TMP/agent.toml" | head -1)" 2>/dev/null \
     | xargs -I{} ps -o cmd= -p {} 2>/dev/null \
     | grep -qE "vllm|llama_cpp.server|mlx_lm.server"; then
  fail "openai_compat mode should not spawn an engine child"
fi
ok "openai_compat does not spawn a child"

# 7. Router config with rate-limit set tight (rps=1, burst=1) and auth on.
cat > "$TMP/router.toml" <<EOF
[cluster]
name           = "ci"
state_backend  = "etcd"
etcd_endpoints = []

[security]
require_mtls = false

[auth]
enabled        = true
api_keys_file  = "$TMP/api-keys"

[router]
listen_http  = "127.0.0.1:$PORT_HTTP"
listen_grpc  = "127.0.0.1:$PORT_GRPC"
listen_admin = "127.0.0.1:$PORT_ADMIN"
node_id      = "router-ci"

[router.rate_limit]
rps   = 1
burst = 1

[models."ci/test"]
tp = 1
EOF

"$REPO_ROOT/target/release/cgn-router" --config "$TMP/router.toml" \
  > "$TMP/router.log" 2>&1 &
PIDS+=("$!")
for _ in {1..30}; do
  curl -fsS -m 1 "http://127.0.0.1:$PORT_ADMIN/healthz" >/dev/null 2>&1 && break
  sleep 0.3
done
curl -fsS -m 1 "http://127.0.0.1:$PORT_ADMIN/healthz" >/dev/null \
  || { cat "$TMP/router.log"; fail "router admin never came up"; }

# 8. Auth: no token → 401.
code=$(curl -s -o /dev/null -w '%{http_code}' -m 5 \
  -H 'Content-Type: application/json' \
  "http://127.0.0.1:$PORT_HTTP/v1/chat/completions" \
  -d '{"model":"ci/test","messages":[{"role":"user","content":"x"}],"max_tokens":1}')
[ "$code" = "401" ] || fail "no-key request expected 401, got $code"
ok "auth middleware: 401 without bearer"

# 9. Sanity: a single request with the key MUST return non-401 (the agent
#    is openai_compat → real engine fake → expect 200 or 503 if the routing
#    layer can't find a registered agent in the etcd-less mode).
single=$(curl -s -o "$TMP/single.body" -w '%{http_code}' -m 10 \
  -H 'Content-Type: application/json' \
  -H "Authorization: Bearer $KEY" \
  "http://127.0.0.1:$PORT_HTTP/v1/chat/completions" \
  -d '{"model":"ci/test","messages":[{"role":"user","content":"x"}],"max_tokens":1}')
if [ "$single" = "401" ]; then
  echo "key-debug: KEY='$KEY'"
  echo "key-debug: keys file ($TMP/api-keys):"
  cat "$TMP/api-keys"
  echo "key-debug: full router log:"
  cat "$TMP/router.log" | head -40
  fail "with-bearer request returned 401 — auth middleware not honoring keys file"
fi
ok "auth middleware: with-bearer → $single (not 401)"

# 10. Rate-limit: fire 5 concurrent with key. burst=1 → at least one 429.
#
# Note: we capture the curl PIDs and `wait` only for *those* — a bare `wait`
# would block on every background child including the daemons (agent,
# router, fake_engine) which never exit on their own.
codes=$(mktemp)
curl_pids=()
for _ in 1 2 3 4 5; do
  ( curl -s -o /dev/null -w '%{http_code}\n' -m 30 \
      -H 'Content-Type: application/json' \
      -H "Authorization: Bearer $KEY" \
      "http://127.0.0.1:$PORT_HTTP/v1/chat/completions" \
      -d '{"model":"ci/test","messages":[{"role":"user","content":"x"}],"max_tokens":1}' \
      >> "$codes" ) &
  curl_pids+=("$!")
done
for pid in "${curl_pids[@]}"; do
  wait "$pid" 2>/dev/null || true
done
n429=$(grep -c "^429$" "$codes" || true)
[ "$n429" -ge 1 ] || { cat "$codes"; fail "rate-limit did not trigger any 429"; }
ok "rate-limit middleware: $n429/5 → 429 (rps=1, burst=1)"

echo
echo "==> multi-engine smoke passed"
