#!/usr/bin/env bash
# Tear down the bench stack started by up-bench.sh.
# Does NOT touch external engines (Ollama / llama-cpp-python).
set -uo pipefail

for proc in cgn-router cgn-agent cgn-kvcached etcd ; do
  pkill -f "^$proc"      || true
  pkill -f "/$proc"      || true
done
echo "stack down"
