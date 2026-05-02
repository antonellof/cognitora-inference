#!/usr/bin/env bash
# recipes/_lib/recipe.sh
#
# Shared "bring up a Cognitora cluster from this recipe" driver. Every
# recipe ships an `up.sh` that just sources this file and calls
# `recipe_up`. The recipe directory is expected to look like:
#
#     recipes/<model>/<engine>/<topology>/
#       README.md
#       router.toml
#       agent-*.toml
#       kvcached.toml          # optional
#
# Behaviour:
#
# 1. Verify (or build) the six Cognitora binaries.
# 2. Start a local etcd if no `ETCD_ENDPOINT` is reachable.
# 3. Start kvcached (if `kvcached.toml` is present).
# 4. Start one cgn-agent per `agent-*.toml`.
# 5. Start cgn-router (`router.toml`).
# 6. Optionally hit `/v1/models` and emit a usage hint.
#
# All of these steps are taken from `scripts/run/up.sh` so behaviour is
# identical to the existing profile-based bring-up. The point of this
# file is to give every recipe a one-line entry point:
#
#     bash recipes/llama3-8b/vllm/agg/up.sh
#
# Environment overrides:
#
#   ETCD_ENDPOINT  External etcd (default: 127.0.0.1:2379, embedded if absent)
#   CGN_PREBUILT   1 = skip the cargo build preflight (binaries already on PATH)
#   CGN_TARGET     Override the workspace `target/release` directory
#   CGN_SKIP_PROBE 1 = don't probe `/v1/models` after bring-up
#
# This script never breaks: if the cluster fails to come up, individual
# log files under `$HOME/.cache/cognitora/run/` carry the details.

set -euo pipefail

# ---- Resolve workspace root (this file lives at recipes/_lib/recipe.sh) ---
RECIPE_LIB_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$RECIPE_LIB_DIR/../.." && pwd)
export ROOT

# Hand off to the existing profile-based runner where possible. Anything
# below adds the recipe-specific bits on top.
# shellcheck disable=SC1091
. "$ROOT/scripts/run/lib.sh"

# ---- Public entry point ---------------------------------------------------

# recipe_up <recipe-dir>
#
# `<recipe-dir>` is the directory containing the recipe's TOML profile.
# For an in-tree recipe that's the dir holding this `up.sh`; for a
# user-managed recipe it can be any folder with the same shape.
recipe_up() {
  local profile=${1:-}
  [ -n "$profile" ] || fail "recipe_up: missing profile dir"
  [ -d "$profile" ] || fail "recipe_up: no such dir: $profile"
  profile=$(cd "$profile" && pwd)

  log "recipe profile: $profile"

  _recipe_preflight
  _recipe_probe_engines "$profile"

  bash "$ROOT/scripts/run/up.sh" "$profile"

  if [ "${CGN_SKIP_PROBE:-0}" != "1" ]; then
    _recipe_probe_router "$profile"
  fi

  _recipe_print_usage "$profile"
}

# ---- Internals ------------------------------------------------------------

_recipe_preflight() {
  local missing=0
  local target=${CGN_TARGET:-$ROOT/target/release}
  for b in cgn-router cgn-agent cgn-kvcached; do
    if [ ! -x "$target/$b" ] && ! command -v "$b" >/dev/null 2>&1; then
      missing=1
      break
    fi
  done
  if [ "$missing" = "1" ] && [ "${CGN_PREBUILT:-0}" != "1" ]; then
    log "building cognitora binaries (release)"
    ( cd "$ROOT" && cargo build --release --no-default-features \
        -p cgn-router -p cgn-agent -p cgn-kvcached )
  fi
}

# Surface a clear error if the recipe expects an engine binary that isn't
# installed yet (vllm / python -m sglang.launch_server / llama_cpp).
_recipe_probe_engines() {
  local profile=$1
  shopt -s nullglob
  for cfg in "$profile"/agent-*.toml; do
    local kind
    kind=$(awk -F'[="\t ]+' '/^kind[[:space:]]*=/ { print $2; exit }' "$cfg" 2>/dev/null || true)
    case "$kind" in
      vllm)
        command -v vllm >/dev/null 2>&1 || warn "$(basename "$cfg") expects vllm in PATH (pip install vllm)"
        ;;
      sglang)
        python -c 'import sglang' >/dev/null 2>&1 \
          || warn "$(basename "$cfg") expects 'python -m sglang.launch_server' (pip install \"sglang[all]\")"
        ;;
      llama_cpp)
        python -c 'import llama_cpp' >/dev/null 2>&1 \
          || warn "$(basename "$cfg") expects llama_cpp.server (pip install llama-cpp-python[server])"
        ;;
      openai_compat|"") : ;;
      *) warn "$(basename "$cfg") declares unknown engine kind: $kind" ;;
    esac
  done
  shopt -u nullglob
}

_recipe_probe_router() {
  local profile=$1
  local addr
  addr=$(awk '/^[[:space:]]*listen_http[[:space:]]*=/ {
                gsub(/["= ]/, "", $0);
                split($0, a, "listen_http");
                print a[2];
                exit
              }' "$profile/router.toml" | head -1)
  addr=${addr:-127.0.0.1:8080}
  local url="http://$addr/v1/models"
  if wait_for_url "$url" 60; then
    pass "router /v1/models reachable at $url"
  else
    warn "router /v1/models did not respond within 60s — see $WORK/router.log"
  fi
}

_recipe_print_usage() {
  local profile=$1
  local model
  # Pick the first model declared in any agent-*.toml.
  model=$(awk -F'"' '/^\[models\."/ { print $2; exit }' "$profile"/agent-*.toml 2>/dev/null || true)
  model=${model:-<model>}

  cat <<EOF

────────────────────────────────────────────────────────────────────────
Cognitora cluster is up.

  Try it:
    curl -fsS http://127.0.0.1:8080/v1/models | jq

    curl -fsS http://127.0.0.1:8080/v1/chat/completions \\
      -H 'Content-Type: application/json' \\
      -d '{"model":"$model","messages":[{"role":"user","content":"hello"}]}'

  Tear it down:
    bash $ROOT/scripts/run/down.sh $profile

  Logs: $WORK/{router,agent-*,kvcached}.log
────────────────────────────────────────────────────────────────────────
EOF
}
