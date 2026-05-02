# Readshot

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
  tables. Lossy on multi-column docs (see Roadmap), but terminal
  output, indented code, YAML, and basic tables round-trip cleanly.
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
  `docs/MCP.md`.

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

## Install (macOS, dev build)

There's no notarised release yet. The repo ships a script that
builds the binary, assembles a `.app` bundle with the proper icon,
ad-hoc codesigns it, installs it to `/Applications`, and resets
the TCC entry so Screen Recording grants apply cleanly:

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
   If macOS doesn't ask, the welcome window's **Restart Readshot
   now** button does the same thing.

The TCC entry is reset on every install (`tccutil reset
ScreenCapture dev.pawanpaudel93.readshot`) so a fresh build never
inherits a stale grant from an earlier ad-hoc-signed cdhash.

### Uninstall

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

`AGENTS.md` is the high-level orientation for any contributor; the
per-task design notes live under `docs/superpowers/` (gitignored).

## Roadmap

The product wedge — **"every screenshot you've ever taken is
searchable"** — is shipped end-to-end: capture → background OCR →
sidecar archive → searchable browser → per-row actions (open / copy
image / copy text / pin / delete). What's left is polish, parity,
and the bits we deliberately parked.

### Older roadmap (still real)

- **`readshot://` URL scheme while running** — argv-based delivery
  works at boot; in-process delivery via `NSAppleEventManager` is
  blocked on a shared `objc2` major-version pin across `tray-icon`,
  `muda`, and `iced_winit`.
- **Sparkle update plumbing** — `Info.plist` carries the feed URL
  but the appcast pipeline isn't wired.
- **Notarised DMG + Developer ID signing** for distribution. The
  current build is ad-hoc-signed and only runs on the build
  machine.
- **Linux + Windows capture / OCR backends** — stubbed.

### Parked (the AI-native wedge + structured OCR)

- **"Send to assistant" intent** — opens Claude / ChatGPT with the
  capture inline.
- **Structured OCR — code blocks → fenced.** Detecting monospace
  from raster pixels is unreliable; would need a heuristic or a
  separate model.
- **Multi-column reading order.** Two-column documents currently
  read top-down per column instead of zig-zagging across rows. The
  row-clustering pass would have to detect column boundaries first.

These layer cleanly on the wedge, so they're deliberately deferred
rather than abandoned.

## Licence

[MIT](LICENSE).
