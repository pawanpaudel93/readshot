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

One test is ignored on purpose: `ui_thread_time_of_rotate_and_undo_at_96_mp`
(`readshot-app/src/runtime/tests/large_documents.rs`) is a manual
benchmark. It asserts nothing and prints how long rotate, undo, redo,
an annotation and an added part keep the UI thread busy on a ~96 MP
document. Run it in release when a change touches `compose_jobs` or the
editor's render path, and compare against a run on `main`:

```bash
cargo test --release -p readshot-app --lib ui_thread_time -- --ignored --nocapture
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
| Any history step (geometry, undo)   | `readshot-app::runtime::compose_jobs` (`apply_step_now` / `queue_step`, one landing hook) |
| Crop presets, tool options bar      | `readshot-ui::editor::style`, `readshot-app::runtime::editor_options` |
| Magnifier                           | `readshot-core::magnifier`, `readshot-ui::editor::canvas` |
| Stamps (emoji on macOS)             | `readshot-core::stamp`, `readshot-app::emoji_macos` |
| Eyedropper, color presets           | `readshot-core::eyedropper`, `readshot-app::runtime::editor_options` |
| Beautify (background, shadow, …)    | `readshot-core::beautify`, `readshot-app::runtime::{beautify_panel,beautify_preview}` |
| Side panel, Text tab, Redact        | `readshot-app::runtime::{side_panel,text_panel}`, `readshot-core::{redact,scrub,text_lines}` |
| Large-document responsiveness       | `readshot-app::runtime::compose_jobs`          |
| Editor status line and toasts       | `readshot-app::editor` (`Status`, `StatusTone`, `set_status`), `readshot-app::runtime::toast` |
| History window                      | `readshot-app::runtime::{history_ops,history_select,history_images}`, `readshot-core::history` |
| Settings window                     | `readshot-app::runtime::{settings_ops,view}` (`settings_view`), `readshot-ui::settings` |
| Scrolling capture                   | `readshot-app::runtime::scroll`, `readshot-core::scroll_stitch` |
| Look and feel (colors, controls)    | `readshot-app::runtime::{tokens,ui_kit}`, `readshot-app::icons` |
| In-app glyph (toolbar, buttons)     | `readshot-app::icons` (shared actions go through `icons::action`; the tests rasterise every glyph, `READSHOT_ICON_SHEET_DIR=dir` dumps them as PNGs) |
| App / brand icon                    | `packaging/macos/icon.svg` (+ `icon-small.svg` for 20–32 px, `icon-16.svg` for 16 px), then run `packaging/render-icons.sh` to regenerate every committed raster (in-app, Linux hicolor, Windows `.ico`/`.res`, website) |
| macOS notifications                 | `readshot-app::runtime::platform` (`show_macos_notification`) → `readshot-app/src/macos_notify.swift` (built by `build.rs`) |
| New OCR engine option               | `readshot-ocr` + `default_engine` switch       |
| New capture backend                 | `readshot-capture` (mind the `Capturer` trait) |
| New CLI subcommand                  | `readshot-app::cli`                            |
| New MCP tool                        | `readshot-mcp` (lib.rs `tool_*` + descriptors) |
| New preference field                | `readshot-core::preferences` (mind round-trip) |
| New in-app keyboard shortcut        | `readshot-app::runtime::key_maps` (editor and window key maps; History's is `history_key_message` in `runtime.rs`), `readshot-app::runtime::key_focus` (focus zones, chords), `readshot-app::runtime::keys` (labels, tested per platform), README §Accessibility |
| Global capture hotkey               | `readshot-ui::hotkey` (parsing), `readshot-app::runtime::hotkeys` |
| Per-OS packaging tweak              | `packaging/<os>/` and `release.yml`            |

### `readshot-app::runtime` modules

| Module               | What it holds                                                     |
|----------------------|-------------------------------------------------------------------|
| `view`               | Every window's view functions                                     |
| `editor_ops`         | Editor message handling, annotation edits, History sync           |
| `compose_jobs`       | The history-step pipeline, off-thread composes and flattens       |
| `image_menu`         | Image menu, Resize dialog, refused-change statuses                |
| `combine`            | Add Capture / Add Image, gap and alignment                        |
| `editor_options`     | Tool options bar, color and width popovers                        |
| `side_panel`         | Side panel frame and tabs                                         |
| `text_panel`         | Text tab: recognition, line selection, copy, smart redact         |
| `beautify_panel`     | Beautify tab controls                                             |
| `beautify_preview`   | Live Beautify canvas preview                                      |
| `key_maps`           | Editor key map and the pin / Settings / Welcome / scroll key maps |
| `key_focus`          | Editor keyboard focus zones and chords                            |
| `keys`               | Platform key labels (one constant per key)                        |
| `toast`              | Toast kind, button and dismissal rules                            |
| `history_ops`        | History window messages                                           |
| `history_select`     | History visibility, search and selection                          |
| `history_images`     | History thumbnail and preview image handles                       |
| `settings_ops`       | Settings and CLI-tools window messages                            |
| `window_settings`    | Window sizes and levels per window kind                           |
| `hotkeys`            | Global hotkey registration                                        |
| `scroll`             | Scrolling capture session                                         |
| `platform`           | OS integration: appearance, relaunch, reveal / open folder        |
| `tokens`, `ui_kit`   | Design tokens and shared controls                                 |

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
* an eyedropper (`I`) and a color popover with presets and recent
  colors; the highlighter's Snap to text, which follows recognized text
  lines;
* keyboard focus zones (`runtime::key_focus`): the options bar, the
  side panel, the Image menu and the Resize dialog are reachable with
  F6, Tab and the chords listed in README §Accessibility, since iced
  widgets can't take focus themselves;
* off-UI-thread composing for documents of 16 MP and more
  (`compose_jobs`): pending steps settle before any interaction or
  output, so nothing lands under an open drag. Small and large documents
  share one pipeline: every history step (rotate, flip, resize, align,
  gap, added part, undo, redo) is a `ComposeKind` applied on the UI
  thread (`apply_step_now`) or composed off it (`queue_step`), and both
  land through the same hook (status, bake bookkeeping, recognition,
  loupe and Discard clears, History sync). Only where the pixels are
  composed differs.

Privacy rule: after a redaction, nothing persisted or served — History
text, images, thumbnails and search, the MCP server, the CLI, clipboard
copies — exposes content under a redaction (pixels or recognized text).
Over-masking is acceptable, under-masking never:

* What counts as a redaction, and its rect, is defined once:
  `Annotation::redaction_rect` (Solid block, Blur, Pixelate). Every site
  uses it.
* Text masking (`readshot-core::scrub::hidden_spans`): a word whose box
  any redaction overlaps is masked in full (only hairline contact, under
  1 px deep and under 1 % of the box, is ignored); a word with no box of
  its own is masked when a redaction overlaps its line (flips and turns
  make a position inside the line untrustworthy). Without word positions
  (Windows/Linux OCR) a redaction makes the text unmaskable:
  `hidden_spans` returns `None` and the text is withheld (History stores
  none, MCP serves `null`, Copy Lines refuses). A magnifier bubble whose
  source shows a redaction counts as hidden too
  (`scrub::magnified_redactions`: Copy All adds it, and a scrub stores it
  with the record's redactions for every later reader).
* History fails closed: a sidecar that exists but can't be read or
  parsed counts as scrubbed for every guard (no text or unredacted PNG
  is written over it), index paths are only ever used inside the
  history root, and a thumbnail backfill re-checks under the scrub's lock
  that the PNG it decoded is still the one on disk (never written from
  pre-redaction pixels).
* Redaction guards (`redactions_complete`, `drawn_annotations()`, the
  canvas's opaque cover for redactions not rendered yet) must not be
  weakened.

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

* `unsafe` anywhere but platform FFI: the per-OS backends of
  `readshot-capture` and `readshot-ocr`, and the macOS glue in
  `readshot-app` (`app_events`, `url_events`, `updater`, `uninstall`,
  `emoji_macos`, `runtime::platform`, `runtime::window_settings`). Every
  unsafe block carries a `// SAFETY:` comment saying why it is sound;
  those three crates (and the `readshot` binary) deny
  `clippy::undocumented_unsafe_blocks`. `readshot-core`, `readshot-ui`
  and `readshot-mcp` `forbid(unsafe_code)`.
* Re-introducing `anyhow` as a public type.
* Mocking the database / filesystem in integration tests where a
  `tempfile::TempDir` works (it does, and we use it everywhere).
* Hidden network calls — the only sanctioned outbound HTTP is the
  Sparkle / `latest.json` updater, and it lives in one place.
