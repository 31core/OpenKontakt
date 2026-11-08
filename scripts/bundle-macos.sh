#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."
PROFILE="${PROFILE:-release}"
PACKAGE="openkontakt-plugin"
NAME="OpenKontakt"
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BUNDLE="${TARGET_DIR}/bundled"
LIB="${TARGET_DIR}/${PROFILE}/libopenkontakt_plugin.dylib"
STANDALONE="${TARGET_DIR}/${PROFILE}/openkontakt"

echo "Building ${PROFILE}…"
if [[ -z "${CARGO_HOME:-}" && -d "$PWD/.cargo-home" ]]; then
  export CARGO_HOME="$PWD/.cargo-home"
fi
cargo build -p "$PACKAGE" --profile "$PROFILE" --lib --bins

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This helper currently creates macOS bundles only." >&2
  exit 1
fi

rm -rf "$BUNDLE"
mkdir -p "$BUNDLE"

make_bundle() {
  local kind="$1" root="$2" executable="$3"
  mkdir -p "$root/Contents/MacOS"
  cp "$LIB" "$root/Contents/MacOS/$executable"
  cat > "$root/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist>
  <dict>
    <key>CFBundleExecutable</key><string>$executable</string>
    <key>CFBundleIdentifier</key><string>com.openkontakt.${kind}</string>
    <key>CFBundleName</key><string>$NAME</string>
    <key>CFBundleDisplayName</key><string>$NAME</string>
    <key>CFBundlePackageType</key><string>BNDL</string>
    <key>CFBundleSignature</key><string>????</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>NSHighResolutionCapable</key><true/>
  </dict>
</plist>
PLIST
  printf 'BNDL????' > "$root/Contents/PkgInfo"
  codesign --force --deep --sign - "$root" >/dev/null 2>&1 || true
}

make_bundle vst3 "$BUNDLE/$NAME.vst3" "$NAME"
make_bundle clap "$BUNDLE/$NAME.clap" "$NAME"

APP="$BUNDLE/$NAME.app"
mkdir -p "$APP/Contents/MacOS"
cp "$STANDALONE" "$APP/Contents/MacOS/$NAME"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist>
  <dict>
    <key>CFBundleExecutable</key><string>$NAME</string>
    <key>CFBundleIdentifier</key><string>com.openkontakt.standalone</string>
    <key>CFBundleName</key><string>$NAME</string>
    <key>CFBundleDisplayName</key><string>$NAME</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleSignature</key><string>????</string>
    <key>CFBundleShortVersionString</key><string>0.1.0</string>
    <key>CFBundleVersion</key><string>0.1.0</string>
    <key>NSHighResolutionCapable</key><true/>
  </dict>
</plist>
PLIST
mkdir -p "$BUNDLE/standalone"
cp "$STANDALONE" "$BUNDLE/standalone/openkontakt"
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || true

echo "Created:"
find "$BUNDLE" -maxdepth 2 -type d -print | sort
