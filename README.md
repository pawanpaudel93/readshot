# Readshot

[![test](https://github.com/pawanpaudel93/readshot/actions/workflows/test.yml/badge.svg)](https://github.com/pawanpaudel93/readshot/actions/workflows/test.yml)
[![release](https://github.com/pawanpaudel93/readshot/actions/workflows/release.yml/badge.svg)](https://github.com/pawanpaudel93/readshot/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A free, open-source screenshot tool with on-device OCR — and a
searchable archive of everything you've captured.

Capture a region of any monitor, annotate it, copy out the image or
the recognised text (with indentation preserved), and find it again
later in a built-in history browser that searches by what's *inside*
the image. Entirely offline.

> **Status:** macOS 14+ only for now. The capture and OCR backends for
> Windows / Linux are stubs in the source tree but not wired up; this
> README only documents the platform that's actually testable today.

## Features

- **Region capture** with a transparent see-through overlay: drag to
  select, hold ⇧ for a square, press Enter for the full screen, Esc to
  cancel.
- **Multi-monitor**: one overlay window per display, positioned at the
  display's real global origin and sized to its logical bounds.
  Captures are taken at the display's HiDPI scale so Retina output
  is sharp.
- **Marching-ants selection** with a live `W × H px` size badge in
  physical pixels — what the saved PNG will actually be.
- **Annotation editor** with 12 tools (Select / Rectangle / Ellipse /
  Line / Arrow / Pen / Highlighter / Text / Blur / Pixelate / Numbered
  Pin / Crop), a 12-swatch colour palette, line-width slider, and full
  undo/redo. Crop is non-destructive — ⌘Z brings the image back.
- **Pin to desktop**: click Pin in the editor and the flattened
  capture becomes a borderless always-on-top window you can drag
  around, double-click to dismiss.
- **Native Save dialog** — pick where to save; the editor remembers
  the last directory for the rest of the session.
- **Offline OCR** via Apple Vision (`VNRecognizeTextRequest`). Copy
  Text is layout-aware — same-row fragments stay on one line, indents
  become leading spaces, and simple aligned tables become Markdown
  tables. Multi-column prose reads each column top-to-bottom.
  Terminal output and indented code become fenced blocks; YAML, basic
  tables, and two-column articles round-trip cleanly.
- **Searchable history** — every capture lands in
  `~/Library/Application Support/.../history/` as PNG + JSON sidecar,
  pre-rendered thumbnail, background OCR fills the text, and a
  **History…** browser lets you fuzzy-search by content. Per-row
  actions: open in editor, copy image, copy text, reveal in Finder,
  pin, delete; the browser can also clear all history.
- **Menu-bar app** (no Dock icon). Click the tray icon to capture, or
  right-click for the menu. Default global hotkey: **⌘⇧X**.
- **MCP server** companion binary so AI agents can request captures,
  OCR, and search/retrieve saved captures programmatically — see
  [docs/MCP.md](docs/MCP.md).

## Usage

Launch the menu-bar app from Applications, then use the tray icon or
the default global hotkey **⌘⇧X** to start a capture.

The installed app also includes a scriptable CLI. To expose `readshot`
and `readshot-mcp` in your shell, first create the symlinks:

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

Then use:

```bash
readshot list-displays
readshot capture --output capture.png
readshot ocr --input capture.png
readshot capture-and-ocr --also-image capture.png
```

For AI-agent hosts, use the installed MCP command:

```text
readshot-mcp
```

See [docs/MCP.md](docs/MCP.md) for Claude Desktop, Cursor, OpenAI
desktop, Codex CLI, and custom-client examples. Those examples use the
absolute app-bundle path, which is also valid and does not require
installing the command-line symlinks first.

## Keyboard shortcuts

While the editor window has focus:

| Key | Action |
|---|---|
| `V` | Select |
| `R` | Rectangle |
| `O` | Ellipse |
| `L` | Line |
| `A` | Arrow |
| `P` | Pen |
| `H` | Highlighter |
| `T` | Text |
| `B` | Blur |
| `X` | Pixelate |
| `N` | Numbered pin |
| `C` | Crop |
| `1`–`9` | Set line width to N px |
| `[` / `]` | Decrease / increase line width by 1 |
| `,` / `.` | Cycle previous / next colour |
| `⌘Z` / `⌘⇧Z` | Undo / redo |
| `⌘S` | Save (opens file picker) |
| `⌘W` | Discard |
| `Esc` | Cancel pending text input |

In the region overlay:

| Key | Action |
|---|---|
| Drag | Select a region |
| `⇧` (held while dragging) | Constrain to a square |
| `Enter` / `Space` | Capture the full screen |
| `Esc` | Cancel |

## Install (macOS)

Download the latest Apple Silicon or Intel DMG from
[GitHub Releases](https://github.com/pawanpaudel93/readshot/releases):

- `readshot-macos-aarch64.dmg` for Apple Silicon Macs.
- `readshot-macos-x86_64.dmg` for Intel Macs.

Open the DMG and drag `Readshot.app` to `Applications`.

The app bundle includes both command-line entry points:

- `/Applications/Readshot.app/Contents/MacOS/readshot` — GUI plus
  CLI subcommands.
- `/Applications/Readshot.app/Contents/MacOS/readshot-mcp` — MCP
  server for AI hosts. See [docs/MCP.md](docs/MCP.md).

The menu-bar item **Install Command Line Tools…** points users back to
these copy-pasteable setup commands. Readshot does not write shell
symlinks automatically. Use the setup block in **Usage** above.

Current releases are self-signed, not Apple-notarised. If macOS shows
the Gatekeeper warning, use either Finder's one-time bypass:

1. Open **Finder → Applications**.
2. Right-click `Readshot.app` → **Open**.
3. Click **Open** in the warning dialog.

Or clear quarantine from Terminal:

```bash
xattr -dr com.apple.quarantine /Applications/Readshot.app
open /Applications/Readshot.app
```

Readshot includes Sparkle update checks backed by GitHub Releases.
Use the menu-bar item **Check for Updates…** after installing.

### Build and install locally

For development, the repo ships a script that builds the binary,
assembles a `.app` bundle with the proper icon, ad-hoc codesigns it,
installs it to `/Applications`, and resets the TCC entry so Screen
Recording grants apply cleanly:

```bash
git clone https://github.com/pawanpaudel93/readshot
cd readshot
packaging/macos/install-local.sh
```

Then open the app:

```bash
open /Applications/Readshot.app
```

Requirements:

- macOS 14 (Sonoma) or newer.
- A stable Rust toolchain — `rust-toolchain.toml` pins it via
  `rustup`.
- Homebrew `librsvg` (for icon rasterisation):
  `brew install librsvg`.

### First-run permission flow

1. The app opens a small welcome window. Click **Allow Screen
   Recording** — Readshot pokes ScreenCaptureKit, which prompts
   macOS to register the bundle in *Privacy & Security → Screen &
   System Audio Recording*.
2. Click **Open System Settings** in the macOS prompt → toggle
   **Readshot** on.
3. macOS will offer **Quit & Reopen** — click it. Readshot relaunches
   into the menu bar and a system notification confirms it's alive.
   If macOS doesn't ask, the welcome window's **Restart Readshot**
   button does the same thing.

The local install script resets the TCC entry (`tccutil reset
ScreenCapture dev.pawanpaudel93.readshot`) so a fresh development
build never inherits a stale grant from an earlier ad-hoc-signed
cdhash.

### Uninstall

For a GitHub Release install, quit Readshot and remove:

- `/Applications/Readshot.app`
- `~/Library/Application Support/dev.pawanpaudel93.Readshot`

For a local development install from a repo checkout:

```bash
packaging/macos/install-local.sh --uninstall
```

## Build from source (no install)

```bash
cargo run --bin readshot
```

This skips the bundle assembly, so macOS won't apply the
`Info.plist` (no Screen Recording grant, no menu-bar agent). Useful
for unit tests; not useful for actual capture.

## Tests

```bash
cargo fmt --all -- --check
cargo nextest run --workspace
cargo clippy --workspace -- -D warnings
```

## Architecture

The source is a Cargo workspace under `crates/`:

- `readshot-core` — pure-data types: annotations, history, renderer,
  preferences, geometry.
- `readshot-capture` — `Capturer` trait + per-OS implementations.
  macOS uses Apple's `ScreenCaptureKit`.
- `readshot-ocr` — `OcrEngine` trait + per-OS impls. macOS uses
  Apple Vision.
- `readshot-ui` — testable UI primitives (editor model, tool state,
  history stack, palette, canvas program).
- `readshot-app` — the `iced` 0.14 daemon that wires everything
  together. The binary entry-point.
- `readshot-mcp` — the companion MCP server binary.

[AGENTS.md](AGENTS.md) is the high-level orientation for contributors.
[docs/MCP.md](docs/MCP.md) explains how to wire the packaged
`readshot-mcp` binary into AI-agent hosts.

## Roadmap

The product wedge — **"every screenshot you've ever taken is
searchable"** — is shipped end-to-end: capture → background OCR →
sidecar archive → searchable browser → per-row actions (open / copy
image / copy text / pin / delete).

### Remaining, in order

- **macOS release polish** — keep the GitHub Release DMG, Sparkle
  appcast, first-launch permission flow, and install docs smoke-tested
  on a clean Mac before each public release.
- **`readshot://` delivery to a running app** — opening the URL already
  works at app boot; forwarding URLs to an already-running menu-bar
  process still needs a stable AppKit event bridge.
- **Optional Developer ID notarisation** — only if the project later
  joins the paid Apple Developer Program. Current releases intentionally
  remain self-signed.
- **Linux + Windows capture / OCR backends** — source stubs exist, but
  public releases are macOS-only until those backends are wired and
  tested.

## License

[MIT](LICENSE).
