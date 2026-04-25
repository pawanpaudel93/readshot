//! Typed error enums shared across the workspace.
//!
//! The CLI exit-code mapper (Task 18) and the MCP `ToolError` mapper
//! (Task 19) match on these variants directly; replacing them with
//! `anyhow::Error` would erase the discriminants both layers need.

use thiserror::Error;

/// Aggregate error returned by composite operations (capture-and-ocr in the
/// CLI, the same flow inside the MCP `capture_region_and_text` tool, etc.).
#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Ocr(#[from] OCRError),
    #[error(transparent)]
    Export(#[from] ExportError),
}

/// Errors from any [`crate::Capturer`] implementation.
#[derive(Debug, Error)]
pub enum CaptureError {
    /// macOS Screen Recording or Linux xdg-desktop-portal ScreenCast consent
    /// has not been granted. Maps to CLI exit code `1` and MCP error code
    /// `permission_denied`.
    #[error("screen capture permission denied")]
    PermissionDenied,

    /// The supplied `display_id` does not match any currently-attached
    /// display. Common cause: agent cached a display id then a monitor was
    /// unplugged. Maps to MCP error code `display_not_found`.
    #[error("display not found: {0}")]
    DisplayNotFound(String),

    /// Any other failure from the underlying OS API. The string is the
    /// platform's error message (already localised by the OS); the CLI emits
    /// it on stderr verbatim. Maps to CLI exit code `2` and MCP error code
    /// `capture_backend`.
    #[error("capture backend failure: {0}")]
    Backend(String),
}

/// Errors from any [`crate::OCREngine`] implementation. `NoText` is the only
/// non-error semantic outcome that still uses this enum — callers that want
/// to treat empty results as success (the MCP server does) match the variant
/// before propagating.
#[derive(Debug, Error)]
pub enum OCRError {
    /// The engine ran successfully but found no recognisable text. Editor
    /// surfaces this as a "No text recognised" toast; the MCP server
    /// converts it to `text: ""` rather than a tool error.
    #[error("no text recognised in the input image")]
    NoText,

    /// Any other failure from the underlying OCR engine. Maps to CLI exit
    /// code `3` and MCP error code `ocr_backend`.
    #[error("ocr backend failure: {0}")]
    Backend(String),
}

/// Errors from saving captures to disk and writing the clipboard.
#[derive(Debug, Error)]
pub enum ExportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// A format-conversion or encoder failure. The variant carries a string
    /// rather than a typed underlying error to keep the public surface small
    /// — the encoder details (`image::ImageError`, `arboard::Error`, etc.)
    /// aren't useful to most callers.
    #[error("export format failure: {0}")]
    Format(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_error_displays_human_readable() {
        let e = CaptureError::DisplayNotFound("CGDirectDisplay-42".into());
        let s = format!("{e}");
        assert_eq!(s, "display not found: CGDirectDisplay-42");
    }

    #[test]
    fn core_error_wraps_capture_error_via_from() {
        // The `#[from]` derives are how the `?` operator propagates a
        // CaptureError out of a function returning `Result<_, CoreError>`.
        // Behaviour-test the conversion path so a future refactor can't
        // silently break it.
        let inner = CaptureError::PermissionDenied;
        let outer: CoreError = inner.into();
        match outer {
            CoreError::Capture(CaptureError::PermissionDenied) => {}
            other => panic!("expected Capture(PermissionDenied), got {other:?}"),
        }
    }

    #[test]
    fn ocr_no_text_is_distinct_from_backend() {
        // The MCP server matches on NoText specifically; ensure the variants
        // remain easy to distinguish.
        let no_text = OCRError::NoText;
        let backend = OCRError::Backend("vision returned VNError 9999".into());
        assert!(matches!(no_text, OCRError::NoText));
        assert!(matches!(backend, OCRError::Backend(_)));
    }

    #[test]
    fn export_error_io_wraps_std_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "perm");
        let exp: ExportError = io_err.into();
        assert!(matches!(exp, ExportError::Io(_)));
    }
}
