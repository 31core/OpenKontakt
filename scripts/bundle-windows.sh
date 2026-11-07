#!/usr/bin/env bash
# Build and package the Windows VST3 (plus the single-file standalone). Run on Windows from
# Git Bash, MSYS2, or any bash that can call the MSVC/MinGW cargo toolchain.
set -euo pipefail

cd "$(dirname "$0")/.."
PROFILE="${PROFILE:-release}"
PACKAGE="openkontakt-plugin"
NAME="OpenKontakt"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUNDLE="${TARGET_DIR}/bundled"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) ;;
  *)
    echo "This script must run on Windows (Git Bash or MSYS2)." >&2
    exit 1
    ;;
esac

if ! command -v cargo >/dev/null && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

echo "Building ${PROFILE}…"
cargo build -p "$PACKAGE" --profile "$PROFILE" --lib --bins

DLL="${TARGET_DIR}/${PROFILE}/openkontakt_plugin.dll"
STANDALONE="${TARGET_DIR}/${PROFILE}/openkontakt.exe"
if [[ ! -f "$DLL" || ! -f "$STANDALONE" ]]; then
  echo "build artifacts are missing (${DLL} / ${STANDALONE})" >&2
  exit 1
fi

rm -rf "$BUNDLE/${NAME}.vst3"
mkdir -p "$BUNDLE/${NAME}.vst3/Contents/x86_64-win"
cp "$DLL" "$BUNDLE/${NAME}.vst3/Contents/x86_64-win/${NAME}.vst3"

# The standalone is a single executable, so it needs no bundle.
cp "$STANDALONE" "$BUNDLE/${NAME}.exe"

ARCHIVE="${TARGET_DIR}/${NAME}-${VERSION}-windows-x86_64.zip"
rm -f "$ARCHIVE"
if command -v zip >/dev/null; then
  (cd "$BUNDLE" && zip -qr "../$(basename "$ARCHIVE")" "${NAME}.vst3")
else
  # Git Bash normally has PowerShell but not zip; Compress-Archive wants Windows paths.
  BUNDLE_WIN="$(cd "$BUNDLE" && { pwd -W 2>/dev/null || pwd; })"
  ARCHIVE_WIN="$(cd "$(dirname "$ARCHIVE")" && { pwd -W 2>/dev/null || pwd; })/$(basename "$ARCHIVE")"
  powershell -NoProfile -Command \
    "Compress-Archive -Path '${BUNDLE_WIN}/${NAME}.vst3' -DestinationPath '${ARCHIVE_WIN}' -Force"
fi

echo "Created:"
find "$BUNDLE" -maxdepth 4 -print | sort
echo "Archive: $ARCHIVE"
