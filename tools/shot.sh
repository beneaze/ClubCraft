#!/bin/sh
# Ask a running dev instance (started with CLUBCRAFT_DEVCTL=$CTL) for a screenshot.
# usage: tools/shot.sh <ctl file> <out.png> [extra devctl lines...]
CTL="$1"; OUT="$2"; shift 2
rm -f "$OUT"
{ for l in "$@"; do echo "$l"; done; echo "shot $OUT"; } > "$CTL"
for i in $(seq 1 40); do [ -f "$OUT" ] && exit 0; sleep 0.25; done
echo "no screenshot" >&2; exit 1
