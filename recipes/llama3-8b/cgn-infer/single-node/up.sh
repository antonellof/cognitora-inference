#!/usr/bin/env bash
# Bring up Cognitora with Llama-3.1-8B on cgn-infer (native engine).
# Set LLAMA_GGUF=/abs/path/to/model.gguf first.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if [ -z "${LLAMA_GGUF:-}" ]; then
  echo "warn: LLAMA_GGUF is not set — the agent will fail to spawn." >&2
  echo "      Download a Llama-3 GGUF and re-run, e.g.:" >&2
  echo "        export LLAMA_GGUF=~/models/Meta-Llama-3.1-8B-Instruct.Q4_K_M.gguf" >&2
fi

if ! command -v cgn-infer >/dev/null 2>&1; then
  echo "warn: cgn-infer not found on PATH — build it first:" >&2
  echo "        cargo build --release -p cgn-infer" >&2
  echo "      or point [engine.cgn_infer].binary_path at the binary." >&2
fi

# Render a tmpfile with the GGUF path expanded, since TOML cannot do
# environment-variable substitution natively.
TMPDIR_LOCAL=$(mktemp -d)
trap 'rm -rf "$TMPDIR_LOCAL"' EXIT
cp "$HERE/router.toml" "$TMPDIR_LOCAL/router.toml"
sed "s|\${LLAMA_GGUF}|${LLAMA_GGUF:-/MISSING_LLAMA_GGUF}|" \
  "$HERE/agent.toml" > "$TMPDIR_LOCAL/agent.toml"

. "$HERE/../../../_lib/recipe.sh"
recipe_up "$TMPDIR_LOCAL"
