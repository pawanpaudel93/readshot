#!/usr/bin/env bash
# Install Readshot from the mounted DMG and clear quarantine.

set -euo pipefail

APP_NAME="Readshot"
SOURCE_DIR="$(cd "$(dirname "$0")" && pwd)"
SOURCE_APP="${SOURCE_DIR}/${APP_NAME}.app"
DEST_APP="/Applications/${APP_NAME}.app"

if [[ ! -d "${SOURCE_APP}" ]]; then
  osascript -e 'display alert "Readshot installer" message "Readshot.app was not found next to this installer. Open the Readshot DMG and run this helper from there." as critical'
  exit 1
fi

if pgrep -x readshot >/dev/null 2>&1; then
  pkill -x readshot >/dev/null 2>&1 || true
fi
if pgrep -x readshot-mcp >/dev/null 2>&1; then
  pkill -x readshot-mcp >/dev/null 2>&1 || true
fi

if [[ -d "${DEST_APP}" ]]; then
  rm -rf "${DEST_APP}"
fi

ditto "${SOURCE_APP}" "${DEST_APP}"
xattr -dr com.apple.quarantine "${DEST_APP}" 2>/dev/null || true
tccutil reset ScreenCapture dev.pawanpaudel93.readshot >/dev/null 2>&1 || true

touch "${DEST_APP}"
open "/Applications/Readshot.app"

cat <<EOF
Readshot installed to:
  ${DEST_APP}

Quarantine was removed and Screen Recording permission was reset.
If macOS asks for Screen Recording permission, allow it and restart Readshot.
EOF
