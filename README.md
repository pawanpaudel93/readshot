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
  around, adjust opacity, lock in place, or double-click to dismiss.
- **Workflow shortcuts**: the menu-bar menu includes Retake Last
  Region for repetitive captures. The overlay toolbar includes Copy
  Text for a no-editor OCR copy flow; there is no separate tray menu
  item for text-only capture.
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
  OCR, list recent captures, retrieve the latest or a specific saved
  capture, and search history programmatically — see
  [docs/MCP.md](docs/MCP.md).

## Install (macOS)

Requires macOS 14 Sonoma or newer.

Fast install:

```bash
curl -fsSL https://raw.githubusercontent.com/pawanpaudel93/readshot/main/install.sh | bash
```

The installer downloads the latest architecture-matching DMG, verifies
it against the release `SHA256SUMS`, installs `Readshot.app` into
`/Applications`, creates `readshot` and `readshot-mcp` symlinks in
`~/.local/bin`, and clears macOS quarantine from the installed app.

Manual DMG, Homebrew, and first-launch permission details are in
[docs/INSTALL.md](docs/INSTALL.md).

Readshot includes Sparkle update checks backed by GitHub Releases.
Use the menu-bar item **Check for Updates…** after installing.

### Screen Recording

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

### Uninstall

Quit Readshot and remove:

- `/Applications/Readshot.app`
- `~/Library/Application Support/dev.pawanpaudel93.Readshot`

## Usage

Launch the menu-bar app from Applications, then use the tray icon or
the default global hotkey **⌘⇧X** to start a capture.

The installed app also includes a scriptable CLI. The one-line
installer links `readshot` and `readshot-mcp` into `~/.local/bin`; add
that directory to your `PATH` if your shell does not already include
it. Manual DMG users can run the binaries from
`/Applications/Readshot.app/Contents/MacOS/`.

Verify:

```bash
readshot --help
```

Then use:

```bash
readshot list-displays
readshot list-windows
readshot capture --output capture.png
readshot capture-window --window <id> --output window.png
readshot capture-window --window <id> --rect 100,100,800,500 --output window-region.png
readshot capture-text
readshot ocr --input capture.png
readshot capture-and-ocr --also-image capture.png
```

Use `readshot list-windows` to find a capturable window id, then pass
that id to `readshot capture-window --window <id>`. Add `--rect
x,y,width,height` to crop a region relative to the window's top-left.
Window capture is a CLI-first workflow; the tray and GUI do not expose
a window picker yet.

`readshot capture-text` is the scriptable text-only capture path: it
captures a region, runs OCR, and prints the recognised text to stdout
by default. In the GUI, use the overlay toolbar's **Copy Text** button
for the same no-editor OCR flow.

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

## Development

Build from source without installing:

```bash
cargo run --bin readshot
```

This skips the bundle assembly, so macOS won't apply the
`Info.plist` (no Screen Recording grant, no menu-bar agent). Useful
for unit tests; not useful for actual capture.

For local app testing, the repo ships a script that builds the
release binaries, assembles `Readshot.app`, installs it to
`/Applications`, and resets the Screen Recording TCC entry:

```bash
packaging/macos/install-local.sh
```

Requirements:

- macOS 14 Sonoma or newer.
- A stable Rust toolchain; `rust-toolchain.toml` pins it via `rustup`.
- Homebrew `librsvg` for icon rasterisation:
  `brew install librsvg`.

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

- **Per-release macOS smoke testing** — the artifact script and
  clean-machine checklist live in `docs/RELEASING.md`; run them before
  each public release.
- **Optional Developer ID notarisation** — only if the project later
  joins the paid Apple Developer Program. Current releases intentionally
  remain self-signed.
- **Linux + Windows capture / OCR backends** — source stubs exist, but
  public releases are macOS-only until those backends are wired and
  tested.

## License

[MIT](LICENSE).
