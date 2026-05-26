#!/bin/sh
# T-446 — CodeWiki one-liner installer (Linux / macOS)
# Usage:  curl -fsSL https://raw.githubusercontent.com/0xsyncroot/codewiki/main/install.sh | sh
#    Or:  ./install.sh [--version <tag>] [--dir <install-dir>] [--uninstall] [--help]
#
# Downloads the correct pre-built Rust binary from GitHub Releases,
# verifies the SHA-256 checksum, and places it in ~/.local/bin/.
#
# RE-RUN / UPGRADE: running this again detects the installed version, compares
# it against the target (latest release, or a pinned --version), and:
#   * exits as a no-op when already on the target version,
#   * upgrades (or downgrades, with a notice) otherwise,
# backing up the current binary and rolling back if the new one fails to run.
#
# POSIX sh — works under dash/ash/bash; no bashisms (so `curl … | sh` is safe).

set -eu

REPO="0xsyncroot/codewiki"
INSTALL_DIR="${CODEWIKI_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${CODEWIKI_VERSION:-latest}"
# Override for tests: a URL/path `curl` can fetch returning the releases/latest JSON.
RELEASE_API="${CODEWIKI_RELEASE_API:-https://api.github.com/repos/${REPO}/releases/latest}"
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
  CODEWIKI_RELEASE_API   Override the releases/latest API URL (testing)
  CODEWIKI_DOWNLOAD_BASE Override the release-asset download base URL (testing)

EOF
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)  VERSION="$2"; shift 2 ;;
    --dir)      INSTALL_DIR="$2"; shift 2 ;;
    --uninstall) UNINSTALL=1; shift ;;
    --help|-h)  usage ;;
    *)  echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

# ── Uninstall path ────────────────────────────────────────────────────────────

if [ "$UNINSTALL" -eq 1 ]; then
  target="$INSTALL_DIR/codewiki"
  if [ -f "$target" ]; then
    rm -f "$target"
    echo "Removed $target"
  else
    echo "codewiki not found at $INSTALL_DIR"
  fi
  exit 0
fi

# ── Version helpers ───────────────────────────────────────────────────────────

# Strip a leading `v`, drop any -prerelease/+build suffix, echo MAJOR.MINOR.PATCH
# (missing components default to 0). Echoes nothing if unparseable.
normalize_version() {
  raw="${1:-}"
  raw="${raw#v}"; raw="${raw#V}"
  # Drop everything from the first '-' or '+'.
  raw="${raw%%-*}"; raw="${raw%%+*}"
  [ -z "$raw" ] && return 0
  # Split on '.' without arrays (POSIX): set the positional params via IFS.
  _oldifs="$IFS"; IFS='.'
  # shellcheck disable=SC2086
  set -- $raw
  IFS="$_oldifs"
  _major="${1:-0}"; _minor="${2:-0}"; _patch="${3:-0}"; _extra="${4:-}"
  # All three must be pure integers, and there must be no 4th component.
  [ -z "$_extra" ] || return 0
  case "$_major" in ''|*[!0-9]*) return 0 ;; esac
  case "$_minor" in ''|*[!0-9]*) return 0 ;; esac
  case "$_patch" in ''|*[!0-9]*) return 0 ;; esac
  echo "${_major}.${_minor}.${_patch}"
}

# Numeric semver compare. Echoes -1 (a<b), 0 (a==b), or 1 (a>b).
# Inputs must already be normalized MAJOR.MINOR.PATCH.
compare_versions() {
  _a="$1"; _b="$2"
  _oldifs="$IFS"; IFS='.'
  # shellcheck disable=SC2086
  set -- $_a; _a1="${1:-0}"; _a2="${2:-0}"; _a3="${3:-0}"
  # shellcheck disable=SC2086
  set -- $_b; _b1="${1:-0}"; _b2="${2:-0}"; _b3="${3:-0}"
  IFS="$_oldifs"
  if [ "$_a1" -gt "$_b1" ]; then echo 1; return; fi
  if [ "$_a1" -lt "$_b1" ]; then echo -1; return; fi
  if [ "$_a2" -gt "$_b2" ]; then echo 1; return; fi
  if [ "$_a2" -lt "$_b2" ]; then echo -1; return; fi
  if [ "$_a3" -gt "$_b3" ]; then echo 1; return; fi
  if [ "$_a3" -lt "$_b3" ]; then echo -1; return; fi
  echo 0
}

# Echo the installed version token (raw tag) by running the binary, or nothing.
detect_installed_version() {
  _bin="$1"
  [ -x "$_bin" ] || return 0
  # `codewiki --version` prints e.g. "codewiki 0.1.1"; grab the last whitespace
  # token. Tolerate any failure → echo nothing (treated as force install).
  _out="$("$_bin" --version 2>/dev/null)" || return 0
  printf '%s\n' "$_out" | head -1 | awk '{print $NF}'
}

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

# ── Target version resolution ─────────────────────────────────────────────────

