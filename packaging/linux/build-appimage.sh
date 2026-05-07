#!/usr/bin/env bash
# Build a Readshot AppImage from a pre-built release binary.
#
# Output:
#   target/release/Readshot-${VERSION}-x86_64.AppImage
#
# This script is the v1 minimum. A future revision can switch to
# `cargo-packager` once cargo-packager's AppImage path stabilises;
# until then doing it by hand keeps the dependency surface small.

set -euo pipefail

VERSION="${VERSION:-0.1.9}"
TARGET="${TARGET:-x86_64-unknown-linux-gnu}"
BIN_PATH="target/${TARGET}/release/readshot"
APP_DIR="target/release/Readshot.AppDir"
OUT="target/release/Readshot-${VERSION}-x86_64.AppImage"

if [[ ! -f "${BIN_PATH}" ]]; then
  echo "error: ${BIN_PATH} not found — run 'cargo build --release --target ${TARGET}' first" >&2
  exit 1
fi

# 1. Build the AppDir layout.
rm -rf "${APP_DIR}"
mkdir -p "${APP_DIR}/usr/bin"
mkdir -p "${APP_DIR}/usr/share/applications"
mkdir -p "${APP_DIR}/usr/share/icons/hicolor/512x512/apps"

cp "${BIN_PATH}" "${APP_DIR}/usr/bin/readshot"
chmod +x "${APP_DIR}/usr/bin/readshot"

cp packaging/linux/readshot.desktop "${APP_DIR}/readshot.desktop"
cp packaging/linux/readshot.desktop "${APP_DIR}/usr/share/applications/readshot.desktop"

cp packaging/linux/readshot.png "${APP_DIR}/readshot.png" 2>/dev/null || \
  echo "warning: packaging/linux/readshot.png missing — AppImage will be iconless"
cp packaging/linux/readshot.png "${APP_DIR}/usr/share/icons/hicolor/512x512/apps/readshot.png" 2>/dev/null || true

cat > "${APP_DIR}/AppRun" <<'EOF'
#!/usr/bin/env bash
HERE="$(dirname "$(readlink -f "$0")")"
exec "${HERE}/usr/bin/readshot" "$@"
EOF
chmod +x "${APP_DIR}/AppRun"

# 2. Fetch appimagetool (cached locally on the runner).
APPIMAGETOOL="target/release/appimagetool"
if [[ ! -f "${APPIMAGETOOL}" ]]; then
  curl -L -o "${APPIMAGETOOL}" \
    https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "${APPIMAGETOOL}"
fi

# 3. Pack.
"${APPIMAGETOOL}" "${APP_DIR}" "${OUT}"

echo "✓ AppImage ready at ${OUT}"
