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

assert_true() {
  # $1: a test-command result already captured as "yes"/"no"
  assert_eq "yes" "$1" "$2"
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

# --- Uninstall path (entirely inside temp dirs; system calls stubbed) ---
#
# READSHOT_UNINSTALL_SKIP_SYSTEM=1 makes tccutil / launchctl no-ops, and
# every path below points at a throwaway HOME / install-dir / bin-dir, so
# this never touches the real machine. uninstall_readshot requires macOS
# (require_macos), so skip the block elsewhere.
if [[ "$(uname -s)" == "Darwin" ]]; then
uninstall_root="$(mktemp -d)"
export READSHOT_UNINSTALL_SKIP_SYSTEM=1
HOME="${uninstall_root}/home"
INSTALL_DIR="${uninstall_root}/apps"
BIN_DIR="${uninstall_root}/bin"
PURGE=1

fake_app="${INSTALL_DIR}/Readshot.app"
app_support="${HOME}/Library/Application Support/np.com.pawanpaudel.Readshot"
login_plist="${HOME}/Library/LaunchAgents/np.com.pawanpaudel.readshot.login.plist"

mkdir -p "${fake_app}/Contents/MacOS"
printf 'bin\n' > "${fake_app}/Contents/MacOS/readshot"
mkdir -p "${BIN_DIR}"
# A Readshot-owned symlink (should be removed) ...
ln -s "${fake_app}/Contents/MacOS/readshot" "${BIN_DIR}/readshot"
# ... and a regular file at the other CLI name (must be preserved).
printf 'not a link\n' > "${BIN_DIR}/readshot-mcp"
mkdir -p "$(dirname "${login_plist}")"
printf '<plist/>\n' > "${login_plist}"
mkdir -p "${app_support}"
printf 'x\n' > "${app_support}/preferences.toml"

uninstall_readshot >/dev/null 2>&1

assert_true "$([[ ! -L "${BIN_DIR}/readshot" ]] && echo yes || echo no)" \
  "uninstall removes the Readshot-owned CLI symlink"
assert_true "$([[ -f "${BIN_DIR}/readshot-mcp" ]] && echo yes || echo no)" \
  "uninstall leaves a regular file at the CLI path alone"
assert_true "$([[ ! -d "${fake_app}" ]] && echo yes || echo no)" \
  "uninstall moves the app bundle out of the install dir"
assert_true "$([[ -d "${HOME}/.Trash/Readshot.app" ]] && echo yes || echo no)" \
  "uninstall moves the app bundle into the Trash"
assert_true "$([[ ! -e "${login_plist}" ]] && echo yes || echo no)" \
  "uninstall removes the login agent plist"
assert_true "$([[ ! -d "${app_support}" ]] && echo yes || echo no)" \
  "uninstall --purge deletes the app support directory"

# A foreign symlink at a CLI path is never removed.
rm -rf "${BIN_DIR}"
mkdir -p "${BIN_DIR}" "${uninstall_root}/other"
printf 'other\n' > "${uninstall_root}/other/readshot"
ln -s "${uninstall_root}/other/readshot" "${BIN_DIR}/readshot"
remove_cli_symlink "${BIN_DIR}/readshot" >/dev/null 2>&1
assert_true "$([[ -L "${BIN_DIR}/readshot" ]] && echo yes || echo no)" \
  "a symlink not pointing into Readshot.app is preserved"

rm -rf "${uninstall_root}"
unset READSHOT_UNINSTALL_SKIP_SYSTEM
else
  echo "ok - uninstall path tests skipped (not macOS)"
fi

if [[ "${failures}" -ne 0 ]]; then
  exit 1
fi
