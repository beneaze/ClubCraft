#!/bin/sh
# Build and package ClubCraft for Linux into dist/ClubCraft (binary, desktop entry, icon, and the
# ONNX separation models if they exist in ./models).
#   tools/package-linux.sh              # build + package
#   tools/package-linux.sh --no-build   # package an existing release build
# Building needs the ALSA headers and cmake (for libopus): `sudo apt install libasound2-dev pkg-config cmake` (Debian/Ubuntu).
set -e
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env" 2>/dev/null || true
[ "$1" = "--no-build" ] || cargo build --release -j 4 -p clubcraft

DIST=dist/ClubCraft
rm -rf "$DIST"
mkdir -p "$DIST/models"
cp target/release/ClubCraft "$DIST/ClubCraft"
# ONNX Runtime links statically on Linux; copy any shared libraries the ort crate placed next to the build anyway.
for so in target/release/*.so*; do [ -f "$so" ] && cp "$so" "$DIST/"; done
cp crates/app/assets/icon-256.png "$DIST/clubcraft.png"
cp README.md "$DIST/"

# Models: ONNX files + manifests (CoreML packages are macOS-only).
if [ -d models ]; then
  for f in models/*.json models/*.onnx models/*.onnx.data models/cocola_w.bin; do
    [ -f "$f" ] || continue
    case "$(basename "$f")" in *_ref_* | htdemucs_6s*) continue ;; esac
    cp "$f" "$DIST/models/"
  done
fi
cp models/README.md "$DIST/models/README.md" 2>/dev/null || true

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

n=$(find "$DIST/models" -name '*.onnx' | wc -l | tr -d ' ')
echo "Packaged $DIST ($n ONNX models). Without models, ClubCraft runs but cannot separate stems; see models/README.md."
