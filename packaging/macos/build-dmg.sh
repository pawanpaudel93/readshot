#!/usr/bin/env bash
# Build, sign, optionally notarise, and package a Readshot DMG.
#
# Reads from environment:
#   MACOS_SELF_SIGN_CERT_BASE64   — Base64 of the project's self-signed .p12
#   MACOS_SELF_SIGN_CERT_PASSWORD — Password for the .p12
#   APPLE_DEVELOPER_ID_P12_BASE64 — Optional Base64 Developer ID .p12.
#   APPLE_DEVELOPER_ID_P12_PASSWORD
#   MACOS_SIGNING_IDENTITY        — Optional codesign identity override.
#                                   Defaults to Developer ID when present,
#                                   otherwise Readshot Project Self-Signed.
#   APPLE_ID                      — Optional notarization Apple ID.
#   APPLE_TEAM_ID                 — Optional notarization team ID.
#   APPLE_APP_SPECIFIC_PASSWORD   — Optional notarization app password.
#   SPARKLE_ED_KEY_BASE64         — Base64 of the EdDSA private key used by
#                                   `sign_update` for Sparkle. Optional in
#                                   workflow_dispatch runs.
#
# Output:
#   target/release/readshot.dmg
#   target/release/readshot.dmg.sparkle.eddsa.txt (when SPARKLE_ED_KEY is present)
#
# If Developer ID credentials are present, the DMG is submitted to
# Apple's notary service and stapled. Otherwise the project-held
# self-signed identity is used; Gatekeeper then shows a one-time
# right-click → Open prompt documented in INSTALL.md.

set -euo pipefail

TARGET="${TARGET:-aarch64-apple-darwin}"
APP_NAME="Readshot"
BIN_PATH="target/${TARGET}/release/readshot"
MCP_BIN_PATH="target/${TARGET}/release/readshot-mcp"
APP_BUNDLE="target/release/${APP_NAME}.app"
ICON_OUT="target/release/AppIcon.icns"
DMG_STAGING="target/release/dmg-root"
DMG_PATH="target/release/readshot.dmg"
KEYCHAIN=""
CERT_PATH=""
PREVIOUS_KEYCHAIN="$(security default-keychain | tr -d ' "')"

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

cleanup() {
  if [[ -n "${PREVIOUS_KEYCHAIN}" ]]; then
    security default-keychain -s "${PREVIOUS_KEYCHAIN}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${KEYCHAIN}" ]]; then
    security delete-keychain "${KEYCHAIN}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${CERT_PATH}" ]]; then
    rm -f "${CERT_PATH}"
  fi
  if [[ -d "${DMG_STAGING}" ]]; then
    rm -rf "${DMG_STAGING}"
  fi
}
trap cleanup EXIT

if [[ ! -f "${BIN_PATH}" ]]; then
  echo "error: ${BIN_PATH} not found — run 'cargo build --release --target ${TARGET}' first" >&2
  exit 1
fi
if [[ ! -f "${MCP_BIN_PATH}" ]]; then
  echo "error: ${MCP_BIN_PATH} not found — run 'cargo build --release --target ${TARGET} --bin readshot-mcp' first" >&2
  exit 1
fi

# 1. Build the .app bundle layout.
rm -rf "${APP_BUNDLE}"
mkdir -p "${APP_BUNDLE}/Contents/MacOS"
mkdir -p "${APP_BUNDLE}/Contents/Resources"
mkdir -p "${APP_BUNDLE}/Contents/Frameworks"
cp "${BIN_PATH}" "${APP_BUNDLE}/Contents/MacOS/readshot"
cp "${MCP_BIN_PATH}" "${APP_BUNDLE}/Contents/MacOS/readshot-mcp"
packaging/macos/render-app-icon.sh "${ICON_OUT}" --png-512 packaging/linux/readshot.png
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

SPARKLE_FRAMEWORK_PATH="$(resolve_sparkle_framework_path)"
if [[ ! -d "${SPARKLE_FRAMEWORK_PATH}" ]]; then
  echo "error: Sparkle.framework not found at ${SPARKLE_FRAMEWORK_PATH}" >&2
  echo "       install Sparkle with: brew install --cask sparkle" >&2
  exit 1
fi
cp -R "${SPARKLE_FRAMEWORK_PATH}" "${APP_BUNDLE}/Contents/Frameworks/Sparkle.framework"

