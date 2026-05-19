#!/usr/bin/env bash
# Local release-readiness checks for Readshot.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION=""
ALLOW_DIRTY=0
ERRORS=0
WARNINGS=0

usage() {
  cat <<'EOF'
Usage: scripts/check-release.sh [VERSION] [--allow-dirty]

Checks local release inputs before tagging:
  - workspace/package versions
  - release notes
  - macOS packaging metadata
  - release workflow/appcast/install script presence
  - shell syntax for release and installer scripts

VERSION defaults to the workspace version from Cargo.toml.
EOF
}

ok() {
  printf 'ok: %s\n' "$*"
}

warn() {
  WARNINGS=$((WARNINGS + 1))
  printf 'warn: %s\n' "$*" >&2
}

fail() {
  ERRORS=$((ERRORS + 1))
  printf 'error: %s\n' "$*" >&2
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

require_file() {
  [[ -f "${ROOT}/$1" ]] && ok "$1 exists" || fail "$1 is missing"
}

require_contains() {
  local file="$1"
  local pattern="$2"
  local description="$3"

  if grep -Eq -- "${pattern}" "${ROOT}/${file}"; then
    ok "${description}"
  else
    fail "${description}"
  fi
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --allow-dirty)
        ALLOW_DIRTY=1
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      [0-9]*.[0-9]*.[0-9]*|v[0-9]*.[0-9]*.[0-9]*)
        VERSION="${1#v}"
        shift
        ;;
      *)
        fail "unknown argument: $1"
        shift
        ;;
    esac
  done
}

workspace_version() {
  awk '
    /^\[workspace.package\]/ { in_workspace = 1; next }
    /^\[/ { in_workspace = 0 }
    in_workspace && /^version[[:space:]]*=/ {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' "${ROOT}/Cargo.toml"
}

check_git_state() {
  if [[ "${ALLOW_DIRTY}" == "1" ]]; then
    warn "skipping clean-worktree check because --allow-dirty was passed"
    return
  fi

  if git -C "${ROOT}" diff --quiet && git -C "${ROOT}" diff --cached --quiet; then
    ok "git worktree is clean"
  else
    fail "git worktree has uncommitted changes"
  fi
}

check_tag_state() {
  local tag="$1"

  if git -C "${ROOT}" rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
    warn "local tag ${tag} already exists"
  else
    ok "local tag ${tag} is available"
  fi
}

check_script_syntax() {
  local script="$1"

  if bash -n "${ROOT}/${script}"; then
    ok "${script} syntax"
  else
    fail "${script} syntax"
  fi
}

main() {
  parse_args "$@"
  require_command awk
  require_command bash
  require_command cargo
  require_command git
  require_command grep

  if [[ -z "${VERSION}" ]]; then
    VERSION="$(workspace_version)"
  fi

  if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    fail "version must be SemVer X.Y.Z, got '${VERSION}'"
  fi

  local tag="v${VERSION}"
  printf 'Readshot release readiness: %s\n' "${tag}"

  check_git_state
  check_tag_state "${tag}"

  require_file "Cargo.toml"
  require_file "Cargo.lock"
  require_file ".github/workflows/release.yml"
  require_file "install.sh"
  require_file "packaging/macos/Info.plist"
  require_file "packaging/macos/build-dmg.sh"
  require_file "packaging/macos/update-appcast.sh"
  require_file "packaging/macos/sparkle-appcast.xml.tmpl"
  require_file "docs/RELEASING.md"
  require_file "docs/INSTALL.md"
  require_file "docs/releases/${tag}.md"

  require_contains "Cargo.toml" "version[[:space:]]*=[[:space:]]*\"${VERSION}\"" "workspace version is ${VERSION}"
  require_contains "Cargo.lock" "version = \"${VERSION}\"" "Cargo.lock contains ${VERSION}"
  require_contains "packaging/macos/Info.plist" "<string>${VERSION}</string>" "macOS Info.plist contains ${VERSION}"
  require_contains "docs/releases/${tag}.md" "Readshot ${tag}|${tag}" "release notes mention ${tag}"
  require_contains ".github/workflows/release.yml" "v\\*\\.\\*\\.\\*" "release workflow listens for SemVer tags"
  require_contains ".github/workflows/release.yml" "readshot-macos-aarch64\\.dmg" "release workflow builds arm64 DMG"
  require_contains ".github/workflows/release.yml" "readshot-macos-x86_64\\.dmg" "release workflow builds x86_64 DMG"
  require_contains ".github/workflows/release.yml" "SHA256SUMS" "release workflow publishes SHA256SUMS"
  require_contains "install.sh" "readshot-macos-\\$\\{arch\\}\\.dmg" "installer resolves architecture-specific DMG"
  require_contains "packaging/macos/Info.plist" "SUFeedURL" "Sparkle feed URL is configured"
  require_contains "packaging/macos/Info.plist" "SUPublicEDKey" "Sparkle public key is configured"

  check_script_syntax "install.sh"
  check_script_syntax "scripts/bump-version.sh"
  check_script_syntax "scripts/smoke-macos-release.sh"
  check_script_syntax "scripts/release-local.sh"
  check_script_syntax "scripts/test-install-script.sh"
  check_script_syntax "scripts/test-update-appcast.sh"
  check_script_syntax "packaging/macos/build-dmg.sh"
  check_script_syntax "packaging/macos/update-appcast.sh"

  if (( ERRORS > 0 )); then
    printf 'Release readiness failed: %d error(s), %d warning(s)\n' "${ERRORS}" "${WARNINGS}" >&2
    exit 1
  fi

  printf 'Release readiness passed: %d warning(s)\n' "${WARNINGS}"
}

main "$@"
