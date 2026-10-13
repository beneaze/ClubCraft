#!/bin/sh
# Build ClubCraft.app (release) with the CoreML stem models bundled.
# usage: tools/bundle-macos.sh [--with-onnx]   → target/ClubCraft.app
set -e
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env" 2>/dev/null || true
cargo build --release -p clubcraft
APP=target/ClubCraft.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/models"
cp target/release/clubcraft "$APP/Contents/MacOS/ClubCraft"
for m in models/*.json; do cp "$m" "$APP/Contents/Resources/models/"; done
for m in models/*.mlmodelc; do cp -R "$m" "$APP/Contents/Resources/models/"; done
if [ "$1" = "--with-onnx" ]; then cp models/*.onnx "$APP/Contents/Resources/models/" 2>/dev/null || true; fi
rm -f "$APP"/Contents/Resources/models/*_ref_*
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>ClubCraft</string>
  <key>CFBundleDisplayName</key><string>ClubCraft</string>
  <key>CFBundleIdentifier</key><string>org.clubcraft.app</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleExecutable</key><string>ClubCraft</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>ClubCraft can record from an audio input.</string>
</dict></plist>
PLIST
codesign --force --deep -s - "$APP" >/dev/null 2>&1 || true
du -sh "$APP"
echo "built $APP"
