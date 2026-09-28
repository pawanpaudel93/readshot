# Installing Readshot

Readshot's official packaged releases are **macOS-only** right now.
Windows and Linux source modules exist in the repository, but they are
not wired into public release artifacts yet.

macOS releases can ship **Developer ID signed + notarised** when Apple
release secrets are configured; otherwise they use the project's
stable self-signed certificate. Unsigned/self-signed builds show a
one-time security prompt; subsequent launches are silent.

## macOS (14 Sonoma and later)

### Recommended: one-line installer

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash
```

The installer detects Apple Silicon vs Intel, downloads the matching
GitHub Release DMG, verifies it against `SHA256SUMS`, installs
`Readshot.app` into `/Applications`, creates `readshot` and
`readshot-mcp` symlinks in `~/.local/bin`, and removes macOS
quarantine from the installed app by running
`xattr -dr com.apple.quarantine /Applications/Readshot.app`.

To pin a specific version instead of the latest, pass the version tag
(replace `<version>` with e.g. `0.8.0`):

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash -s -- <version>
```

To check compatibility, release availability, and the current local
install state without installing:

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash -s -- --check
```

GitHub Release asset:

* `readshot-macos-aarch64.dmg` for Apple Silicon Macs.
* `readshot-macos-x86_64.dmg` for Intel Macs.

The DMG installs `Readshot.app`. The app bundle includes both
executables:

* `/Applications/Readshot.app/Contents/MacOS/readshot` — GUI and CLI
  subcommands.
* `/Applications/Readshot.app/Contents/MacOS/readshot-mcp` — MCP
  server for AI hosts.

To make those commands available as `readshot` and `readshot-mcp`,
first create the symlinks:

```bash
mkdir -p "$HOME/.local/bin"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot" "$HOME/.local/bin/readshot"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot-mcp" "$HOME/.local/bin/readshot-mcp"
```

Then add `~/.local/bin` to the shell you use.

For zsh:

```bash
export PATH="$HOME/.local/bin:$PATH"
grep -qxF 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/.zshrc" 2>/dev/null || echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.zshrc"
```

For bash:

```bash
export PATH="$HOME/.local/bin:$PATH"
grep -qxF 'export PATH="$HOME/.local/bin:$PATH"' "$HOME/.bashrc" 2>/dev/null || echo 'export PATH="$HOME/.local/bin:$PATH"' >> "$HOME/.bashrc"
```

For fish:

```fish
fish_add_path "$HOME/.local/bin"
mkdir -p "$HOME/.config/fish"
grep -qxF 'fish_add_path "$HOME/.local/bin"' "$HOME/.config/fish/config.fish" 2>/dev/null || echo 'fish_add_path "$HOME/.local/bin"' >> "$HOME/.config/fish/config.fish"
```

Verify:

```bash
readshot --help
readshot capture --interactive --output capture.png
```

Readshot does not run shell setup automatically; use the commands above
if you want terminal access.

### First launch

If the release is Developer ID signed and notarised, double-clicking
opens normally. If it is self-signed, the first time you open Readshot,
macOS Gatekeeper shows:

> "Readshot can't be opened because Apple cannot check it for malicious software."

This is expected for self-signed builds. Bypass:

1. Open **Finder → Applications**.
2. **Right-click** `Readshot.app` → **Open**.
3. Click **Open** in the warning dialog.

You only do this once. Future launches open normally. The tray menu
includes **Check for Updates…** on macOS; it uses Sparkle to read the
appcast published from GitHub Releases and verifies each download with
the app's compiled-in EdDSA public key. Readshot does not check for
updates in the background unless a future version adds an explicit
preference for it.

If you'd rather use the command line:

```bash
xattr -dr com.apple.quarantine /Applications/Readshot.app
open /Applications/Readshot.app
```

### Alternative: download the DMG manually

```bash
# Downloads the latest release. For a specific version, swap
# `latest/download` for `download/v<version>`.
curl -L -o readshot.dmg \
  https://github.com/pawanpaudel93/readshot/releases/latest/download/readshot-macos-aarch64.dmg

# Verify the hash against the value in the GitHub Release notes.
shasum -a 256 readshot.dmg

# Install.
hdiutil attach readshot.dmg
cp -R "/Volumes/Readshot/Readshot.app" /Applications/
hdiutil detach "/Volumes/Readshot"
```

Then follow the right-click → Open step above on first launch if the
release is self-signed.

### Granting Screen Recording

Readshot's overlay needs the macOS Screen Recording permission. The
welcome window walks you through it on first launch:

1. Click **Allow Screen Recording**.
2. The system prompt opens; click **Allow**.
3. The welcome window dismisses automatically.

If you accidentally denied: open **System Settings → Privacy &
Security → Screen Recording**, find Readshot, toggle it on, and
restart the app.

## Uninstalling

Readshot can remove itself, so you never have to hunt down the pieces
it installs (the app bundle, the `readshot` / `readshot-mcp` symlinks,
the launch-at-login item, and the Screen Recording permission).

### In the app

Open **Settings → Uninstall → Uninstall Readshot…**. A confirmation
step appears with an **Also delete history and preferences** checkbox;
tick it to remove your data too. Click **Uninstall** and Readshot
performs the cleanup and quits.

### With the CLI

```bash
readshot uninstall --dry-run     # preview the plan, change nothing
readshot uninstall               # confirm interactively, then remove
readshot uninstall --purge       # also delete history, logs, preferences
readshot uninstall --yes         # skip the confirmation prompt (scripts)
```

`--dry-run` prints exactly what would be removed. Without `--yes`, the
command asks for confirmation on a terminal and refuses to run
non-interactively.

### With the installer script

The same installer can uninstall:

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash -s -- --uninstall
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash -s -- --uninstall --purge
```

If you installed into custom locations, pass the same `--install-dir`
and `--bin-dir` you used to install.

### Manually

```bash
# Launch-at-login item (current and, if present, the legacy id).
launchctl bootout "gui/$(id -u)/np.com.pawanpaudel.readshot.login" 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/np.com.pawanpaudel.readshot.login.plist"
rm -f "$HOME/Library/LaunchAgents/dev.pawanpaudel93.readshot.login.plist"

# Command-line symlinks (only if they point into Readshot.app).
rm -f "$HOME/.local/bin/readshot" "$HOME/.local/bin/readshot-mcp"

# Screen Recording permission.
tccutil reset ScreenCapture np.com.pawanpaudel.readshot

# The app itself.
rm -rf /Applications/Readshot.app

# Optional: history, logs, and preferences.
rm -rf "$HOME/Library/Application Support/np.com.pawanpaudel.Readshot"
```

## Windows and Linux

There are no official Windows or Linux packages yet. Do not expect a
published MSI, Winget package, Flatpak, AppImage, or AUR package for
the current public release.

Developers can still build from source on those platforms, but the
end-user install path is macOS until the platform capture and OCR
backends are wired and tested.

## Building from source

Requires a stable Rust toolchain (matched to `rust-toolchain.toml`).

```bash
git clone https://github.com/pawanpaudel93/readshot
cd readshot
cargo run --release --bin readshot
```

On Linux you need the system packages listed in
`.github/workflows/test.yml` under "Install Linux system
dependencies".
