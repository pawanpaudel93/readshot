# Structured OCR Tables Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert simple aligned positioned-OCR tables into GitHub-flavored Markdown tables while preserving existing plain-text reconstruction for non-table captures.

**Architecture:** Keep the public API unchanged: `readshot_core::ocr_layout::reconstruct(&[RecognizedLine]) -> String`. Extract the current row-clustering logic into private helpers, attempt conservative table detection on the clustered rows, and fall back to the current plain renderer when the detector is not confident.

**Tech Stack:** Rust, `readshot-core`, deterministic unit tests, existing `cargo test`, `cargo nextest`, `cargo clippy`, and `cargo fmt` workflow.

---

## File Structure

- Modify `crates/readshot-core/src/ocr_layout.rs`: add private `Row` representation, row clustering helper, Markdown table detection/formatting helpers, and tests.
- No new public types.
- No changes to OCR engines, app, UI, MCP, or history storage.

## Task 1: Add Failing Table Tests

**Files:**
- Modify: `crates/readshot-core/src/ocr_layout.rs`

- [ ] **Step 1: Add tests for accepted table layouts**

Append these tests inside the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn aligned_two_column_rows_emit_markdown_table() {
    let lines = vec![
        line("Name", 0.00, 0.10, 0.04, 0.04),
        line("Status", 0.30, 0.10, 0.06, 0.04),
        line("Readshot", 0.00, 0.20, 0.08, 0.04),
        line("Shipped", 0.30, 0.20, 0.07, 0.04),
    ];

    assert_eq!(
        reconstruct(&lines),
        "| Name | Status |\n| --- | --- |\n| Readshot | Shipped |"
    );
}

#[test]
fn column_jitter_still_emits_markdown_table() {
    let lines = vec![
        line("Package", 0.00, 0.10, 0.07, 0.04),
        line("Version", 0.30, 0.10, 0.07, 0.04),
        line("State", 0.62, 0.10, 0.05, 0.04),
        line("core", 0.01, 0.20, 0.04, 0.04),
        line("1.0", 0.305, 0.20, 0.03, 0.04),
        line("ok", 0.615, 0.20, 0.02, 0.04),
    ];

    assert_eq!(
        reconstruct(&lines),
        "| Package | Version | State |\n| --- | --- | --- |\n| core | 1.0 | ok |"
    );
}

#[test]
fn markdown_table_cells_escape_pipes() {
    let lines = vec![
        line("Expr", 0.00, 0.10, 0.04, 0.04),
        line("Meaning", 0.30, 0.10, 0.07, 0.04),
        line("a | b", 0.00, 0.20, 0.05, 0.04),
        line("choice", 0.30, 0.20, 0.06, 0.04),
    ];

    assert_eq!(
        reconstruct(&lines),
        "| Expr | Meaning |\n| --- | --- |\n| a \\| b | choice |"
    );
}
```

- [ ] **Step 2: Add tests for rejected false positives**

Append these tests in the same test module:

```rust
#[test]
fn prose_fragments_do_not_become_a_table() {
    let lines = vec![
        line("The product wedge", 0.00, 0.10, 0.16, 0.04),
        line("is searchable history", 0.24, 0.10, 0.21, 0.04),
        line("Capture text remains", 0.00, 0.20, 0.20, 0.04),
        line("plain when it is prose", 0.24, 0.20, 0.22, 0.04),
    ];

    assert_eq!(
        reconstruct(&lines),
        "The product wedge        is searchable history\nCapture text remains    plain when it is prose"
    );
}

#[test]
fn inconsistent_column_counts_remain_plain_text() {
    let lines = vec![
        line("Name", 0.00, 0.10, 0.04, 0.04),
        line("Status", 0.30, 0.10, 0.06, 0.04),
        line("Readshot shipped today", 0.00, 0.20, 0.23, 0.04),
    ];

    assert_eq!(
        reconstruct(&lines),
        "Name                          Status\nReadshot shipped today"
    );
}
```

- [ ] **Step 3: Verify tests fail for missing table support**

Run:

```bash
cargo test -p readshot-core ocr_layout
```

Expected: the three Markdown table tests fail because `reconstruct` still emits plain text. Existing tests should still compile.

- [ ] **Step 4: Commit failing tests**

```bash
git add crates/readshot-core/src/ocr_layout.rs
git commit -m "test(core): specify OCR table reconstruction"
```

## Task 2: Extract Row Clustering Without Behaviour Change

**Files:**
- Modify: `crates/readshot-core/src/ocr_layout.rs`

- [ ] **Step 1: Add private row type and clustering helper**

Insert below `RecognizedLine`:

```rust
#[derive(Clone, Debug)]
struct Row<'a> {
    fragments: Vec<&'a RecognizedLine>,
}