# 2. Import the signing cert into a temporary keychain.
KEYCHAIN="readshot-build.keychain-db"
KEYCHAIN_PASS="ephemeral-$(date +%s)"
security create-keychain -p "${KEYCHAIN_PASS}" "${KEYCHAIN}"
security default-keychain -s "${KEYCHAIN}"
security unlock-keychain -p "${KEYCHAIN_PASS}" "${KEYCHAIN}"

if [[ -n "${APPLE_DEVELOPER_ID_P12_BASE64:-}" ]]; then
  CERT_BASE64="${APPLE_DEVELOPER_ID_P12_BASE64}"
  CERT_PASSWORD="${APPLE_DEVELOPER_ID_P12_PASSWORD:?APPLE_DEVELOPER_ID_P12_PASSWORD required when APPLE_DEVELOPER_ID_P12_BASE64 is set}"
  SIGNING_IDENTITY="${MACOS_SIGNING_IDENTITY:-Developer ID Application}"
  # Developer ID shares a Team ID with the re-signed Sparkle, so the
  # hardened runtime's library validation passes as-is.
  ENTITLEMENT_ARGS=()
else
  CERT_BASE64="${MACOS_SELF_SIGN_CERT_BASE64:?MACOS_SELF_SIGN_CERT_BASE64 required when Developer ID cert is absent}"
  CERT_PASSWORD="${MACOS_SELF_SIGN_CERT_PASSWORD:?MACOS_SELF_SIGN_CERT_PASSWORD required when Developer ID cert is absent}"
  SIGNING_IDENTITY="${MACOS_SIGNING_IDENTITY:-Readshot Project Self-Signed}"
  # A self-signed certificate has no Team ID, so library validation
  # would refuse to load the bundled Sparkle.framework.
  ENTITLEMENT_ARGS=(--entitlements packaging/macos/Readshot.entitlements)
fi

CERT_PATH="$(mktemp -t readshot-cert).p12"
echo "${CERT_BASE64}" | base64 -d > "${CERT_PATH}"
security import "${CERT_PATH}" \
  -k "${KEYCHAIN}" \
  -P "${CERT_PASSWORD}" \
  -T /usr/bin/codesign

security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s -k "${KEYCHAIN_PASS}" "${KEYCHAIN}"

# 3. Strip Cargo's build-time ad-hoc signatures before signing the
# real bundle. Those signatures were generated before the binaries
# lived inside an app bundle and can otherwise seal stale metadata.
for bin in readshot readshot-mcp; do
  codesign --remove-signature "${APP_BUNDLE}/Contents/MacOS/${bin}" 2>/dev/null || true
done
xattr -cr "${APP_BUNDLE}" 2>/dev/null || true
find "${APP_BUNDLE}" -name '._*' -delete

# 4. Sign nested code first, then the app bundle. This avoids
# hardened-runtime library validation rejecting Sparkle because it was
# still signed by Sparkle's release identity instead of Readshot's.
sign_sparkle_framework "${APP_BUNDLE}/Contents/Frameworks/Sparkle.framework" \
  "${SIGNING_IDENTITY}"

for bin in readshot-mcp readshot; do
  codesign --force --options runtime \
    --sign "${SIGNING_IDENTITY}" \
    "${APP_BUNDLE}/Contents/MacOS/${bin}"
done

codesign --force --options runtime \
  ${ENTITLEMENT_ARGS[@]+"${ENTITLEMENT_ARGS[@]}"} \
  --sign "${SIGNING_IDENTITY}" \
  "${APP_BUNDLE}"

codesign --verify --deep --strict "${APP_BUNDLE}"

