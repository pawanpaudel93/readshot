# Releasing Readshot

## One-time setup

Before the first release, the maintainer does this once.

### 1. Generate the macOS self-signed certificate

```bash
# Open Keychain Access → Certificate Assistant → "Create a Certificate"
# Settings:
#   Name:           Readshot Project Self-Signed
#   Identity Type:  Self Signed Root
#   Certificate Type: Code Signing
#   Let me override defaults: ✓
#   Validity:       10 years (so we don't have to rotate often)
#   Subject Name:   CN=Readshot Project Self-Signed
```

Export the resulting cert + private key as a `.p12`, base64-encode,
and save as the `MACOS_SELF_SIGN_CERT_BASE64` GitHub Actions secret
along with the password as `MACOS_SELF_SIGN_CERT_PASSWORD`.

This is the zero-cost fallback. If the project later uses Apple
Developer ID distribution, add these GitHub Actions secrets instead:

* `APPLE_DEVELOPER_ID_P12_BASE64`
* `APPLE_DEVELOPER_ID_P12_PASSWORD`
* `MACOS_SIGNING_IDENTITY` (for example,
  `Developer ID Application: Example Name (TEAMID)`)
* `APPLE_ID`
* `APPLE_TEAM_ID`
* `APPLE_APP_SPECIFIC_PASSWORD`

When those Apple secrets are present, `packaging/macos/build-dmg.sh`
uses the Developer ID certificate, submits the DMG with
`xcrun notarytool submit --wait`, and staples the ticket with
`xcrun stapler staple`. Without them, it signs with the stable
self-signed identity and skips notarisation.

### 2. Generate the Sparkle EdDSA key pair

```bash
brew install --cask sparkle

# Homebrew installs Sparkle under its Caskroom, not always /Applications.
SPARKLE_ROOT="$(brew list --cask sparkle | awk '/\/bin\/generate_keys$/ { sub("/bin/generate_keys", ""); print; exit }')"
"${SPARKLE_ROOT}/bin/generate_keys" -x sparkle_ed_private_key
```

* Save the **private** half as `SPARKLE_ED_KEY_BASE64` (base64-encoded).
* Paste the **public** half into `packaging/macos/Info.plist`'s
  `SUPublicEDKey` slot. This commits to the repo — the public key is
  not secret.
* The packaging scripts auto-detect Sparkle's runtime framework from
  Homebrew's cask install. Override with `SPARKLE_FRAMEWORK_PATH` only
  if your local Sparkle install lives elsewhere.

### 3. Optional future: apply for SignPath OSS programme

Not needed for the macOS-only first release. When Windows packaging is
re-enabled, apply at <https://signpath.io/open-source>. Once
approved, store the credentials as GitHub Actions secrets:

* `SIGNPATH_API_TOKEN`
* `SIGNPATH_ORG_ID`
* `SIGNPATH_PROJECT_SLUG`

If the application is still pending when you cut a release, the
release workflow detects the missing token and ships an unsigned MSI
with a documented SmartScreen-bypass note. Both are acceptable per
spec §Architectural Decisions; the Apple Developer Program is **not**
joined.

### 4. Set up the GitHub Pages branch

The release workflow pushes `appcast.xml` to a `gh-pages` branch.
Create it once:

```bash
git checkout --orphan gh-pages
git rm -rf .
echo "Readshot release feed" > index.md
git add index.md
git commit -m "chore: initialise gh-pages"
git push origin gh-pages
git checkout main
```

In repository settings, point GitHub Pages at the `gh-pages` branch.

## Per-release procedure

### 1. Bump the version

Edit `[workspace.package].version` in `Cargo.toml` to the next
SemVer. Touch every relevant module if a feature changed.

```bash
sed -i '' 's/^version = "0\.1\.0"/version = "0.2.0"/' Cargo.toml
cargo build --release --workspace  # confirm it still builds
git commit -am "chore: bump version to 0.2.0"
```

### 2. Tag and push

```bash
git tag v0.2.0
git push origin main
git push origin v0.2.0
```

The `release.yml` workflow fires on the tag push, builds macOS
artefacts, creates the GitHub Release, and publishes the Sparkle
appcast to `gh-pages`.

The GitHub Release is the canonical distribution point. It publishes:

* `readshot-macos-aarch64.dmg` and `readshot-macos-x86_64.dmg`, each
  containing `Readshot.app` with both `readshot` and `readshot-mcp`.
* `SHA256SUMS`.

### 3. Verify

* Download the macOS DMG from the GitHub Release on a clean macOS
  VM. If it was self-signed, right-click → Open. If it was Developer
  ID signed and notarised, double-click should open normally. Confirm
  the welcome window appears, then use the tray menu's
  **Check for Updates…** item to confirm Sparkle can read the appcast.
* Open `https://pawanpaudel93.github.io/readshot/appcast.xml` and
  confirm it contains the new version, DMG URL, byte length, and
  `sparkle:edSignature`.
### 4. Update Homebrew Cask

The release workflow opens a PR against the homebrew-readshot tap
automatically. Review and merge.

## Hotfix releases

Patch (Z) releases follow the same procedure with a smaller version
bump. The Sparkle appcast appends another `<item>` and Sparkle
clients pick up the newest version on their next check.

## Rolling back

There is no roll-backward: Sparkle / self-check / Flatpak all prefer
the newest version. To stop distributing a bad release:

1. Delete the GitHub Release (artefacts + tag).
2. Remove the bad `<item>` from the Sparkle appcast on `gh-pages`.
3. Open hotfix issues + cut a patch release (Z+1) with the fix.

Already-updated users stay on the bad version until the patch goes
out — there is no remote-disable mechanism by design (offline-first
software shouldn't have one).
