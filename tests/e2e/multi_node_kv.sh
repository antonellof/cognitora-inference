#!/usr/bin/env bash
# tests/e2e/multi_node_kv.sh
#
# Multi-node control-plane smoke for KV-aware routing. Seeds 4 fake
# agent records into a real etcd, boots cgn-router against it, replays
# the tests/fixtures/traces/openai_chatlog.jsonl fixture, and verifies:
#
#   1. The router discovers all 4 nodes from the etcd snapshot and
#      attempts dispatch to them (every replayed request errors with a
#      dispatch failure, NOT "no live node serving model").
#   2. cgn_router_chat_requests_total{status="5xx"} counts every
#      replayed request (routing decisions were made and metered).
#   3. Live etcd watch propagation: cordoning all 4 nodes via
#      /cognitora/cordon/<id> flips the router to "no live node",
#      and uncordoning flips it back.
#   4. Real data path + prefix affinity: two real cgn-agents in
#      openai_compat mode (each fronting its own stub_engine.py SSE
#      server) self-register in etcd, the router dispatches a chat
#      request end-to-end (HTTP 200 with stub content), and a second
#      identical request lands on the *same* node (KV prefix-affinity),
#      verified via the per-stub /hits counters.
#
# Gating
# ------
# Runs by default when etcd is reachable. The matrix:
#
#   CGN_E2E_MULTINODE=0            → always SKIPPED (explicit opt-out).
#   etcd unreachable, local run    → SKIPPED with a clear message.
#   etcd unreachable, CI=true or
#     CGN_E2E_MULTINODE=1          → hard FAIL (etcd was expected).
#   etcd reachable                 → runs; non-zero exit on any failure.
#
# Requirements: cargo (or prebuilt target/release/{cgn-router,cgn-agent}
# with CGN_SKIP_BUILD=1), curl, python3, etcdctl, etcd on
# $COGNITORA_ETCD (default 127.0.0.1:2379).

set -euo pipefail

ETCD_ENDPOINTS="${COGNITORA_ETCD:-127.0.0.1:2379}"

ok()   { printf '\033[1;32mok\033[0m   %s\n' "$*"; }
fail() { printf '\033[1;31mfail\033[0m %s\n' "$*"; exit 1; }

skip_or_fail() {
  local reason=$1
  if [[ "${CI:-false}" == "true" || "${CGN_E2E_MULTINODE:-}" == "1" ]]; then
    fail "$reason (etcd is required in CI / when CGN_E2E_MULTINODE=1)"
  fi
  cat >&2 <<EOF
==> tests/e2e/multi_node_kv.sh SKIPPED: $reason

This harness needs a reachable etcd. To run it locally:

  1. Start etcd:      etcd --data-dir /tmp/etcd-e2e &
  2. Re-run:          ./tests/e2e/multi_node_kv.sh

Endpoint probed: \$COGNITORA_ETCD (currently: $ETCD_ENDPOINTS).
Set CGN_E2E_MULTINODE=0 to opt out explicitly.
EOF
  exit 0
}

# ---- Gating ---------------------------------------------------------------

if [[ "${CGN_E2E_MULTINODE:-}" == "0" ]]; then
  echo "==> tests/e2e/multi_node_kv.sh SKIPPED (CGN_E2E_MULTINODE=0)" >&2
  exit 0
fi

