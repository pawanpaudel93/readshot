//! Layout-aware reconstruction of OCR output.
//!
//! Apple Vision (and any future positioned-OCR backend) gives us a
//! list of recognised text fragments together with their normalised
//! bounding boxes. The flat `String` you get from joining those with
//! newlines loses indentation — terminal sessions, indented code,
//! YAML, and hierarchical lists all paste as a left-justified blob.
//!
//! [`reconstruct`] turns the positioned input into a multi-line
//! string that preserves left-edge indentation by mapping each
//! line's X-offset (relative to the leftmost line on the page) into
//! leading ASCII spaces. The conversion uses the *median per-line
//! character width* as the em-unit so the indent count adapts to
//! whatever font size the original capture used.
//!
//! What this module deliberately doesn't do (yet):
//!
//! * Reflow paragraphs.
//!
//! Those belong to the parked "structured OCR" feature.

/// One positioned recognised line.
///
/// `x`, `y`, `w`, `h` are normalised to `[0, 1]` with origin at the
/// **top-left** of the image — engines that report bottom-left
/// (Apple Vision) flip Y at the engine boundary so this struct is
/// uniform across backends.
#[derive(Clone, Debug, PartialEq)]
pub struct RecognizedLine {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

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

/// Reconstruct an indent-preserving plain-text version of the OCR
/// output.
///
/// The algorithm:
///
/// 1. Drop empty fragments.
/// 2. Cluster fragments by Y-centre: any two whose centres lie within
///    half a median line-height belong to the **same row**. Apple
///    Vision often returns several observations per visual row
///    (think tree-branch indented terminals where the branch and the
///    payload are recognised separately) — without clustering each
///    fragment becomes its own line and the row layout vanishes.
/// 3. Within each row, sort fragments by X.
/// 4. If repeated column starts form a simple table, emit a Markdown
///    table. If the rows look like a multi-column prose document, read
///    each column top-to-bottom. If the plain-text layout looks like
///    code or terminal output, wrap it in a fenced block. Otherwise
///    emit one plain-text output line per row. The leftmost fragment's
///    X-offset becomes leading spaces (relative to the leftmost X
///    across the whole page); the gap between subsequent fragments
///    becomes inline whitespace, scaled by the median per-character
///    width.
///
/// Returns the empty string when `lines` is empty.
pub fn reconstruct(lines: &[RecognizedLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let active: Vec<&RecognizedLine> = lines.iter().filter(|l| !l.text.is_empty()).collect();
    if active.is_empty() {
        return String::new();
    }

    let em = median_em(&active);
    let med_h = median_height(&active);
    // Two fragments are part of the same visual row when their Y
    // centres are within half a line height of each other. The floor
    // (0.005 of image height) keeps the heuristic stable when
    // bounding boxes are unusually thin.
    let row_tol = (med_h * 0.5).max(0.005);

    let rows = cluster_rows(&active, row_tol);

    let min_x = active.iter().map(|l| l.x).fold(f32::INFINITY, f32::min);
    if let Some(table) = render_markdown_table(&rows, em) {
        return table;
    }
    if let Some(columns) = render_multi_column_text(&rows, em) {
        return columns;
    }
    let plain = render_plain_text(&rows, min_x, em);
    if looks_like_code_block(&plain) {
        return fence_code_block(&plain);
    }
    plain
}

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
            Some(ref_y) if (y_centre - ref_y).abs() <= row_tol => {
                current.push(*line);
            }
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

fn render_plain_text(rows: &[Row<'_>], min_x: f32, em: f32) -> String {
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let row = row.sorted_fragments();

        // Indent the row by the leftmost fragment's offset.
        let first = row[0];
        let indent = ((first.x - min_x) / em).round().max(0.0) as usize;
        let indent = indent.min(80);
        for _ in 0..indent {
            out.push(' ');
        }
        out.push_str(&first.text);

        // Subsequent same-row fragments → inline whitespace.
        let mut prev_end = first.x + first.w;
        for frag in &row[1..] {
            let gap = frag.x - prev_end;
            // Always emit at least one space between fragments,
            // however small the gap. Cap large gaps the same way as
            // outer indentation.
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

fn render_multi_column_text(rows: &[Row<'_>], em: f32) -> Option<String> {
    if rows.len() < 2 {
        return None;
    }

    let sorted_rows: Vec<Vec<&RecognizedLine>> = rows.iter().map(Row::sorted_fragments).collect();
    let column_count = sorted_rows.first()?.len();
    if !(2..=3).contains(&column_count) {
        return None;
    }
    if sorted_rows.iter().any(|row| row.len() != column_count) {
        return None;
    }
    if !looks_like_wrapped_prose(&sorted_rows) {
        return None;
    }

    let min_gutter = (em * 12.0).max(0.12);
    for column in 0..(column_count - 1) {
        if sorted_rows.iter().any(|row| {
            let left = row[column];
            let right = row[column + 1];
            right.x - (left.x + left.w) < min_gutter
        }) {
            return None;
        }
    }

    let mut columns = Vec::with_capacity(column_count);
    for column in 0..column_count {
        let fragments: Vec<&RecognizedLine> = sorted_rows.iter().map(|row| row[column]).collect();
        let min_x = fragments
            .iter()
            .map(|fragment| fragment.x)
            .fold(f32::INFINITY, f32::min);
        let rows: Vec<Row<'_>> = fragments
            .into_iter()
            .map(|fragment| Row {
                fragments: vec![fragment],
            })
            .collect();
        columns.push(render_plain_text(&rows, min_x, em));
    }
    Some(columns.join("\n\n"))
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

fn looks_like_code_block(text: &str) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 2 {
        return false;
    }

    let mut score = 0usize;
    let mut indented_lines = 0usize;
    let mut prompt_lines = 0usize;
    let mut symbolic_lines = 0usize;
    for line in &lines {
        let trimmed = line.trim_start();
        if line.len() > trimmed.len() {
            indented_lines += 1;
        }
        if is_shell_prompt(trimmed) {
            prompt_lines += 1;
        }
        let symbol_count = trimmed
            .chars()
            .filter(|c| {
                matches!(
                    c,
                    '{' | '}'
                        | '['
                        | ']'
                        | '('
                        | ')'
                        | ';'
                        | '='
                        | '<'
                        | '>'
                        | '!'
                        | '|'
                        | '&'
                        | '*'
                        | ':'
                )
            })
            .count();
        if symbol_count >= 2 || trimmed.ends_with('{') || trimmed.ends_with(';') {
            symbolic_lines += 1;
        }
    }

    if indented_lines > 0 && symbolic_lines > 0 {
        score += 3;
    }
    if prompt_lines > 0 {
        score += 3;
    }
    if symbolic_lines >= 2 {
        score += 2;
    }
    if text.contains("=>") || text.contains("->") || text.contains("::") {
        score += 1;
    }

    score >= 3
}

fn is_shell_prompt(trimmed: &str) -> bool {
    trimmed.starts_with("$ ")
        || trimmed.starts_with("> ")
        || trimmed.starts_with("% ")
        || trimmed.starts_with("λ ")
}

fn fence_code_block(text: &str) -> String {
    format!("```\n{}\n```", text.trim_end())
}

fn median_em(lines: &[&RecognizedLine]) -> f32 {
    let mut widths: Vec<f32> = lines
        .iter()
        .filter_map(|l| {
            let chars = l.text.chars().count() as f32;
            if chars > 0.0 && l.w > 0.0 {
                Some(l.w / chars)
            } else {
                None
            }
        })
        .collect();
    if widths.is_empty() {
        return 0.01;
    }
    widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    widths[widths.len() / 2]
}

fn median_height(lines: &[&RecognizedLine]) -> f32 {
    let mut heights: Vec<f32> = lines.iter().filter(|l| l.h > 0.0).map(|l| l.h).collect();
    if heights.is_empty() {
        return 0.04;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    heights[heights.len() / 2]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: f32, y: f32, w: f32, h: f32) -> RecognizedLine {
        RecognizedLine {
            text: text.into(),
            x,
            y,
            w,
            h,
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        assert_eq!(reconstruct(&[]), "");
    }

    #[test]
    fn single_line_passes_through() {
        let lines = vec![line("hello", 0.10, 0.10, 0.30, 0.05)];
        assert_eq!(reconstruct(&lines), "hello");
    }

    #[test]
    fn lines_emit_top_to_bottom_in_y_order() {
        // Provide out-of-order to confirm the sort.
        let lines = vec![
            line("third", 0.10, 0.30, 0.20, 0.05),
            line("first", 0.10, 0.10, 0.20, 0.05),
            line("second", 0.10, 0.20, 0.20, 0.05),
        ];
        assert_eq!(reconstruct(&lines), "first\nsecond\nthird");
    }

    #[test]
    fn relative_indent_is_preserved_via_leading_spaces() {
        // Same Y for all so order is by X. Indent units derived from
        // median char width = (0.05 / 5) = 0.01 per char.
        let lines = vec![
            line("root", 0.00, 0.10, 0.05, 0.04),  // 5 chars -> em = 0.01
            line("child", 0.04, 0.20, 0.05, 0.04), // 5 chars
            line("grandchild", 0.08, 0.30, 0.10, 0.04), // 10 chars
        ];
        let out = reconstruct(&lines);
        // root has zero offset; child = 0.04 / 0.01 = 4 spaces;
        // grandchild = 0.08 / 0.01 = 8 spaces.
        assert_eq!(out, "root\n    child\n        grandchild");
    }

    #[test]
    fn empty_text_lines_are_skipped_not_indented() {
        let lines = vec![
            line("hello", 0.0, 0.1, 0.05, 0.04),
            line("", 0.5, 0.2, 0.05, 0.04),
            line("world", 0.0, 0.3, 0.05, 0.04),
        ];
        assert_eq!(reconstruct(&lines), "hello\nworld");
    }

    #[test]
    fn fragments_on_the_same_row_merge_into_one_line() {
        // "3 files changed,"  and  "5 insertions(+), 13 deletions(-)"
        // are returned by Vision as two observations on the same row.
        // Without clustering they'd stack vertically; with it they
        // share one line separated by inline whitespace.
        // Median height = 0.04, so row tolerance = 0.02.
        let lines = vec![
            line("3 files changed,", 0.10, 0.20, 0.16, 0.04), // 16 chars / 0.16 → em 0.01
            line("5 insertions(+), 13 deletions(-)", 0.30, 0.205, 0.32, 0.04), // same row (Y diff 0.005 < 0.02)
        ];
        let out = reconstruct(&lines);
        assert!(out.starts_with("3 files changed,"));
        assert!(out.contains("5 insertions(+), 13 deletions(-)"));
        // No newline should appear — same row.
        assert!(!out.contains('\n'));
    }

    #[test]
    fn rows_separated_by_more_than_a_line_height_become_distinct_lines() {
        let lines = vec![
            line("first row", 0.00, 0.10, 0.09, 0.04),
            line("second row", 0.00, 0.30, 0.10, 0.04),
        ];
        let out = reconstruct(&lines);
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn mixed_rows_indent_and_inline_spacing_combine() {
        // Row 1: one fragment at far-left.
        // Row 2: two fragments — leftmost indented, second further right.
        let lines = vec![
            line("aaaaa", 0.00, 0.10, 0.05, 0.04), // em = 0.01
            line("bbbbb", 0.04, 0.30, 0.05, 0.04), // 4 chars indent on row 2
            line("ccccc", 0.20, 0.30, 0.05, 0.04), // gap from b's end (0.09) to c.x (0.20) = 0.11 → 11 spaces
        ];
        let out = reconstruct(&lines);
        let rendered: Vec<&str> = out.lines().collect();
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0], "aaaaa");
        assert_eq!(rendered[1], "    bbbbb           ccccc");
    }

    #[test]
    fn extreme_x_offset_is_clamped_to_80_chars() {
        // All lines feed em = 0.01 (chars / w = 0.01), so the
        // median is unambiguous. x = 0.95 → 95 chars unclamped,
        // should clamp to 80.
        let lines = vec![
            line("aaaaa", 0.00, 0.10, 0.05, 0.04), // 5 / 0.05 → em 0.01
            line("bbbbb", 0.00, 0.20, 0.05, 0.04),
            line("far", 0.95, 0.30, 0.03, 0.04), // 3 / 0.03 → em 0.01
        ];
        let out = reconstruct(&lines);
        let last = out.lines().last().unwrap();
        assert!(last.starts_with(&" ".repeat(80)));
        assert!(last.ends_with("far"));
        assert_eq!(last.len(), 80 + 3);
    }

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
    fn two_column_prose_reads_each_column_top_to_bottom() {
        let lines = vec![
            line("Left column starts here", 0.00, 0.10, 0.24, 0.04),
            line("Right column starts here", 0.58, 0.10, 0.25, 0.04),
            line("Left column continues", 0.00, 0.20, 0.22, 0.04),
            line("Right column continues", 0.58, 0.20, 0.24, 0.04),
        ];

        assert_eq!(
            reconstruct(&lines),
            "Left column starts here\nLeft column continues\n\nRight column starts here\nRight column continues"
        );
    }

    #[test]
    fn nearby_same_row_prose_fragments_do_not_become_columns() {
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
    fn indented_braced_code_emits_fenced_block() {
        let lines = vec![
            line("fn main() {", 0.00, 0.10, 0.11, 0.04),
            line("println!(\"hi\");", 0.04, 0.20, 0.16, 0.04),
            line("}", 0.00, 0.30, 0.01, 0.04),
        ];

        assert_eq!(
            reconstruct(&lines),
            "```\nfn main() {\n    println!(\"hi\");\n}\n```"
        );
    }

    #[test]
    fn shell_prompt_output_emits_fenced_block() {
        let lines = vec![
            line("$ cargo test", 0.00, 0.10, 0.12, 0.04),
            line("test result: ok. 42 passed", 0.00, 0.20, 0.27, 0.04),
        ];

        assert_eq!(
            reconstruct(&lines),
            "```\n$ cargo test\ntest result: ok. 42 passed\n```"
        );
    }

    #[test]
    fn normal_prose_lines_do_not_become_fenced_code() {
        let lines = vec![
            line(
                "Readshot keeps screenshots searchable",
                0.00,
                0.10,
                0.36,
                0.04,
            ),
            line("The editor preserves annotations", 0.00, 0.20, 0.32, 0.04),
        ];

        assert_eq!(
            reconstruct(&lines),
            "Readshot keeps screenshots searchable\nThe editor preserves annotations"
        );
    }

    #[test]
    fn nan_coordinates_do_not_panic() {
        // A misbehaving backend could hand us NaN bounding boxes. Sorting
        // (partial_cmp), median/em math, and the indent cast must all
        // degrade gracefully rather than panic, and the text must survive.
        let n = f32::NAN;
        let lines = vec![
            line("alpha", n, n, n, n),
            line("beta", 0.0, n, 0.05, n),
            line("gamma", n, 0.2, n, 0.04),
        ];
        let out = reconstruct(&lines);
        assert!(out.contains("alpha"));
        assert!(out.contains("beta"));
        assert!(out.contains("gamma"));
    }

    #[test]
    fn infinite_coordinates_do_not_panic() {
        // ±inf indents cast to a saturating usize and are clamped to 80;
        // this must not panic or allocate unboundedly.
        let inf = f32::INFINITY;
        let neg = f32::NEG_INFINITY;
        let lines = vec![
            line("left", 0.0, 0.10, 0.04, 0.04),
            line("far", inf, 0.20, 0.03, 0.04),
            line("back", neg, 0.30, 0.03, 0.04),
            line("huge", 0.0, 0.40, inf, 0.04),
        ];
        let out = reconstruct(&lines);
        assert!(out.contains("left"));
        assert!(out.contains("far"));
        assert!(out.contains("back"));
        assert!(out.contains("huge"));
        // No line should exceed the 80-space indent clamp plus its text.
        for l in out.lines() {
            let leading = l.len() - l.trim_start().len();
            assert!(leading <= 80, "indent {leading} exceeded the clamp");
        }
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
}
