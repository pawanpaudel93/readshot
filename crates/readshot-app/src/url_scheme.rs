//! `readshot://` URL scheme parser.
//!
//! Spec §6 (API Contracts) declares one external entry point: a
//! URL scheme that triggers the *interactive* capture overlay. The
//! scheme intentionally cannot drive a silent capture — that's the
//! job of the CLI and MCP surfaces (Tasks 18, 19), which run as the
//! user's own process and inherit the user's grants.
//!
//! Supported URLs (case-insensitive scheme):
//!
//! | URL                 | Action                               |
//! |---------------------|--------------------------------------|
//! | `readshot://new`    | Open the overlay, wait for selection |
//! | `readshot://`       | Same as `new` (default action)       |
//!
//! Anything else is rejected with [`UrlAction::Unknown`] and logged.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UrlAction {
    /// Open the capture overlay (the only supported action today).
    NewCapture,
    /// Unrecognised path; the App logs the URL and ignores it. The
    /// `String` is the original path, useful for diagnostics.
    Unknown(String),
}

/// Errors from [`parse`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UrlParseError {
    #[error("URL is empty")]
    Empty,
    #[error("URL has no scheme: `{0}`")]
    NoScheme(String),
    #[error("URL scheme `{0}` is not `readshot`")]
    WrongScheme(String),
}

/// Parse a `readshot://...` URL into a [`UrlAction`]. Lenient on
/// case (`Readshot://NEW` works) and trailing slashes.
pub fn parse(url: &str) -> Result<UrlAction, UrlParseError> {
    let url = url.trim();
    if url.is_empty() {
        return Err(UrlParseError::Empty);
    }

    let (scheme, rest) = match url.split_once("://") {
        Some(parts) => parts,
        None => return Err(UrlParseError::NoScheme(url.into())),
    };

    if !scheme.eq_ignore_ascii_case("readshot") {
        return Err(UrlParseError::WrongScheme(scheme.into()));
    }

    let path = rest.trim_matches('/').to_ascii_lowercase();
    Ok(match path.as_str() {
        "" | "new" => UrlAction::NewCapture,
        other => UrlAction::Unknown(other.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_urls_are_drained_in_order_once() {
        let _lock = crate::url_events::lock_for_tests();
        crate::url_events::clear_for_tests();

        crate::url_events::deliver_url_string("readshot://new").unwrap();
        crate::url_events::deliver_url_string("readshot://history").unwrap();

        assert_eq!(
            crate::url_events::drain_actions(),
            vec![UrlAction::NewCapture, UrlAction::Unknown("history".into())]
        );
        assert_eq!(crate::url_events::drain_actions(), Vec::new());
    }

    #[test]
    fn rejected_delivered_urls_are_not_queued() {
        let _lock = crate::url_events::lock_for_tests();
        crate::url_events::clear_for_tests();

        assert_eq!(
            crate::url_events::deliver_url_string("file:///tmp/nope"),
            Err(UrlParseError::WrongScheme("file".into()))
        );
        assert_eq!(crate::url_events::drain_actions(), Vec::new());
    }

    #[test]
    fn parses_canonical_new_capture() {
        assert_eq!(parse("readshot://new"), Ok(UrlAction::NewCapture));
    }

    #[test]
    fn empty_path_means_new_capture() {
        assert_eq!(parse("readshot://"), Ok(UrlAction::NewCapture));
        assert_eq!(parse("readshot:///"), Ok(UrlAction::NewCapture));
    }

    #[test]
    fn case_insensitive_scheme_and_path() {
        assert_eq!(parse("Readshot://NEW"), Ok(UrlAction::NewCapture));
        assert_eq!(parse("READSHOT://New"), Ok(UrlAction::NewCapture));
    }

    #[test]
    fn unknown_path_rounds_to_unknown_variant() {
        match parse("readshot://history") {
            Ok(UrlAction::Unknown(p)) => assert_eq!(p, "history"),
            other => panic!("expected Unknown(history), got {other:?}"),
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(parse(""), Err(UrlParseError::Empty));
        assert_eq!(parse("   "), Err(UrlParseError::Empty));
    }

    #[test]
    fn rejects_url_without_scheme() {
        match parse("just-a-path") {
            Err(UrlParseError::NoScheme(_)) => {}
            other => panic!("expected NoScheme, got {other:?}"),
        }
    }

    #[test]
    fn rejects_other_schemes() {
        match parse("file:///etc/passwd") {
            Err(UrlParseError::WrongScheme(s)) => assert_eq!(s, "file"),
            other => panic!("expected WrongScheme(file), got {other:?}"),
        }
    }
}
