#!/usr/bin/env bash
# Install the latest GitHub Release binary.
set -euo pipefail
REPO="${UNWRECK_REPO:-ayushdoesdev/unwreck}"
PREFIX="${UNWRECK_PREFIX:-$HOME/.local}"
os=$(uname -s)
arch=$(uname -m)
case "$os-$arch" in
  Linux-x86_64) t=x86_64-unknown-linux-musl ;;
  Linux-aarch64)
    echo "aarch64 Linux binaries ship in 0.1.1. Until then: cargo install unwreck" >&2
    exit 1
    ;;
  Darwin-arm64) t=aarch64-apple-darwin ;;
  Darwin-x86_64) t=x86_64-apple-darwin ;;
  *) echo "unsupported: $os $arch" >&2; exit 1 ;;
esac
url="https://github.com/${REPO}/releases/latest/download/unwreck-${t}.tar.xz"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL "$url" | tar -xJ -C "$tmp"
mkdir -p "$PREFIX/bin"
install -m 755 "$tmp"/unwreck-*/unwreck "$PREFIX/bin/unwreck"
echo "installed $PREFIX/bin/unwreck"
