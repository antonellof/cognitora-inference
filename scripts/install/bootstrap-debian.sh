#!/usr/bin/env bash
# scripts/install/bootstrap-debian.sh
#
# Install the Debian/Ubuntu host packages Cognitora needs to build the Rust
# binaries. Idempotent. Designed to be run on a fresh GCP/EC2 VM, a
# bare-metal node, or a laptop.
#
# Inputs (env, all optional):
#   SKIP_RUST=1          - assume rustup is already installed
#   RUST_TOOLCHAIN=...    - default: 1.89.0
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/.../bootstrap-debian.sh | bash
#   # or
#   bash scripts/install/bootstrap-debian.sh
#
# Side effects:
#   apt-get update + install of build essentials and protobuf headers.
#   Installs rustup into ~/.cargo via the official one-liner.

set -euo pipefail

RUST_TOOLCHAIN=${RUST_TOOLCHAIN:-1.89.0}

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

if [ "$(id -u)" -ne 0 ]; then
  SUDO=sudo
else
  SUDO=
fi

log "installing apt packages"
$SUDO apt-get update -y
$SUDO apt-get install -y --no-install-recommends \
  build-essential pkg-config curl ca-certificates git \
  libssl-dev libclang-dev libprotobuf-dev protobuf-compiler \
  python3 python3-pip python3-venv jq

if [ "${SKIP_RUST:-0}" != "1" ]; then
  if ! command -v rustup >/dev/null 2>&1; then
    log "installing rustup ($RUST_TOOLCHAIN)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --default-toolchain "$RUST_TOOLCHAIN"
  fi
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
  rustup default "$RUST_TOOLCHAIN" >/dev/null
  log "rustc $(rustc --version)"
fi

log "bootstrap done"
