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
//! * Detect tables and emit Markdown table syntax.
//! * Detect code blocks (no reliable monospace cue from raster pixels).
//! * Handle multi-column documents by reading each column top-to-bottom.
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
/// 4. Emit one output line per row. The leftmost fragment's X-offset
///    becomes leading spaces (relative to the leftmost X across the
///    whole page); the gap between subsequent fragments becomes
///    inline whitespace, scaled by the median per-character width.
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

    // Sort by Y centre so we can walk the page top-to-bottom.
    let mut sorted = active.clone();
    sorted.sort_by(|a, b| {
        let ay = a.y + a.h / 2.0;
        let by = b.y + b.h / 2.0;
        ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Walk the sorted list, breaking into rows whenever the Y centre
    // exceeds the running row's reference centre by more than `row_tol`.
    let mut rows: Vec<Vec<&RecognizedLine>> = Vec::new();
    let mut current: Vec<&RecognizedLine> = Vec::new();
    let mut current_ref_y: Option<f32> = None;
    for line in &sorted {
        let y_centre = line.y + line.h / 2.0;
        match current_ref_y {
            Some(ref_y) if (y_centre - ref_y).abs() <= row_tol => {
                current.push(*line);
            }
            _ => {
                if !current.is_empty() {
                    rows.push(std::mem::take(&mut current));
                }
                current.push(*line);
                current_ref_y = Some(y_centre);
            }
        }
    }
    if !current.is_empty() {
        rows.push(current);
    }

    let min_x = active.iter().map(|l| l.x).fold(f32::INFINITY, f32::min);

    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let mut row = row.clone();
        row.sort_by(|a, b| a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal));

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
}
