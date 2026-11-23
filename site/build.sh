#!/bin/sh
# Builds the website into OUT_DIR: one page per language, with the latest release's version, file
# sizes and SHA-256 hashes filled in so the download buttons always point at the current files.
# Needs `gh` and `perl` (with JSON::PP, part of Perl since 5.14).
set -eu
out=${1:?usage: site/build.sh OUT_DIR}
src=$(dirname "$0")
repo=25Dark25/ObsLite

release=$(gh release view -R "$repo" --json tagName,publishedAt,assets \
  -q '[.tagName, .publishedAt[:10]] + [.assets[] | .name + " " + (.size|tostring) + " " + (.digest // "" | ltrimstr("sha256:"))] | .[]')
VERSION=$(echo "$release" | sed -n 1p | sed 's/^v//')
DATE=$(echo "$release" | sed -n 2p)

# Prints "size-in-MB sha256" of the release file ending in $1, or fails.
asset() {
  line=$(echo "$release" | grep "^obs-lite-$VERSION-$1 ") || { echo "missing release file: $1" >&2; exit 1; }
  echo "$line" | awk '{ if ($3 == "") exit 1; printf "%.1f %s\n", $2 / 1048576, $3 }' \
    || { echo "no SHA-256 for $1" >&2; exit 1; }
}
msi=$(asset windows-x64.msi); MSI_MB=${msi% *} MSI_SHA=${msi#* }
zip=$(asset portable-windows-x64.zip); ZIP_MB=${zip% *} ZIP_SHA=${zip#* }
flatpak=$(asset linux-x86_64.flatpak); FLATPAK_MB=${flatpak% *} FLATPAK_SHA=${flatpak#* }
export VERSION DATE MSI_MB MSI_SHA ZIP_MB ZIP_SHA FLATPAK_MB FLATPAK_SHA

mkdir -p "$out"
cp "$src/favicon.svg" "$src/apple-touch-icon.png" "$src/og.png" "$src/robots.txt" "$out/"
perl "$src/build.pl" "$out"
echo "Built $out for $VERSION ($DATE)"
