#!/usr/bin/env bash
# Render Readshot's canonical SVG app icon into platform assets.
#
# Usage:
#   packaging/macos/render-app-icon.sh path/to/AppIcon.icns
#   packaging/macos/render-app-icon.sh path/to/AppIcon.icns --png-512 path/to/readshot.png

set -euo pipefail

if [[ "$#" -ne 1 && "$#" -ne 3 ]]; then
  echo "usage: $0 <AppIcon.icns> [--png-512 <readshot.png>]" >&2
  exit 64
fi

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ICON_SRC="${REPO_ROOT}/packaging/macos/icon.svg"
ICON_OUT="$1"
PNG_512_OUT=""

if [[ "$#" -eq 3 ]]; then
  if [[ "$2" != "--png-512" ]]; then
    echo "usage: $0 <AppIcon.icns> [--png-512 <readshot.png>]" >&2
    exit 64
  fi
  PNG_512_OUT="$3"
fi

if [[ ! -f "${ICON_SRC}" ]]; then
  echo "error: ${ICON_SRC} missing" >&2
  exit 1
fi
if ! command -v rsvg-convert >/dev/null 2>&1; then
  echo "error: rsvg-convert not found (brew install librsvg)" >&2
  exit 1
fi
if ! command -v iconutil >/dev/null 2>&1 && ! command -v xcrun >/dev/null 2>&1; then
  echo "error: iconutil or xcrun actool required to build AppIcon.icns" >&2
  exit 1
fi

mkdir -p "$(dirname "${ICON_OUT}")"
ICON_BUILD_DIR="$(mktemp -d -t readshot-iconset)"
cleanup() {
  rm -rf "${ICON_BUILD_DIR}"
}
trap cleanup EXIT

ICONSET="${ICON_BUILD_DIR}/AppIcon.iconset"
mkdir -p "${ICONSET}"

echo "-> rasterising ${ICON_SRC} into AppIcon.iconset"
for entry in \
  "16 icon_16x16.png"        \
  "32 icon_16x16@2x.png"     \
  "32 icon_32x32.png"        \
  "64 icon_32x32@2x.png"     \
  "128 icon_128x128.png"     \
  "256 icon_128x128@2x.png"  \
  "256 icon_256x256.png"     \
  "512 icon_256x256@2x.png"  \
  "512 icon_512x512.png"     \
  "1024 icon_512x512@2x.png" \
; do
  size="${entry%% *}"
  name="${entry#* }"
  rsvg-convert -w "${size}" -h "${size}" "${ICON_SRC}" -o "${ICONSET}/${name}"
done

if ! iconutil -c icns "${ICONSET}" -o "${ICON_OUT}" 2>"${ICON_BUILD_DIR}/iconutil.log"; then
  if ! command -v xcrun >/dev/null 2>&1; then
    cat "${ICON_BUILD_DIR}/iconutil.log" >&2
    exit 1
  fi

  echo "-> iconutil rejected iconset; falling back to xcrun actool"
  ASSET_CATALOG="${ICON_BUILD_DIR}/Assets.xcassets"
  APP_ICON_SET="${ASSET_CATALOG}/AppIcon.appiconset"
  ACTOOL_OUT="${ICON_BUILD_DIR}/actool-out"
  mkdir -p "${APP_ICON_SET}" "${ACTOOL_OUT}"
  cp "${ICONSET}"/*.png "${APP_ICON_SET}/"
  cat > "${APP_ICON_SET}/Contents.json" <<'JSON'
{
  "images": [
    { "size": "16x16", "idiom": "mac", "filename": "icon_16x16.png", "scale": "1x" },
    { "size": "16x16", "idiom": "mac", "filename": "icon_16x16@2x.png", "scale": "2x" },
    { "size": "32x32", "idiom": "mac", "filename": "icon_32x32.png", "scale": "1x" },
    { "size": "32x32", "idiom": "mac", "filename": "icon_32x32@2x.png", "scale": "2x" },
    { "size": "128x128", "idiom": "mac", "filename": "icon_128x128.png", "scale": "1x" },
    { "size": "128x128", "idiom": "mac", "filename": "icon_128x128@2x.png", "scale": "2x" },
    { "size": "256x256", "idiom": "mac", "filename": "icon_256x256.png", "scale": "1x" },
    { "size": "256x256", "idiom": "mac", "filename": "icon_256x256@2x.png", "scale": "2x" },
    { "size": "512x512", "idiom": "mac", "filename": "icon_512x512.png", "scale": "1x" },
    { "size": "512x512", "idiom": "mac", "filename": "icon_512x512@2x.png", "scale": "2x" }
  ],
  "info": { "author": "xcode", "version": 1 }
}
JSON
  if ! xcrun actool \
    --compile "${ACTOOL_OUT}" \
    --platform macosx \
    --minimum-deployment-target 14.0 \
    --app-icon AppIcon \
    --output-partial-info-plist "${ICON_BUILD_DIR}/partial.plist" \
    "${ASSET_CATALOG}" \
    >"${ICON_BUILD_DIR}/actool.log" 2>&1; then
    cat "${ICON_BUILD_DIR}/iconutil.log" >&2
    cat "${ICON_BUILD_DIR}/actool.log" >&2
    exit 1
  fi
  cp "${ACTOOL_OUT}/AppIcon.icns" "${ICON_OUT}"
fi

if [[ -n "${PNG_512_OUT}" ]]; then
  mkdir -p "$(dirname "${PNG_512_OUT}")"
  rsvg-convert -w 512 -h 512 "${ICON_SRC}" -o "${PNG_512_OUT}"
fi
