#!/usr/bin/env bash
# tests/e2e/multi_node_kv.sh
#
# 4-node cluster smoke for KV-aware routing. Brings up 1 router + 4
# fake agents on the loopback interface, replays the
# tests/fixtures/traces/openai_chatlog.jsonl fixture, and verifies the
# cache hit ratio meets the M2 SLO (>= 0.55).
#
# Status: SKELETON. The fake-agent driver and fixture trace are
# tracked under M2 in plan.md; this script wires up the harness and
# fails fast with a clear marker so CI doesn't accidentally pass.

set -euo pipefail

if [[ "${CGN_M2_READY:-0}" != "1" ]]; then
  cat <<EOF >&2
==> tests/e2e/multi_node_kv.sh is a skeleton.

The fake-agent driver and replay fixture land in M2 (see plan.md).
Set CGN_M2_READY=1 once those land to enable this gate.
EOF
  exit 64    # custom code: feature not ready
fi

# (intentionally empty — populated by M2 work)
echo "TODO: spin up router + 4 fake agents, replay fixture"
exit 1
