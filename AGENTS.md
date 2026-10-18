# AGENTS.md — Notes for AI coding assistants

This repository follows the [agents.md](https://agents.md) convention.
Tools that read this file (Cursor, Continue, Aider, OpenAI Codex CLI,
the `Agent` SDK, …) get a fast, accurate orientation here.

If you are **end-user-facing** AI software that wants to ask Readshot
to take screenshots and recognise text on behalf of a user, you do
**not** want this file. Read `docs/MCP.md` instead — that document
explains how to wire the `readshot-mcp` server into your host (Claude
Desktop, Cursor, OpenAI desktop, custom client).

---

## What Readshot is

Readshot is a cross-platform screenshot + offline OCR application,
written in Rust. It runs on macOS, Windows, and Linux. Three
external entry points:

| Surface       | Crate              | Binary             | Use case                       |
|---------------|--------------------|--------------------|--------------------------------|
| GUI           | `readshot-app`     | `readshot`         | Interactive screen capture     |
| CLI           | `readshot-app`     | `readshot capture` | Scriptable / shell automation  |
| MCP server    | `readshot-mcp`     | `readshot-mcp`     | AI agent stdio tool surface    |

A `readshot://` URL scheme opens the interactive overlay (GUI only).

## Repository layout

```
crates/
  readshot-core/          Domain types, geometry, preferences, history,
                          render pipeline. No platform code.
  readshot-capture/       Capturer trait + per-OS backends
                          (screencapturekit on macOS, xcap on win/linux).
  readshot-ocr/           OCREngine trait + per-OS backends
                          (Apple Vision, Windows.Media.Ocr, ocrs on linux).
  readshot-ui/            iced widgets and models: the annotation editor
                          (state, canvas, toolbar, undo), settings, hotkey.
  readshot-app/           Composition root. App state, iced runtime,
                          capture overlay, tray, coordinator, permissions,
                          URL scheme, headless CLI.
  readshot-mcp/           Model Context Protocol stdio server.
  readshot-test-fixtures/ Shared image fixtures for snapshot tests.
docs/
  INSTALL.md              End-user install + first-launch instructions.
  RELEASING.md            Maintainer release runbook.
  MCP.md                  How to wire readshot-mcp into AI-agent hosts.
  CLI.md                  The command-line reference.
  ROADMAP.md              Near-term workflow gaps (not a release promise).
packaging/                Per-OS packaging configs (DMG, MSI, AppImage,
                          Flatpak, AUR, Sparkle appcast).
.github/workflows/        CI test matrix + tag-driven release pipeline.
```

## How to build & test

```bash
cargo build --workspace
cargo nextest run --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

System dependencies on Linux (matched to `.github/workflows/test.yml`):
`libxcb`, `libxkbcommon`, `wayland`, `dbus`, `fontconfig`,
`libayatana-appindicator3-dev`, `gtk3`, `libpipewire-0.3-dev`
(newer than Ubuntu 22.04's 0.3.48; 24.04 works), `clang`/`libclang-dev`
(bindgen), `libxdo-dev` and `libgbm-dev`. macOS and Windows need no extra system
packages beyond a stable Rust toolchain.

## House style for code changes

* **Errors are typed, never `anyhow`.** Each crate ships its own
  `thiserror` enums (`CaptureError`, `OCRError`, `PreferencesError`,
  `HistoryError`, `CliError`, …). Wrap external errors at the boundary;
  do not bubble `anyhow::Error` across crates.
* **Per-OS code is `cfg`-gated.** Unsupported targets emit
  `compile_error!`. Look at `readshot-capture::default_capturer` for
  the canonical shape.
* **Tests use fakes.** `FakeCapturer`, `FakeOcrEngine`, and
  `FakePermissions` live next to their traits. Any new test that hits
  the real OS belongs in a `#[cfg(target_os = "...")]` smoke-test
  module or behind the `[run integration]` CI gate.
* **Snapshots use `insta`.** `cargo insta review` to accept changes;
  PNG snapshots are stored as `*.snap.png` next to the test source.
* **Commit straight to `main`.** No per-task feature branches in this
  repo. CI gates apply on push.
* **Never include `Co-Authored-By` lines in commit messages.**

## Common changes & where they land

| Change                              | Likely files                                   |
|-------------------------------------|------------------------------------------------|
| New annotation tool                 | `readshot-core::annotation`, `readshot-ui::editor` |
| Editor message handling             | `readshot-app::runtime::editor_ops`, `readshot-app::editor` |
| Image menu (resize, rotate, flip)   | `readshot-app::runtime::image_menu`, `readshot-core::document` |
| Combine (add capture / image file)  | `readshot-app::runtime::combine`, `readshot-core::document` |
| Crop presets, tool options bar      | `readshot-ui::editor::style`, `readshot-app::runtime::editor_options` |
| Magnifier                           | `readshot-core::magnifier`, `readshot-ui::editor::canvas` |
| Beautify (background, shadow, …)    | `readshot-core::beautify`, `readshot-app::runtime::beautify_panel` |
| Side panel Text tab, Redact         | `readshot-app::runtime::text_panel`, `readshot-core::{redact,scrub,text_lines}` |
| Large-document responsiveness       | `readshot-app::runtime::compose_jobs`          |
| New OCR engine option               | `readshot-ocr` + `default_engine` switch       |
| New capture backend                 | `readshot-capture` (mind the `Capturer` trait) |
| New CLI subcommand                  | `readshot-app::cli`                            |
| New MCP tool                        | `readshot-mcp` (lib.rs `tool_*` + descriptors) |
| New preference field                | `readshot-core::preferences` (mind round-trip) |
| New keyboard shortcut               | `readshot-ui::hotkey` + `readshot-app::app`    |
| Per-OS packaging tweak              | `packaging/<os>/` and `release.yml`            |

## The editor at a glance

The editor opens a capture or a History record. Besides the
annotation tools (shapes, arrows, pen, highlighter, text, blur,
pixelate, spotlight, stamps, numbered pins, magnifier `M`, crop `C` with
Free / 16:9 / 4:3 / 1:1 presets) it has:

* the title bar's **Image** menu: resize (`⌥⌘I`), rotate (`⌘[` / `⌘]`),
  flip, and combine with another capture (below / beside) or an image
  file, with adjustable gap and alignment;
* a side panel (`⌥⌘P`) with a **Text** tab (recognized lines, copy
  lines, find and redact sensitive text) and a **Beautify** tab
  (background, padding, corners, shadow, canvas size, presets), which
  replaced the old Frame styles (`tests/beautify_parity.rs` keeps the
  built-in presets byte-identical to them);
* off-UI-thread composing for documents of 16 MP and more
  (`compose_jobs`): pending steps settle before any interaction or
  output, so nothing lands under an open drag.

Privacy rule: after a redaction, History and the MCP server never
serve unredacted content (pixels or recognized text). Redaction guards
(`redactions_complete`, `drawn_annotations()`, the canvas's opaque
cover for redactions not rendered yet) must not be weakened.

## Design constraints worth knowing

* **Offline by default.** No telemetry. Update checks are the only
  network call, gated behind a preference.
* **Zero recurring cost.** macOS uses self-signed code-signing; Windows
  uses SignPath OSS; Linux is unsigned. Don't add Apple/Microsoft
  paid-cert assumptions.
* **iced 0.14 as the GUI runtime.** Multi-window via `iced::daemon`.
  `tiny-skia` for deterministic CPU rendering when an iced canvas isn't
  the right fit.
* **All async work goes through `tokio`.** The coordinator and MCP
  server share the same runtime. Capturer / OCREngine traits are
  `async_trait + Send + Sync`.

## Useful entry points if you're new to the code

* `crates/readshot-core/src/lib.rs` — domain glossary
* `crates/readshot-app/src/coordinator.rs` — how a capture flows
  through the system
* `crates/readshot-app/src/cli.rs` — wiring a feature without touching
  the GUI
* `crates/readshot-mcp/src/lib.rs` — the JSON-RPC tool dispatch
* `docs/superpowers/` (gitignored) — design loop output, useful as
  archival reading but not authoritative for current code

## Things to avoid

* Reaching for `unsafe` outside the `readshot-capture` /
  `readshot-ocr` per-OS backends.
* Re-introducing `anyhow` as a public type.
* Mocking the database / filesystem in integration tests where a
  `tempfile::TempDir` works (it does, and we use it everywhere).
* Hidden network calls — the only sanctioned outbound HTTP is the
  Sparkle / `latest.json` updater, and it lives in one place.
