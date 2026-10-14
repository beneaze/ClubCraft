#!/bin/sh
# Build ClubCraft.app (release) with the CoreML stem models bundled.
# usage: tools/bundle-macos.sh [--with-onnx]   → target/ClubCraft.app
set -e
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env" 2>/dev/null || true
cargo build --release -j 4 -p clubcraft
APP=target/ClubCraft.app
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/models"
cp target/release/ClubCraft "$APP/Contents/MacOS/ClubCraft"
cp crates/app/assets/ClubCraft.icns "$APP/Contents/Resources/ClubCraft.icns"
# Models used by the app: htdemucs (fast/live), htdemucs_ft (high), melroformer (best vocals),
# allin1 (optional deep structure analysis, ONNX only). htdemucs_6s is not bundled.
for m in htdemucs htdemucs_ft melroformer allin1; do
  [ -f "models/$m.json" ] && cp "models/$m.json" "$APP/Contents/Resources/models/"
done
for m in models/htdemucs.mlmodelc models/htdemucs_ft_*.mlmodelc; do
  [ -d "$m" ] && cp -R "$m" "$APP/Contents/Resources/models/"
done
# the vocal model in half precision (1.4x faster, inaudible difference); full precision if missing
if [ -d models/melroformer_fp16.mlmodelc ]; then
  cp -R models/melroformer_fp16.mlmodelc "$APP/Contents/Resources/models/"
else
  cp -R models/melroformer.mlmodelc "$APP/Contents/Resources/models/"
fi
[ -f models/allin1.onnx ] && cp models/allin1.onnx "$APP/Contents/Resources/models/"
# COCOLA (mashup finder re-ranking, ONNX, 19 MB)
[ -f models/cocola.onnx ] && cp models/cocola.onnx models/cocola_w.bin models/cocola.json "$APP/Contents/Resources/models/"
if [ "$1" = "--with-onnx" ]; then cp models/htdemucs*.onnx models/melroformer.onnx "$APP/Contents/Resources/models/" 2>/dev/null || true; fi
rm -f "$APP"/Contents/Resources/models/*_ref_*
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>ClubCraft</string>
  <key>CFBundleDisplayName</key><string>ClubCraft</string>
  <key>CFBundleIdentifier</key><string>org.clubcraft.app</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>ClubCraft</string>
  <key>CFBundleIconFile</key><string>ClubCraft</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>ClubCraft can record from an audio input.</string>
</dict></plist>
PLIST
codesign --force --deep -s - "$APP" >/dev/null 2>&1 || true
du -sh "$APP"
echo "built $APP"
