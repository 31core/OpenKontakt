#!/usr/bin/env bash
# Build and package the Windows VST3 (plus the single-file standalone).
#
# On Windows run it from Git Bash, MSYS2 or any bash that can call the MSVC/MinGW cargo toolchain.
# On Linux the same script cross-compiles with cargo-xwin:
#
#   rustup target add x86_64-pc-windows-msvc
#   cargo install cargo-xwin
#   ./scripts/bundle-windows.sh
set -euo pipefail

cd "$(dirname "$0")/.."
PROFILE="${PROFILE:-release}"
PACKAGE="openkontakt-plugin"
NAME="OpenKontakt"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUNDLE="${TARGET_DIR}/bundled"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)"

if ! command -v cargo >/dev/null && [[ -x "$HOME/.cargo/bin/cargo" ]]; then
  export PATH="$HOME/.cargo/bin:$PATH"
fi

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    HOST_KIND=windows
    ;;
  Linux*)
    HOST_KIND=linux
    ;;
  *)
    echo "This script runs on Windows (Git Bash or MSYS2) or on Linux (with cargo-xwin)." >&2
    exit 1
    ;;
esac

if [[ "$HOST_KIND" == linux ]]; then
  if ! command -v cargo-xwin >/dev/null 2>&1; then
    echo "cargo-xwin is required for cross-compiling on Linux:" >&2
    echo "  cargo install cargo-xwin" >&2
    exit 1
  fi
  echo "Cross-compiling ${PROFILE} for x86_64-pc-windows-msvc with cargo-xwin…"
  cargo xwin build -p "$PACKAGE" --profile "$PROFILE" --lib --bins \
    --target x86_64-pc-windows-msvc
  ARTIFACT_DIR="${TARGET_DIR}/x86_64-pc-windows-msvc/${PROFILE}"
else
  echo "Building ${PROFILE}…"
  cargo build -p "$PACKAGE" --profile "$PROFILE" --lib --bins
  ARTIFACT_DIR="${TARGET_DIR}/${PROFILE}"
fi

DLL="${ARTIFACT_DIR}/openkontakt_plugin.dll"
STANDALONE="${ARTIFACT_DIR}/openkontakt.exe"
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
elif [[ "$HOST_KIND" == windows ]]; then
  # Git Bash normally has PowerShell but not zip; Compress-Archive wants Windows paths.
  BUNDLE_WIN="$(cd "$BUNDLE" && { pwd -W 2>/dev/null || pwd; })"
  ARCHIVE_WIN="$(cd "$(dirname "$ARCHIVE")" && { pwd -W 2>/dev/null || pwd; })/$(basename "$ARCHIVE")"
  powershell -NoProfile -Command \
    "Compress-Archive -Path '${BUNDLE_WIN}/${NAME}.vst3' -DestinationPath '${ARCHIVE_WIN}' -Force"
else
  echo "zip is required to package the archive on Linux (apt install zip)." >&2
  exit 1
fi

echo "Created:"
find "$BUNDLE" -maxdepth 4 -print | sort
echo "Archive: $ARCHIVE"
