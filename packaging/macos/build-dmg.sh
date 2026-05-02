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
DMG_PATH="target/release/readshot.dmg"
KEYCHAIN=""
CERT_PATH=""
PREVIOUS_KEYCHAIN="$(security default-keychain | tr -d ' "')"

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
cp "${BIN_PATH}" "${APP_BUNDLE}/Contents/MacOS/Readshot"
ln -s Readshot "${APP_BUNDLE}/Contents/MacOS/readshot"
cp "${MCP_BIN_PATH}" "${APP_BUNDLE}/Contents/MacOS/readshot-mcp"
cp packaging/macos/Info.plist "${APP_BUNDLE}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleExecutable Readshot" \
  "${APP_BUNDLE}/Contents/Info.plist"

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
else
  CERT_BASE64="${MACOS_SELF_SIGN_CERT_BASE64:?MACOS_SELF_SIGN_CERT_BASE64 required when Developer ID cert is absent}"
  CERT_PASSWORD="${MACOS_SELF_SIGN_CERT_PASSWORD:?MACOS_SELF_SIGN_CERT_PASSWORD required when Developer ID cert is absent}"
  SIGNING_IDENTITY="${MACOS_SIGNING_IDENTITY:-Readshot Project Self-Signed}"
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
for bin in Readshot readshot-mcp; do
  codesign --remove-signature "${APP_BUNDLE}/Contents/MacOS/${bin}" 2>/dev/null || true
done

# 4. Sign the bundle. Hardened-runtime is required for notarisation
# and still keeps self-signed builds well-formed.
codesign --deep --force --options runtime \
  --sign "${SIGNING_IDENTITY}" \
  "${APP_BUNDLE}"

codesign --verify --deep --strict "${APP_BUNDLE}"

# 5. Build the DMG via `hdiutil`. A plain layout — no fancy
# background image; the Homebrew Cask is the recommended install
# path so DMG aesthetics matter little.
rm -f "${DMG_PATH}"
hdiutil create \
  -volname "${APP_NAME}" \
  -srcfolder "${APP_BUNDLE}" \
  -ov \
  -format UDZO \
  "${DMG_PATH}"

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
  echo "${SPARKLE_ED_KEY_BASE64}" | base64 -d > "${KEY_PATH}"
  sign_update "${DMG_PATH}" "${KEY_PATH}" \
    > "${DMG_PATH}.sparkle.eddsa.txt"
  rm -f "${KEY_PATH}"
fi

echo "✓ DMG ready at ${DMG_PATH}"
