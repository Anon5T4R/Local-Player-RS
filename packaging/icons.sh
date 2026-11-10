#!/usr/bin/env bash
# Regenerates the committed icon tree (assets/app-icon/hicolor) from the PNG
# master at icons/icon.png.
#
# O local-player-rs só tem o PNG 128x128 (não há master 1024): a árvore cobre
# apenas os tamanhos até 128 — e package.sh usa a 128x128 como ícone do AppDir.
#
# Needs: ImageMagick (`magick`). The outputs are committed, so builds and
# packaging never need these tools.
#
#   packaging/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/icons/icon.png"
DIR="$ROOT/assets/app-icon/hicolor"
ID="local-player-rs"

command -v magick >/dev/null || { echo "error: magick not found (imagemagick)" >&2; exit 1; }
[[ -f "$SRC" ]] || { echo "error: master not found: $SRC" >&2; exit 1; }

read -r WIDTH _ < <(magick identify -format '%w %h\n' "$SRC") || WIDTH=0
if [[ "$WIDTH" -lt 128 ]]; then
  echo "error: master $SRC is ${WIDTH}px wide; expected at least 128" >&2
  exit 1
fi

for s in 16 24 32 48 64 128; do
  install -d "$DIR/${s}x$s/apps"
  magick "$SRC" -resize ${s}x${s} "$DIR/${s}x$s/apps/$ID.png"
  echo "wrote $DIR/${s}x$s/apps/$ID.png"
done
