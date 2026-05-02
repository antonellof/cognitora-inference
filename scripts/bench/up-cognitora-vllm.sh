#!/usr/bin/env bash
# Bring up the cognitora stack on top of running vLLM engines.
set -euo pipefail

CFG_DIR="$(cd "$(dirname "$0")/configs-gpu" && pwd)"
LOG_DIR="${LOG_DIR:-/workspace/logs}"
DATA_DIR="${DATA_DIR:-/workspace/cognitora-data}"
mkdir -p "$LOG_DIR" "$DATA_DIR/etcd"

# 1) etcd
if ! pgrep -f "^etcd " >/dev/null && ! pgrep -f "/etcd " >/dev/null ; then
  nohup etcd \
    --name bench --data-dir "$DATA_DIR/etcd/bench" \
    --listen-client-urls http://127.0.0.1:2379 \
    --advertise-client-urls http://127.0.0.1:2379 \
    --listen-peer-urls http://127.0.0.1:2380 \
    >"$LOG_DIR/etcd.log" 2>&1 &
  disown
  sleep 2
fi
echo "[1] etcd"

# 2) cgn-kvcached
nohup cgn-kvcached --config "$CFG_DIR/kvcached.toml" \
  >"$LOG_DIR/kvcached.log" 2>&1 &
disown
echo "[2] cgn-kvcached pid=$!"

# 3-N) cgn-agent processes
for cfg in agent-vllm-small agent-vllm-small-b agent-vllm-mid agent-vllm-small-nocache ; do
  nohup cgn-agent --config "$CFG_DIR/$cfg.toml" \
    >"$LOG_DIR/$cfg.log" 2>&1 &
  disown
  echo "[+] $cfg pid=$!"
done

sleep 4

# router
nohup cgn-router --config "$CFG_DIR/router.toml" \
  >"$LOG_DIR/router.log" 2>&1 &
disown
echo "[router] pid=$!"

sleep 3
echo
echo "=== ports ==="
ss -ltn | grep -E ":(2379|7184|7185|7186|7187|8080|9091|810[1-4])\b" || true
echo
echo "=== /v1/models on router ==="
curl -fsS http://127.0.0.1:8080/v1/models | python3 -m json.tool | head -50 || echo "router not ready yet"
