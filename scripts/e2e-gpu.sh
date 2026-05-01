#!/usr/bin/env bash
# scripts/e2e-gpu.sh
#
# End-to-end smoke on a self-hosted runner with a real GPU. Triggered
# by .github/workflows/e2e.yml when a PR carries the `gpu` label.

set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "${CGN_E2E_GPU:-0}" != "1" ]]; then
  cat <<EOF >&2
==> scripts/e2e-gpu.sh is gated.

The GPU end-to-end pipeline runs only on a self-hosted runner with a
visible CUDA device. Set CGN_E2E_GPU=1 (and provide CUDA_VISIBLE_DEVICES,
HF_TOKEN) to enable this gate. The pipeline:

  1. Builds cgn-{router,agent,kvcached,metrics} with --release.
  2. Boots vLLM v0.6+ via docker compose against the GPU.
  3. Issues a dev API key and drives 1k requests through the OpenAI
     surface.
  4. Asserts TTFT p99 < 1s and cgn:router:cache_hit_ratio >= 0.55.

Skipping (CGN_E2E_GPU != 1).
EOF
  exit 0
fi

echo "==> Building release binaries"
cargo build --release \
  -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-metrics \
  --no-default-features

echo "==> Issuing dev PKI + API key"
target/release/cgn-ctl pki bootstrap --out /tmp/cgn-gpu-pki >/dev/null
API_KEY=$(target/release/cgn-ctl key create --scope chat,embed --quiet)
export API_KEY

echo "==> Booting vLLM + Cognitora stack via docker compose"
docker compose -f deploy/docker/compose-gpu.yaml up -d

echo "==> Waiting for /healthz"
for _ in {1..60}; do
  if curl -fsS http://localhost:8080/healthz >/dev/null 2>&1; then break; fi
  sleep 1
done

echo "==> Driving 1k requests via tests/fixtures/sharegpt-1k.jsonl"
target/release/cgn-ctl bench \
  --base-url http://localhost:8080 \
  --api-key "$API_KEY" \
  --fixture tests/fixtures/sharegpt-1k.jsonl \
  --concurrency 16 \
  --json-out /tmp/cgn-gpu-bench.json

echo "==> Asserting SLOs"
TTFT_P99=$(jq '.ttft_p99_ms' /tmp/cgn-gpu-bench.json)
HIT_RATIO=$(jq '.cache_hit_ratio' /tmp/cgn-gpu-bench.json)
if (( $(echo "$TTFT_P99 > 1000" | bc -l) )); then
  echo "FAIL: TTFT p99 ${TTFT_P99}ms > 1000ms" >&2
  exit 1
fi
if (( $(echo "$HIT_RATIO < 0.55" | bc -l) )); then
  echo "FAIL: cache hit ratio ${HIT_RATIO} < 0.55" >&2
  exit 1
fi
echo "PASS: TTFT p99 ${TTFT_P99}ms, hit ratio ${HIT_RATIO}"

echo "==> Tearing down"
docker compose -f deploy/docker/compose-gpu.yaml down -v
