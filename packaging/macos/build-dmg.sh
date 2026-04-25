#!/usr/bin/env bash
# Build, self-sign, and package a Readshot DMG.
#
# Reads from environment:
#   MACOS_SELF_SIGN_CERT_BASE64   — Base64 of the project's self-signed .p12
#   MACOS_SELF_SIGN_CERT_PASSWORD — Password for the .p12
#   SPARKLE_ED_KEY_BASE64         — Base64 of the EdDSA private key used by
#                                   `sign_update` for Sparkle. Optional in
#                                   workflow_dispatch runs.
#
# Output:
#   target/release/readshot.dmg
#   target/release/readshot.dmg.sparkle.eddsa.txt (when SPARKLE_ED_KEY is present)
#
# Per spec §Architectural Decisions: NOT notarised. The cert is project-
# held (not Apple Developer ID), so Gatekeeper shows a one-time
# right-click → Open prompt; the README and Homebrew Cask caveats
# walk the user through it.

set -euo pipefail

TARGET="${TARGET:-aarch64-apple-darwin}"
APP_NAME="Readshot"
BIN_PATH="target/${TARGET}/release/readshot"
APP_BUNDLE="target/release/${APP_NAME}.app"
DMG_PATH="target/release/readshot.dmg"

if [[ ! -f "${BIN_PATH}" ]]; then
  echo "error: ${BIN_PATH} not found — run 'cargo build --release --target ${TARGET}' first" >&2
  exit 1
fi

# 1. Build the .app bundle layout.
rm -rf "${APP_BUNDLE}"
mkdir -p "${APP_BUNDLE}/Contents/MacOS"
mkdir -p "${APP_BUNDLE}/Contents/Resources"
cp "${BIN_PATH}" "${APP_BUNDLE}/Contents/MacOS/readshot"
cp packaging/macos/Info.plist "${APP_BUNDLE}/Contents/Info.plist"

# 2. Import the self-signed cert into a temporary keychain.
KEYCHAIN="readshot-build.keychain-db"
KEYCHAIN_PASS="ephemeral-$(date +%s)"
security create-keychain -p "${KEYCHAIN_PASS}" "${KEYCHAIN}"
security default-keychain -s "${KEYCHAIN}"
security unlock-keychain -p "${KEYCHAIN_PASS}" "${KEYCHAIN}"

CERT_PATH="$(mktemp -t readshot-cert).p12"
echo "${MACOS_SELF_SIGN_CERT_BASE64}" | base64 -d > "${CERT_PATH}"
security import "${CERT_PATH}" \
  -k "${KEYCHAIN}" \
  -P "${MACOS_SELF_SIGN_CERT_PASSWORD}" \
  -T /usr/bin/codesign

security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s -k "${KEYCHAIN_PASS}" "${KEYCHAIN}"

# 3. Self-sign the bundle. Hardened-runtime is preserved so the OS
# considers the binary "well-formed" even without notarisation.
codesign --deep --force --options runtime \
  --sign "Readshot Project Self-Signed" \
  "${APP_BUNDLE}"

codesign --verify --deep --strict "${APP_BUNDLE}"

# 4. Build the DMG via `hdiutil`. A plain layout — no fancy
# background image; the Homebrew Cask is the recommended install
# path so DMG aesthetics matter little.
rm -f "${DMG_PATH}"
hdiutil create \
  -volname "${APP_NAME}" \
  -srcfolder "${APP_BUNDLE}" \
  -ov \
  -format UDZO \
  "${DMG_PATH}"

# 5. EdDSA signature for Sparkle (skipped if key is absent — keeps
# `workflow_dispatch` runs producing a usable artefact).
if [[ -n "${SPARKLE_ED_KEY_BASE64:-}" ]]; then
  KEY_PATH="$(mktemp -t readshot-edkey)"
  echo "${SPARKLE_ED_KEY_BASE64}" | base64 -d > "${KEY_PATH}"
  # `sign_update` ships in Sparkle's `bin/`; the release workflow
  # downloads it explicitly when needed.
  sign_update "${DMG_PATH}" "${KEY_PATH}" \
    > "${DMG_PATH}.sparkle.eddsa.txt"
  rm -f "${KEY_PATH}"
fi

# 6. Tear down the temp keychain so the runner doesn't leak state.
security delete-keychain "${KEYCHAIN}"
rm -f "${CERT_PATH}"

echo "✓ DMG ready at ${DMG_PATH}"
