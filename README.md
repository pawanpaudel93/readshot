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
  bold), blur, pixelate, spotlight, stamps, numbered pins, crop, undo,
  and redo.
  Hold Shift for squares, circles, and 45° lines.
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

- **Capture overlay:** drag to select a region; `Enter` captures the
  full display; `esc` cancels. With a selection: `⌘C` / `⇧⌘C` / `⌘S` / `⌘P`
  copy image / copy text / save / pin, and the arrow keys nudge it (`⇧` ×10).
- **Editor tools:** `V` select, `R` rectangle, `O` ellipse, `L` line,
  `A` arrow, `P` pen, `H` highlighter, `T` text, `B` blur, `X` pixelate,
  `S` spotlight, `N` numbered pin, `E` stamp, `C` crop. `I` picks a color
  from the image (eyedropper). Arrow keys nudge the selection (hold `⇧` for
  larger steps), `⌫` removes it, `esc` deselects.
- **Editor actions:** `⌘C` copy image, `⇧⌘C` copy text, `⌘S` save,
  `⌘P` pin, `⌥⌘P` show or hide the side panel, `⌘Z` / `⇧⌘Z` undo and
  redo. `⌘W` closes; with unsaved
  edits the first press only warns; press again to discard, or click
  Keep Editing.
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
  to carry on capturing).
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
