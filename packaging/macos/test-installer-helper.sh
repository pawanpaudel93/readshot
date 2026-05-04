#!/usr/bin/env bash
# Smoke-test the DMG installer helper contract without building a DMG.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HELPER="${REPO_ROOT}/packaging/macos/Install Readshot.command"
BUILD_DMG="${REPO_ROOT}/packaging/macos/build-dmg.sh"

if [[ ! -f "${HELPER}" ]]; then
  echo "missing installer helper: ${HELPER}" >&2
  exit 1
fi

bash -n "${HELPER}"

grep -q 'Readshot.app' "${HELPER}"
grep -q '/Applications/Readshot.app' "${HELPER}"
grep -q 'com.apple.quarantine' "${HELPER}"
grep -q 'tccutil reset ScreenCapture dev.pawanpaudel93.readshot' "${HELPER}"
grep -q 'open "/Applications/Readshot.app"' "${HELPER}"
grep -q 'Install Readshot.command' "${BUILD_DMG}"
