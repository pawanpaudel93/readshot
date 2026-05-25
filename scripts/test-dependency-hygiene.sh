#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

failures=0

check_no_fixture_dep() {
  local package="$1"

  if cargo tree --manifest-path "${ROOT}/Cargo.toml" -p "${package}" -e normal \
    | grep -q 'readshot-test-fixtures'; then
    echo "not ok - ${package} production deps include readshot-test-fixtures" >&2
    failures=$((failures + 1))
  else
    echo "ok - ${package} production deps exclude readshot-test-fixtures"
  fi
}

check_no_fixture_dep readshot-capture
check_no_fixture_dep readshot-ocr
check_no_fixture_dep readshot-app
check_no_fixture_dep readshot-mcp

if [[ "${failures}" -ne 0 ]]; then
  exit 1
fi
