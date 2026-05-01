#!/usr/bin/env bash
# scripts/run/up.sh
#
# Bring up a Cognitora stack from a profile directory. A profile is a folder
# of TOML files; each file becomes one daemon:
#
#   <profile>/router.toml         → cgn-router    (1)
#   <profile>/kvcached.toml       → cgn-kvcached  (0/1)
#   <profile>/agent-*.toml        → cgn-agent     (n)
#
# This script also boots a local etcd (via scripts/install/install-etcd.sh)
# so single-node profiles work out of the box. Set ETCD_ENDPOINT=... to
# point at an externally managed etcd instead.
#
# Usage:
#   bash scripts/run/up.sh examples/multi-llm
#   ETCD_ENDPOINT=10.0.0.5:2379 bash scripts/run/up.sh /etc/cognitora/profile

set -euo pipefail

PROFILE=${1:-}
[ -n "$PROFILE" ] || { echo "usage: $0 <profile-dir>" >&2; exit 64; }
[ -d "$PROFILE" ] || { echo "no such profile: $PROFILE" >&2; exit 1; }
PROFILE=$(cd "$PROFILE" && pwd)

# shellcheck disable=SC1091
. "$(dirname "$0")/lib.sh"

CGN_ROUTER=${CGN_ROUTER:-$ROOT/target/release/cgn-router}
CGN_AGENT=${CGN_AGENT:-$ROOT/target/release/cgn-agent}
CGN_KVCACHED=${CGN_KVCACHED:-$ROOT/target/release/cgn-kvcached}

for b in "$CGN_ROUTER" "$CGN_AGENT" "$CGN_KVCACHED"; do
  [ -x "$b" ] || fail "missing binary: $b — run \`cargo build --release -p cgn-router -p cgn-agent -p cgn-kvcached --no-default-features\`"
done

# 1. etcd
if [ -z "${ETCD_ENDPOINT:-}" ] || [ "${ETCD_ENDPOINT:-127.0.0.1:2379}" = "127.0.0.1:2379" ]; then
  ETCD_ENDPOINT=127.0.0.1:2379
  if ! curl -fsS -m 1 "http://$ETCD_ENDPOINT/health" >/dev/null 2>&1; then
    [ -x "$ETCD_DIR/etcd" ] \
      || fail "etcd not installed; run scripts/install/install-etcd.sh"
    log "starting embedded etcd (data: $WORK/etcd)"
    rm -rf "$WORK/etcd"
    spawn "$WORK/etcd.pid" "$WORK/etcd.log" \
      "$ETCD_DIR/etcd" --data-dir "$WORK/etcd" \
      --listen-client-urls "http://$ETCD_ENDPOINT" \
      --advertise-client-urls "http://$ETCD_ENDPOINT" \
      --listen-peer-urls http://127.0.0.1:2380 \
      --initial-advertise-peer-urls http://127.0.0.1:2380 \
      --initial-cluster default=http://127.0.0.1:2380 \
      --logger zap --log-level error
    for _ in {1..30}; do
      etcdctl endpoint health >/dev/null 2>&1 && break
      sleep 0.3
    done
  fi
fi

# 2. cgn-kvcached (optional; only if kvcached.toml exists)
if [ -f "$PROFILE/kvcached.toml" ]; then
  log "starting cgn-kvcached"
  spawn "$WORK/kvcached.pid" "$WORK/kvcached.log" \
    "$CGN_KVCACHED" --config "$PROFILE/kvcached.toml"
  sleep 1
fi

# 3. cgn-agent (one per agent-*.toml). Activate the venv so spawned engines
#    can find python/llama_cpp/vllm.
if [ -d "$VENV" ]; then
  # shellcheck disable=SC1091
  . "$VENV/bin/activate"
fi
shopt -s nullglob
for cfg in "$PROFILE"/agent-*.toml; do
  name=$(basename "$cfg" .toml)
  log "starting $name"
  spawn "$WORK/$name.pid" "$WORK/$name.log" \
    "$CGN_AGENT" --config "$cfg"
done
shopt -u nullglob

# 4. cgn-router
[ -f "$PROFILE/router.toml" ] || fail "$PROFILE/router.toml is required"
sleep 2
log "starting cgn-router"
spawn "$WORK/router.pid" "$WORK/router.log" \
  "$CGN_ROUTER" --config "$PROFILE/router.toml"

# Best-effort wait for the admin endpoint
admin_url=$(awk '/listen_admin/ { gsub(/["= ]/, "", $0); split($0, a, "listen_admin"); print "http://" a[2] }' "$PROFILE/router.toml" \
  | head -1)
admin_url=${admin_url:-http://127.0.0.1:9091}
if wait_for_url "$admin_url/healthz" 60; then
  pass "router admin healthy at $admin_url"
else
  warn "router admin not responding yet — see $WORK/router.log"
fi

bash "$(dirname "$0")/status.sh" "$PROFILE"
