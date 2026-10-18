#!/usr/bin/env bash
# Regenerate every committed raster of the Readshot brand icon from the
# two SVG sources, so no surface ever drifts from the canonical art:
#
#   packaging/macos/icon.svg        canonical art, on Apple's 824/1024 grid
#   packaging/macos/icon-small.svg  hand-tuned variant for 32px and below
#
# Outputs (all committed):
#   crates/readshot-app/assets/readshot-tile-128.png  Welcome brand tile and
#                                                     the Windows/Linux window icon
#   crates/readshot-app/assets/tray-color-32.png      Windows/Linux tray icon
#   packaging/linux/icons/hicolor/*/apps/*            Linux hicolor icon theme
#   packaging/windows/readshot.ico + .rc + .res       Windows .exe icon
#   site/favicon.svg, favicon.ico, apple-touch-icon.png, icon-192.png,
#   icon-512.png, og-image.png                        website
#
# The macOS AppIcon.icns is not committed: packaging/macos/render-app-icon.sh
# builds it on every bundle build.
#
# Needs rsvg-convert (brew install librsvg) and ImageMagick 7 (brew install
# imagemagick); packaging/windows/render-icon.sh also needs llvm-rc (brew
# install llvm) or a MinGW windres.
#
# Usage: packaging/render-icons.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICON="${REPO_ROOT}/packaging/macos/icon.svg"
SMALL="${REPO_ROOT}/packaging/macos/icon-small.svg"

for tool in rsvg-convert magick; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "error: ${tool} not found (brew install librsvg imagemagick)" >&2
    exit 1
  fi
done

# Portable across BSD (macOS) and GNU (Linux) mktemp: an explicit
# template with X's, not BSD's `-t prefix`.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/readshot-icons.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT

# Both sources sit on Apple's 824/1024 grid, so every size of the macOS
# icns and the Linux hicolor theme has the same footprint. Cropping the
# viewBox to the 824px tile gives the bare tile, for surfaces that do not
# use that margin (Windows, the web, in-app images, the off-macOS tray).
crop_to_tile() { # src out
  sed 's|viewBox="0 0 1024 1024"|viewBox="100 100 824 824"|' "$1" > "$2"
  grep -q 'viewBox="100 100 824 824"' "$2" || {
    echo "error: could not crop $1 to its tile" >&2
    exit 1
  }
}
TILE="${WORK}/tile.svg"
SMALL_TILE="${WORK}/small-tile.svg"
crop_to_tile "${ICON}" "${TILE}"
crop_to_tile "${SMALL}" "${SMALL_TILE}"
# The tile with square corners, for apple-touch-icon: iOS applies its own
# corner mask and fills transparent corners with black.
SQUARE="${WORK}/square.svg"
sed 's|<rect id="tile" \(.*\) rx="224"|<rect id="tile" \1 rx="0"|' "${TILE}" > "${SQUARE}"
grep -q 'id="tile" .* rx="0"' "${SQUARE}" || {
  echo "error: could not square the tile in ${ICON}" >&2
  exit 1
}
export READSHOT_ICON_TILE="${TILE}"
export READSHOT_ICON_SMALL_TILE="${SMALL_TILE}"

png() { # svg size out
  mkdir -p "$(dirname "$3")"
  rsvg-convert -w "$2" -h "$2" "$1" -o "$3"
}

# Pick the source for a pixel size: the hand-tuned art at 32px and below.
source_for() { # size small-source large-source
  if (( $1 <= 32 )); then echo "$2"; else echo "$3"; fi
}

echo "-> in-app assets"
APP_ASSETS="${REPO_ROOT}/crates/readshot-app/assets"
png "${TILE}" 128 "${APP_ASSETS}/readshot-tile-128.png"
png "${SMALL_TILE}" 32 "${APP_ASSETS}/tray-color-32.png"

echo "-> Linux hicolor icons"
HICOLOR="${REPO_ROOT}/packaging/linux/icons/hicolor"
rm -rf "${HICOLOR}"
for size in 16 24 32 48 64 128 256 512; do
  png "$(source_for "${size}" "${SMALL}" "${ICON}")" "${size}" \
    "${HICOLOR}/${size}x${size}/apps/np.com.pawanpaudel.readshot.png"
done
mkdir -p "${HICOLOR}/scalable/apps"
cp "${ICON}" "${HICOLOR}/scalable/apps/np.com.pawanpaudel.readshot.svg"
# The AppImage root icon and the 512px rendition the bundle scripts refresh.
png "${ICON}" 512 "${REPO_ROOT}/packaging/linux/readshot.png"

echo "-> Windows icon"
"${REPO_ROOT}/packaging/windows/render-icon.sh"

echo "-> website"
SITE="${REPO_ROOT}/site"
cp "${TILE}" "${SITE}/favicon.svg"
for size in 16 32 48; do
  png "$(source_for "${size}" "${SMALL_TILE}" "${TILE}")" "${size}" "${WORK}/fav-${size}.png"
done
magick "${WORK}/fav-16.png" "${WORK}/fav-32.png" "${WORK}/fav-48.png" "${SITE}/favicon.ico"
png "${SQUARE}" 180 "${SITE}/apple-touch-icon.png"
png "${TILE}" 192 "${SITE}/icon-192.png"
png "${TILE}" 512 "${SITE}/icon-512.png"

# Open Graph / Twitter card: 1200x630, the tile beside the wordmark.
png "${TILE}" 280 "${WORK}/og-tile.png"
cat > "${WORK}/og.svg" <<'SVG'
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="1200" height="630" viewBox="0 0 1200 630">
  <defs>
    <radialGradient id="glowA" cx="18%" cy="0%" r="70%">
      <stop offset="0%" stop-color="#21B6A8" stop-opacity="0.26"/>
      <stop offset="100%" stop-color="#21B6A8" stop-opacity="0"/>
    </radialGradient>
    <radialGradient id="glowB" cx="88%" cy="100%" r="60%">
      <stop offset="0%" stop-color="#5C6AF2" stop-opacity="0.2"/>
      <stop offset="100%" stop-color="#5C6AF2" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="1200" height="630" fill="#101316"/>
  <rect width="1200" height="630" fill="url(#glowA)"/>
  <rect width="1200" height="630" fill="url(#glowB)"/>
  <image x="110" y="175" width="280" height="280" xlink:href="og-tile.png"/>
  <text x="450" y="300" font-family="Helvetica Neue, Helvetica, Arial, sans-serif" font-size="104" font-weight="700" fill="#F3F7F8">Readshot</text>
  <text x="454" y="372" font-family="Helvetica Neue, Helvetica, Arial, sans-serif" font-size="38" fill="#A9B3BD">Screenshots, annotation and offline OCR</text>
  <text x="454" y="424" font-family="Helvetica Neue, Helvetica, Arial, sans-serif" font-size="30" fill="#5BE0D0">readshot.pawanpaudel.com.np</text>
</svg>
SVG
(cd "${WORK}" && rsvg-convert -w 1200 -h 630 og.svg -o "${SITE}/og-image.png")

echo "✓ icons rendered"
