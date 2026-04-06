#!/usr/bin/env bash
#
# c9watch CLI installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/minchenlee/c9watch/main/install-cli.sh | bash
#
# This script:
#   1. Detects your OS and architecture
#   2. Downloads the latest c9watch CLI binary from GitHub
#   3. Installs it to ~/.local/bin (or /usr/local/bin if --global)
#
set -euo pipefail

REPO="minchenlee/c9watch"
BINARY_NAME="c9watch"

# --- Helpers ---

info()  { printf '\033[1;34m=>\033[0m %s\n' "$*"; }
warn()  { printf '\033[1;33m=>\033[0m %s\n' "$*"; }
error() { printf '\033[1;31mError:\033[0m %s\n' "$*" >&2; exit 1; }

# --- Parse args ---

INSTALL_DIR="${HOME}/.local/bin"
for arg in "$@"; do
  case "$arg" in
    --global) INSTALL_DIR="/usr/local/bin" ;;
  esac
done

# --- Detect platform ---

ARCH=$(uname -m)
case "$ARCH" in
  arm64|aarch64) ARCH_LABEL="aarch64" ;;
  x86_64)        ARCH_LABEL="x86_64" ;;
  *)             error "Unsupported architecture: $ARCH" ;;
esac

OS=$(uname -s)
case "$OS" in
  Darwin) TARGET="${ARCH_LABEL}-apple-darwin" ;;
  Linux)  TARGET="${ARCH_LABEL}-unknown-linux-gnu" ;;
  *)      error "Unsupported OS: $OS" ;;
esac

info "Detected ${OS} (${ARCH_LABEL})"

# --- Find latest release ---

info "Fetching latest release..."

LATEST_TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep '"tag_name":' \
  | head -1 \
  | cut -d'"' -f4)

if [ -z "$LATEST_TAG" ]; then
  error "Could not determine the latest release. Check https://github.com/${REPO}/releases"
fi

info "Latest version: ${LATEST_TAG}"

# --- Download and extract ---

DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${LATEST_TAG}/${BINARY_NAME}-cli-${TARGET}.tar.gz"
TMPDIR=$(mktemp -d)
trap 'rm -rf "$TMPDIR"' EXIT

info "Downloading c9watch CLI for ${TARGET}..."
curl -fSL --progress-bar "$DOWNLOAD_URL" -o "$TMPDIR/c9watch-cli.tar.gz"

info "Extracting..."
tar -xzf "$TMPDIR/c9watch-cli.tar.gz" -C "$TMPDIR"

# --- Install ---

mkdir -p "$INSTALL_DIR"

if [ -f "$INSTALL_DIR/$BINARY_NAME" ]; then
  info "Replacing existing installation..."
fi

cp "$TMPDIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
chmod +x "$INSTALL_DIR/$BINARY_NAME"

# --- Verify ---

echo ""
info "c9watch CLI ${LATEST_TAG} installed to ${INSTALL_DIR}/${BINARY_NAME}"

# Check if install dir is in PATH
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    warn "${INSTALL_DIR} is not in your PATH. Add it with:"
    echo ""
    echo "    export PATH=\"${INSTALL_DIR}:\$PATH\""
    echo ""
    ;;
esac

info "Run 'c9watch --help' to get started."
echo ""
