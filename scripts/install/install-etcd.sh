#!/usr/bin/env bash
# scripts/install/install-etcd.sh
#
# Download a pinned etcd release into ~/.local/cognitora/etcd. Idempotent.
# Use this for dev/test stacks; production deployments should use a managed
# etcd cluster.
#
# Usage:
#   bash scripts/install/install-etcd.sh
#   ETCD_VERSION=v3.5.18 bash scripts/install/install-etcd.sh

set -euo pipefail

ETCD_VERSION=${ETCD_VERSION:-v3.5.18}
ETCD_DIR=${ETCD_DIR:-$HOME/.local/cognitora/etcd}

OS=$(uname -s)
ARCH=$(uname -m)
case "$OS:$ARCH" in
  Linux:x86_64)        PLATFORM=linux-amd64;  EXT=tar.gz ;;
  Linux:aarch64)       PLATFORM=linux-arm64;  EXT=tar.gz ;;
  Linux:arm64)         PLATFORM=linux-arm64;  EXT=tar.gz ;;
  Darwin:x86_64)       PLATFORM=darwin-amd64; EXT=zip ;;
  Darwin:arm64)        PLATFORM=darwin-arm64; EXT=zip ;;
  *) echo "unsupported platform $OS/$ARCH" >&2; exit 1 ;;
esac

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

mkdir -p "$ETCD_DIR"
if [ -x "$ETCD_DIR/etcd-$ETCD_VERSION-$PLATFORM/etcd" ]; then
  log "etcd $ETCD_VERSION already installed at $ETCD_DIR"
else
  log "downloading etcd $ETCD_VERSION ($PLATFORM)"
  url=https://github.com/etcd-io/etcd/releases/download/$ETCD_VERSION/etcd-$ETCD_VERSION-$PLATFORM.$EXT
  if [ "$EXT" = "tar.gz" ]; then
    curl -fsSL -o "$ETCD_DIR/etcd.tgz" "$url"
    tar -xzf "$ETCD_DIR/etcd.tgz" -C "$ETCD_DIR"
    rm -f "$ETCD_DIR/etcd.tgz"
  else
    command -v unzip >/dev/null 2>&1 || { echo "unzip not installed; install with 'brew install unzip' or 'apt-get install unzip'" >&2; exit 1; }
    curl -fsSL -o "$ETCD_DIR/etcd.zip" "$url"
    unzip -q -o "$ETCD_DIR/etcd.zip" -d "$ETCD_DIR"
    rm -f "$ETCD_DIR/etcd.zip"
  fi
fi

# Symlink for stable lookup
ln -sf "$ETCD_DIR/etcd-$ETCD_VERSION-$PLATFORM/etcd"    "$ETCD_DIR/etcd"
ln -sf "$ETCD_DIR/etcd-$ETCD_VERSION-$PLATFORM/etcdctl" "$ETCD_DIR/etcdctl"

"$ETCD_DIR/etcd" --version | head -1
log "etcd ready (set ETCD_DIR=$ETCD_DIR)"
