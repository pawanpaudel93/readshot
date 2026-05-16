#!/usr/bin/env bash
# Regression tests for the Sparkle appcast renderer.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP_DIR="$(mktemp -d)"
command -v ruby >/dev/null 2>&1 || {
  echo "error: ruby is required" >&2
  exit 1
}
cleanup() {
  rm -rf "${TMP_DIR}"
}
trap cleanup EXIT

ARTIFACT_DIR="${TMP_DIR}/artifacts"
SITE_DIR="${TMP_DIR}/site"
mkdir -p "${ARTIFACT_DIR}" "${SITE_DIR}"

printf 'arm dmg\n' > "${ARTIFACT_DIR}/readshot-macos-aarch64.dmg"
printf 'intel dmg\n' > "${ARTIFACT_DIR}/readshot-macos-x86_64.dmg"

cat > "${ARTIFACT_DIR}/readshot-macos-aarch64.dmg.sparkle.eddsa.txt" <<'EOF'
sparkle:edSignature="arm-signature" length="8"
EOF
cat > "${ARTIFACT_DIR}/readshot-macos-x86_64.dmg.sparkle.eddsa.txt" <<'EOF'
sparkle:edSignature="intel-signature" length="10"
EOF

TAG_NAME=v9.8.7 \
GITHUB_REPOSITORY=example/readshot \
GITHUB_SERVER_URL=https://github.example.test \
  bash "${REPO_ROOT}/packaging/macos/update-appcast.sh" \
    "${ARTIFACT_DIR}" \
    "${SITE_DIR}" >/dev/null

APPCAST="${SITE_DIR}/appcast.xml"

assert_contains() {
  local pattern="$1"
  local description="$2"
  if ! grep -q "${pattern}" "${APPCAST}"; then
    echo "not ok - ${description}" >&2
    exit 1
  fi
  echo "ok - ${description}"
}

assert_not_contains_between() {
  local start="$1"
  local end="$2"
  local pattern="$3"
  local description="$4"
  if awk -v start="${start}" -v end="${end}" '
    index($0, start) { in_range = 1 }
    in_range { print }
    index($0, end) { in_range = 0 }
  ' "${APPCAST}" | grep -q "${pattern}"; then
    echo "not ok - ${description}" >&2
    exit 1
  fi
  echo "ok - ${description}"
}

assert_contains '<sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>' \
  "apple silicon item is gated to arm64"
assert_contains 'readshot-macos-aarch64.dmg' "apple silicon DMG is present"
assert_contains 'readshot-macos-x86_64.dmg' "intel DMG is present"
assert_not_contains_between \
  'Readshot 9.8.7 (x86_64)' \
  '</item>' \
  'sparkle:hardwareRequirements' \
  "intel item stays available to Intel macs"

ruby -r rexml/document -e 'REXML::Document.new(File.read(ARGV.fetch(0)))' "${APPCAST}"
echo "ok - appcast is well-formed XML"

TAG_NAME=v9.8.7 \
GITHUB_REPOSITORY=example/readshot \
GITHUB_SERVER_URL=https://github.example.test \
  bash "${REPO_ROOT}/packaging/macos/update-appcast.sh" \
    "${ARTIFACT_DIR}" \
    "${SITE_DIR}" >/dev/null

item_count="$(grep -c '<sparkle:version>9.8.7</sparkle:version>' "${APPCAST}")"
if [[ "${item_count}" != "2" ]]; then
  echo "not ok - rerender replaces same-version items, got ${item_count}" >&2
  exit 1
fi
echo "ok - rerender replaces same-version items"
