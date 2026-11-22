#!/bin/sh
# Build and package ClubCraft for Linux into dist/ClubCraft (binary, desktop entry, icon). The
# separation models ship apart (tools/package-models.sh) and the app downloads them on request.
#   tools/package-linux.sh                 # build + package
#   tools/package-linux.sh --no-build      # package an existing release build
#   tools/package-linux.sh --with-models   # also copy the ONNX models from ./models (offline package)
# Building needs the ALSA headers and cmake (for libopus): `sudo apt install libasound2-dev pkg-config cmake` (Debian/Ubuntu).
set -e
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env" 2>/dev/null || true
BUILD=1
MODELS=0
for a in "$@"; do
  case "$a" in
    --no-build) BUILD=0 ;;
    --with-models) MODELS=1 ;;
    *) echo "unknown option: $a" >&2; exit 2 ;;
  esac
done
[ $BUILD = 0 ] || cargo build --release -j 4 -p clubcraft

DIST=dist/ClubCraft
rm -rf "$DIST"
mkdir -p "$DIST"
cp target/release/ClubCraft "$DIST/ClubCraft"
# ONNX Runtime links statically on Linux; copy any shared libraries the ort crate placed next to the build anyway.
for so in target/release/*.so*; do [ -f "$so" ] && cp "$so" "$DIST/"; done
cp crates/app/assets/icon-256.png "$DIST/clubcraft.png"
cp README.md "$DIST/"

# Models (offline package only): ONNX files + manifests (CoreML packages are macOS-only).
if [ $MODELS = 1 ]; then
  mkdir -p "$DIST/models"
  for f in models/*.json models/*.onnx models/*.onnx.data models/cocola_w.bin; do
    [ -f "$f" ] || continue
    case "$(basename "$f")" in *_ref_* | htdemucs_6s*) continue ;; esac
    cp "$f" "$DIST/models/"
  done
  cp models/README.md "$DIST/models/README.md" 2>/dev/null || true
fi

# Adds ClubCraft to the application menu of the user who runs it, pointing at this folder.
cat > "$DIST/install-desktop-entry.sh" <<'SH'
#!/bin/sh
# Add ClubCraft to your application menu (run again after moving this folder).
set -e
DIR=$(cd "$(dirname "$0")" && pwd)
APPS=${XDG_DATA_HOME:-$HOME/.local/share}/applications
mkdir -p "$APPS"
cat > "$APPS/clubcraft.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=ClubCraft
Comment=DJ studio and live decks with stem separation
Exec="$DIR/ClubCraft"
Icon=$DIR/clubcraft.png
Categories=AudioVideo;Audio;Music;
StartupWMClass=clubcraft
Terminal=false
EOF
echo "Added $APPS/clubcraft.desktop"
SH
chmod +x "$DIST/ClubCraft" "$DIST/install-desktop-entry.sh"

if [ $MODELS = 1 ]; then
  n=$(find "$DIST/models" -name '*.onnx' | wc -l | tr -d ' ')
  echo "Packaged $DIST ($n ONNX models)."
else
  echo "Packaged $DIST without models (Settings › Analysis & stems › Download models)."
fi