impl<'a> Row<'a> {
    fn sorted_fragments(&self) -> Vec<&'a RecognizedLine> {
        let mut fragments = self.fragments.clone();
        fragments.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));
        fragments
    }
}
```

Move the existing Y-sort and row-building logic from `reconstruct` into:

```rust
fn cluster_rows<'a>(active: &[&'a RecognizedLine], row_tol: f32) -> Vec<Row<'a>> {
    let mut sorted = active.to_vec();
    sorted.sort_by(|a, b| {
        let ay = a.y + a.h / 2.0;
        let by = b.y + b.h / 2.0;
        ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut rows: Vec<Row<'a>> = Vec::new();
    let mut current: Vec<&'a RecognizedLine> = Vec::new();
    let mut current_ref_y: Option<f32> = None;
    for line in &sorted {
        let y_centre = line.y + line.h / 2.0;
        match current_ref_y {
            Some(ref_y) if (y_centre - ref_y).abs() <= row_tol => current.push(*line),
            _ => {
                if !current.is_empty() {
                    rows.push(Row {
                        fragments: std::mem::take(&mut current),
                    });
                }
                current.push(*line);
                current_ref_y = Some(y_centre);
            }
        }
    }
    if !current.is_empty() {
        rows.push(Row { fragments: current });
    }
    rows
}
```

- [ ] **Step 2: Extract existing renderer**

Move the existing output loop into:

```rust
fn render_plain_text(rows: &[Row<'_>], min_x: f32, em: f32) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let row = row.sorted_fragments();
        let first = row[0];
        let indent = ((first.x - min_x) / em).round().max(0.0) as usize;
        let indent = indent.min(80);
        for _ in 0..indent {
            out.push(' ');
        }
        out.push_str(&first.text);

        let mut prev_end = first.x + first.w;
        for frag in &row[1..] {
            let gap = frag.x - prev_end;
            let spaces = ((gap / em).round().max(1.0) as usize).min(80);
            for _ in 0..spaces {
                out.push(' ');
            }
            out.push_str(&frag.text);
            prev_end = frag.x + frag.w;
        }
    }
    out
}
```

Update `reconstruct` to compute `rows = cluster_rows(&active, row_tol)` and return `render_plain_text(&rows, min_x, em)`.

- [ ] **Step 3: Verify no behaviour changed except new table tests still fail**

Run:

```bash
cargo test -p readshot-core ocr_layout
```

Expected: existing non-table tests pass. The Markdown table tests still fail.

- [ ] **Step 4: Commit refactor**

```bash
git add crates/readshot-core/src/ocr_layout.rs
git commit -m "refactor(core): share OCR row clustering"
```

## Task 3: Implement Conservative Markdown Table Detection

**Files:**
- Modify: `crates/readshot-core/src/ocr_layout.rs`

- [ ] **Step 1: Add table detection before fallback**

In `reconstruct`, after `rows` and `min_x` are computed:

```rust
if let Some(table) = render_markdown_table(&rows, em) {
    return table;
}

render_plain_text(&rows, min_x, em)
```

- [ ] **Step 2: Add private Markdown helpers**

Insert below `render_plain_text`:

```rust
fn render_markdown_table(rows: &[Row<'_>], em: f32) -> Option<String> {
    if rows.len() < 2 {
        return None;
    }

    let sorted_rows: Vec<Vec<&RecognizedLine>> = rows.iter().map(Row::sorted_fragments).collect();
    let column_count = sorted_rows.first()?.len();
    if column_count < 2 {
        return None;
    }
    if sorted_rows.iter().any(|row| row.len() != column_count) {
        return None;
    }
    if looks_like_wrapped_prose(&sorted_rows) {
        return None;
    }

    let tolerance = (em * 2.0).max(0.015);
    for column in 0..column_count {
        let reference_x = sorted_rows[0][column].x;
        if sorted_rows
            .iter()
            .skip(1)
            .any(|row| (row[column].x - reference_x).abs() > tolerance)
        {
            return None;
        }
    }

    let mut out = String::new();
    write_markdown_row(&mut out, &sorted_rows[0]);
    out.push('\n');
    out.push('|');
    for _ in 0..column_count {
        out.push_str(" --- |");
    }
    for row in sorted_rows.iter().skip(1) {
        out.push('\n');
        write_markdown_row(&mut out, row);
    }
    Some(out)
}

fn looks_like_wrapped_prose(rows: &[Vec<&RecognizedLine>]) -> bool {
    let total_cells = rows.iter().map(Vec::len).sum::<usize>();
    let long_cells = rows
        .iter()
        .flat_map(|row| row.iter())
        .filter(|cell| cell.text.split_whitespace().count() >= 3)
        .count();
    long_cells * 2 >= total_cells
}

fn write_markdown_row(out: &mut String, row: &[&RecognizedLine]) {
    out.push('|');
    for cell in row {
        out.push(' ');
        out.push_str(&normalise_markdown_cell(&cell.text));
        out.push_str(" |");
    }
}

fn normalise_markdown_cell(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('|', "\\|")
}
```

- [ ] **Step 3: Verify focused tests pass**

Run:

```bash
cargo test -p readshot-core ocr_layout
```

Expected: all `ocr_layout` tests pass.

- [ ] **Step 4: Commit implementation**

```bash
git add crates/readshot-core/src/ocr_layout.rs
git commit -m "feat(core): reconstruct OCR tables as Markdown"
```

## Task 4: Full Verification

**Files:**
- No source changes expected unless verification finds an issue.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt --all
```

Expected: command exits 0.

- [ ] **Step 2: Run core package verification**

Run:

```bash
cargo nextest run -p readshot-core
cargo clippy -p readshot-core -- -D warnings
```

Expected: both commands exit 0.

- [ ] **Step 3: Run workspace verification**

Run:

```bash
cargo build --workspace
cargo clippy --workspace -- -D warnings
cargo nextest run --workspace
```

Expected: all commands exit 0.

- [ ] **Step 4: Commit formatting or verification fixes if needed**

Only if verification changed files:

```bash
git add crates/readshot-core/src/ocr_layout.rs
git commit -m "fix(core): polish OCR table reconstruction"
```

If no files changed, do not create a commit.

## Self-Review

- Spec coverage: covered table detection, Markdown formatting, fallback behaviour, private architecture, and deterministic tests.
- Placeholder scan: no placeholder steps; all code changes and commands are explicit.
- Type consistency: `Row<'a>`, `cluster_rows`, `render_plain_text`, `render_markdown_table`, `write_markdown_row`, and `normalise_markdown_cell` are private helpers in `ocr_layout.rs`; the public `reconstruct` signature remains unchanged.
