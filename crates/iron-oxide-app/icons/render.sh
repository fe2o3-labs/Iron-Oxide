#!/usr/bin/env sh
# Regenerates the app icons in ../public/ from the SVG sources in this folder.
# Needs rsvg-convert (librsvg) and ImageMagick 7 (`magick`).
set -eu
cd "$(dirname "$0")"
out=../public
mkdir -p "$out/icons"
rsvg-convert -w 192 -h 192 icon.svg -o "$out/icons/icon-192.png"
rsvg-convert -w 512 -h 512 icon.svg -o "$out/icons/icon-512.png"
rsvg-convert -w 512 -h 512 icon-maskable.svg -o "$out/icons/icon-maskable-512.png"
rsvg-convert -w 180 -h 180 apple-touch-icon.svg -o "$out/icons/apple-touch-icon.png"
cp icon.svg "$out/icons/icon.svg"
cp favicon.svg "$out/icons/favicon.svg"
tmp=$(mktemp -d)
for s in 16 32 48; do rsvg-convert -w "$s" -h "$s" favicon.svg -o "$tmp/favicon-$s.png"; done
magick "$tmp/favicon-16.png" "$tmp/favicon-32.png" "$tmp/favicon-48.png" "$out/favicon.ico"
rm -r "$tmp"
