#!/usr/bin/env bash
# Smoke-check downloaded macOS release artifacts before announcing a release.
#
# Usage:
#   scripts/smoke-macos-release.sh v0.2.0 /path/to/release-artifacts [appcast.xml-or-url]
#
# The artifact directory should contain:
#   readshot-macos-aarch64.dmg
#   readshot-macos-x86_64.dmg
#   SHA256SUMS
#
# Run this on a clean macOS VM after downloading the GitHub Release
# artifacts. It verifies hashes, mounts each DMG, checks the app bundle
# shape, and confirms the appcast points at the released DMGs.

set -euo pipefail

usage() {
  echo "usage: $0 <vX.Y.Z> <artifact-dir> [appcast.xml-or-url]" >&2
}

if [[ $# -lt 2 || $# -gt 3 ]]; then
  usage
  exit 2
fi

TAG="$1"
ARTIFACT_DIR="$2"
APPCAST_SOURCE="${3:-https://pawanpaudel93.github.io/readshot/appcast.xml}"
VERSION="${TAG#v}"

if [[ "${TAG}" != v*.*.* ]]; then
  echo "error: tag must look like vX.Y.Z, got ${TAG}" >&2
  exit 2
fi
if [[ ! -d "${ARTIFACT_DIR}" ]]; then
  echo "error: artifact directory not found: ${ARTIFACT_DIR}" >&2
  exit 1
fi
if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: this smoke test must run on macOS" >&2
  exit 1
fi

required_tools=(codesign curl hdiutil plutil shasum)
for tool in "${required_tools[@]}"; do
  if ! command -v "${tool}" >/dev/null 2>&1; then
    echo "error: required tool not found: ${tool}" >&2
    exit 1
  fi
done

TMP_DIR="$(mktemp -d)"
MOUNT_POINTS=()
cleanup() {
  for mount in "${MOUNT_POINTS[@]:-}"; do
    hdiutil detach "${mount}" -quiet >/dev/null 2>&1 || true
  done
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

echo "==> Verifying SHA256SUMS"
if [[ ! -f "${ARTIFACT_DIR}/SHA256SUMS" ]]; then
  echo "error: missing SHA256SUMS" >&2
  exit 1
fi

for arch in aarch64 x86_64; do
  dmg="readshot-macos-${arch}.dmg"
  dmg_path="${ARTIFACT_DIR}/${dmg}"
  if [[ ! -f "${dmg_path}" ]]; then
    echo "error: missing ${dmg_path}" >&2
    exit 1
  fi
  expected="$(awk -v f="${dmg}" '
    $2 == f || $2 == "./" f || $2 == "out/" f || $2 ~ "/" f "$" { print $1; exit }
  ' "${ARTIFACT_DIR}/SHA256SUMS")"
  if [[ -z "${expected}" ]]; then
    echo "error: SHA256SUMS has no entry for ${dmg}" >&2
    exit 1
  fi
  actual="$(shasum -a 256 "${dmg_path}" | awk '{ print $1 }')"
  if [[ "${actual}" != "${expected}" ]]; then
    echo "error: SHA-256 mismatch for ${dmg}" >&2
    echo "expected: ${expected}" >&2
    echo "actual:   ${actual}" >&2
    exit 1
  fi
done

echo "==> Inspecting DMG contents"
for arch in aarch64 x86_64; do
  dmg_path="${ARTIFACT_DIR}/readshot-macos-${arch}.dmg"
  mount_point="${TMP_DIR}/mount-${arch}"
  mkdir -p "${mount_point}"
  hdiutil attach "${dmg_path}" -readonly -nobrowse -mountpoint "${mount_point}" -quiet
  MOUNT_POINTS+=("${mount_point}")

  app="${mount_point}/Readshot.app"
  plist="${app}/Contents/Info.plist"
  if [[ ! -d "${app}" ]]; then
    echo "error: ${dmg_path} does not contain Readshot.app" >&2
    exit 1
  fi
  for path in \
    "${app}/Contents/MacOS/readshot" \
    "${app}/Contents/MacOS/readshot-mcp" \
    "${app}/Contents/Frameworks/Sparkle.framework" \
    "${plist}"; do
    if [[ ! -e "${path}" ]]; then
      echo "error: missing bundle path: ${path}" >&2
      exit 1
    fi
  done

  plutil -lint "${plist}" >/dev/null
  if [[ "$(plutil -extract CFBundleShortVersionString raw -o - "${plist}")" != "${VERSION}" ]]; then
    echo "error: ${arch} bundle version does not match ${VERSION}" >&2
    exit 1
  fi
  if [[ "$(plutil -extract SUFeedURL raw -o - "${plist}")" != "https://pawanpaudel93.github.io/readshot/appcast.xml" ]]; then
    echo "error: ${arch} bundle SUFeedURL is not the production appcast" >&2
    exit 1
  fi
  if [[ "$(plutil -extract SUEnableAutomaticChecks raw -o - "${plist}")" != "true" ]]; then
    echo "error: ${arch} bundle does not enable Sparkle automatic checks" >&2
    exit 1
  fi
  if ! /usr/libexec/PlistBuddy -c "Print :CFBundleURLTypes:0:CFBundleURLSchemes:0" "${plist}" | grep -qx "readshot"; then
    echo "error: ${arch} bundle does not register readshot:// URL scheme" >&2
    exit 1
  fi
  codesign --verify --deep --strict --verbose=2 "${app}" >/dev/null
done

echo "==> Checking Sparkle appcast"
APPCAST_PATH="${TMP_DIR}/appcast.xml"
if [[ "${APPCAST_SOURCE}" == http://* || "${APPCAST_SOURCE}" == https://* ]]; then
  curl -fsSL "${APPCAST_SOURCE}" -o "${APPCAST_PATH}"
else
  cp "${APPCAST_SOURCE}" "${APPCAST_PATH}"
fi

if ! grep -q "<sparkle:version>${VERSION}</sparkle:version>" "${APPCAST_PATH}"; then
  echo "error: appcast does not contain sparkle:version ${VERSION}" >&2
  exit 1
fi
for arch in aarch64 x86_64; do
  dmg="readshot-macos-${arch}.dmg"
  dmg_path="${ARTIFACT_DIR}/${dmg}"
  dmg_bytes="$(stat -f %z "${dmg_path}")"
  if ! grep -q "releases/download/${TAG}/${dmg}" "${APPCAST_PATH}"; then
    echo "error: appcast does not point at ${dmg}" >&2
    exit 1
  fi
  if ! grep -q "length=\"${dmg_bytes}\"" "${APPCAST_PATH}"; then
    echo "error: appcast byte length for ${dmg} does not match ${dmg_bytes}" >&2
    exit 1
  fi
done
if ! awk '/Readshot .* \(aarch64\)/,/<\/item>/' "${APPCAST_PATH}" \
  | grep -q "<sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>"; then
  echo "error: appcast Apple Silicon item is missing arm64 hardware requirements" >&2
  exit 1
fi
if awk '/Readshot .* \(x86_64\)/,/<\/item>/' "${APPCAST_PATH}" \
  | grep -q "<sparkle:hardwareRequirements>"; then
  echo "error: appcast Intel item should not have arm64 hardware requirements" >&2
  exit 1
fi
if ! grep -q "sparkle:edSignature=" "${APPCAST_PATH}"; then
  echo "error: appcast is missing Sparkle EdDSA signatures" >&2
  exit 1
fi

echo "macOS release artifacts look structurally valid for ${TAG}."
echo "Next: install one DMG on a clean macOS VM and complete docs/RELEASING.md clean-machine checks."
