#!/usr/bin/env bash
# Build and publish a Readshot macOS release from a local machine.
#
# This is the local fallback for maintainers when GitHub Actions cannot run.
# It reads signing secrets from .env by default.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION=""
ENV_FILE="${ROOT}/.env"
FORCE_TAG=0
SKIP_APPCAST=0
PAGES_DIR=""

usage() {
  cat <<'EOF'
Usage: scripts/release-local.sh [VERSION] [--force-tag] [--env-file PATH] [--skip-appcast]

Builds both macOS DMGs locally, uploads/updates the GitHub Release, and
publishes the Sparkle appcast to gh-pages.

VERSION defaults to the workspace version from Cargo.toml.

Required .env values:
  MACOS_SELF_SIGN_CERT_BASE64
  MACOS_SELF_SIGN_CERT_PASSWORD
  SPARKLE_ED_KEY_BASE64

Use --force-tag when reusing an existing tag for the current HEAD.
EOF
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

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: required command not found: $1" >&2
    exit 1
  }
}

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "error: ${name} is required" >&2
    exit 1
  fi
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --force-tag)
        FORCE_TAG=1
        shift
        ;;
      --skip-appcast)
        SKIP_APPCAST=1
        shift
        ;;
      --env-file)
        ENV_FILE="${2:?--env-file requires a path}"
        shift 2
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
        echo "error: unknown argument: $1" >&2
        usage >&2
        exit 2
        ;;
    esac
  done
}

resolve_sparkle_root() {
  local root
  root="$(brew list --cask sparkle 2>/dev/null | awk '/\/bin\/sign_update$/ { sub("/bin/sign_update", ""); print; exit }')"
  if [[ -z "${root}" ]]; then
    echo "error: Sparkle tools not found; install with: brew install --cask sparkle" >&2
    exit 1
  fi
  echo "${root}"
}

cleanup() {
  if [[ -n "${PAGES_DIR}" && -d "${PAGES_DIR}" ]]; then
    git -C "${ROOT}" worktree remove "${PAGES_DIR}" --force >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

build_dmg() {
  local target="$1"
  local artifact="$2"

  cargo build --release --bin readshot --bin readshot-mcp --target "${target}"
  TARGET="${target}" bash "${ROOT}/packaging/macos/build-dmg.sh"
  mv "${ROOT}/target/release/readshot.dmg" "${artifact}"
  mv "${ROOT}/target/release/readshot.dmg.sparkle.eddsa.txt" "${artifact}.sparkle.eddsa.txt"
}

publish_appcast() {
  local tag="$1"
  local out_dir="$2"

  PAGES_DIR="$(mktemp -d)"
  if git -C "${ROOT}" ls-remote --exit-code --heads origin gh-pages >/dev/null 2>&1; then
    git -C "${ROOT}" fetch origin gh-pages
    git -C "${ROOT}" worktree add -B gh-pages "${PAGES_DIR}" origin/gh-pages
  else
    git -C "${ROOT}" worktree add --detach "${PAGES_DIR}" HEAD
    git -C "${PAGES_DIR}" checkout --orphan gh-pages
    git -C "${PAGES_DIR}" rm -rf . >/dev/null 2>&1 || true
  fi

  TAG_NAME="${tag}" bash "${ROOT}/packaging/macos/update-appcast.sh" "${out_dir}" "${PAGES_DIR}"
  touch "${PAGES_DIR}/.nojekyll"
  git -C "${PAGES_DIR}" add appcast.xml .nojekyll
  if git -C "${PAGES_DIR}" diff --cached --quiet; then
    echo "Sparkle appcast unchanged"
  else
    git -C "${PAGES_DIR}" commit -m "chore: publish Sparkle appcast ${tag}"
    git -C "${PAGES_DIR}" push origin HEAD:gh-pages
  fi
}

main() {
  parse_args "$@"
  cd "${ROOT}"

  require_command awk
  require_command bash
  require_command brew
  require_command cargo
  require_command gh
  require_command git
  require_command rustup
  require_command shasum

  if [[ -z "${VERSION}" ]]; then
    VERSION="$(workspace_version)"
  fi
  if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "error: version must be SemVer X.Y.Z, got '${VERSION}'" >&2
    exit 2
  fi

  local tag="v${VERSION}"
  local out_dir="${ROOT}/target/release/local-${tag}"
  local sparkle_root

  if [[ ! -f "${ENV_FILE}" ]]; then
    echo "error: env file not found: ${ENV_FILE}" >&2
    exit 1
  fi

  set -a
  # shellcheck disable=SC1090
  source "${ENV_FILE}"
  set +a

  require_env MACOS_SELF_SIGN_CERT_BASE64
  require_env MACOS_SELF_SIGN_CERT_PASSWORD
  require_env SPARKLE_ED_KEY_BASE64

  if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: git worktree has uncommitted changes" >&2
    exit 1
  fi

  bash "${ROOT}/scripts/check-release.sh" "${VERSION}"

  sparkle_root="$(resolve_sparkle_root)"
  export PATH="${sparkle_root}/bin:${PATH}"
  export SPARKLE_FRAMEWORK_PATH="${sparkle_root}/Sparkle.framework"

  rustup target add aarch64-apple-darwin x86_64-apple-darwin

  git push origin main
  if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
    if [[ "${FORCE_TAG}" != "1" ]]; then
      echo "error: ${tag} already exists; pass --force-tag to move it to HEAD" >&2
      exit 1
    fi
    git tag -f "${tag}" HEAD
    git push origin -f "${tag}"
  else
    git tag "${tag}"
    git push origin "${tag}"
  fi

  rm -rf "${out_dir}"
  mkdir -p "${out_dir}"

  build_dmg aarch64-apple-darwin "${out_dir}/readshot-macos-aarch64.dmg"
  build_dmg x86_64-apple-darwin "${out_dir}/readshot-macos-x86_64.dmg"

  (
    cd "${out_dir}"
    shasum -a 256 readshot-macos-*.dmg > SHA256SUMS
  )

  if gh release view "${tag}" >/dev/null 2>&1; then
    gh release upload "${tag}" "${out_dir}"/readshot-macos-*.dmg "${out_dir}/SHA256SUMS" --clobber
    gh release edit "${tag}" \
      --title "Readshot ${VERSION}" \
      --notes-file "${ROOT}/docs/releases/${tag}.md"
  else
    gh release create "${tag}" "${out_dir}"/readshot-macos-*.dmg "${out_dir}/SHA256SUMS" \
      --verify-tag \
      --title "Readshot ${VERSION}" \
      --notes-file "${ROOT}/docs/releases/${tag}.md"
  fi

  if [[ "${SKIP_APPCAST}" == "1" ]]; then
    echo "Skipped Sparkle appcast update."
  else
    publish_appcast "${tag}" "${out_dir}"
  fi

  echo "Local release complete: ${tag}"
  echo "Artifacts: ${out_dir}"
}

main "$@"
