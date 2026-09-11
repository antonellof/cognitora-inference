#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
VENV="${VENV:-/tmp/cgn-proto-venv}"
python3 -m venv "$VENV"
"$VENV/bin/pip" install -q grpcio-tools
"$VENV/bin/python" -m grpc_tools.protoc \
  -I "$ROOT/rust/libraries/cgn-proto/proto" \
  --python_out="$HERE/cognitora" \
  --grpc_python_out="$HERE/cognitora" \
  cognitora/v1/common.proto \
  cognitora/v1/kv.proto
touch "$HERE/cognitora/__init__.py" "$HERE/cognitora/v1/__init__.py"
