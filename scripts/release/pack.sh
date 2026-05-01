#!/usr/bin/env bash
# scripts/release/pack.sh
#
# Build the Cognitora release tarball *for the host* — same layout as
# .github/workflows/release.yml so you can dry-run install.sh without
# tagging or pushing.
#
# Usage:
#   bash scripts/release/pack.sh [TAG]
#
# Default TAG is "v0.0.0-dev". Output lands in ./dist/.
#
# Then validate the install flow end-to-end:
#
#   bash scripts/release/pack.sh v0.0.0-dev
#   ( cd dist && python3 -m http.server 8765 ) &
#   CGN_BASE_URL=http://127.0.0.1:8765 \
#     CGN_VERSION=v0.0.0-dev \
#     CGN_PREFIX=/tmp/cgn-test \
#     sh deploy/installer/install.sh

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
TAG="${1:-v0.0.0-dev}"
DIST="${ROOT}/dist"

# ---- detect target ---------------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"
# `TARGET` is the Rust target triple (passed to `cargo --target`).
# `PLATFORM` is the user-friendly archive suffix that ships in the
# release filename and that install.sh resolves from uname(1).
case "${OS}/${ARCH}" in
  Linux/x86_64)        TARGET="x86_64-unknown-linux-gnu";  PLATFORM="linux-x86_64" ;;
  Linux/aarch64)       TARGET="aarch64-unknown-linux-gnu"; PLATFORM="linux-arm64"  ;;
  Linux/arm64)         TARGET="aarch64-unknown-linux-gnu"; PLATFORM="linux-arm64"  ;;
  Darwin/*)
    echo "Cognitora ships Linux-only release artefacts; pack.sh requires a Linux host." >&2
    echo "On macOS, build directly: cargo build --release --no-default-features -p cgn-router -p cgn-agent -p cgn-kvcached -p cgn-metrics -p cgn-ctl -p cgn-operator" >&2
    exit 1
    ;;
  *) echo "unsupported host: ${OS}/${ARCH}" >&2; exit 1 ;;
esac

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
log()  { printf '\033[1;32m==>\033[0m %s\n' "$*"; }

bold "Cognitora release pack"
log "tag      = ${TAG}"
log "target   = ${TARGET}"
log "platform = ${PLATFORM}"
log "out      = ${DIST}"

# ---- build ----------------------------------------------------------------

log "building binaries (cargo build --release --no-default-features)"
cd "$ROOT"
cargo build --release --no-default-features \
  -p cgn-router -p cgn-agent -p cgn-kvcached \
  -p cgn-metrics -p cgn-ctl -p cgn-operator

# ---- pack -----------------------------------------------------------------

mkdir -p "$DIST"
STAGING="cognitora-${TAG}-${PLATFORM}"
WORK="${DIST}/${STAGING}"
rm -rf "$WORK"
mkdir -p "$WORK"

missing=()
for b in cgn-router cgn-agent cgn-kvcached cgn-metrics cgn-ctl cgn-operator; do
  src="${ROOT}/target/release/${b}"
  if [ -x "$src" ]; then
    cp "$src" "${WORK}/${b}"
  else
    missing+=("$b")
  fi
done
if [ "${#missing[@]}" -gt 0 ]; then
  echo "error: missing release binaries: ${missing[*]}" >&2
  exit 1
fi

cp "${ROOT}/LICENSE" "${WORK}/" 2>/dev/null || true
{
  echo "Cognitora ${TAG} for ${PLATFORM} (Rust target: ${TARGET})"
  echo
  echo "This archive was produced locally via scripts/release/pack.sh — it is"
  echo "NOT a signed/published release. Use only for dry-running the install."
  echo
  echo "Binaries:"
  for f in "$WORK"/cgn-*; do
    [ -e "$f" ] && printf '  - %s\n' "$(basename "$f")"
  done
} > "${WORK}/README.txt"

ARCHIVE="${STAGING}.tar.gz"
( cd "$DIST" && tar -czf "${ARCHIVE}" "${STAGING}" )
rm -rf "$WORK"

( cd "$DIST" && sha256sum "${ARCHIVE}" > "${ARCHIVE}.sha256" )

log "wrote ${DIST}/${ARCHIVE}"
log "wrote ${DIST}/${ARCHIVE}.sha256"
echo
bold "Dry-run install.sh against the local archive:"
cat <<EOF

  ( cd dist && python3 -m http.server 8765 ) &

  CGN_BASE_URL=http://127.0.0.1:8765 \\
    CGN_VERSION=${TAG} \\
    CGN_PREFIX=/tmp/cgn-test \\
    sh deploy/installer/install.sh

EOF
