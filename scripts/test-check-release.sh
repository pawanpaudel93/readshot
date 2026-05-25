#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(
  awk '
    /^\[workspace.package\]/ { in_workspace = 1; next }
    /^\[/ { in_workspace = 0 }
    in_workspace && /^version[[:space:]]*=/ {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' "${ROOT}/Cargo.toml"
)"
TAG="v${VERSION}"

failures=0
created_tag=0

cleanup() {
  if [[ "${created_tag}" == "1" ]]; then
    git -C "${ROOT}" tag -d "${TAG}" >/dev/null 2>&1 || true
  fi
  rm -f /tmp/readshot-check-release.out
}
trap cleanup EXIT

if git -C "${ROOT}" rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
  echo "ok - existing release tag is already present; stale-tag regression skipped"
  exit 0
fi

if ! stale_ref="$(git -C "${ROOT}" rev-parse HEAD^ 2>/dev/null)"; then
  echo "ok - repository has no parent commit; stale-tag regression skipped"
  exit 0
fi
git -C "${ROOT}" tag "${TAG}" "${stale_ref}"
created_tag=1

if "${ROOT}/scripts/check-release.sh" "${VERSION}" --allow-dirty >/tmp/readshot-check-release.out 2>&1; then
  echo "not ok - existing release tag away from HEAD is rejected" >&2
  failures=$((failures + 1))
else
  if grep -q "does not point at HEAD" /tmp/readshot-check-release.out; then
    echo "ok - existing release tag away from HEAD is rejected"
  else
    echo "not ok - expected tag/HEAD mismatch message" >&2
    cat /tmp/readshot-check-release.out >&2
    failures=$((failures + 1))
  fi
fi

if [[ "${failures}" -ne 0 ]]; then
  exit 1
fi
