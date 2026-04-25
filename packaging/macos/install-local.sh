#!/usr/bin/env bash
# Local-install builder for Readshot on macOS.
#
# Builds the release binaries, assembles a `.app` bundle, ad-hoc
# codesigns it (so Apple Silicon can run it without quarantine
# nags on the build machine), and optionally installs into
# `/Applications`. No paid Developer ID required — the same
# bundle won't run on someone else's Mac without right-click → Open.
#
# Usage:
#   packaging/macos/install-local.sh             # build + install to /Applications
#   packaging/macos/install-local.sh --build     # build only, no install
#   packaging/macos/install-local.sh --uninstall # remove the installed copy

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
APP_BUNDLE="${REPO_ROOT}/target/release/${APP_NAME}.app"
INSTALLED="/Applications/${APP_NAME}.app"

if [[ "${UNINSTALL}" -eq 1 ]]; then
  if [[ -d "${INSTALLED}" ]]; then
    echo "→ removing ${INSTALLED}"
    rm -rf "${INSTALLED}"
  else
    echo "(${INSTALLED} not installed, skipping)"
  fi
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
cp "${BIN_DIR}/readshot-mcp" "${APP_BUNDLE}/Contents/MacOS/readshot-mcp"
# Rewrite CFBundleExecutable to the capitalised name. We do this
# in-flight so the source plist stays canonical for the release
# pipeline; sed -i '' is the macOS-portable form.
sed 's|<string>readshot</string>|<string>Readshot</string>|' \
    packaging/macos/Info.plist \
    > "${APP_BUNDLE}/Contents/Info.plist"

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
# Sanity verify — `--deep` walks the recursive seals.
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

cat <<EOF
✓ Installed: ${INSTALLED}

Launch from Finder or:
  open /Applications/Readshot.app

Companion MCP binary lives inside the bundle at:
  /Applications/Readshot.app/Contents/MacOS/readshot-mcp

The first launch on macOS may ask for Screen Recording permission
when you trigger a capture. Grant it via the welcome window's
"Grant access" button, or:
  System Settings → Privacy & Security → Screen Recording.

To remove:
  packaging/macos/install-local.sh --uninstall
EOF
