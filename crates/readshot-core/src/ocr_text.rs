//! Tiny normaliser for OCR engine output.
//!
//! Apple Vision (and the other backends behind the `OCREngine` trait)
//! return text that's *almost* clipboard-ready but always benefits
//! from a small pass of housekeeping:
//!
//! * Line endings are normalised to `\n` (the OCR engine occasionally
//!   emits `\r\n` for content that was rendered from a Windows /
//!   email source).
//! * Each line is right-trimmed — trailing whitespace is invisible
//!   to the user and pollutes search.
//! * Runs of 2+ blank lines collapse to a single blank line, so a
//!   capture that crossed a vertical gap doesn't paste as a wall of
//!   newlines.
//! * Leading and trailing blank lines are stripped entirely.
//!
//! What it deliberately doesn't do: reflow paragraphs, spell-correct,
//! strip punctuation, or attempt to recover layout for tables / code.
//! Those belong to a separate "structured OCR" pass downstream.

/// Apply the housekeeping rules described in the module docs. Pure
/// function — given the same input always returns the same output.
pub fn clean(input: &str) -> String {
    let normalised = input.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;
    for raw in normalised.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            blank_run += 1;
            // Allow at most one blank line between content blocks.
            if blank_run == 1 {
                lines.push("");
            }
        } else {
            blank_run = 0;
            lines.push(line);
        }
    }
    // Drop blank lines from the head and tail.
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_trailing_whitespace_per_line() {
        assert_eq!(clean("hello   \nworld\t"), "hello\nworld");
    }

    #[test]
    fn strips_leading_and_trailing_blank_lines() {
        assert_eq!(clean("\n\nhello\n\n"), "hello");
        assert_eq!(clean("   \n\nhello\n  \n"), "hello");
    }

    #[test]
    fn collapses_runs_of_blank_lines_to_one() {
        assert_eq!(clean("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(clean("a\n\n\n\n\nb"), "a\n\nb");
    }

    #[test]
    fn preserves_single_blank_between_paragraphs() {
        assert_eq!(clean("para 1\n\npara 2"), "para 1\n\npara 2");
    }

    #[test]
    fn normalises_crlf_and_cr_line_endings() {
        assert_eq!(clean("a\r\nb"), "a\nb");
        assert_eq!(clean("a\rb"), "a\nb");
        assert_eq!(clean("a\r\n\r\nb"), "a\n\nb");
    }

    #[test]
    fn empty_and_whitespace_only_inputs_become_empty() {
        assert_eq!(clean(""), "");
        assert_eq!(clean("   \n\n  \n"), "");
    }

    #[test]
    fn single_line_passes_through() {
        assert_eq!(clean("Hello, world!"), "Hello, world!");
    }
}
