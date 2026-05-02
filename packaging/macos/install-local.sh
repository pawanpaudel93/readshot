#!/usr/bin/env bash
# Local-install builder for Readshot on macOS.
#
# Builds the release binaries, assembles a `.app` bundle (with a
# generated AppIcon.icns), ad-hoc codesigns it, optionally installs
# into `/Applications`, and then resets the TCC entry for our bundle
# id so the user sees a clean Screen Recording prompt path on the
# very next launch.
#
# Usage:
#   packaging/macos/install-local.sh             # build + install + reset TCC
#   packaging/macos/install-local.sh --build     # build only, no install
#   packaging/macos/install-local.sh --uninstall # remove + reset TCC
#
# Why TCC reset matters
# ---------------------
# Ad-hoc codesigning produces a fresh cdhash on every rebuild. macOS
# TCC keys Screen Recording grants on (bundle id, cdhash). When you
# rebuild and reinstall, the old grant points to a stale cdhash and
# `CGPreflightScreenCaptureAccess` will keep returning false even
# after the user toggles "Readshot" ON in System Settings — because
# they are toggling the *old* entry. Resetting the TCC entry forces
# the new cdhash to register cleanly the first time the user clicks
# our welcome button.

set -euo pipefail

INSTALL=1
UNINSTALL=0
for arg in "$@"; do
  case "$arg" in
    --build) INSTALL=0 ;;
    --uninstall) UNINSTALL=1 ;;
    *) echo "unknown arg: $arg" >&2; exit 64 ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
APP_NAME="Readshot"
BUNDLE_ID="dev.pawanpaudel93.readshot"
APP_BUNDLE="${REPO_ROOT}/target/release/${APP_NAME}.app"
INSTALLED="/Applications/${APP_NAME}.app"
ICON_SRC="${REPO_ROOT}/packaging/macos/icon.svg"
ICON_OUT="${REPO_ROOT}/target/release/AppIcon.icns"

reset_tcc() {
  # Reset the Screen Recording grant for our bundle id so a stale
  # cdhash entry can't survive a reinstall. `tccutil reset` is silent
  # success when the bundle id has no current entry, so this is safe
  # to run unconditionally.
  if command -v tccutil >/dev/null 2>&1; then
    tccutil reset ScreenCapture "${BUNDLE_ID}" >/dev/null 2>&1 || true
    echo "→ tccutil: reset Screen Recording grant for ${BUNDLE_ID}"
  fi
}

if [[ "${UNINSTALL}" -eq 1 ]]; then
  if [[ -d "${INSTALLED}" ]]; then
    echo "→ removing ${INSTALLED}"
    rm -rf "${INSTALLED}"
  else
    echo "(${INSTALLED} not installed, skipping)"
  fi
  reset_tcc
  exit 0
fi

cd "${REPO_ROOT}"

# Pick the host arch unless TARGET is overridden.
TARGET="${TARGET:-$(rustc -vV | awk '/host:/ {print $2}')}"
echo "→ building for ${TARGET}"
cargo build --release --target "${TARGET}" --bin readshot --bin readshot-mcp

BIN_DIR="target/${TARGET}/release"
if [[ ! -x "${BIN_DIR}/readshot" ]]; then
  echo "error: ${BIN_DIR}/readshot missing after build" >&2
  exit 1
fi

# Generate AppIcon.icns from the SVG. We rasterise to all the macOS
# AppIcon sizes via rsvg-convert (16, 32, 64, 128, 256, 512, 1024 —
# both 1x and 2x for sizes < 1024), assemble an .iconset directory,
# and let `iconutil` produce the final binary plist .icns. This pipe
# only runs on macOS so requiring `rsvg-convert` here is fine; ship
# `brew install librsvg` in `docs/INSTALL.md` if a contributor lands
# without it.
if [[ ! -f "${ICON_SRC}" ]]; then
  echo "error: ${ICON_SRC} missing" >&2
  exit 1
