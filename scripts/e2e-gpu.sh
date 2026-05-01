#!/usr/bin/env bash
# scripts/e2e-gpu.sh
#
# End-to-end smoke on a self-hosted runner with a real GPU. Triggered
# by .github/workflows/e2e.yml when a PR carries the `gpu` label.
#
# Status: SKELETON. The full pipeline (vLLM container + agent +
# kvcached + router with mTLS on) is tracked under M3 in plan.md. The
# harness is wired up so we don't forget it; it exits 64 ("feature
# not ready") until M3 lands.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "${CGN_M3_READY:-0}" != "1" ]]; then
  cat <<EOF >&2
==> scripts/e2e-gpu.sh is a skeleton.

The GPU end-to-end harness lands in M3 (see plan.md). It will:

  1. Build cgn-{router,agent,kvcached,metrics} with --release.
  2. Run docker compose with vLLM v0.6+ on the GPU.
  3. Issue a dev API key and drive 1k requests through the OpenAI
     surface.
  4. Assert TTFT p99 < 1s and cgn_router_cache_hit_ratio >= 0.55.

Set CGN_M3_READY=1 to enable this gate.
EOF
  exit 64
fi

# (populated when M3 lands)
echo "TODO: GPU end-to-end run"
exit 1
