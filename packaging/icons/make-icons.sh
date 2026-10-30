#!/bin/sh
# Renders one of packaging/icons/variants into every icon the packagers install.
#
# The variant to use is the argument, and the default is the one squint ships:
#   ./packaging/icons/make-icons.sh underscore
set -e
root=$(cd "$(dirname "$0")/../.." && pwd)
out="$root/packaging/icons"
variant=${1:-caret}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

swift "$out/render.swift" "$out/variants/$variant.svg" "$tmp" 16 32 48 64 128 256 512 1024

cp "$tmp/1024.png" "$out/squint.png"
cp "$tmp/256.png" "$out/squint-256.png"

set=$tmp/squint.iconset
mkdir "$set"
for s in 16 32 128 256 512; do
  cp "$tmp/$s.png" "$set/icon_${s}x${s}.png"
  cp "$tmp/$((s * 2)).png" "$set/icon_${s}x${s}@2x.png"
done
iconutil -c icns "$set" -o "$out/squint.icns"

python3 -I - "$out/squint.ico" "$tmp" <<'PY'
import struct, sys
ico, tmp = sys.argv[1], sys.argv[2]
sizes = [16, 32, 48, 64, 128, 256]
pngs = [open(f"{tmp}/{s}.png", "rb").read() for s in sizes]
offset = 6 + 16 * len(sizes)
head = struct.pack("<HHH", 0, 1, len(sizes))
for s, png in zip(sizes, pngs):
    # 0 in the byte fields means 256; the ICO header has no room for more.
    head += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(png), offset)
    offset += len(png)
open(ico, "wb").write(head + b"".join(pngs))
PY
