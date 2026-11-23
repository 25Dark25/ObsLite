#!/bin/sh
# Builds the website into OUT_DIR, filling in the latest release's version, file sizes and
# SHA-256 hashes so the download buttons always point at the current files. Needs `gh`.
set -eu
out=${1:?usage: site/build.sh OUT_DIR}
src=$(dirname "$0")
repo=25Dark25/ObsLite

release=$(gh release view -R "$repo" --json tagName,publishedAt,assets \
  -q '[.tagName, .publishedAt[:10]] + [.assets[] | .name + " " + (.size|tostring) + " " + (.digest // "" | ltrimstr("sha256:"))] | .[]')
version=$(echo "$release" | sed -n 1p | sed 's/^v//')
date=$(echo "$release" | sed -n 2p)

# Prints "size-in-MB sha256" of the release file ending in $1, or fails.
asset() {
  line=$(echo "$release" | grep "^obs-lite-$version-$1 ") || { echo "missing release file: $1" >&2; exit 1; }
  echo "$line" | awk '{ if ($3 == "") exit 1; printf "%.1f %s\n", $2 / 1048576, $3 }' \
    || { echo "no SHA-256 for $1" >&2; exit 1; }
}
msi=$(asset windows-x64.msi); msi_mb=${msi% *} msi_sha=${msi#* }
zip=$(asset portable-windows-x64.zip); zip_mb=${zip% *} zip_sha=${zip#* }
flatpak=$(asset linux-x86_64.flatpak); flatpak_mb=${flatpak% *} flatpak_sha=${flatpak#* }

mkdir -p "$out"
cp "$src/index.html" "$src/favicon.svg" "$src/apple-touch-icon.png" "$src/og.png" \
  "$src/robots.txt" "$src/sitemap.xml" "$src/llms.txt" "$out/"
for f in "$out/index.html" "$out/sitemap.xml" "$out/llms.txt"; do
  sed -i \
    -e "s/{{VERSION}}/$version/g" -e "s/{{DATE}}/$date/g" -e "s/{{BUILD_DATE}}/$(date -u +%F)/g" \
    -e "s/{{MSI_SIZE_NUM}}/$msi_mb/g" -e "s/{{MSI_SIZE}}/$msi_mb MB/g" -e "s/{{MSI_SHA}}/$msi_sha/g" \
    -e "s/{{ZIP_SIZE}}/$zip_mb MB/g" -e "s/{{ZIP_SHA}}/$zip_sha/g" \
    -e "s/{{FLATPAK_SIZE}}/$flatpak_mb MB/g" -e "s/{{FLATPAK_SHA}}/$flatpak_sha/g" \
    "$f"
done
# What a visit downloads: the page (styles and script inline) and its icon.
kb=$(cat "$out/index.html" "$out/favicon.svg" | wc -c | awk '{ printf "%d", ($1 + 1023) / 1024 }')
sed -i "s/{{PAGE_KB}}/$kb/g" "$out/index.html"

if grep -n '{{' "$out/index.html" "$out/sitemap.xml" "$out/llms.txt"; then
  echo "unfilled placeholders above" >&2
  exit 1
fi
echo "Built $out for $version ($date): page $kb KB"
