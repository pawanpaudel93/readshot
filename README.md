# Readshot

A free, open-source, cross-platform screenshot tool with offline OCR.

Capture a region of the screen, annotate it (rectangle, ellipse, arrow, pen, highlighter, text, blur, pixelate, numbered pin, crop), and copy out either the image or the recognised text — all on-device.

## Status

Pre-release. Tracking an in-repo design and implementation plan; v0.1.0 is in active scaffolding.

## Supported platforms

| OS | Floor | Capture API | OCR engine |
|---|---|---|---|
| macOS | 14.0 (Sonoma) | ScreenCaptureKit | Apple Vision (`VNRecognizeTextRequest`) |
| Windows | 10 20H1 | `Windows.Graphics.Capture` | `Windows.Media.Ocr` |
| Linux | xdg-desktop-portal-aware Wayland or X11 | PipeWire portal / X11 | `ocrs` (pure Rust) |

## Install

Pre-built binaries are not yet published. Once the first release lands, the install paths will be:

- **macOS:** `brew install --cask readshot` (Homebrew Cask), or download the DMG from GitHub Releases. The DMG is self-signed; on first launch right-click the app and choose **Open**, or run `xattr -d com.apple.quarantine /Applications/Readshot.app`.
- **Windows:** `winget install Readshot`, or download the MSI from GitHub Releases.
- **Linux:** Flatpak from Flathub (`flatpak install flathub dev.pawanpaudel93.readshot`), AppImage from GitHub Releases, or AUR (`paru -S readshot`).

## Build from source

```bash
git clone https://github.com/pawanpaudel93/readshot
cd readshot
cargo run --bin readshot
```

Requires a stable Rust toolchain (the `rust-toolchain.toml` file pins it for you via rustup).

## Funding

Readshot is free, open-source, and has zero recurring cost obligations to anyone. If you'd like to support the project, sponsor on GitHub Sponsors / Open Collective (links to be added with the first release).

## Contributing

The design and implementation plan live under `docs/superpowers/` (kept locally; gitignored). Open an issue or PR for any contribution; see `CONTRIBUTING.md` (to be added) for the development workflow once v0.1 ships.

## Licence

[MIT](LICENSE).
