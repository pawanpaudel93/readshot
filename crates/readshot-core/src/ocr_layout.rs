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
/// output. Lines are emitted top-to-bottom (sorted by Y ascending,
/// then X ascending) with leading spaces proportional to each line's
/// horizontal offset from the leftmost line.
///
/// Returns the empty string when `lines` is empty.
pub fn reconstruct(lines: &[RecognizedLine]) -> String {
    if lines.is_empty() {
        return String::new();
    }

    // Sort by Y (top-to-bottom) then X (left-to-right within a row).
    // We don't mutate the caller's slice; reconstruction is read-only.
    let mut indexed: Vec<&RecognizedLine> = lines.iter().filter(|l| !l.text.is_empty()).collect();
    if indexed.is_empty() {
        return String::new();
    }
    indexed.sort_by(|a, b| {
        a.y.partial_cmp(&b.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });

    // Median per-line character width gives us a stable em-unit even
    // when individual lines are short or noisy. Skip lines with zero
    // characters to avoid divide-by-zero; if all lines are zero-width
    // (shouldn't happen) fall back to a sane constant.
    let mut widths: Vec<f32> = indexed
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
    let em = if widths.is_empty() {
        0.01 // ~1% of image width per character — very rough fallback
    } else {
        widths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        widths[widths.len() / 2]
    };

    let min_x = indexed.iter().map(|l| l.x).fold(f32::INFINITY, f32::min);

    let mut out = String::new();
    for (i, line) in indexed.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let indent = ((line.x - min_x) / em).round().max(0.0) as usize;
        // Cap indent so a stray bbox can't blow up the output. 80
        // columns is a reasonable upper bound for source-code-style
        // content.
        let indent = indent.min(80);
        for _ in 0..indent {
            out.push(' ');
        }
        out.push_str(&line.text);
    }
    out
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
