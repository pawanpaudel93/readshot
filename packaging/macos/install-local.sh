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
RENDER_ICON="${REPO_ROOT}/packaging/macos/render-app-icon.sh"

workspace_version() {
  sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1
}

resolve_sparkle_framework_path() {
  if [[ -n "${SPARKLE_FRAMEWORK_PATH:-}" ]]; then
    echo "${SPARKLE_FRAMEWORK_PATH}"
    return
  fi
  if [[ -d "/Applications/Sparkle.app/Contents/SharedSupport/Sparkle.framework" ]]; then
    echo "/Applications/Sparkle.app/Contents/SharedSupport/Sparkle.framework"
    return
  fi
  if command -v brew >/dev/null 2>&1; then
    local framework
    framework="$(brew list --cask sparkle 2>/dev/null | awk '/\/Sparkle\.framework$/ { print; exit }')"
    if [[ -n "${framework}" ]]; then
      echo "${framework}"
      return
    fi
  fi
  find /opt/homebrew/Caskroom/sparkle /usr/local/Caskroom/sparkle \
    -path "*/Sparkle.framework" -type d -print -quit 2>/dev/null || true
}

sign_sparkle_framework() {
  local framework="$1"
  local identity="$2"
  local version_dir="${framework}/Versions/B"
  local sign_args=(--force --sign "${identity}" --options runtime)

  if [[ ! -d "${version_dir}" ]]; then
    version_dir="$(cd "${framework}/Versions/Current" && pwd -P)"
  fi

  sign_existing() {
    local path="$1"
    shift
    if [[ -e "${path}" ]]; then
      codesign "${sign_args[@]}" "$@" "${path}"
    fi
  }

  sign_existing "${version_dir}/Sparkle"
  sign_existing "${version_dir}/Autoupdate"
  sign_existing "${version_dir}/Updater.app/Contents/MacOS/Updater"
  sign_existing "${version_dir}/Updater.app"
  sign_existing "${version_dir}/XPCServices/Downloader.xpc/Contents/MacOS/Downloader"
  sign_existing "${version_dir}/XPCServices/Downloader.xpc" --preserve-metadata=entitlements
  sign_existing "${version_dir}/XPCServices/Installer.xpc/Contents/MacOS/Installer"
  sign_existing "${version_dir}/XPCServices/Installer.xpc"
  sign_existing "${version_dir}"
  sign_existing "${framework}"
}

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

# Generate AppIcon.icns from the canonical SVG. The release package
# uses the same renderer so Finder, the Dock, and the DMG all see the
# same app icon.
if [[ ! -f "${ICON_SRC}" ]]; then
  echo "error: ${ICON_SRC} missing" >&2
  exit 1
fi
"${RENDER_ICON}" "${ICON_OUT}" --png-512 "${REPO_ROOT}/packaging/linux/readshot.png"

# Assemble the .app bundle layout. The bundle is named `Readshot.app`,
# but the executable stays lowercase `readshot` so the same binary can
# act as both GUI entry point and packaged CLI inside Contents/MacOS.
echo "→ assembling ${APP_BUNDLE}"
rm -rf "${APP_BUNDLE}"
mkdir -p "${APP_BUNDLE}/Contents/MacOS"
mkdir -p "${APP_BUNDLE}/Contents/Resources"
mkdir -p "${APP_BUNDLE}/Contents/Frameworks"
cp "${BIN_DIR}/readshot" "${APP_BUNDLE}/Contents/MacOS/readshot"
cp "${BIN_DIR}/readshot-mcp" "${APP_BUNDLE}/Contents/MacOS/readshot-mcp"
cp "${ICON_OUT}" "${APP_BUNDLE}/Contents/Resources/AppIcon.icns"
cp packaging/macos/Info.plist "${APP_BUNDLE}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable readshot" \
  "${APP_BUNDLE}/Contents/Info.plist"
BUNDLE_VERSION="$(workspace_version)"
if [[ -z "${BUNDLE_VERSION}" ]]; then
  echo "error: could not read workspace version from Cargo.toml" >&2
  exit 1
fi
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString ${BUNDLE_VERSION}" \
  "${APP_BUNDLE}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${BUNDLE_VERSION}" \
  "${APP_BUNDLE}/Contents/Info.plist"
plutil -lint "${APP_BUNDLE}/Contents/Info.plist" >/dev/null

SPARKLE_FRAMEWORK_PATH="$(resolve_sparkle_framework_path)"
if [[ -d "${SPARKLE_FRAMEWORK_PATH}" ]]; then
  cp -R "${SPARKLE_FRAMEWORK_PATH}" "${APP_BUNDLE}/Contents/Frameworks/Sparkle.framework"
else
  echo "warning: Sparkle.framework not found at ${SPARKLE_FRAMEWORK_PATH}" >&2
  echo "         install Sparkle with: brew install --cask sparkle" >&2
  echo "         local app will run, but Check for Updates… will be unavailable" >&2
fi

# Strip Cargo's build-time ad-hoc signatures from the inner binaries
# before re-signing the bundle. Cargo's inner signatures get sealed
# against an Info.plist that doesn't exist yet (the binary lives
# outside any bundle at build time), and that stale seal makes
# `codesign --verify` complain about a "modified Info.plist" the
# moment we wrap the binaries in a real bundle.
echo "→ stripping Cargo's build-time signatures"
for bin in readshot readshot-mcp; do
  codesign --remove-signature "${APP_BUNDLE}/Contents/MacOS/${bin}" 2>/dev/null || true
done
xattr -cr "${APP_BUNDLE}" 2>/dev/null || true
find "${APP_BUNDLE}" -name '._*' -delete

echo "→ ad-hoc codesigning bundle"
if [[ -d "${APP_BUNDLE}/Contents/Frameworks/Sparkle.framework" ]]; then
  sign_sparkle_framework "${APP_BUNDLE}/Contents/Frameworks/Sparkle.framework" -
fi
for bin in readshot-mcp readshot; do
  codesign --force --options runtime --sign - \
    "${APP_BUNDLE}/Contents/MacOS/${bin}"
done
codesign --force --options runtime --sign - "${APP_BUNDLE}"
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
