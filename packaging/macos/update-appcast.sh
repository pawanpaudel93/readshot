#!/usr/bin/env bash
# Generate / update the Sparkle appcast published on gh-pages.
#
# Usage:
#   packaging/macos/update-appcast.sh <release-artifact-dir> <site-dir>
#
# Inputs:
#   TAG_NAME              Release tag, e.g. v0.2.0. Defaults to GITHUB_REF_NAME.
#   GITHUB_REPOSITORY     owner/repo, used for GitHub Release URLs.
#   GITHUB_SERVER_URL     Defaults to https://github.com.
#
# Expects one or more files named:
#   readshot-macos-<arch>.dmg
#   readshot-macos-<arch>.dmg.sparkle.eddsa.txt
#
# Sparkle's `sign_update` output contains attributes such as:
#   sparkle:edSignature="..." length="..."

set -euo pipefail

ARTIFACT_DIR="${1:?artifact directory required}"
SITE_DIR="${2:?site directory required}"
TAG_NAME="${TAG_NAME:-${GITHUB_REF_NAME:-}}"
REPOSITORY="${GITHUB_REPOSITORY:-pawanpaudel93/readshot}"
SERVER_URL="${GITHUB_SERVER_URL:-https://github.com}"

if [[ -z "${TAG_NAME}" ]]; then
  echo "error: TAG_NAME or GITHUB_REF_NAME must be set" >&2
  exit 1
fi

VERSION="${TAG_NAME#v}"
PUB_DATE="$(LC_ALL=C date -Ru)"
RELEASE_URL="${SERVER_URL}/${REPOSITORY}/releases/tag/${TAG_NAME}"
DOWNLOAD_BASE="${SERVER_URL}/${REPOSITORY}/releases/download/${TAG_NAME}"
APPCAST="${SITE_DIR}/appcast.xml"

mkdir -p "${SITE_DIR}"

existing_items=""
if [[ -f "${APPCAST}" ]]; then
  existing_items=$(
    awk -v version="${VERSION}" '
      /<item>/ { in_item = 1 }
      in_item { item = item $0 "\n" }
      /<\/item>/ {
        if (item !~ "<sparkle:version>" version "</sparkle:version>") {
          printf "%s", item
        }
        item = ""
        in_item = 0
      }
    ' "${APPCAST}"
  )
fi

new_items=""
shopt -s nullglob
dmgs=("${ARTIFACT_DIR}"/readshot-macos-*.dmg)
shopt -u nullglob

if [[ ${#dmgs[@]} -eq 0 ]]; then
  echo "error: no macOS DMG artifacts found in ${ARTIFACT_DIR}" >&2
  exit 1
fi

for dmg in "${dmgs[@]}"; do
  file_name="$(basename "${dmg}")"
  arch="${file_name#readshot-macos-}"
  arch="${arch%.dmg}"
  hardware_requirement=""
  if [[ "${arch}" == "aarch64" ]]; then
    # Readshot publishes separate Apple Silicon and Intel DMGs. Sparkle
    # needs the Apple Silicon item marked explicitly so Intel clients
    # skip it and then fall through to the x86_64 item for the same
    # version.
    hardware_requirement="            <sparkle:hardwareRequirements>arm64</sparkle:hardwareRequirements>"$'\n'
  fi
  sig_file="${dmg}.sparkle.eddsa.txt"
  if [[ ! -f "${sig_file}" ]]; then
    echo "error: missing Sparkle signature file for ${file_name}: ${sig_file}" >&2
    exit 1
  fi

  signature="$(sed -n 's/.*sparkle:edSignature="\([^"]*\)".*/\1/p' "${sig_file}" | head -n 1)"
  if [[ -z "${signature}" ]]; then
    echo "error: could not parse sparkle:edSignature from ${sig_file}" >&2
    exit 1
  fi

  length="$(wc -c < "${dmg}" | tr -d '[:space:]')"
  item=$(cat <<EOF
        <item>
            <title>Readshot ${VERSION} (${arch})</title>
            <pubDate>${PUB_DATE}</pubDate>
            <sparkle:version>${VERSION}</sparkle:version>
            <sparkle:shortVersionString>${VERSION}</sparkle:shortVersionString>
${hardware_requirement}\
            <sparkle:minimumSystemVersion>14.0</sparkle:minimumSystemVersion>
            <description><![CDATA[
                <p><a href="${RELEASE_URL}">Release notes for Readshot ${VERSION}</a></p>
            ]]></description>
            <enclosure
                url="${DOWNLOAD_BASE}/${file_name}"
                length="${length}"
                type="application/octet-stream"
                sparkle:edSignature="${signature}"
            />
        </item>
EOF
)
  new_items="${new_items}${item}"$'\n'
done

cat > "${APPCAST}" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle">
    <channel>
        <title>Readshot</title>
        <link>${SERVER_URL}/${REPOSITORY}</link>
        <description>Readshot release feed.</description>
        <language>en</language>

${new_items}${existing_items}
    </channel>
</rss>
EOF

echo "updated ${APPCAST}"
