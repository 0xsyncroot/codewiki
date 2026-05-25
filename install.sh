#!/usr/bin/env bash
# T-446 — CodeWiki one-liner installer (Linux / macOS)
# Usage:  curl -fsSL https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.sh | sh
#    Or:  ./install.sh [--version <tag>] [--dir <install-dir>] [--uninstall] [--help]
#
# Downloads the correct pre-built Rust binary from GitHub Releases,
# verifies the SHA-256 checksum, and places it in ~/.local/bin/.

set -euo pipefail

REPO="0xsyncroot/codewiki"
INSTALL_DIR="${CODEWIKI_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${CODEWIKI_VERSION:-latest}"
UNINSTALL=0

# ── Argument parsing ──────────────────────────────────────────────────────────

usage() {
  cat <<EOF
Usage: install.sh [OPTIONS]

OPTIONS:
  --version <tag>   Install a specific version (default: latest)
  --dir <path>      Install directory (default: ~/.local/bin)
  --uninstall       Remove codewiki from the install directory
  --help            Show this help

ENVIRONMENT:
  CODEWIKI_INSTALL_DIR   Override install directory
  CODEWIKI_VERSION       Override version tag

EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)  VERSION="$2"; shift 2 ;;
    --dir)      INSTALL_DIR="$2"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    --help|-h)  usage ;;
    *)  echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

# ── Uninstall path ────────────────────────────────────────────────────────────

if [[ $UNINSTALL -eq 1 ]]; then
  target="$INSTALL_DIR/codewiki"
  if [[ -f "$target" ]]; then
    rm -f "$target"
    echo "Removed $target"
  else
    echo "codewiki not found at $INSTALL_DIR"
  fi
  exit 0
fi

# ── OS / arch detection ───────────────────────────────────────────────────────

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  OSFAM="unknown-linux-gnu" ;;
  darwin) OSFAM="apple-darwin" ;;
  *)      echo "Unsupported OS: $OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64 | amd64) ARCH_NORM="x86_64" ;;
  aarch64 | arm64) ARCH_NORM="aarch64" ;;
  *)  echo "Unsupported architecture: $ARCH" >&2; exit 1 ;;
esac

TARGET_TRIPLE="${ARCH_NORM}-${OSFAM}"

# ── Version resolution ────────────────────────────────────────────────────────

if [[ "$VERSION" == "latest" ]]; then
  echo "Resolving latest version…"
  VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
  if [[ -z "$VERSION" ]]; then
    echo "Could not resolve latest version. Set CODEWIKI_VERSION explicitly." >&2
    exit 1
  fi
fi

echo "Installing codewiki ${VERSION} for ${TARGET_TRIPLE}…"

# ��─ Download ──────────────────────────────────────────────────────────────────

BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
ARCHIVE="codewiki-${TARGET_TRIPLE}.tar.gz"
CHECKSUM_FILE="${ARCHIVE}.sha256"

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL "${BASE_URL}/${ARCHIVE}" -o "${TMP_DIR}/${ARCHIVE}"
curl -fsSL "${BASE_URL}/${CHECKSUM_FILE}" -o "${TMP_DIR}/${CHECKSUM_FILE}"

# ── SHA-256 verification ──────────────────────────────────────────────────────

echo "Verifying checksum…"
cd "$TMP_DIR"
if command -v sha256sum &>/dev/null; then
  sha256sum -c "$CHECKSUM_FILE"
elif command -v shasum &>/dev/null; then
  shasum -a 256 -c "$CHECKSUM_FILE"
else
  echo "WARNING: Neither sha256sum nor shasum found; skipping checksum verification." >&2
fi
cd - >/dev/null

# ── Extract and install ───────────────────────────────────────────────────────

tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "$TMP_DIR"
mkdir -p "$INSTALL_DIR"
BINARY=$(find "$TMP_DIR" -maxdepth 1 -name "codewiki" -not -name "*.tar.gz" | head -1)
if [[ -z "$BINARY" ]]; then
  echo "Could not find codewiki binary in archive." >&2
  exit 1
fi
install -m 755 "$BINARY" "$INSTALL_DIR/codewiki"

# ── PATH hint ─────────────────────────────────────────────────────────────────

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
  echo ""
  echo "  NOTE: $INSTALL_DIR is not in your PATH."
  echo "  Add the following to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
  echo ""
  echo "    export PATH=\"\$PATH:$INSTALL_DIR\""
  echo ""
fi

echo "codewiki ${VERSION} installed to ${INSTALL_DIR}/codewiki"
"${INSTALL_DIR}/codewiki" --version 2>/dev/null || true
