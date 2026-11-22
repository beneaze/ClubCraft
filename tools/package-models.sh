#!/bin/sh
# Build the model packs that ship apart from the app (they change far less often than the app), and
# write their URLs, sizes and SHA-256 checksums into crates/app/model-packs.json, which the app
# embeds to download and verify them.
#   tools/package-models.sh 1   # dist/models/ClubCraft-models-v1-{coreml,onnx}.tar for release tag models-v1
# The files come from ./models (MODELS_SRC=<folder> reads another folder, e.g. a main checkout's).
# Upload both files to a GitHub release named `models-v<N>` and commit model-packs.json with them.
# A new pack version is only needed when the model files change.
set -eu
cd "$(dirname "$0")/.."
VERSION=${1:?usage: tools/package-models.sh <pack version, e.g. 1>}
TAG="models-v$VERSION"
REPO=${CLUBCRAFT_REPO:-beneaze/ClubCraft}
SRC=${MODELS_SRC:-models}
OUT=dist/models
mkdir -p "$OUT"

# Same selection as the earlier bundled packages: htdemucs (Fast, live), htdemucs_ft (High),
# Mel-Band RoFormer (Best vocals), All-In-One (deep structure), COCOLA (mashup re-ranking).
# htdemucs_6s and the *_ref_* test tensors are left out.
MANIFESTS="htdemucs.json htdemucs_ft.json melroformer.json allin1.json cocola.json"
COMMON="allin1.onnx cocola.onnx cocola_w.bin"
VOCALS_COREML=melroformer_fp16.mlmodelc
[ -d "$SRC/$VOCALS_COREML" ] || VOCALS_COREML=melroformer.mlmodelc
COREML="$MANIFESTS $COMMON htdemucs.mlmodelc htdemucs_ft_0.mlmodelc htdemucs_ft_1.mlmodelc htdemucs_ft_2.mlmodelc htdemucs_ft_3.mlmodelc $VOCALS_COREML"
ONNX="$MANIFESTS $COMMON htdemucs.onnx htdemucs_ft_0.onnx htdemucs_ft_1.onnx htdemucs_ft_2.onnx htdemucs_ft_3.onnx melroformer.onnx"
for f in "$SRC"/*.onnx.data; do if [ -f "$f" ]; then ONNX="$ONNX $(basename "$f")"; fi; done

sha256() {
  if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d' ' -f1; else sha256sum "$1" | cut -d' ' -f1; fi
}

entries=""
for backend in coreml onnx; do
  if [ $backend = coreml ]; then files=$COREML; else files=$ONNX; fi
  for f in $files; do
    [ -e "$SRC/$f" ] || { echo "missing $SRC/$f (see models/README.md)" >&2; exit 1; }
  done
  name="ClubCraft-models-v$VERSION-$backend.tar"
  # ustar: plain headers both tar programs write and the app reads; no macOS metadata files.
  # shellcheck disable=SC2086
  COPYFILE_DISABLE=1 tar --format=ustar -cf "$OUT/$name" -C "$SRC" $files
  size=$(wc -c < "$OUT/$name" | tr -d ' ')
  sum=$(sha256 "$OUT/$name")
  echo "$name  $size bytes  $sum"
  [ -z "$entries" ] || entries="$entries,
"
  entries="$entries    {
      \"id\": \"$TAG-$backend\",
      \"backend\": \"$backend\",
      \"url\": \"https://github.com/$REPO/releases/download/$TAG/$name\",
      \"size\": $size,
      \"sha256\": \"$sum\"
    }"
done

cat > crates/app/model-packs.json <<JSON
{
  "version": $VERSION,
  "packs": [
$entries
  ]
}
JSON
echo "wrote crates/app/model-packs.json; upload $OUT/*.tar to the $TAG release"
