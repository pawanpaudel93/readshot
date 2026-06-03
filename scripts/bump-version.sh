#!/usr/bin/env bash
# Bump Readshot's release version in every file that carries package
# metadata. This intentionally does not commit or tag; it leaves those
# release decisions visible to the maintainer.

set -euo pipefail

usage() {
  cat >&2 <<'EOF'
Usage: scripts/bump-version.sh <major.minor.patch> [release-summary]

Updates:
  - Cargo.toml workspace package version
  - Cargo.lock workspace crate versions
  - packaging/macos/Info.plist bundle versions
  - packaging/linux/build-appimage.sh fallback VERSION
  - packaging/linux/arch/PKGBUILD pkgver
  - docs/releases/v<major.minor.patch>.md release summary draft

Then run release verification, commit, tag, and push.
EOF
}

if [[ $# -lt 1 || $# -gt 2 ]]; then
  usage
  exit 64
fi

VERSION="$1"
TAG="v${VERSION}"
SUMMARY="${2:-TODO: Write the user-facing release summary before tagging.}"

if [[ ! "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: version must be SemVer major.minor.patch, got '${VERSION}'" >&2
  exit 64
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree must be clean before bumping the release version" >&2
  git status --short >&2
  exit 1
fi

if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  echo "error: local tag ${TAG} already exists" >&2
  exit 1
fi

CURRENT_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
if [[ -z "${CURRENT_VERSION}" ]]; then
  echo "error: could not read current workspace version from Cargo.toml" >&2
  exit 1
fi
PREVIOUS_TAG="$(git describe --tags --abbrev=0 --match 'v[0-9]*.[0-9]*.[0-9]*' 2>/dev/null || true)"
if [[ -n "${PREVIOUS_TAG}" ]]; then
  COMMIT_RANGE="${PREVIOUS_TAG}..HEAD"
else
  COMMIT_RANGE="HEAD"
fi

perl -0pi -e "s/(\\[workspace\\.package\\]\\nversion = \")[^\"]+(\")/\${1}${VERSION}\${2}/" Cargo.toml
perl -0pi -e "s/(<key>CFBundleShortVersionString<\\/key>\\s*<string>)[^<]+(<\\/string>)/\${1}${VERSION}\${2}/" packaging/macos/Info.plist
perl -0pi -e "s/(<key>CFBundleVersion<\\/key>\\s*<string>)[^<]+(<\\/string>)/\${1}${VERSION}\${2}/" packaging/macos/Info.plist
perl -0pi -e "s/VERSION=\"\\\${VERSION:-[0-9]+\\.[0-9]+\\.[0-9]+}\"/VERSION=\"\\\${VERSION:-${VERSION}}\"/" packaging/linux/build-appimage.sh
perl -0pi -e "s/^pkgver=.*/pkgver=${VERSION}/m" packaging/linux/arch/PKGBUILD

mkdir -p docs/releases
RELEASE_NOTES="docs/releases/${TAG}.md"
if [[ -e "${RELEASE_NOTES}" ]]; then
  echo "error: ${RELEASE_NOTES} already exists" >&2
  exit 1
fi

{
  echo "# Readshot ${TAG}"
  echo
  echo "## Summary"
  echo
  echo "- ${SUMMARY}"
  echo
  echo "## Changes"
  echo
  git log --reverse --pretty=format:'- %s (%h)' "${COMMIT_RANGE}" || true
} > "${RELEASE_NOTES}"

cargo check --workspace >/dev/null

cat <<EOF
Bumped Readshot from ${CURRENT_VERSION} to ${VERSION}.
Created ${RELEASE_NOTES}.

Next:
  cargo test -p readshot-capture -p readshot-core -p readshot-app -p readshot-mcp -p readshot-ocr
  cargo clippy --workspace --all-targets -- -D warnings
  cargo fmt --all -- --check
  git diff --check
  git add Cargo.toml Cargo.lock packaging/macos/Info.plist packaging/linux/build-appimage.sh packaging/linux/arch/PKGBUILD ${RELEASE_NOTES}
  git commit -m "chore: bump version to ${VERSION}"
  git tag ${TAG}
  git push origin main
  git push origin ${TAG}
EOF
