#!/usr/bin/env bash
# Cognitora one-line installer.
#
#   curl -sSfL https://get.cognitora.dev | sh
#
# Or pin a specific channel:
#
#   COGNITORA_CHANNEL=stable curl -sSfL https://get.cognitora.dev | sh
#   COGNITORA_VERSION=v0.1.0 curl -sSfL https://get.cognitora.dev | sh
#
# All release artefacts are cosign-signed; this script verifies the
# signature against the public key shipped in the repository. The script
# is intentionally bash-only, no jq, no curl-pipe-bash anti-patterns.

set -euo pipefail

CHANNEL="${COGNITORA_CHANNEL:-stable}"
VERSION="${COGNITORA_VERSION:-}"
PREFIX="${COGNITORA_PREFIX:-/usr/local}"
GH_REPO="${COGNITORA_REPO:-cognitora/cognitora}"
COSIGN_PUBKEY_URL="${COGNITORA_COSIGN_PUBKEY:-https://raw.githubusercontent.com/${GH_REPO}/main/SECURITY/cosign.pub}"

BINS=(cgn-ctl cgn-router cgn-agent cgn-kvcached cgn-metrics cgn-operator)

log()   { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mwarn\033[0m %s\n' "$*"; }
fatal() { printf '\033[1;31merror\033[0m %s\n' "$*"; exit 1; }

# ---- discovery -------------------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"
case "${OS}/${ARCH}" in
  Linux/x86_64)   TARGET="x86_64-unknown-linux-gnu"  ;;
  Linux/aarch64)  TARGET="aarch64-unknown-linux-gnu" ;;
  Darwin/arm64)   TARGET="aarch64-apple-darwin"      ;;
  Darwin/x86_64)  TARGET="x86_64-apple-darwin"       ;;
  *) fatal "unsupported platform: ${OS}/${ARCH}" ;;
esac

if [[ -z "${VERSION}" ]]; then
  log "discovering latest ${CHANNEL} release"
  if ! VERSION=$(curl -sSfL "https://api.github.com/repos/${GH_REPO}/releases/latest" \
                  | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p'); then
    fatal "could not query latest release"
  fi
  [[ -z "${VERSION}" ]] && fatal "could not parse latest tag"
fi
log "installing ${VERSION} for ${TARGET}"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

# ---- fetch + verify --------------------------------------------------------

ARCHIVE="cognitora-${VERSION}-${TARGET}.tar.gz"
ARCHIVE_URL="https://github.com/${GH_REPO}/releases/download/${VERSION}/${ARCHIVE}"
SIG_URL="${ARCHIVE_URL}.sig"
SUM_URL="${ARCHIVE_URL}.sha256"

log "downloading ${ARCHIVE}"
curl -sSfL "${ARCHIVE_URL}" -o "${TMP}/${ARCHIVE}"
curl -sSfL "${SIG_URL}"     -o "${TMP}/${ARCHIVE}.sig"
curl -sSfL "${SUM_URL}"     -o "${TMP}/${ARCHIVE}.sha256"

log "verifying sha256"
( cd "${TMP}" && sha256sum -c "${ARCHIVE}.sha256" )

if command -v cosign >/dev/null 2>&1; then
  log "verifying cosign signature"
  curl -sSfL "${COSIGN_PUBKEY_URL}" -o "${TMP}/cosign.pub"
  cosign verify-blob \
    --key "${TMP}/cosign.pub" \
    --signature "${TMP}/${ARCHIVE}.sig" \
    "${TMP}/${ARCHIVE}"
else
  warn "cosign not found; skipping signature check (install cosign for hardened deploys)"
fi

# ---- extract + place -------------------------------------------------------

log "extracting"
tar -xzf "${TMP}/${ARCHIVE}" -C "${TMP}"

INSTALL_DIR="${PREFIX}/bin"
mkdir -p "${INSTALL_DIR}"
for b in "${BINS[@]}"; do
  if [[ -f "${TMP}/${b}" ]]; then
    install -m 0755 "${TMP}/${b}" "${INSTALL_DIR}/${b}"
    log "installed ${INSTALL_DIR}/${b}"
  fi
done

# ---- next steps ------------------------------------------------------------

cat <<EOF

  Cognitora ${VERSION} installed to ${INSTALL_DIR}.

  Next steps:

    cgn-ctl pki bootstrap                   # dev PKI material
    cgn-ctl install single-node             # systemd units + config
    cgn-ctl key create alice                # API key for the OpenAI surface

  Documentation: https://github.com/${GH_REPO}
EOF
