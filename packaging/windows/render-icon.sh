#!/usr/bin/env bash
# Build the Windows .exe icon from the canonical SVG art:
#
#   packaging/windows/readshot.ico  16, 20, 24, 32, 40, 48, 64 and 256 px
#   packaging/windows/readshot.res  readshot.rc compiled; build.rs links it
#                                   into readshot.exe on MSVC targets
#
# 16px comes from packaging/macos/icon-16.svg (tile and lens only), 20 to
# 32px from packaging/macos/icon-small.svg (hand-tuned for a few pixels),
# larger sizes from packaging/macos/icon.svg. Each is cropped to its tile
# (Windows icons fill their canvas; Apple's grid margin is macOS-only).
#
# Needs rsvg-convert, ImageMagick 7 and python3, plus a resource compiler: llvm-rc
# (brew install llvm; also searched in Homebrew's keg-only llvm@N) or a
# MinGW windres (brew install mingw-w64). Both produce the same .res.
#
# Usage: packaging/windows/render-icon.sh   (packaging/render-icons.sh runs it)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT_DIR="${REPO_ROOT}/packaging/windows"
ICON="${REPO_ROOT}/packaging/macos/icon.svg"
SMALL="${REPO_ROOT}/packaging/macos/icon-small.svg"
TINY="${REPO_ROOT}/packaging/macos/icon-16.svg"

# Portable across BSD (macOS) and GNU (Linux) mktemp.
WORK="$(mktemp -d "${TMPDIR:-/tmp}/readshot-ico.XXXXXX")"
trap 'rm -rf "${WORK}"' EXIT

# Every source sits on Apple's grid; Windows icons fill their canvas, so
# crop each to its tile (packaging/render-icons.sh passes its crops in).
TILE="${READSHOT_ICON_TILE:-}"
if [[ -z "${TILE}" ]]; then
  TILE="${WORK}/tile.svg"
  sed 's|viewBox="0 0 1024 1024"|viewBox="100 100 824 824"|' "${ICON}" > "${TILE}"
fi
SMALL_TILE="${READSHOT_ICON_SMALL_TILE:-}"
if [[ -z "${SMALL_TILE}" ]]; then
  SMALL_TILE="${WORK}/small-tile.svg"
  sed 's|viewBox="0 0 1024 1024"|viewBox="100 100 824 824"|' "${SMALL}" > "${SMALL_TILE}"
fi
TINY_TILE="${READSHOT_ICON_TINY_TILE:-}"
if [[ -z "${TINY_TILE}" ]]; then
  TINY_TILE="${WORK}/tiny-tile.svg"
  sed 's|viewBox="0 0 1024 1024"|viewBox="100 100 824 824"|' "${TINY}" > "${TINY_TILE}"
fi

frames=()
for size in 16 20 24 32 40 48 64 256; do
  src="${TILE}"
  if (( size <= 32 )); then src="${SMALL_TILE}"; fi
  if (( size <= 16 )); then src="${TINY_TILE}"; fi
  rsvg-convert -w "${size}" -h "${size}" "${src}" -o "${WORK}/${size}.png"
  frames+=("${WORK}/${size}.png")
done
# One frame per rendered size, as rendered (no resampling). The small
# frames are 32-bit BMP, which every Windows icon reader accepts; the 256px
# frame is stored as PNG, as Windows expects (a BMP one is ~270 KB).
magick "${frames[@]:0:7}" "${WORK}/small.ico"
python3 - "${WORK}/small.ico" "${WORK}/256.png" "${OUT_DIR}/readshot.ico" <<'PY'
import struct, sys
small, png256, out = sys.argv[1:4]
data = open(small, "rb").read()
_, _, count = struct.unpack("<HHH", data[:6])
entries = [struct.unpack("<BBBBHHII", data[6 + 16 * i : 22 + 16 * i]) for i in range(count)]
frames = [(e[:6], data[e[7] : e[7] + e[6]]) for e in entries]
frames.append(((0, 0, 0, 0, 1, 32), open(png256, "rb").read()))
offset = 6 + 16 * len(frames)
header = struct.pack("<HHH", 0, 1, len(frames))
body = b""
for fields, blob in frames:
    header += struct.pack("<BBBBHHII", *fields, len(blob), offset + len(body))
    body += blob
open(out, "wb").write(header + body)
PY

find_rc() {
  if command -v llvm-rc >/dev/null 2>&1; then command -v llvm-rc; return; fi
  local brew_prefix
  brew_prefix="$(brew --prefix 2>/dev/null || true)"
  if [[ -n "${brew_prefix}" ]]; then
    local candidate
    for candidate in "${brew_prefix}"/opt/llvm/bin/llvm-rc "${brew_prefix}"/opt/llvm@*/bin/llvm-rc; do
      if [[ -x "${candidate}" ]]; then echo "${candidate}"; return; fi
    done
  fi
}

RC="$(find_rc)"
cd "${OUT_DIR}"
if [[ -n "${RC}" ]]; then
  "${RC}" -no-preprocess -fo readshot.res readshot.rc
elif command -v x86_64-w64-mingw32-windres >/dev/null 2>&1; then
  x86_64-w64-mingw32-windres --preprocessor=cat -O res -i readshot.rc -o readshot.res
else
  echo "error: no resource compiler (brew install llvm, or brew install mingw-w64)" >&2
  exit 1
fi
echo "✓ ${OUT_DIR}/readshot.ico and readshot.res"
