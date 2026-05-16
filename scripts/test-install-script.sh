#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL_SH="${REPO_ROOT}/install.sh"

if [[ ! -f "${INSTALL_SH}" ]]; then
  echo "install.sh is missing" >&2
  exit 1
fi

# shellcheck source=/dev/null
READSHOT_INSTALL_SKIP_MAIN=1 source "${INSTALL_SH}"

failures=0

assert_eq() {
  local expected="$1"
  local actual="$2"
  local name="$3"

  if [[ "${actual}" != "${expected}" ]]; then
    echo "not ok - ${name}: expected '${expected}', got '${actual}'" >&2
    failures=$((failures + 1))
  else
    echo "ok - ${name}"
  fi
}

assert_eq "aarch64" "$(release_arch_for_machine arm64)" "arm64 maps to release arch"
assert_eq "aarch64" "$(release_arch_for_machine aarch64)" "aarch64 maps to release arch"
assert_eq "x86_64" "$(release_arch_for_machine x86_64)" "x86_64 maps to release arch"
assert_eq "x86_64" "$(release_arch_for_machine amd64)" "amd64 maps to release arch"

INSTALL_DIR="/Applications"
BIN_DIR="${HOME}/.local/bin"
TARGET_VERSION="latest"
parse_args --install-dir /tmp/readshot-apps --bin-dir /tmp/readshot-bin 0.4.1
assert_eq "/tmp/readshot-apps" "${INSTALL_DIR}" "install dir flag is parsed"
assert_eq "/tmp/readshot-bin" "${BIN_DIR}" "bin dir flag is parsed"
assert_eq "0.4.1" "${TARGET_VERSION}" "explicit version is parsed"

INSTALL_DIR="/tmp/readshot-apps"
BIN_DIR="/tmp/readshot-bin"
TARGET_VERSION="0.4.1"
parse_args
assert_eq "/Applications" "${INSTALL_DIR}" "default install dir is restored"
assert_eq "${HOME}/.local/bin" "${BIN_DIR}" "default bin dir is restored"
assert_eq "latest" "${TARGET_VERSION}" "default version is latest"

if READSHOT_INSTALL_SKIP_MAIN=1 bash -c 'source ./install.sh; parse_args --no-remove-quarantine' >/dev/null 2>&1; then
  echo "not ok - quarantine opt-out flag is rejected" >&2
  failures=$((failures + 1))
else
  echo "ok - quarantine opt-out flag is rejected"
fi

sha_file="$(mktemp)"
cat > "${sha_file}" <<'EOF'
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  out/readshot-macos-aarch64.dmg
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb  out/readshot-macos-x86_64.dmg
EOF

assert_eq \
  "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" \
  "$(sha_for_artifact "${sha_file}" readshot-macos-aarch64.dmg)" \
  "checksum lookup matches aarch64 artifact by basename"

rm -f "${sha_file}"

if [[ "${failures}" -ne 0 ]]; then
  exit 1
fi