ETCD_HOST=${ETCD_ENDPOINTS%%:*}
ETCD_PORT=${ETCD_ENDPOINTS##*:}
if ! (exec 3<>"/dev/tcp/$ETCD_HOST/$ETCD_PORT") 2>/dev/null; then
  skip_or_fail "etcd not reachable at $ETCD_ENDPOINTS"
fi
command -v etcdctl >/dev/null 2>&1 \
  || skip_or_fail "etcdctl not found on PATH"

# ---- Setup ----------------------------------------------------------------

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO_ROOT"

TRACE="$REPO_ROOT/tests/fixtures/traces/openai_chatlog.jsonl"
[[ -f "$TRACE" ]] || fail "missing fixture: $TRACE"

PORT_HTTP=28080
PORT_GRPC=29090
PORT_ADMIN=29091
PORT_STUB_A=28085
PORT_STUB_B=28086
PORT_AGENT_A=28087
PORT_AGENT_B=28088
MODEL="llama3-8b"
# Phase-4 model id, distinct from the fake-node model so the two phases
# cannot cross-route.
STUB_MODEL="stub-oai"

WORK=$(mktemp -d)
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do
    [[ -n "$p" ]] && kill -9 "$p" 2>/dev/null || true
  done
  etcdctl --endpoints="$ETCD_ENDPOINTS" del --prefix /cognitora/nodes/node- >/dev/null 2>&1 || true
  etcdctl --endpoints="$ETCD_ENDPOINTS" del --prefix /cognitora/nodes/agent- >/dev/null 2>&1 || true
  etcdctl --endpoints="$ETCD_ENDPOINTS" del --prefix /cognitora/cordon/node- >/dev/null 2>&1 || true
  rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

command -v python3 >/dev/null 2>&1 || fail "python3 not found on PATH"

for b in cgn-router cgn-agent; do
  if [[ ! -x "$REPO_ROOT/target/release/$b" ]]; then
    if [[ "${CGN_SKIP_BUILD:-0}" == "1" ]]; then
      fail "target/release/$b missing and CGN_SKIP_BUILD=1"
    fi
    echo "==> building cgn-router + cgn-agent (release, no-default-features)"
    cargo build --release --no-default-features -p cgn-router -p cgn-agent 2>&1 | tail -3
    break
  fi
done

echo "==> seeding 4 fake agent records into etcd ($ETCD_ENDPOINTS)"
for i in 0 1 2 3; do
  KEY="/cognitora/nodes/node-${i}"
  VAL=$(cat <<JSON
{"node_id":"node-${i}","address":"http://127.0.0.1:4707${i}","role":3,
 "queue_depth":0,"free_blocks":1024,"total_blocks":1024,"power_watts":120,
 "model":"${MODEL}","gpu_index":${i}}
JSON
)
  etcdctl --endpoints="$ETCD_ENDPOINTS" put "$KEY" "$VAL" >/dev/null
done
# Ensure no stale cordon flags survive from a previous interrupted run.
etcdctl --endpoints="$ETCD_ENDPOINTS" del --prefix /cognitora/cordon/node- >/dev/null

echo "==> booting cgn-router"
cat >"$WORK/cognitora.toml" <<EOF
[cluster]
name           = "e2e-multinode"
state_backend  = "etcd"
etcd_endpoints = ["http://${ETCD_ENDPOINTS}"]

[security]
require_mtls = false

[auth]
enabled = false

[router]
listen_http  = "127.0.0.1:${PORT_HTTP}"
listen_grpc  = "127.0.0.1:${PORT_GRPC}"
listen_admin = "127.0.0.1:${PORT_ADMIN}"
node_id      = "router-e2e-multinode"

[router.score_weights]
kv       = 0.55
load     = 0.25
power    = 0.10
capacity = 0.10

[models.${MODEL}]

[models.${STUB_MODEL}]
EOF

RUST_LOG="${RUST_LOG:-info}" \
  "$REPO_ROOT/target/release/cgn-router" --config "$WORK/cognitora.toml" \
  >"$WORK/router.log" 2>&1 &
PIDS+=("$!")

for _ in {1..30}; do
  curl -fsS -m 1 "http://127.0.0.1:${PORT_ADMIN}/healthz" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -fsS -m 1 "http://127.0.0.1:${PORT_ADMIN}/healthz" >/dev/null \
  || { cat "$WORK/router.log"; fail "router /healthz did not come up"; }
ok "router up (4 fake nodes seeded via etcd)"

# Helper: one chat request; prints "<code>" and writes the body to $1.
chat() {
  local out=$1 body=$2
  curl -s -o "$out" -w '%{http_code}' -m 10 \
    -H 'content-type: application/json' \
    -d "$body" \
    "http://127.0.0.1:${PORT_HTTP}/v1/chat/completions" || true
}

# ---- 1. Replay the trace: every request must route (then fail dispatch) ---

echo "==> replaying fixture trace"
N_LINES=0
while IFS= read -r line; do
  [[ -n "$line" ]] || continue
  N_LINES=$((N_LINES + 1))
  code=$(chat "$WORK/resp.json" "$line")
  [[ "$code" == 5* ]] \
    || { cat "$WORK/resp.json"; fail "request $N_LINES: expected 5xx (fake agents unreachable), got $code"; }
  if grep -q 'no live node serving model' "$WORK/resp.json"; then
    cat "$WORK/router.log" | tail -20
    fail "request $N_LINES: router found no candidates — etcd node discovery broken"
  fi
done <"$TRACE"
ok "replayed $N_LINES requests; all routed to etcd-discovered nodes (dispatch 5xx as expected)"

# ---- 2. Metrics: every replayed request was metered --------------------—--

METERED=$(curl -s "http://127.0.0.1:${PORT_ADMIN}/metrics" \
  | awk -v m="$MODEL" '$0 ~ "^cgn_router_chat_requests_total\\{model=\"" m "\",status=\"5xx\"\\}" { print $2 }')
[[ "${METERED:-0}" == "$N_LINES" ]] \
  || fail "cgn_router_chat_requests_total{model=\"$MODEL\",status=\"5xx\"} = ${METERED:-<absent>}, want $N_LINES"
ok "metrics: chat_requests_total 5xx == $N_LINES"

# ---- 3. Live etcd watch: cordon all nodes, expect 'no live node' ----------

PROBE='{"model":"llama3-8b","messages":[{"role":"user","content":"cordon probe"}],"max_tokens":4}'

echo "==> cordoning all 4 nodes via etcd"
for i in 0 1 2 3; do
  etcdctl --endpoints="$ETCD_ENDPOINTS" put "/cognitora/cordon/node-${i}" "1" >/dev/null
done
CORDONED=0
for _ in {1..20}; do
  chat "$WORK/cordon.json" "$PROBE" >/dev/null
  if grep -q 'no live node serving model' "$WORK/cordon.json"; then
    CORDONED=1
    break
  fi
  sleep 0.5
done
[[ "$CORDONED" == "1" ]] \
  || { tail -20 "$WORK/router.log"; fail "cordon flags did not propagate through the etcd watch"; }
ok "cordon propagated: router reports no live node"

echo "==> uncordoning"
etcdctl --endpoints="$ETCD_ENDPOINTS" del --prefix /cognitora/cordon/node- >/dev/null
UNCORDONED=0
for _ in {1..20}; do
  chat "$WORK/uncordon.json" "$PROBE" >/dev/null
  if ! grep -q 'no live node serving model' "$WORK/uncordon.json"; then
    UNCORDONED=1
    break
  fi
  sleep 0.5
done
[[ "$UNCORDONED" == "1" ]] \
  || { tail -20 "$WORK/router.log"; fail "uncordon did not propagate through the etcd watch"; }
ok "uncordon propagated: routing resumed"

# ---- 4. Real agents + stub engines: end-to-end 200 + prefix affinity ------
#
# Two real cgn-agents in openai_compat mode, each fronting its own
# stub_engine.py. They self-register in etcd (lease-bound heartbeat),
# the router discovers them via the watch, and requests for
# $STUB_MODEL flow router → agent (gRPC) → stub (SSE) → back. The
# per-stub /hits counters tell us exactly which node served each
# request, which is how we assert KV prefix-affinity.

echo "==> starting 2 stub engines (tests/e2e/stub_engine.py)"
python3 "$REPO_ROOT/tests/e2e/stub_engine.py" "$PORT_STUB_A" stub-a \
  >"$WORK/stub-a.log" 2>&1 &
PIDS+=("$!")
python3 "$REPO_ROOT/tests/e2e/stub_engine.py" "$PORT_STUB_B" stub-b \
  >"$WORK/stub-b.log" 2>&1 &
PIDS+=("$!")
for port in "$PORT_STUB_A" "$PORT_STUB_B"; do
  STUB_UP=0
  for _ in {1..20}; do
    if curl -fsS -m 1 "http://127.0.0.1:${port}/health" >/dev/null 2>&1; then
      STUB_UP=1
      break
    fi
    sleep 0.3
  done
  [[ "$STUB_UP" == "1" ]] || fail "stub engine on :${port} did not come up"
done
ok "stub engines up (:$PORT_STUB_A, :$PORT_STUB_B)"

echo "==> booting 2 cgn-agents (openai_compat, etcd-registered)"
for side in a b; do
  if [[ "$side" == "a" ]]; then
    AGENT_PORT=$PORT_AGENT_A STUB_PORT=$PORT_STUB_A
  else
    AGENT_PORT=$PORT_AGENT_B STUB_PORT=$PORT_STUB_B
  fi
  cat >"$WORK/agent-${side}.toml" <<EOF
[cluster]
name           = "e2e-multinode"
state_backend  = "etcd"
etcd_endpoints = ["http://${ETCD_ENDPOINTS}"]

[security]
require_mtls = false

[auth]
enabled = false

[agent]
listen  = "127.0.0.1:${AGENT_PORT}"
role    = "both"
node_id = "agent-${side}"
kv_uds  = "$WORK/kv-${side}.sock"

[engine]
kind = "openai_compat"
url  = "http://127.0.0.1:${STUB_PORT}"

[models.${STUB_MODEL}]
EOF
  RUST_LOG="${RUST_LOG:-info}" \
    "$REPO_ROOT/target/release/cgn-agent" --config "$WORK/agent-${side}.toml" \
    >"$WORK/agent-${side}.log" 2>&1 &
  PIDS+=("$!")
done

echo "==> waiting for both agents to self-register in etcd"
for side in a b; do
  REGISTERED=0
  for _ in {1..60}; do
    if etcdctl --endpoints="$ETCD_ENDPOINTS" get "/cognitora/nodes/agent-${side}" \
        --print-value-only 2>/dev/null | grep -q "\"model\":\"${STUB_MODEL}\""; then
      REGISTERED=1
      break
    fi
    sleep 0.5
  done
  [[ "$REGISTERED" == "1" ]] \
    || { tail -20 "$WORK/agent-${side}.log"; fail "agent-${side} did not register in etcd within 30s"; }
done
ok "both agents registered in etcd (model=$STUB_MODEL, lease-bound)"

stub_hits() { # $1 = stub port → prints its POST count
  local h
  h=$(curl -fsS -m 2 "http://127.0.0.1:$1/hits" 2>/dev/null \
    | sed -n 's/.*"chat": *\([0-9][0-9]*\).*/\1/p')
  echo "${h:-0}"
}

# Warm up with a throwaway prompt until the full path returns 200: the
# router needs a beat after the watch event, and an agent may register
# a moment before its gRPC listener accepts. Startup retries during
# this loop may scatter hits across both stubs — that's fine, the
# affinity check below uses hit *deltas* with a fresh prompt.
WARMUP_BODY=$(printf '{"model":"%s","messages":[{"role":"user","content":"warmup probe"}],"max_tokens":4}' \
  "$STUB_MODEL")
echo "==> routing a real request through router → agent → stub"
ROUTED=0
code=""
for _ in {1..40}; do
  code=$(chat "$WORK/e2e.json" "$WARMUP_BODY")
  if [[ "$code" == "200" ]]; then
    ROUTED=1
    break
  fi
  sleep 0.5
done
[[ "$ROUTED" == "1" ]] \
  || { tail -20 "$WORK/router.log"; tail -20 "$WORK/agent-a.log"; \
       fail "request for $STUB_MODEL never returned 200 (last: ${code:-<none>})"; }
grep -q 'Hello from stub-' "$WORK/e2e.json" \
  || { cat "$WORK/e2e.json"; fail "response body missing stub engine content"; }
ok "end-to-end 200: router → cgn-agent (gRPC) → stub engine (SSE)"

# Fresh multi-chunk prompt (>32 whitespace tokens = >1 prefix chunk) so
# the router's sequence-chained prefix hashing has something to match.
AFFINITY_PROMPT="Summarize the design of a distributed inference control \
plane that separates prefill from decode, explains how key value cache \
blocks are handed between nodes over a fast transport, why time to first \
token improves when the prefill stage is scheduled on a dedicated node, \
and what role an etcd based registry plays in discovering healthy agents."
AFFINITY_BODY=$(printf '{"model":"%s","messages":[{"role":"user","content":"%s"}],"max_tokens":16}' \
  "$STUB_MODEL" "$AFFINITY_PROMPT")

echo "==> prefix affinity: first dispatch of the affinity prompt"
A0=$(stub_hits "$PORT_STUB_A"); B0=$(stub_hits "$PORT_STUB_B")
code=$(chat "$WORK/aff1.json" "$AFFINITY_BODY")
[[ "$code" == "200" ]] \
  || { cat "$WORK/aff1.json"; fail "affinity request 1 expected 200, got $code"; }
A1=$(stub_hits "$PORT_STUB_A"); B1=$(stub_hits "$PORT_STUB_B")
if [[ $((A1 - A0)) -eq 1 && $((B1 - B0)) -eq 0 ]]; then
  FIRST_PORT=$PORT_STUB_A; FIRST_NAME="agent-a"
elif [[ $((B1 - B0)) -eq 1 && $((A1 - A0)) -eq 0 ]]; then
  FIRST_PORT=$PORT_STUB_B; FIRST_NAME="agent-b"
else
  fail "affinity request 1 hit-count deltas ambiguous (stub-a +$((A1 - A0)), stub-b +$((B1 - B0)))"
fi

echo "==> prefix affinity: replaying the identical prompt (expect $FIRST_NAME again)"
code=$(chat "$WORK/aff2.json" "$AFFINITY_BODY")
[[ "$code" == "200" ]] \
  || { cat "$WORK/aff2.json"; fail "affinity request 2 expected 200, got $code"; }
A2=$(stub_hits "$PORT_STUB_A"); B2=$(stub_hits "$PORT_STUB_B")
if [[ "$FIRST_PORT" == "$PORT_STUB_A" ]]; then
  SAME=$((A2 - A1)); OTHER=$((B2 - B1))
else
  SAME=$((B2 - B1)); OTHER=$((A2 - A1))
fi
[[ "$SAME" -eq 1 && "$OTHER" -eq 0 ]] \
  || { tail -20 "$WORK/router.log"; \
       fail "prefix affinity broken: same-node delta=$SAME, other-node delta=$OTHER (want 1/0 on $FIRST_NAME)"; }
ok "prefix affinity: identical prompt re-routed to $FIRST_NAME (peer untouched)"

echo ""
echo "==> multi-node KV routing smoke passed ($N_LINES trace requests, 4 fake nodes, cordon round-trip, 2 real agents, prefix affinity)"
