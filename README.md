# Readshot

[![test](https://github.com/pawanpaudel93/readshot/actions/workflows/test.yml/badge.svg)](https://github.com/pawanpaudel93/readshot/actions/workflows/test.yml)
[![release](https://github.com/pawanpaudel93/readshot/actions/workflows/release.yml/badge.svg)](https://github.com/pawanpaudel93/readshot/actions/workflows/release.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Readshot is a free, offline screenshot tool for macOS with OCR,
annotation, and searchable capture history.

Website: <https://readshot.pawanpaudel.com.np>

Capture a region, mark it up, copy the image or recognized text, and
find it later by searching the text inside the screenshot.

> **Status:** packaged releases are macOS 14+ only for now. Windows and
> Linux modules exist in the source tree, but they are not wired into
> public release artifacts yet.

## Features

- Region capture from the menu bar or global hotkey.
- Multi-monitor capture with Retina-quality output.
- Annotation editor with shapes, arrows, pen, highlighter, text, blur,
  pixelate, numbered pins, crop, undo, and redo.
- Offline OCR via Apple Vision with layout-aware copied text.
- Searchable capture history with open, reveal, copy, pin, and delete
  actions.
- Pin captures as always-on-top reference windows.
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

Manual DMG, Homebrew, first-launch, and uninstall details are in
[docs/INSTALL.md](docs/INSTALL.md).

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
readshot capture --delay 2 --format jpg --output capture.jpg
readshot capture-window --window 11474 --clipboard
readshot capture-text --interactive --clipboard
readshot ocr --input capture.png --clipboard
```

For AI-agent hosts, use the bundled MCP command:

```text
readshot-mcp
```

See [docs/MCP.md](docs/MCP.md) for Claude Desktop, Cursor, OpenAI
desktop, Codex CLI, and custom-client examples.
See [docs/CLI.md](docs/CLI.md) for the full command-line workflow
reference.

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
cargo clippy --workspace -- -D warnings
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
