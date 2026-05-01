#!/usr/bin/env bash
# scripts/run/down.sh
#
# Stop everything launched by scripts/run/up.sh. Idempotent.

set -euo pipefail

# shellcheck disable=SC1091
. "$(dirname "$0")/lib.sh"

# Stop daemons in reverse-dependency order: router → agents → kvcached → etcd.
for f in "$WORK"/router.pid "$WORK"/agent-*.pid "$WORK"/kvcached.pid "$WORK"/etcd.pid; do
  if [ -f "$f" ]; then
    name=$(basename "$f" .pid)
    log "stopping $name"
    stop_pid "$f"
  fi
done

# Some engine subprocesses (llama-cpp-python, vllm) escape the agent's
# process group. Best-effort cleanup of the python ones.
for p in $(pgrep -f "llama_cpp.server" 2>/dev/null) \
         $(pgrep -f "vllm serve"      2>/dev/null) \
         $(pgrep -f "vllm.entrypoints" 2>/dev/null); do
  kill -9 "$p" 2>/dev/null || true
done

pass "stack down"
