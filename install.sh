#!/usr/bin/env bash
# Install Readshot from the latest GitHub Release.
#
# Intended public usage:
#   curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash
#
# This installer removes the quarantine attribute from the installed
# app after verifying the release SHA-256.

set -euo pipefail

REPO="pawanpaudel93/readshot"
APP_NAME="Readshot"
APP_BUNDLE="${APP_NAME}.app"
INSTALL_DIR="/Applications"
BIN_DIR="${HOME}/.local/bin"
TARGET_VERSION="latest"

usage() {
  cat <<'EOF'
Usage: install.sh [latest|stable|VERSION] [options]

Options:
  --install-dir DIR          Install Readshot.app into DIR. Default: /Applications
  --bin-dir DIR              Symlink readshot and readshot-mcp into DIR. Default: ~/.local/bin
  --help                     Show this help

Examples:
  curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash
  curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash -s -- 0.4.1
EOF
}

log() {
  printf '%s\n' "$*"
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

parse_args() {
  INSTALL_DIR="/Applications"
  BIN_DIR="${HOME}/.local/bin"
  TARGET_VERSION="latest"

  while [[ $# -gt 0 ]]; do
    case "$1" in
      --install-dir)
        [[ $# -ge 2 ]] || die "--install-dir requires a directory"
        INSTALL_DIR="$2"
        shift 2
        ;;
      --bin-dir)
        [[ $# -ge 2 ]] || die "--bin-dir requires a directory"
        BIN_DIR="$2"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      stable|latest|v[0-9]*.[0-9]*.[0-9]*|[0-9]*.[0-9]*.[0-9]*)
        TARGET_VERSION="${1#v}"
        shift
        ;;
      *)
        die "unknown argument: $1"
        ;;
    esac
  done
}

require_macos() {
  [[ "$(uname -s)" == "Darwin" ]] || die "this installer currently supports macOS only"
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "$1 is required"
}

release_arch_for_machine() {
  case "$1" in
    arm64|aarch64) printf 'aarch64\n' ;;
    x86_64|amd64) printf 'x86_64\n' ;;
    *) die "unsupported architecture: $1" ;;
  esac
}

host_machine() {
  local machine
  machine="$(uname -m)"

  if [[ "${machine}" == "x86_64" ]] \
    && [[ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" == "1" ]]; then
    machine="arm64"
  fi

  printf '%s\n' "${machine}"
}

download_file() {
  local url="$1"
  local output="$2"
  curl -fL --retry 3 --connect-timeout 15 -o "${output}" "${url}"
}

resolve_version() {
  local requested="$1"
  local effective_url
  local tag

  if [[ "${requested}" != "latest" && "${requested}" != "stable" ]]; then
    printf '%s\n' "${requested#v}"
    return
  fi

  effective_url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/${REPO}/releases/latest")"
  tag="${effective_url##*/}"
  tag="${tag#v}"

  [[ "${tag}" =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]] || die "could not resolve latest release version"
  printf '%s\n' "${tag}"
}

sha_for_artifact() {
  local sha_file="$1"
  local artifact="$2"
  awk -v artifact="${artifact}" '{ path=$2; sub(/^.*\//, "", path); if (path == artifact) print $1 }' "${sha_file}"
}

verify_checksum() {
  local file="$1"
  local expected="$2"
  local actual

  actual="$(shasum -a 256 "${file}" | awk '{print $1}')"
  [[ "${actual}" == "${expected}" ]] || die "checksum mismatch for ${file}"
}

copy_app() {
  local source_app="$1"
  local target_app="$2"

  if [[ -d "${target_app}" ]]; then
    if [[ -w "${target_app%/*}" ]]; then
      rm -rf "${target_app}"
    else
      sudo rm -rf "${target_app}"
    fi
  fi

  mkdir -p "${target_app%/*}" 2>/dev/null || sudo mkdir -p "${target_app%/*}"
  if [[ -w "${target_app%/*}" ]]; then
    ditto "${source_app}" "${target_app}"
  else
    sudo ditto "${source_app}" "${target_app}"
  fi
}

remove_quarantine() {
  local target_app="$1"

  if xattr -dr com.apple.quarantine "${target_app}" 2>/dev/null; then
    return
  fi
  sudo xattr -dr com.apple.quarantine "${target_app}" 2>/dev/null || true
}

install_symlinks() {
  local target_app="$1"

  mkdir -p "${BIN_DIR}"
  ln -sf "${target_app}/Contents/MacOS/readshot" "${BIN_DIR}/readshot"
  ln -sf "${target_app}/Contents/MacOS/readshot-mcp" "${BIN_DIR}/readshot-mcp"
}

main() {
  parse_args "$@"
  require_macos
  require_command curl
  require_command hdiutil
  require_command shasum
  require_command ditto
  require_command xattr

  local version
  local tag
  local arch
  local artifact
  local work_dir
  local mount_dir
  local dmg_path
  local sums_path
  local checksum
  local target_app

  version="$(resolve_version "${TARGET_VERSION}")"
  tag="v${version}"
  arch="$(release_arch_for_machine "$(host_machine)")"
  artifact="readshot-macos-${arch}.dmg"
  work_dir="$(mktemp -d)"
  mount_dir="${work_dir}/mnt"
  dmg_path="${work_dir}/${artifact}"
  sums_path="${work_dir}/SHA256SUMS"
  target_app="${INSTALL_DIR%/}/${APP_BUNDLE}"

  cleanup() {
    hdiutil detach "${mount_dir}" -quiet >/dev/null 2>&1 || true
    rm -rf "${work_dir}"
  }
  trap cleanup EXIT

  log "Installing Readshot ${version} for macOS ${arch}"
  log "Downloading ${artifact}"
  download_file "https://github.com/${REPO}/releases/download/${tag}/${artifact}" "${dmg_path}"
  download_file "https://github.com/${REPO}/releases/download/${tag}/SHA256SUMS" "${sums_path}"

  checksum="$(sha_for_artifact "${sums_path}" "${artifact}")"
  [[ -n "${checksum}" ]] || die "SHA256SUMS does not contain ${artifact}"
  verify_checksum "${dmg_path}" "${checksum}"
  log "Verified SHA-256: ${checksum}"

  mkdir -p "${mount_dir}"
  hdiutil attach "${dmg_path}" -nobrowse -quiet -mountpoint "${mount_dir}"
  [[ -d "${mount_dir}/${APP_BUNDLE}" ]] || die "${APP_BUNDLE} not found in DMG"

  log "Installing ${target_app}"
  copy_app "${mount_dir}/${APP_BUNDLE}" "${target_app}"

  remove_quarantine "${target_app}"

  install_symlinks "${target_app}"

  cat <<EOF

Readshot installed:
  ${target_app}

Command-line tools:
  ${BIN_DIR}/readshot
  ${BIN_DIR}/readshot-mcp

If ${BIN_DIR} is not in your PATH, add it to your shell profile.

Open Readshot:
  open "${target_app}"

Then grant Screen Recording permission when macOS asks.
EOF
}

if [[ "${READSHOT_INSTALL_SKIP_MAIN:-0}" != "1" ]]; then
  main "$@"
fi