if [ "$VERSION" = "latest" ]; then
  echo "Resolving latest version…"
  VERSION=$(curl -fsSL "$RELEASE_API" \
    | grep '"tag_name"' \
    | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')
  if [ -z "$VERSION" ]; then
    echo "Could not resolve latest version. Set CODEWIKI_VERSION explicitly." >&2
    exit 1
  fi
fi

DEST="$INSTALL_DIR/codewiki"

# ── Re-run / upgrade decision ─────────────────────────────────────────────────

INSTALLED_RAW="$(detect_installed_version "$DEST")"
INSTALLED_NORM="$(normalize_version "$INSTALLED_RAW")"
TARGET_NORM="$(normalize_version "$VERSION")"

if [ -n "$INSTALLED_NORM" ] && [ -n "$TARGET_NORM" ]; then
  cmp="$(compare_versions "$INSTALLED_NORM" "$TARGET_NORM")"
  case "$cmp" in
    0)
      echo "codewiki is already on ${VERSION} (installed: ${INSTALLED_RAW})."
      exit 0
      ;;
    1)
      echo "Notice: downgrading ${INSTALLED_RAW} -> ${VERSION}."
      ;;
    -1)
      echo "Upgrading ${INSTALLED_RAW} -> ${VERSION}…"
      ;;
  esac
elif [ -n "$INSTALLED_RAW" ]; then
  # Binary exists but its version is unparseable → force (re)install.
  echo "Reinstalling codewiki (installed version '${INSTALLED_RAW}' unparseable) -> ${VERSION}…"
else
  echo "Installing codewiki ${VERSION} for ${TARGET_TRIPLE}…"
fi

# ── Download ──────────────────────────────────────────────────────────────────

# Override for tests: a base URL/path `curl` can fetch the release assets from.
# Defaults to the public GitHub releases download URL for the resolved tag.
BASE_URL="${CODEWIKI_DOWNLOAD_BASE:-https://github.com/${REPO}/releases/download/${VERSION}}"
ARCHIVE="codewiki-${TARGET_TRIPLE}.tar.gz"
CHECKSUM_FILE="${ARCHIVE}.sha256"

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

curl -fsSL "${BASE_URL}/${ARCHIVE}" -o "${TMP_DIR}/${ARCHIVE}"
curl -fsSL "${BASE_URL}/${CHECKSUM_FILE}" -o "${TMP_DIR}/${CHECKSUM_FILE}"

# ── SHA-256 verification ──────────────────────────────────────────────────────

echo "Verifying checksum…"
cd "$TMP_DIR"
if command -v sha256sum >/dev/null 2>&1; then
  sha256sum -c "$CHECKSUM_FILE"
elif command -v shasum >/dev/null 2>&1; then
  shasum -a 256 -c "$CHECKSUM_FILE"
else
  echo "WARNING: Neither sha256sum nor shasum found; skipping checksum verification." >&2
fi
cd - >/dev/null

# ── Extract ───────────────────────────────────────────────────────────────────

tar -xzf "${TMP_DIR}/${ARCHIVE}" -C "$TMP_DIR"
mkdir -p "$INSTALL_DIR"
BINARY=$(find "$TMP_DIR" -maxdepth 1 -name "codewiki" -not -name "*.tar.gz" | head -1)
if [ -z "$BINARY" ]; then
  echo "Could not find codewiki binary in archive." >&2
  exit 1
fi

# ── Install / atomic replace with backup + smoke test + rollback ──────────────

BACKUP=""
if [ -f "$DEST" ]; then
  # Back up the current binary so we can roll back if the new one is broken.
  BACKUP="${DEST}.bak.$$"
  cp -p "$DEST" "$BACKUP"
fi

# Stage into the install dir, then atomically rename over the destination so a
# reader never observes a half-written file.
STAGED="${DEST}.new.$$"
install -m 755 "$BINARY" "$STAGED"
mv -f "$STAGED" "$DEST"

# Smoke test: the freshly-installed binary must run `--version`.
if ! "$DEST" --version >/dev/null 2>&1; then
  echo "ERROR: the new codewiki binary failed its smoke test." >&2
  if [ -n "$BACKUP" ]; then
    echo "Rolling back to the previous version…" >&2
    mv -f "$BACKUP" "$DEST"
    BACKUP=""
  else
    rm -f "$DEST"
  fi
  exit 1
fi

# Success → drop the backup.
if [ -n "$BACKUP" ]; then
  rm -f "$BACKUP"
fi

# ── PATH hint ─────────────────────────────────────────────────────────────────

if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
  echo ""
  echo "  NOTE: $INSTALL_DIR is not in your PATH."
  echo "  Add the following to your shell profile (~/.bashrc, ~/.zshrc, etc.):"
  echo ""
  echo "    export PATH=\"\$PATH:$INSTALL_DIR\""
  echo ""
fi

if [ -n "$INSTALLED_NORM" ] && [ -n "$TARGET_NORM" ] && [ "$INSTALLED_NORM" != "$TARGET_NORM" ]; then
  echo "Upgraded ${INSTALLED_RAW} -> ${VERSION} (${INSTALL_DIR}/codewiki)"
else
  echo "codewiki ${VERSION} installed to ${INSTALL_DIR}/codewiki"
fi
"${DEST}" --version 2>/dev/null || true
