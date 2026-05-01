#!/usr/bin/env bash
# scripts/run/lib.sh
#
# Shared helpers for up.sh / down.sh / status.sh.

# shellcheck disable=SC2034
ROOT=${ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}
WORK=${WORK:-$HOME/.cache/cognitora/run}
ETCD_DIR=${ETCD_DIR:-$HOME/.local/cognitora/etcd}
VENV=${VENV:-$HOME/venv}

mkdir -p "$WORK"

color() { local c=$1; shift; printf '\033[%sm%s\033[0m\n' "$c" "$*"; }
log()   { color '1;36' "==> $*"; }
warn()  { color '1;33' "warn $*"; }
fail()  { color '1;31' "FAIL $*"; exit 1; }
pass()  { color '1;32' "ok   $*"; }

# Spawn a daemon as a detached background process and record its pid.
#
# spawn <pid-file> <log-file> <argv...>
spawn() {
  local pidfile=$1 logfile=$2; shift 2
  if [ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2>/dev/null; then
    warn "$(basename "$pidfile" .pid) already running (pid=$(cat "$pidfile"))"
    return 0
  fi
  rm -f "$pidfile"
  nohup "$@" > "$logfile" 2>&1 < /dev/null &
  disown
  echo $! > "$pidfile"
}

# Send SIGTERM, wait up to 5s, then SIGKILL.
stop_pid() {
  local pidfile=$1
  if [ ! -f "$pidfile" ]; then return 0; fi
  local p; p=$(cat "$pidfile")
  if kill -0 "$p" 2>/dev/null; then
    kill -TERM "$p" 2>/dev/null || true
    for _ in $(seq 1 10); do
      kill -0 "$p" 2>/dev/null || break
      sleep 0.5
    done
    kill -9 "$p" 2>/dev/null || true
  fi
  rm -f "$pidfile"
}

# Wait until a TCP port is *not* listening (port released). Works on both
# Linux (ss) and macOS (lsof).
wait_port_free() {
  local port=$1 to=${2:-30}
  for _ in $(seq 1 "$to"); do
    if command -v ss >/dev/null 2>&1; then
      ss -tln 2>/dev/null | grep -q ":${port} " || return 0
    elif command -v lsof >/dev/null 2>&1; then
      lsof -ti tcp:"$port" >/dev/null 2>&1 || return 0
    else
      # No tooling available — give the kernel half a second and trust it.
      sleep 0.5
      return 0
    fi
    sleep 0.3
  done
  return 1
}

# Wait until a URL returns 200.
wait_for_url() {
  local url=$1 to=${2:-60}
  for _ in $(seq 1 "$to"); do
    curl -fsS -m 1 "$url" >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  return 1
}

# Path to the etcdctl binary inside ETCD_DIR.
etcdctl() {
  "$ETCD_DIR/etcdctl" --endpoints="${ETCD_ENDPOINT:-127.0.0.1:2379}" "$@"
}