fi
if ! command -v rsvg-convert >/dev/null 2>&1; then
  echo "error: rsvg-convert not found (brew install librsvg)" >&2
  exit 1
fi
ICON_BUILD_DIR="$(mktemp -d -t readshot-iconset)"
ICONSET="${ICON_BUILD_DIR}/AppIcon.iconset"
mkdir -p "${ICONSET}"
echo "→ rasterising ${ICON_SRC} into AppIcon.iconset"
# (size, suffix) pairs per Apple's iconset convention.
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
iconutil -c icns "${ICONSET}" -o "${ICON_OUT}"
rm -rf "${ICON_BUILD_DIR}"

# Assemble the .app bundle layout. The main executable is named
# `Readshot` (matching the bundle name) — codesign and Launch
# Services both expect that convention, and trying to keep the
# binary lowercase here trips codesign --verify on case-insensitive
# APFS volumes.
echo "→ assembling ${APP_BUNDLE}"
rm -rf "${APP_BUNDLE}"
mkdir -p "${APP_BUNDLE}/Contents/MacOS"
mkdir -p "${APP_BUNDLE}/Contents/Resources"
cp "${BIN_DIR}/readshot" "${APP_BUNDLE}/Contents/MacOS/Readshot"
ln -s Readshot "${APP_BUNDLE}/Contents/MacOS/readshot"
cp "${BIN_DIR}/readshot-mcp" "${APP_BUNDLE}/Contents/MacOS/readshot-mcp"
cp "${ICON_OUT}" "${APP_BUNDLE}/Contents/Resources/AppIcon.icns"
# Rewrite only CFBundleExecutable to the capitalised name. The
# lowercase `readshot` URL scheme and CLI symlink must stay lowercase.
cp packaging/macos/Info.plist "${APP_BUNDLE}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable Readshot" \
  "${APP_BUNDLE}/Contents/Info.plist"
plutil -lint "${APP_BUNDLE}/Contents/Info.plist" >/dev/null

# Strip Cargo's build-time ad-hoc signatures from the inner binaries
# before re-signing the bundle. Cargo's inner signatures get sealed
# against an Info.plist that doesn't exist yet (the binary lives
# outside any bundle at build time), and that stale seal makes
# `codesign --verify` complain about a "modified Info.plist" the
# moment we wrap the binaries in a real bundle.
echo "→ stripping Cargo's build-time signatures"
for bin in Readshot readshot-mcp; do
  codesign --remove-signature "${APP_BUNDLE}/Contents/MacOS/${bin}" 2>/dev/null || true
done

echo "→ ad-hoc codesigning bundle"
codesign --force --deep --sign - "${APP_BUNDLE}"
codesign --verify --deep "${APP_BUNDLE}"

if [[ "${INSTALL}" -eq 0 ]]; then
  echo "✓ ${APP_BUNDLE} ready (skipping install)"
  exit 0
fi

echo "→ installing to ${INSTALLED}"
if [[ -d "${INSTALLED}" ]]; then
  rm -rf "${INSTALLED}"
fi
cp -R "${APP_BUNDLE}" "${INSTALLED}"

# Strip quarantine xattr just in case it got set during the copy.
xattr -dr com.apple.quarantine "${INSTALLED}" 2>/dev/null || true

# Make sure Finder picks up the new icon immediately rather than
# showing the cached generic placeholder.
touch "${INSTALLED}"
killall Finder >/dev/null 2>&1 || true

# Reset stale TCC so the new cdhash registers cleanly on first launch.
reset_tcc

cat <<EOF
✓ Installed: ${INSTALLED}

First-launch checklist (Screen Recording permission):
  1. Open the app:           open ${INSTALLED}
  2. Click "Open System Settings" in the welcome window.
     macOS will reveal Privacy & Security → Screen Recording.
  3. Toggle Readshot ON.
  4. Switch back to Readshot and click "Restart Readshot now".

Companion MCP binary lives inside the bundle at:
  ${INSTALLED}/Contents/MacOS/readshot-mcp

To remove:
  packaging/macos/install-local.sh --uninstall
EOF
