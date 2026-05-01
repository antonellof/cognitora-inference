#!/usr/bin/env sh
# Cognitora one-line installer.
#
#   curl -fsSL https://raw.githubusercontent.com/antonellof/cognitora-inference/main/deploy/installer/install.sh | sh
#
# Pin a version:
#
#   curl -fsSL .../install.sh | CGN_VERSION=v0.1.0 sh
#
# Install to a custom location (defaults to /usr/local/bin if writable,
# otherwise $HOME/.cognitora/bin):
#
#   curl -fsSL .../install.sh | CGN_PREFIX=$HOME/.local sh
#
# Override the source repo (useful for forks / mirrors):
#
#   curl -fsSL .../install.sh | CGN_REPO=org/repo sh
#
# Test against a local artefact directory (the directory must contain
# `cognitora-${CGN_VERSION}-${TARGET}.tar.gz` and matching `.sha256`):
#
#   CGN_BASE_URL=file:///tmp/dist CGN_VERSION=v0.1.0 sh install.sh
#
# All release artefacts ship a sha256 sum; the script verifies it
# unconditionally. Cosign signature verification runs only if `cosign` is
# installed locally — it is *not* fatal when missing.
#
# Plain POSIX sh — no bashisms, no jq, no curl|sh anti-patterns.

set -eu

CGN_VERSION="${CGN_VERSION:-}"
CGN_REPO="${CGN_REPO:-antonellof/cognitora-inference}"
CGN_PREFIX="${CGN_PREFIX:-}"
CGN_BASE_URL="${CGN_BASE_URL:-}"
COSIGN_PUBKEY_URL="${CGN_COSIGN_PUBKEY:-https://raw.githubusercontent.com/${CGN_REPO}/main/SECURITY/cosign.pub}"

# Binaries we ship. Anything missing from the tarball is silently skipped.
BINS="cgn-router cgn-agent cgn-kvcached cgn-ctl cgn-metrics cgn-operator"

bold()  { printf '\033[1m%s\033[0m\n' "$*"; }
log()   { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33mwarn\033[0m %s\n' "$*"; }
fatal() { printf '\033[1;31merror\033[0m %s\n' "$*" >&2; exit 1; }

require() {
  command -v "$1" >/dev/null 2>&1 || fatal "missing dependency: $1"
}

require curl
require tar
require uname
require mktemp

# ---- Detect target ---------------------------------------------------------

OS="$(uname -s)"
ARCH="$(uname -m)"
case "${OS}/${ARCH}" in
  Linux/x86_64)        TARGET="x86_64-unknown-linux-gnu"  ;;
  Linux/aarch64)       TARGET="aarch64-unknown-linux-gnu" ;;
  Linux/arm64)         TARGET="aarch64-unknown-linux-gnu" ;;
  Darwin/arm64)        TARGET="aarch64-apple-darwin"      ;;
  Darwin/x86_64)       TARGET="x86_64-apple-darwin"       ;;
  *) fatal "unsupported platform: ${OS}/${ARCH}" ;;
esac

# ---- Resolve version -------------------------------------------------------

if [ -z "${CGN_VERSION}" ]; then
  log "looking up latest release of ${CGN_REPO}"
  if ! CGN_VERSION="$(curl -fsSL "https://api.github.com/repos/${CGN_REPO}/releases/latest" \
                       | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -1)"; then
    fatal "could not query latest release"
  fi
  [ -z "${CGN_VERSION}" ] && fatal "no published release found for ${CGN_REPO}"
fi
log "installing ${CGN_VERSION} for ${TARGET}"

# ---- Resolve install prefix -----------------------------------------------

if [ -z "${CGN_PREFIX}" ]; then
  if [ -w /usr/local/bin ] 2>/dev/null; then
    CGN_PREFIX=/usr/local
  else
    CGN_PREFIX="$HOME/.cognitora"
  fi
fi
INSTALL_DIR="${CGN_PREFIX}/bin"
mkdir -p "$INSTALL_DIR" 2>/dev/null || fatal "cannot create $INSTALL_DIR (try CGN_PREFIX=\$HOME/.cognitora)"

# ---- Download + verify -----------------------------------------------------