# Refuse to ship a self-signed build signed with a different certificate
# than earlier releases. macOS ties the Screen Recording grant to the
# signing certificate, so a change makes every updated user grant it
# again (0.7.7 was built with one certificate, 0.7.6/0.8.0 with another).
# Developer ID builds are exempt: that switch is a deliberate, one-time move.
EXPECTED_CERT_FILE="packaging/macos/signing-cert.sha1"
if [[ -z "${APPLE_DEVELOPER_ID_P12_BASE64:-}" && -f "${EXPECTED_CERT_FILE}" ]]; then
  CERT_DIR="$(mktemp -d -t readshot-cert-check)"
  APP_ABS="$(pwd)/${APP_BUNDLE}"
  ( cd "${CERT_DIR}" && codesign -d --extract-certificates "${APP_ABS}" 2>/dev/null )
  ACTUAL_CERT="$(openssl x509 -inform DER -in "${CERT_DIR}/codesign0" -noout -fingerprint -sha1 \
    | sed 's/.*=//; s/://g' | tr 'A-F' 'a-f')"
  rm -rf "${CERT_DIR}"
  EXPECTED_CERT="$(tr -d '[:space:]' < "${EXPECTED_CERT_FILE}" | tr 'A-F' 'a-f')"
  if [[ "${ACTUAL_CERT}" != "${EXPECTED_CERT}" ]]; then
    echo "error: app signed with certificate ${ACTUAL_CERT}, expected ${EXPECTED_CERT}" >&2
    echo "       (${EXPECTED_CERT_FILE}). Using a different certificate resets every" >&2
    echo "       user's Screen Recording permission on update. Sign with the pinned" >&2
    echo "       certificate, or update the pin deliberately." >&2
    exit 1
  fi
  echo "→ signing certificate matches the pinned release certificate"
fi

# 5. Build a standard drag-to-Applications DMG.
rm -f "${DMG_PATH}"
rm -rf "${DMG_STAGING}"
mkdir -p "${DMG_STAGING}"
cp -R "${APP_BUNDLE}" "${DMG_STAGING}/${APP_NAME}.app"
ln -s /Applications "${DMG_STAGING}/Applications"

if command -v create-dmg >/dev/null 2>&1; then
  create-dmg \
    --volname "${APP_NAME}" \
    --window-pos 200 120 \
    --window-size 640 360 \
    --icon-size 96 \
    --icon "${APP_NAME}.app" 160 170 \
    --app-drop-link 480 170 \
    --no-internet-enable \
    "${DMG_PATH}" \
    "${DMG_STAGING}"
else
  hdiutil create \
    -volname "${APP_NAME}" \
    -srcfolder "${DMG_STAGING}" \
    -ov \
    -format UDZO \
    "${DMG_PATH}"
fi

# 6. Notarise and staple when Developer ID notary credentials are
# present. Self-signed builds intentionally skip this step.
if [[ -z "${APPLE_DEVELOPER_ID_P12_BASE64:-}" ]] \
  && [[ -n "${APPLE_ID:-}" || -n "${APPLE_TEAM_ID:-}" || -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]]; then
  echo "error: notarisation credentials require APPLE_DEVELOPER_ID_P12_BASE64" >&2
  exit 1
fi

if [[ -n "${APPLE_DEVELOPER_ID_P12_BASE64:-}" ]] \
  && [[ -n "${APPLE_ID:-}" || -n "${APPLE_TEAM_ID:-}" || -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" ]]; then
  : "${APPLE_ID:?APPLE_ID required for notarisation}"
  : "${APPLE_TEAM_ID:?APPLE_TEAM_ID required for notarisation}"
  : "${APPLE_APP_SPECIFIC_PASSWORD:?APPLE_APP_SPECIFIC_PASSWORD required for notarisation}"

  xcrun notarytool submit "${DMG_PATH}" \
    --apple-id "${APPLE_ID}" \
    --team-id "${APPLE_TEAM_ID}" \
    --password "${APPLE_APP_SPECIFIC_PASSWORD}" \
    --wait
  xcrun stapler staple "${DMG_PATH}"
fi

# 7. EdDSA signature for Sparkle (skipped if key is absent — keeps
# `workflow_dispatch` runs producing a usable artefact).
if [[ -n "${SPARKLE_ED_KEY_BASE64:-}" ]]; then
  if ! command -v sign_update >/dev/null 2>&1; then
    echo "error: sign_update not found; install Sparkle tools before packaging" >&2
    exit 1
  fi
  KEY_PATH="$(mktemp -t readshot-edkey)"
  printf '%s' "${SPARKLE_ED_KEY_BASE64}" | base64 -d > "${KEY_PATH}"
  sign_update --ed-key-file "${KEY_PATH}" "${DMG_PATH}" \
    > "${DMG_PATH}.sparkle.eddsa.txt"
  rm -f "${KEY_PATH}"
fi

echo "✓ DMG ready at ${DMG_PATH}"
