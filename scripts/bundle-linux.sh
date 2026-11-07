#!/usr/bin/env bash
# Build and package the Linux VST3 (plus the single-file standalone). Run on Linux.
set -euo pipefail

cd "$(dirname "$0")/.."
PROFILE="${PROFILE:-release}"
PACKAGE="openkontakt-plugin"
NAME="OpenKontakt"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUNDLE="${TARGET_DIR}/bundled"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"
ARCH="$(uname -m)"
ARCH_DIR="${ARCH}-linux"

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "This script must run on Linux." >&2
  exit 1
fi

if ! command -v cargo >/dev/null && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

echo "Building ${PROFILE}…"
cargo build -p "$PACKAGE" --profile "$PROFILE" --lib --bins

LIB="${TARGET_DIR}/${PROFILE}/libopenkontakt_plugin.so"
STANDALONE="${TARGET_DIR}/${PROFILE}/openkontakt"
if [[ ! -f "$LIB" || ! -f "$STANDALONE" ]]; then
  echo "build artifacts are missing (${LIB} / ${STANDALONE})" >&2
  exit 1
fi

rm -rf "$BUNDLE/${NAME}.vst3"
mkdir -p "$BUNDLE/${NAME}.vst3/Contents/${ARCH_DIR}"
cp "$LIB" "$BUNDLE/${NAME}.vst3/Contents/${ARCH_DIR}/${NAME}.so"

# The standalone is a single executable, so it needs no bundle.
cp "$STANDALONE" "$BUNDLE/openkontakt"
chmod +x "$BUNDLE/openkontakt"

ARCHIVE="${TARGET_DIR}/${NAME}-${VERSION}-linux-${ARCH}.tar.gz"
rm -f "$ARCHIVE"
tar -C "$BUNDLE" -czf "$ARCHIVE" "${NAME}.vst3"

echo "Created:"
find "$BUNDLE" -maxdepth 4 -print | sort
echo "Archive: $ARCHIVE"
