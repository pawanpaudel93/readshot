# Structured OCR Tables Design

## Goal

Readshot should turn simple table-like OCR layouts into Markdown tables so copied text preserves row and column structure. Non-table OCR output must continue to use the current plain-text reconstruction path.

## Scope

This slice only handles positioned OCR fragments that already arrive with bounding boxes through `readshot_core::ocr_layout::RecognizedLine`. It does not add new OCR engine capabilities, image analysis, monospace detection, spreadsheet export, or multi-column document reading order.

## Recommended Approach

Add a table-detection pass inside `readshot-core/src/ocr_layout.rs` before the existing plain-text renderer emits rows. The pass should inspect clustered visual rows and decide whether they form a simple table:

- At least two visual rows.
- At least two columns per candidate row.
- Stable column starts across rows, with a tolerance derived from the median character width.
- No row may collapse to a single full-width paragraph fragment inside the candidate table.

When a candidate is accepted, emit a GitHub-flavored Markdown table:

```markdown
| Name | Status |
| --- | --- |
| Readshot | Shipped |
```

Cells should be trimmed, internal whitespace collapsed to one ASCII space, and pipe characters escaped as `\|`. The first row becomes the header. The separator row always uses `---` alignment markers because OCR does not preserve alignment intent reliably.

## Fallback Behaviour

If the heuristic is not confident, `reconstruct` must return the same style of plain text it returns today: rows sorted top-to-bottom, fragments sorted left-to-right, indentation preserved from the left edge, and inline gaps represented as spaces. This keeps normal prose, terminal output, and code-like captures from becoming false-positive tables.

## Architecture

Keep this as a pure `readshot-core` change:

- `RecognizedLine` remains unchanged.
- `reconstruct(&[RecognizedLine]) -> String` remains the public entry point.
- Existing row clustering should be extracted into a small internal helper so both table detection and plain-text rendering consume the same `Vec<Row>`.
- Table-specific helpers stay private to `ocr_layout.rs` until another caller needs structured output.

## Testing

Unit tests in `ocr_layout.rs` should cover:

- A two-column table emits Markdown.
- Three-column rows with slight OCR X jitter still emit Markdown.
- Pipe characters in cells are escaped.
- A normal sentence split into multiple OCR fragments remains plain text.
- Rows with inconsistent column counts remain plain text.
- Existing indentation and row-clustering tests continue to pass.

Run:

```bash
cargo test -p readshot-core ocr_layout
cargo nextest run -p readshot-core
cargo clippy -p readshot-core -- -D warnings
cargo fmt --all
```

## Risks

The main risk is false positives: screenshots of prose can be split into several OCR fragments on the same row. The detector should prefer false negatives over false positives, requiring repeated column boundaries across rows before switching to Markdown.
