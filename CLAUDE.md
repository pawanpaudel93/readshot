# CLAUDE.md — Working notes for Claude Code on this repo

If you are Claude Code (or a similar coding assistant), read this
**before** modifying anything in `crates/`. The same content for
non-Anthropic assistants lives in `AGENTS.md` — keep that and this
file in sync when the rules change.

## Workflow rules specific to this repo

1. **Commit straight to `main`.** No per-task feature branches. CI
   gates apply on push. The user has confirmed this preference more
   than once.
2. **Run `cargo build`, `cargo nextest run`, and `cargo clippy --
   -D warnings` before claiming a task complete.** `cargo fmt --all`
   if you touched `.rs` files.
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

* Architecture overview, build/test commands, common-change matrix:
  see `AGENTS.md`.
* End-user install instructions: `docs/INSTALL.md`.
* Maintainer release runbook: `docs/RELEASING.md`.
* Wiring the MCP server into Claude Desktop / Cursor / OpenAI:
  `docs/MCP.md`.

## Per-task heuristics

| When the user asks about… | Start by reading…                                       |
|--------------------------|----------------------------------------------------------|
| GUI behaviour            | `crates/readshot-ui/src/`, `crates/readshot-app/src/app.rs` |
| Region capture           | `crates/readshot-capture/src/lib.rs` + per-OS `*.rs`     |
| OCR                      | `crates/readshot-ocr/src/lib.rs` + per-OS `*.rs`         |
| CLI                      | `crates/readshot-app/src/cli.rs`                         |
| MCP server               | `crates/readshot-mcp/src/lib.rs`                         |
| History / preferences    | `crates/readshot-core/src/{history,preferences}.rs`      |
| Annotations / rendering  | `crates/readshot-core/src/{annotation,render}.rs`        |
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
