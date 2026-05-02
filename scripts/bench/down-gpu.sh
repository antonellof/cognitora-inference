#!/usr/bin/env bash
# Tear down EVERYTHING — engines and the cognitora stack.
set -uo pipefail
for proc in cgn-router cgn-agent cgn-kvcached etcd ; do
  pkill -f "^$proc" || true
  pkill -f "/$proc" || true
done
pkill -f "vllm.entrypoints" || true
pkill -f "llama_cpp.server" || true
pkill -f "ollama serve"     || true
echo "stack down"
