#!/usr/bin/env bash
# scripts/run/status.sh
#
# Print the runtime status of the daemons launched by scripts/run/up.sh.
# When invoked with a profile dir, also prints the registered models from
# etcd (for routing visibility).

set -uo pipefail

# shellcheck disable=SC1091
. "$(dirname "$0")/lib.sh"

PROFILE=${1:-}

printf '%-16s  %-7s  %-7s  %-50s\n' DAEMON STATE PID LOG
printf -- '-%.0s' $(seq 1 90); echo
for f in "$WORK"/etcd.pid "$WORK"/kvcached.pid "$WORK"/agent-*.pid "$WORK"/router.pid; do
  [ -f "$f" ] || continue
  name=$(basename "$f" .pid)
  pid=$(cat "$f")
  if kill -0 "$pid" 2>/dev/null; then
    state=running
  else
    state=down
  fi
  printf '%-16s  %-7s  %-7s  %s\n' "$name" "$state" "$pid" "$WORK/$name.log"
done

# Etcd model registrations
if curl -fsS -m 1 "http://${ETCD_ENDPOINT:-127.0.0.1:2379}/health" >/dev/null 2>&1; then
  printf '\nregistered nodes (etcd /cognitora/nodes/):\n'
  etcdctl get --prefix /cognitora/nodes/ --print-value-only 2>/dev/null \
    | python3 -c '
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line: continue
    try:
        d = json.loads(line)
        print(f"  {d.get(\"node_id\",\"?\"):<14}  {d.get(\"model\",\"?\"):<40}  ready={d.get(\"ready\",False)}  {d.get(\"address\",\"\")}")
    except Exception:
        print("  " + line[:120])
'
fi

[ -n "$PROFILE" ] && [ -f "$PROFILE/router.toml" ] && {
  printf '\nprofile: %s\n' "$PROFILE"
}
