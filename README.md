# Readshot

[![test](https://github.com/pawanpaudel93/readshot/actions/workflows/test.yml/badge.svg)](https://github.com/pawanpaudel93/readshot/actions/workflows/test.yml)
[![release](https://github.com/pawanpaudel93/readshot/actions/workflows/release.yml/badge.svg)](https://github.com/pawanpaudel93/readshot/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Readshot is a free, offline screenshot tool for macOS with scrolling
capture, OCR, annotation, and searchable capture history.

Website: <https://readshot.pawanpaudel.com.np>

Capture a region, mark it up, copy the image or recognized text, and
find it later by searching the text inside the screenshot.

> **Status:** packaged releases are macOS 14+ only for now. Windows and
> Linux modules exist in the source tree, but they are not wired into
> public release artifacts yet.

## Features

- Region, window, and full-display capture from the menu bar or a
  global hotkey.
- Scrolling capture — lock a region, scroll the page, get one tall
  stitched image with a live HUD preview.
- Multi-monitor capture with Retina-quality output.
- Annotation editor with shapes (filled, dashed, rounded), curved and
  hand-drawn arrows, pen, highlighter, multi-line text (sizes, pill,
  bold), blur, pixelate, spotlight, stamps, numbered pins, magnifier, crop
  (free or 16:9 / 4:3 / 1:1), undo, and redo.
  Hold Shift for squares, circles, and 45° lines.
- An **Image** menu to resize, rotate, and flip a capture, or combine it
  with another capture or an image file.
- A side panel with a **Text** tab (recognized lines to copy, and
  one-click redaction of sensitive text) and a **Beautify** tab
  (background, padding, rounded corners, shadow, canvas size, and
  presets).
- Offline OCR via Apple Vision with layout-aware copied text and a
  choice of recognition languages.
- Searchable capture history with open, reveal, copy, pin, and delete
  actions.
- Pin captures as always-on-top reference windows.
- In-app updates via "Check for Updates…" in the menu bar.
- Clean uninstall from Settings, `readshot uninstall`, or the installer
  script — including the login item, CLI links, and permission.
- Scriptable CLI with interactive capture, clipboard output, OCR,
  delay timers, image formats, and bundled MCP server for AI-agent
  workflows.

## Install

Requires macOS 14 Sonoma or newer.

```bash
curl -fsSL https://readshot.pawanpaudel.com.np/install.sh | bash
```

The installer downloads the latest matching DMG, verifies it, installs
`Readshot.app` into `/Applications`, and links `readshot` plus
`readshot-mcp` into `~/.local/bin`.

Manual DMG, first-launch, and uninstall details are in
[docs/INSTALL.md](docs/INSTALL.md).

To uninstall, use **Settings → Uninstall Readshot…**, or run
`readshot uninstall` (add `--purge` to also delete history and
preferences).

## Usage

Launch Readshot from Applications, then use the menu-bar icon or the
default hotkey:

```text
Command + Shift + X
```

CLI examples:

```bash
readshot --help
readshot list-displays
readshot list-windows
readshot capture --interactive --output capture.png
readshot capture --interactive --output capture.png --json
readshot capture-window --window 11474 --clipboard
readshot capture-text --interactive --clipboard
readshot ocr --input capture.png --json
readshot mcp-config
readshot completions zsh
```

For AI-agent hosts, use the bundled MCP command:

```text
readshot-mcp
```

See [docs/MCP.md](docs/MCP.md) for Claude Desktop, Cursor, OpenAI
desktop, Codex CLI, and custom-client examples.
See [docs/CLI.md](docs/CLI.md) for the full command-line workflow
reference.

## Accessibility

Readshot's graphical interface is built on the iced 0.14 UI toolkit,
which does not yet expose an accessibility tree. As a result, screen
readers such as VoiceOver cannot read the app's buttons, menus, or
editor controls. This is a toolkit limitation rather than a design
choice, and it should improve as iced's accessibility support matures.

Every capture and OCR feature is also available from the command line,
which does work with screen readers:

```bash
readshot capture --interactive --output capture.png   # capture a region to a file
readshot capture-text --interactive --clipboard       # capture a region and copy its text
readshot ocr --input capture.png --output text.txt    # OCR an existing image
```

See [docs/CLI.md](docs/CLI.md) for the complete command reference.

Inside the GUI, keyboard shortcuts cover the main workflows:

- **Capture overlay:** drag to select a region, or press an arrow key to
  start a centred one; `Enter` captures the full display; `esc` cancels.
  With a selection: `⌘C` / `⇧⌘C` / `⌘S` / `⌘P` / `⌘R` copy image / copy
  text / save / pin / start a scrolling capture, the arrow keys nudge it and
  `⌥` + arrow keys resize it from its bottom-right corner (`⇧` ×10).
- **Editor tools:** `V` select, `R` rectangle, `O` ellipse, `L` line,
  `A` arrow, `P` pen, `H` highlighter, `T` text, `B` blur, `X` pixelate,
  `S` spotlight, `N` numbered pin, `E` stamp, `M` magnifier (drag over what to enlarge; drag the bubble anywhere), `C` crop (Free / 16:9 / 4:3 / 1:1 in the options bar; `↵` applies,
  `esc` cancels). `I` picks a color
  from the image (eyedropper). Arrow keys nudge the selection (hold `⇧` for
  larger steps), `⌫` removes it, `esc` deselects (during a drag, `esc`
  cancels only the drag). `Tab` / `⇧Tab` select the next / previous
  annotation; `⌥` + arrow keys resize the selection from its bottom-right
  corner (a selected magnifier moves only its bubble). Pressing `C` starts a
  crop area you can move with the arrow keys and size with `⌥` + arrows.
- **Editor actions:** `⌘C` copy image, `⇧⌘C` copy all text (the side panel's Text tab also copies selected lines), `⌘S` save,
  `⌘P` pin, `⌥⌘P` show or hide the side panel, `⌘Z` / `⇧⌘Z` undo and
  redo, `⌘[` / `⌘]` rotate left / right, `⇧⌘H` / `⇧⌘V` flip horizontally /
  vertically, `⌥⌘I` resize (focus starts in Width; `Tab` moves between the
  fields, `⌥P` switches px / %, `⌥L` locks the aspect), `⇧⌘I` opens the Image
  menu (`↑` / `↓` choose a row, `↵` runs it; `-` / `+` change the gap and
  `,` / `.` the alignment of a combined image). The title bar's
  **Image** menu resizes, rotates and flips the capture, and combines it
  with another capture (Add Capture Below / Beside) or an image file; gap
  and alignment are adjustable, and `⌘Z` removes an added part. `⌘W` closes; with unsaved
  edits the first press only warns; press again to discard, or click
  Keep Editing.
- **Editor keyboard focus:** `⌃Tab` / `F6` move the keyboard between the
  canvas, the options bar and the side panel (`⌃⇧Tab` / `⇧F6` go back);
  `⌥⌘T` / `⌥⌘B` open the side panel's Text / Beautify tab. Inside the bar or
  the panel, `Tab` / `⇧Tab` (or `↓` / `↑`) move between controls, `←` / `→`
  change the focused value (`⇧` for steps of 10 on sliders), `↵` or `Space`
  press it, and `esc` returns to the canvas. In the Text tab's line list,
  `↑` / `↓` move between lines (`⇧` extends the selection), `Space` selects,
  `↵` or `⌘C` copies and `R` redacts the line. A ring marks the focused
  control. On Windows and Linux, `Ctrl` replaces `⌘` and `Alt` replaces `⌥`.
- **History window:** `↑` / `↓` move between captures, `Enter` opens the
  selected one, `⌘C` copies its image and `⇧⌘C` its text, `⌘⌫` deletes it,
  `⌘F` focuses the search, `esc` clears the search.
- **Settings window:** `⌘1`–`⌘7` switch panes; `esc` / `⌘W` close; `⌘L`
  toggles launch-at-login and `⌘D` toggles debug logging; `⌘R`, `⌘E`, `⌘F`
  cycle retention, OCR engine, and format; `⇧⌘R` resets and `⇧⌘U` uninstalls
  (press twice to confirm). Each shortcut also brings up the pane its setting
  lives on.
- **Scrolling-capture window:** `esc` stops and stitches the session;
  `⇧esc` or `⌘.` discards it (press twice to confirm, or click Keep Going
  to carry on capturing; the confirmation waits for your answer). While a
  session runs, `esc` and `⇧esc` work from any app, so you don't have to
  click the window first.
- **Pinned windows:** `esc` / `⌘W` close, `L` lock, `-` / `+` adjust opacity,
  arrow keys move the window by 1 pixel (hold `⇧` for 10).
- **Welcome screen:** `Enter` triggers the primary action (Allow / Open System
  Settings / Capture).

Readshot also respects the system Reduce Motion setting, dropping non-essential
animation when it is enabled.

## Development

Build from source without installing:

```bash
cargo run --bin readshot
```

For local app testing:

```bash
packaging/macos/install-local.sh
```

Run checks:

```bash
cargo fmt --all -- --check
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Useful docs:

- [AGENTS.md](AGENTS.md) — contributor orientation and repo layout.
- [docs/INSTALL.md](docs/INSTALL.md) — install paths and first-launch notes.
- [docs/CLI.md](docs/CLI.md) — command-line capture and OCR recipes.
- [docs/MCP.md](docs/MCP.md) — MCP server setup.
- [docs/ROADMAP.md](docs/ROADMAP.md) — planned screenshot workflow improvements.
- [docs/RELEASING.md](docs/RELEASING.md) — release runbook.

## License

[MIT](LICENSE)
