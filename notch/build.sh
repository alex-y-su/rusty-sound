#!/usr/bin/env bash
# Builds RustyNotch.app into notch/build/ using swiftc (Command Line Tools is enough).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC_DIR="$HERE/Sources"
OUT_DIR="$HERE/build"
APP_NAME="RustyNotch"
APP="$OUT_DIR/$APP_NAME.app"
MIN_MACOS="12.0"
ARCH="$(uname -m)"

if ! command -v swiftc >/dev/null 2>&1; then
    echo "error: swiftc not found. Install Command Line Tools: xcode-select --install" >&2
    exit 1
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

swiftc \
    -O \
    -swift-version 5 \
    -target "$ARCH-apple-macosx$MIN_MACOS" \
    -module-name "$APP_NAME" \
    -o "$APP/Contents/MacOS/$APP_NAME" \
    "$SRC_DIR/main.swift" \
    "$SRC_DIR/RustySoundClient.swift" \
    "$SRC_DIR/StatusItemController.swift"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>RustyNotch</string>
    <key>CFBundleIdentifier</key>
    <string>com.rustysound.notch</string>
    <key>CFBundleName</key>
    <string>RustyNotch</string>
    <key>CFBundleDisplayName</key>
    <string>Rusty Notch</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>CFBundleVersion</key>
    <string>1</string>
    <key>LSMinimumSystemVersion</key>
    <string>12.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
PLIST

printf 'APPL????' > "$APP/Contents/PkgInfo"

# Ad-hoc signature so the bundle is consistently signed. Does not launch anything.
codesign --force --sign - "$APP" >/dev/null 2>&1 || echo "warning: ad-hoc codesign failed" >&2

echo "Built: $APP"
