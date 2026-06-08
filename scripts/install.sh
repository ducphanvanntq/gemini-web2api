#!/usr/bin/env bash
# Install / update gemini-web2api from the latest GitHub release (macOS / Linux).
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/ducphanvanntq/gemini-web2api/main/scripts/install.sh | bash
#
# Environment variables:
#   REPO     GitHub "owner/repo" (default: ducphanvanntq/gemini-web2api)
#   VERSION  Tag to install     (default: latest)
#   PREFIX   Install dir        (default: $HOME/.local/bin)

set -euo pipefail

REPO="${REPO:-ducphanvanntq/gemini-web2api}"
VERSION="${VERSION:-latest}"
PREFIX="${PREFIX:-$HOME/.local/bin}"

uname_s="$(uname -s)"
uname_m="$(uname -m)"

case "$uname_s" in
  Darwin)
    case "$uname_m" in
      arm64|aarch64) ASSET="gemini-web2api-macos-aarch64.tar.gz" ;;
      x86_64)        ASSET="gemini-web2api-macos-x86_64.tar.gz" ;;
      *) echo "Unsupported macOS arch: $uname_m" >&2; exit 1 ;;
    esac
    ;;
  Linux)
    case "$uname_m" in
      x86_64) ASSET="gemini-web2api-linux-x86_64.tar.gz" ;;
      *) echo "Unsupported Linux arch: $uname_m" >&2; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $uname_s. Use scripts/install.ps1 on Windows." >&2
    exit 1
    ;;
esac

echo "Fetching release info..."
if [ "$VERSION" = "latest" ]; then
  API="https://api.github.com/repos/${REPO}/releases/latest"
else
  API="https://api.github.com/repos/${REPO}/releases/tags/${VERSION}"
fi
RELEASE_JSON="$(curl -fsSL -H 'User-Agent: gemini-web2api-install' "$API")"

TAG="$(printf '%s' "$RELEASE_JSON" \
  | grep -oE '"tag_name"[[:space:]]*:[[:space:]]*"[^"]+"' | head -n1 | cut -d'"' -f4)"
if [ -z "$TAG" ]; then
  echo "Could not determine release tag for $REPO ($VERSION)." >&2
  echo "Have you published a release yet? (Actions -> release -> Run workflow)" >&2
  exit 1
fi

URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET}"
echo "Downloading $ASSET ($TAG)..."

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL -o "$TMP/${ASSET}" "$URL"
tar -xzf "$TMP/${ASSET}" -C "$TMP"

BIN_PATH="$(find "$TMP" -maxdepth 3 -type f -name 'gemini-web2api' | head -n1)"
if [ -z "$BIN_PATH" ]; then
  echo "Could not find gemini-web2api binary inside the archive." >&2
  exit 1
fi

mkdir -p "$PREFIX"
DEST="$PREFIX/gemini-web2api"
# install replaces the destination atomically, avoiding "text file busy"
# if the binary is currently running.
install -m 0755 "$BIN_PATH" "$DEST"
echo
echo "Installed: $DEST ($TAG)"

# Drop a default config where the binary auto-discovers it, unless one exists.
CONFIG_DIR="$HOME/.config/gemini-web2api"
CONFIG_FILE="$CONFIG_DIR/config.json"
EXAMPLE="$(find "$TMP" -maxdepth 3 -type f -name 'config.example.json' | head -n1)"
if [ -n "$EXAMPLE" ] && [ ! -f "$CONFIG_FILE" ]; then
  mkdir -p "$CONFIG_DIR"
  cp "$EXAMPLE" "$CONFIG_FILE"
  echo "Default config written: $CONFIG_FILE"
fi

case ":$PATH:" in
  *":$PREFIX:"*) ;;
  *)
    echo
    echo "Note: $PREFIX is not on PATH. Add it with:"
    echo "  echo 'export PATH=\"$PREFIX:\$PATH\"' >> ~/.profile && source ~/.profile"
    ;;
esac

echo
echo "Done! Run the server with:"
echo "  gemini-web2api"
echo
echo "It listens on http://localhost:8081/v1 by default."
echo "Edit $CONFIG_FILE to set api_keys / port / cookie, or pass --port / --config."