if [ -z "${CGN_BASE_URL}" ]; then
  CGN_BASE_URL="https://github.com/${CGN_REPO}/releases/download/${CGN_VERSION}"
fi

ARCHIVE="cognitora-${CGN_VERSION}-${TARGET}.tar.gz"
ARCHIVE_URL="${CGN_BASE_URL}/${ARCHIVE}"
SUM_URL="${ARCHIVE_URL}.sha256"
SIG_URL="${ARCHIVE_URL}.sig"

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT INT TERM

fetch() {
  src="$1"; dst="$2"
  case "$src" in
    file://*)
      cp "${src#file://}" "$dst"
      ;;
    *)
      curl -fsSL "$src" -o "$dst"
      ;;
  esac
}

log "downloading ${ARCHIVE}"
fetch "${ARCHIVE_URL}" "${TMP}/${ARCHIVE}"
fetch "${SUM_URL}"     "${TMP}/${ARCHIVE}.sha256"

log "verifying sha256"
# Linux: sha256sum, macOS: shasum -a 256. The .sha256 file from the workflow
# is in plain `<sum>  <name>` shasum format which both tools accept via -c.
( cd "${TMP}"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "${ARCHIVE}.sha256"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "${ARCHIVE}.sha256"
  else
    fatal "neither sha256sum nor shasum available; cannot verify checksum"
  fi
) || fatal "checksum mismatch — refusing to install"

if command -v cosign >/dev/null 2>&1; then
  if fetch "${SIG_URL}" "${TMP}/${ARCHIVE}.sig" 2>/dev/null; then
    log "verifying cosign signature"
    fetch "${COSIGN_PUBKEY_URL}" "${TMP}/cosign.pub" 2>/dev/null || true
    if [ -s "${TMP}/cosign.pub" ]; then
      cosign verify-blob \
        --key "${TMP}/cosign.pub" \
        --signature "${TMP}/${ARCHIVE}.sig" \
        "${TMP}/${ARCHIVE}" >/dev/null 2>&1 \
        || fatal "cosign verification failed"
    else
      warn "cosign public key not published yet; skipping signature check"
    fi
  fi
else
  warn "cosign not installed — skipping signature check (install via 'brew install cosign' or sigstore.dev)"
fi

# ---- Extract + place ------------------------------------------------------

log "extracting"
tar -xzf "${TMP}/${ARCHIVE}" -C "${TMP}"

# The workflow's tarball top-level dir matches the archive base name.
SRC_DIR="${TMP}/cognitora-${CGN_VERSION}-${TARGET}"
[ -d "$SRC_DIR" ] || SRC_DIR="$TMP"

installed=""
for b in $BINS; do
  if [ -f "${SRC_DIR}/${b}" ]; then
    install -m 0755 "${SRC_DIR}/${b}" "${INSTALL_DIR}/${b}" 2>/dev/null || \
      cp "${SRC_DIR}/${b}" "${INSTALL_DIR}/${b}" && chmod 0755 "${INSTALL_DIR}/${b}"
    installed="${installed} ${b}"
  fi
done
[ -n "$installed" ] || fatal "no binaries found in ${ARCHIVE}; aborting"

# Helpful symlink: cgn-agent → cgn-router does not happen; binaries are
# distinct. Just summarise what we placed.

# ---- PATH advice ----------------------------------------------------------

bold ""
bold "Cognitora ${CGN_VERSION} installed:"
for b in $installed; do
  printf "  %s/%s\n" "${INSTALL_DIR}" "$b"
done
bold ""

case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    # The single-quoted `$PATH` inside the printf is *intentional* — it's
    # the snippet we ask the user to paste verbatim into their shell rc.
    # shellcheck disable=SC2016
    bold "Add ${INSTALL_DIR} to your PATH:"
    # shellcheck disable=SC2016
    printf '\n  export PATH="%s:$PATH"\n\n' "${INSTALL_DIR}"
    ;;
esac

cat <<EOF
Quick start:

  cgn-ctl --version
  cgn-ctl pki bootstrap                   # dev TLS material
  cgn-ctl key create alice                # API key for the OpenAI surface

Documentation: https://github.com/${CGN_REPO}
EOF
