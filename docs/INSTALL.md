# Installing Readshot

Readshot can ship **Developer ID signed + notarised** on macOS when
Apple release secrets are configured; otherwise it uses the
project's stable self-signed certificate. Windows is
**SignPath-signed** when the OSS signing programme is configured, or
unsigned as a fallback. Unsigned/self-signed builds show a one-time
security prompt; subsequent launches are silent.

## macOS (14 Sonoma and later)

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
mkdir -p "$HOME/bin"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot" "$HOME/bin/readshot"
ln -sf "/Applications/Readshot.app/Contents/MacOS/readshot-mcp" "$HOME/bin/readshot-mcp"
```

Then add `~/bin` to the shell you use.

For zsh:

```bash
export PATH="$HOME/bin:$PATH"
grep -qxF 'export PATH="$HOME/bin:$PATH"' "$HOME/.zshrc" 2>/dev/null || echo 'export PATH="$HOME/bin:$PATH"' >> "$HOME/.zshrc"
```

For bash:

```bash
export PATH="$HOME/bin:$PATH"
grep -qxF 'export PATH="$HOME/bin:$PATH"' "$HOME/.bashrc" 2>/dev/null || echo 'export PATH="$HOME/bin:$PATH"' >> "$HOME/.bashrc"
```

For fish:

```fish
fish_add_path "$HOME/bin"
mkdir -p "$HOME/.config/fish"
grep -qxF 'fish_add_path "$HOME/bin"' "$HOME/.config/fish/config.fish" 2>/dev/null || echo 'fish_add_path "$HOME/bin"' >> "$HOME/.config/fish/config.fish"
```

Verify:

```bash
readshot --help
```

The menu-bar item **Install Command Line Tools…** points users back to
these commands. Readshot does not write shell symlinks automatically.

### Recommended: Homebrew Cask

```bash
brew install --cask readshot
```

Homebrew verifies the DMG's SHA-256 hash automatically — no manual
verification step needed. If the release is Developer ID signed and
notarised, double-clicking opens normally. If it is self-signed, the
first time you open Readshot, macOS Gatekeeper shows:

> "Readshot can't be opened because Apple cannot check it for malicious software."

This is expected for self-signed builds. Bypass:

1. Open **Finder → Applications**.
2. **Right-click** `Readshot.app` → **Open**.
3. Click **Open** in the warning dialog.

You only do this once. Future launches and auto-updates run silently.
The tray menu includes **Check for Updates…**; it uses Sparkle to
read the appcast published from GitHub Releases and verifies each
download with the app's compiled-in EdDSA public key.

If you'd rather use the command line:

```bash
xattr -dr com.apple.quarantine /Applications/Readshot.app
open /Applications/Readshot.app
```

### Alternative: download the DMG manually

```bash
# Replace v0.1.0 with the version you want.
curl -L -o readshot.dmg \
  https://github.com/pawanpaudel93/readshot/releases/download/v0.1.0/readshot-macos-aarch64.dmg

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

1. Click **Grant Screen Recording Access**.
2. The system prompt opens; click **Allow**.
3. The welcome window dismisses automatically.

If you accidentally denied: open **System Settings → Privacy &
Security → Screen Recording**, find Readshot, toggle it on, and
restart the app.

## Windows (10 20H1 and later)

GitHub Release assets:

* `readshot-windows-x86_64.msi` — installer.

### Recommended: Winget

```pwsh
winget install Readshot.Readshot
```

If the MSI is signed (SignPath OSS programme), Windows installs
silently. If the SignPath programme application is still pending,
you'll see SmartScreen on first launch:

> "Windows protected your PC."

Bypass: click **More info** → **Run anyway**. One-time only.

### Alternative: download the MSI manually

```pwsh
# Replace v0.1.0 with the version you want.
Invoke-WebRequest `
  -Uri https://github.com/pawanpaudel93/readshot/releases/download/v0.1.0/readshot-windows-x86_64.msi `
  -OutFile readshot.msi

# Install.
msiexec /i readshot.msi /quiet
```

## Linux

GitHub Release asset:

* `readshot-linux-x86_64.AppImage`

### Recommended: Flathub

```bash
flatpak install flathub dev.pawanpaudel93.readshot
flatpak run dev.pawanpaudel93.readshot
```

### AppImage (universal Linux)

```bash
# Replace v0.1.0 with the version you want.
curl -L -o Readshot.AppImage \
  https://github.com/pawanpaudel93/readshot/releases/download/v0.1.0/readshot-linux-x86_64.AppImage
chmod +x Readshot.AppImage
./Readshot.AppImage
```

### Arch Linux (AUR)

```bash
paru -S readshot
# or `yay -S readshot`
```

### Granting Screen Recording

On Wayland, the first capture triggers the
`xdg-desktop-portal-screencast` consent dialog. Click **Allow** to
proceed. On X11, no consent step — capture starts immediately.

If `libayatana-appindicator` is missing the tray icon won't appear;
the global hotkey still works. Install the package via your distro:

* Debian / Ubuntu: `sudo apt install libayatana-appindicator3-1`
* Arch: `sudo pacman -S libayatana-appindicator`
* Fedora: `sudo dnf install libayatana-appindicator-gtk3`

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
