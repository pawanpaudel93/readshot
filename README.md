# Readshot

A free, open-source screenshot tool with on-device OCR.

Capture a region of any monitor, annotate it (rectangle, ellipse, line,
arrow, pen, highlighter, text, blur, pixelate, numbered pin, crop), and
copy out either the image or the recognised text — entirely offline.

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
- **Offline OCR** via Apple Vision (`VNRecognizeTextRequest`). Click
  **Copy Text** in the editor and the recognised text lands on your
  clipboard.
- **Menu-bar app** (no Dock icon). Click the tray icon to capture, or
  right-click for the menu.
- **MCP server** companion binary so AI agents can request captures
  and OCR programmatically — see `docs/MCP.md`.

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

1. The app opens a small welcome window. Click **Open System
   Settings** — Readshot registers itself with TCC and opens
   *Privacy & Security → Screen Recording*.
2. Toggle **Readshot** on.
3. Switch back to Readshot and click **Restart Readshot now**. The
   daemon relaunches, picks up the grant, dismisses the welcome
   window, and shows a system notification confirming it lives in
   the menu bar from here on.

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

The product wedge is **"every screenshot you've ever taken is
searchable"** — an on-device, OCR-indexed history archive. Most of
that loop is built; the items below are what's left.

### Closing out the history wedge

- **Editor → history sync** — annotations made in the editor after a
  capture aren't yet written back into the saved record. Original
  pixels persist; annotations don't.
- **Settings UI for retention** — `Last50` is hardcoded today; expose
  the existing `Off / Last50 / Last30Days / Unlimited` choices.
- **Reveal in Finder + Clear all** affordances on the browser.
- **Pre-rendered thumbnails** — current thumbs decode the full PNG
  off-thread on first show; fine for 50, would chug at 500.

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

### Parked (the AI-native wedge)

- **"Send to assistant" intent** — opens Claude / ChatGPT with the
  capture inline.
- **Structured OCR output** — markdown for tables, fenced code blocks
  for code, instead of flat text.
- **MCP `search_captures` / `recent_captures`** — exposing the
  history archive to AI agents via the existing companion server.

These layer cleanly on the wedge once it's complete, so they're
deliberately deferred rather than abandoned.

## Licence

[MIT](LICENSE).
