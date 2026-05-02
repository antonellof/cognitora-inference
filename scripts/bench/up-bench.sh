#!/usr/bin/env bash
# Brings up the bench-profile stack:
#   etcd + cgn-kvcached + 2 cgn-agent + cgn-router
# Assumes Cognitora binaries are on PATH (or installed under ~/.cognitora/bin)
# and that engines (Ollama on :11434 and llama-cpp-python on :8001) are
# already running.
set -euo pipefail

CFG_DIR="$(cd "$(dirname "$0")/configs" && pwd)"
LOG_DIR="${LOG_DIR:-$HOME/cognitora-data/logs}"
DATA_DIR="${DATA_DIR:-$HOME/cognitora-data}"
mkdir -p "$LOG_DIR" "$DATA_DIR/etcd"

export PATH="$HOME/.cognitora/bin:$PATH"

if ! pgrep -f "^etcd" >/dev/null ; then
  command -v etcd >/dev/null || { echo "etcd not installed" >&2 ; exit 1 ; }
  nohup etcd \
    --name bench --data-dir "$DATA_DIR/etcd/bench" \
    --listen-client-urls http://127.0.0.1:2379 \
    --advertise-client-urls http://127.0.0.1:2379 \
    --listen-peer-urls http://127.0.0.1:2380 \
    >"$LOG_DIR/etcd.log" 2>&1 &
  disown
  sleep 2
fi
echo "[1/5] etcd ready"

nohup cgn-kvcached --config "$CFG_DIR/kvcached.toml" \
  >"$LOG_DIR/kvcached.log" 2>&1 &
disown
echo "[2/5] cgn-kvcached pid=$!"

nohup cgn-agent --config "$CFG_DIR/agent-ollama.toml" \
  >"$LOG_DIR/agent-ollama.log" 2>&1 &
disown
echo "[3/5] cgn-agent (ollama) pid=$!"

nohup cgn-agent --config "$CFG_DIR/agent-llamacpp.toml" \
  >"$LOG_DIR/agent-llamacpp.log" 2>&1 &
disown
echo "[4/5] cgn-agent (llamacpp) pid=$!"

sleep 3

nohup cgn-router --config "$CFG_DIR/router.toml" \
  >"$LOG_DIR/router.log" 2>&1 &
disown
echo "[5/5] cgn-router pid=$!"

sleep 2
echo
echo "=== listening ports ==="
ss -ltn | grep -E ":(2379|7080|7081|8080|9091|11434|8001)\b" || true
