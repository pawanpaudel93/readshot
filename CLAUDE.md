# CLAUDE.md — Working notes for Claude Code on this repo

If you are Claude Code (or a similar coding assistant), read this
**before** modifying anything in `crates/`. The same content for
non-Anthropic assistants lives in `AGENTS.md` — keep that and this
file in sync when the rules change.

## Workflow rules specific to this repo

1. **Commit straight to `main`.** No per-task feature branches. CI
   gates apply on push. The user has confirmed this preference more
   than once.
2. **Run `cargo build`, `cargo nextest run`, and `cargo clippy
   --all-targets -- -D warnings` before claiming a task complete.**
   `cargo fmt --all` if you touched `.rs` files.
3. **Never add `Co-Authored-By` lines to commit messages.**
4. **Don't re-introduce `anyhow`.** All public errors are `thiserror`
   enums.
5. **Use the per-crate fakes for tests.** `FakeCapturer`,
   `FakeOcrEngine`, `FakePermissions` are in the relevant crates'
   `fake.rs`/`tests/fake_*.rs` modules.
6. **Don't commit anything under `docs/superpowers/` or
   `.autoarchitect/`.** The repo's `.gitignore` already excludes them.
7. **Tag releases via `v*.*.*`.** The `release.yml` workflow listens
   for `v*` tag pushes and builds artefacts; do not push tags
   speculatively unless cutting a real release.

## Where to look first

* Architecture overview, build/test commands (including the one
  ignored test, a manual benchmark), the `test-fixtures` and
  `integration` test features, internal argv routes
  (`__interactive-capture`, `--launched-at-login`, `readshot://`) and
  logging, common-change matrix, the `runtime` module map, the
  editor's features at a glance, the redaction privacy rule and
  where `unsafe` is allowed: see `AGENTS.md`.
* End-user install instructions: `docs/INSTALL.md`.
* Maintainer release runbook: `docs/RELEASING.md`.
* Wiring the MCP server into Claude Desktop / Cursor / OpenAI:
  `docs/MCP.md`.

## Per-task heuristics

| When the user asks about… | Start by reading…                                       |
|--------------------------|----------------------------------------------------------|
| GUI behaviour            | `crates/readshot-ui/src/`, `crates/readshot-app/src/app.rs` |
| Editor interactions      | `crates/readshot-app/src/runtime/editor_ops.rs`, `crates/readshot-ui/src/editor/` |
| Image menu / combine     | `crates/readshot-app/src/runtime/{image_menu,combine}.rs`, `crates/readshot-core/src/document.rs` |
| Undo / any history step  | `crates/readshot-app/src/runtime/compose_jobs.rs` (`apply_step_now`, `queue_step`, one landing hook) |
| Crop, tool options bar   | `crates/readshot-ui/src/editor/style.rs`, `crates/readshot-app/src/runtime/editor_options.rs` |
| Magnifier                | `crates/readshot-core/src/magnifier.rs`, `crates/readshot-ui/src/editor/canvas.rs` |
| Stamps                   | `crates/readshot-core/src/stamp.rs`, `crates/readshot-app/src/emoji_macos.rs` |
| Beautify                 | `crates/readshot-core/src/beautify.rs`, `crates/readshot-app/src/runtime/{beautify_panel,beautify_preview}.rs` |
| Side panel / Text / Redact | `crates/readshot-app/src/runtime/{side_panel,text_panel}.rs`, `crates/readshot-core/src/{redact,scrub}.rs` |
| Large documents          | `crates/readshot-app/src/runtime/compose_jobs.rs`         |
| Keyboard shortcuts       | `crates/readshot-app/src/runtime/{key_maps,key_focus,keys}.rs`, README §Accessibility |
| Status line / toasts     | `crates/readshot-app/src/editor.rs` (`set_status` …), `crates/readshot-app/src/runtime/toast.rs` |
| History window           | `crates/readshot-app/src/runtime/{history_ops,history_select,history_images}.rs` |
| UI theme / look          | `crates/readshot-app/src/runtime/{tokens,ui_kit}.rs`, `crates/readshot-app/src/icons.rs` |
| Region capture           | `crates/readshot-capture/src/lib.rs` + per-OS `*.rs`     |
| OCR                      | `crates/readshot-ocr/src/lib.rs` + per-OS `*.rs`; text shape: `crates/readshot-core/src/text_lines.rs` (`layout_text`, shared by GUI, CLI, MCP) |
| CLI                      | `crates/readshot-app/src/cli.rs`                         |
| MCP server               | `crates/readshot-mcp/src/lib.rs`                         |
| History / preferences    | `crates/readshot-core/src/{history,preferences}.rs`      |
| Annotations / rendering  | `crates/readshot-core/src/{annotation,render}.rs`        |
| Logs / RUST_LOG          | `crates/readshot-core/src/log.rs`, `crates/readshot-app/src/main.rs` (`init_logging`), `crates/readshot-mcp/src/lib.rs` (`log_filter`) |
| Releases / packaging     | `.github/workflows/release.yml`, `packaging/`            |

## Things to confirm with the user before doing

* Bumping crate versions, opening tags, or pushing branches that
  trigger CI.
* Adding new dependencies — the project deliberately keeps the dep
  tree small. New crates need a justification.
* Any change to `Cargo.toml`'s `[workspace.dependencies]` — those
  are shared and pin everything downstream.
* Touching `gh-pages` (the Sparkle appcast / `latest.json` feed lives
  there).

## Typical session shape

1. User describes a task in the conversation.
2. Read the file you're about to change before editing it.
3. Make the change, run the build/test/clippy trio for the affected
   crate, then `--workspace` for the final pass.
4. Commit with a focused message that explains *why*, not just *what*.
5. Report what changed in 1-2 sentences and stop.
