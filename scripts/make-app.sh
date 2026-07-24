#!/usr/bin/env bash
# Build a double-clickable macOS .app bundle for the SNES emulator.
#
#   ./scripts/make-app.sh            # release-build, then bundle -> dist/Prisme.app
#   SKIP_BUILD=1 ./scripts/make-app.sh   # bundle the existing target/release binary (no cargo)
#
# Double-click dist/Prisme.app in Finder: it opens the native game picker
# (and menu bar) with no terminal. Since it is built locally it is not
# quarantined, so Gatekeeper lets it run directly.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

APP_NAME="Prisme"
BIN_SRC="target/release/prisme"
APP="dist/${APP_NAME}.app"

if [ "${SKIP_BUILD:-0}" != "1" ]; then
  # cargo is not on the login PATH on this machine; add the toolchain dir.
  export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$HOME/.cargo/bin:$PATH"
  echo "Building release binary..."
  cargo build --release -p prisme
fi

if [ ! -x "$BIN_SRC" ]; then
  echo "error: $BIN_SRC not found. Run without SKIP_BUILD to compile it first." >&2
  exit 1
fi

echo "Assembling $APP ..."
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN_SRC" "$APP/Contents/MacOS/${APP_NAME}"
chmod +x "$APP/Contents/MacOS/${APP_NAME}"

# App icon (optional): packaging/AppIcon.icns -> Resources, referenced in plist.
ICON_KEY=""
if [ -f packaging/AppIcon.icns ]; then
  cp packaging/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
  ICON_KEY="    <key>CFBundleIconFile</key>       <string>AppIcon</string>"
fi

# Version from the frontend crate; never let a missing field abort the script.
VERSION="0.1.0"
if v="$(grep -m1 '^version' frontend/Cargo.toml 2>/dev/null | sed -E 's/.*"([^"]+)".*/\1/')"; then
  [ -n "$v" ] && VERSION="$v"
fi

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>            <string>${APP_NAME}</string>
    <key>CFBundleDisplayName</key>     <string>Prisme - SuperNes</string>
    <key>CFBundleExecutable</key>      <string>${APP_NAME}</string>
${ICON_KEY}
    <key>CFBundleIdentifier</key>      <string>com.stephanecot.prisme</string>
    <key>CFBundlePackageType</key>     <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key> <string>6.0</string>
    <key>CFBundleShortVersionString</key>    <string>${VERSION}</string>
    <key>CFBundleVersion</key>         <string>1</string>
    <key>LSMinimumSystemVersion</key>  <string>10.13</string>
    <key>NSHighResolutionCapable</key> <true/>
    <key>NSPrincipalClass</key>        <string>NSApplication</string>
</dict>
</plist>
PLIST

# Mark it as a bundle for Finder.
touch "$APP"

echo "Done: $APP"

# INSTALL=1 also updates the copy in /Applications (matched by bundle id, so a
# stale copy there would otherwise be what double-click launches).
if [ "${INSTALL:-0}" = "1" ]; then
  osascript -e "quit app \"${APP_NAME}\"" 2>/dev/null || true
  sleep 1
  ditto "$APP" "/Applications/${APP_NAME}.app"
  echo "Installed: /Applications/${APP_NAME}.app"
fi

echo "Double-click it in Finder, or: open \"$APP\""
