//! Filename template expansion for capture exports.
//!
//! Users configure a template like `Screenshot {YYYY-MM-DD at HH.mm.ss}`
//! in [`Preferences::filename_template`](crate::preferences::Preferences::filename_template).
//! [`expand`] substitutes the supported tokens against a [`DateTime<Utc>`]
//! and sanitises the result so the output is safe to use as a file name on
//! all three target OSes.
//!
//! ## Supported tokens
//!
//! | Token | Replacement | Example (epoch) |
//! |---|---|---|
//! | `{YYYY-MM-DD at HH.mm.ss}` | full date + time | `1970-01-01 at 00.00.00` |
//! | `{YYYY-MM-DD}` | ISO date | `1970-01-01` |
//! | `{HH.mm.ss}` | time with dot separators | `00.00.00` |
//! | `{YYYY}` | four-digit year | `1970` |
//! | `{timestamp}` | Unix epoch seconds | `0` |
//!
//! Longer tokens are matched before shorter ones, so a template
//! `{YYYY-MM-DD} {HH.mm.ss}` produces `1970-01-01 00.00.00` rather than
//! `1970-01-01 {HH.mm.ss}`.
//!
//! Unknown tokens (including a future `{ext}`) are left in the output
//! verbatim — callers can post-process them.
//!
//! ## Sanitisation
//!
//! After substitution, every character that is reserved as a path
//! separator or filename component on macOS / Windows / Linux is replaced
//! with `_`. Leading/trailing whitespace and dots are stripped (Windows
//! disallows trailing dots in filenames). Empty results fall back to the
//! literal `"Screenshot"`.

use chrono::{DateTime, Utc};

/// Expand a template against a UTC instant and produce a filesystem-safe
/// filename **base** (no extension — the caller appends).
pub fn expand(template: &str, when: DateTime<Utc>) -> String {
    if template.is_empty() {
        return "Screenshot".to_string();
    }

    // Order matters: replace the longest tokens first so a substring match
    // doesn't shadow a wider one.
    let replacements: [(&str, String); 5] = [
        (
            "{YYYY-MM-DD at HH.mm.ss}",
            when.format("%Y-%m-%d at %H.%M.%S").to_string(),
        ),
        ("{YYYY-MM-DD}", when.format("%Y-%m-%d").to_string()),
        ("{HH.mm.ss}", when.format("%H.%M.%S").to_string()),
        ("{YYYY}", when.format("%Y").to_string()),
        ("{timestamp}", when.timestamp().to_string()),
    ];

    let mut out = template.to_string();
    for (tok, val) in replacements {
        out = out.replace(tok, &val);
    }
    sanitise(&out)
}

fn sanitise(s: &str) -> String {
    let mapped: String = s
        .chars()
        .map(|c| match c {
            // Path separators and Windows-reserved characters.
            '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\0' => '_',
            _ => c,
        })
        .collect();
    let trimmed = mapped.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        "Screenshot".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn epoch() -> DateTime<Utc> {
        Utc.timestamp_opt(0, 0).unwrap()
    }

    fn fixed_2026() -> DateTime<Utc> {
        // 2026-04-25 14:30:45 UTC
        Utc.with_ymd_and_hms(2026, 4, 25, 14, 30, 45).unwrap()
    }

    #[test]
    fn full_template_expands_at_epoch() {
        assert_eq!(
            expand("Screenshot {YYYY-MM-DD at HH.mm.ss}", epoch()),
            "Screenshot 1970-01-01 at 00.00.00"
        );
    }

    #[test]
    fn split_tokens_in_one_template() {
        assert_eq!(
            expand("Cap_{YYYY}-{HH.mm.ss}", fixed_2026()),
            "Cap_2026-14.30.45"
        );
    }

    #[test]
    fn longer_tokens_take_priority() {
        // {YYYY-MM-DD} appears as a substring of the longer token; the
        // longer match must win.
        assert_eq!(
            expand("{YYYY-MM-DD at HH.mm.ss}", epoch()),
            "1970-01-01 at 00.00.00"
        );
    }

    #[test]
    fn unknown_tokens_pass_through() {
        // {ext} is intentionally not a known token — callers may add
        // extensions themselves.
        assert_eq!(expand("foo.{ext}", epoch()), "foo.{ext}");
    }

    #[test]
    fn timestamp_token_uses_unix_seconds() {
        assert_eq!(expand("{timestamp}", epoch()), "0");
    }

    #[test]
    fn path_separators_are_replaced() {
        assert_eq!(expand("foo/bar\\baz", epoch()), "foo_bar_baz");
    }

    #[test]
    fn windows_reserved_chars_are_replaced() {
        assert_eq!(expand(r#"a:b<c>d"e|f?g*h"#, epoch()), "a_b_c_d_e_f_g_h");
    }

    #[test]
    fn leading_and_trailing_dots_are_trimmed() {
        assert_eq!(expand("...foo...", epoch()), "foo");
    }

    #[test]
    fn whitespace_is_trimmed() {
        assert_eq!(expand("  foo  ", epoch()), "foo");
    }

    #[test]
    fn empty_template_returns_default_basename() {
        assert_eq!(expand("", epoch()), "Screenshot");
    }

    #[test]
    fn template_that_sanitises_to_empty_returns_default() {
        // Pure separator chars all collapse to underscores; trim leaves
        // them in place — but if a user constructs something that
        // sanitises to empty (e.g. a string of only `.`), fall back to
        // the default.
        assert_eq!(expand("...", epoch()), "Screenshot");
    }
}
