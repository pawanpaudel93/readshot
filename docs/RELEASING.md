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

* Save the exported private key file as `SPARKLE_ED_KEY_BASE64`
  (base64-encoded). The packaging script decodes it and passes it to
  Sparkle's `sign_update --ed-key-file`.
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

Run the version bump helper with the next SemVer. It updates
`Cargo.toml`, `Cargo.lock`, macOS bundle metadata, packaging
fallbacks, and creates `docs/releases/vX.Y.Z.md` as the repo-local
release summary.

```bash
scripts/bump-version.sh 0.2.0 "One-sentence user-facing release summary."
cargo test -p readshot-capture -p readshot-core -p readshot-app -p readshot-mcp -p readshot-ocr
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
git diff --check
git add Cargo.toml Cargo.lock packaging/macos/Info.plist packaging/linux/build-appimage.sh packaging/linux/arch/PKGBUILD docs/releases/v0.2.0.md
git commit -m "chore: bump version to 0.2.0"
```

Before tagging, edit `docs/releases/v0.2.0.md` so the `Summary`
section says what changed in user terms. The release workflow uses
that file as the GitHub Release body and asks GitHub to append its
generated commit notes.

Run the local readiness check before creating the tag:

```bash
scripts/check-release.sh 0.2.0
```

It verifies local version metadata, release notes, packaging files,
installer syntax, Sparkle inputs, and release workflow assumptions.

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

If GitHub Actions is unavailable, run the same macOS release locally
from a clean checkout. The helper reads signing secrets from `.env`,
builds both DMGs, uploads the GitHub Release, and updates the Sparkle
appcast on `gh-pages`.

```bash
scripts/release-local.sh <version> --force-tag
```

Use `--force-tag` only when intentionally moving an existing tag to
the current `HEAD`.

### 3. Verify release artifacts

Download the GitHub Release artifacts into a clean directory on a
macOS VM and run:

```bash
scripts/smoke-macos-release.sh v0.2.0 /path/to/release-artifacts
```

This checks `SHA256SUMS`, mounts both DMGs, verifies the bundle
shape, confirms the `readshot://` URL scheme and Sparkle feed in
`Info.plist`, runs `codesign --verify`, and checks the published
appcast for the new version, DMG URLs, byte lengths, and
`sparkle:edSignature`.

### 4. Clean-machine smoke test

Use the same clean macOS VM. Resetting the VM snapshot between
releases is best; otherwise remove `/Applications/Readshot.app` and
run `tccutil reset ScreenCapture np.com.pawanpaudel.readshot` before
installing.

1. Open the architecture-matching DMG and drag `Readshot.app` to
   `/Applications`.
2. Launch from `/Applications`. If the release is self-signed,
   right-click -> **Open**. If it is Developer ID signed and
   notarised, double-click should open normally.
3. Confirm the welcome window appears and the app can navigate the
   user to **Privacy & Security -> Screen & System Audio Recording**.
4. Grant permission, quit, and relaunch. Confirm the welcome window
   is gone and the menu-bar icon is present.
5. Trigger a region capture from the tray menu and from the default
   hotkey. Confirm the editor opens and **Copy Text** returns either
   recognised text or the empty-text status.
6. Run `open readshot://new` while Readshot is already running.
   Confirm it opens the same interactive overlay instead of launching
   a second unusable process.
7. Open **History...** from the tray menu. Confirm the latest capture
   appears, can copy image/text, can open in the editor, and can be
   deleted.
8. Use the tray menu's **Check for Updates...** item. Confirm Sparkle
   opens its standard update UI and reads the production appcast.
9. Compare the tested behaviour with `docs/INSTALL.md`, especially
   the self-signed warning, permission flow, and update-check wording.

### 5. Update Homebrew Cask

**Not yet implemented.** Readshot is not currently distributed via
Homebrew — there is no `homebrew-readshot` tap, and the "Bump Homebrew
Cask" step in `release.yml` is a labelled no-op that publishes nothing.
There is nothing to review or merge for this step today. When a tap is
created, wire the formula-bump PR into that workflow step and update
this section.

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
