//! `tracing` target constants used across the workspace.
//!
//! Every `tracing::info!` / `error!` / etc. inside Readshot must specify a
//! target from this module. The reasons:
//!
//! 1. **Filtering.** `RUST_LOG=np.com.pawanpaudel.readshot::ocr=debug` enables
//!    OCR-only diagnostic logging without affecting capture or editor.
//! 2. **Audit visibility.** The CLI and MCP server log every silent capture
//!    they perform; using a stable target makes those entries easy to grep
//!    out of the user's local log file.
//! 3. **macOS OSLog mirroring.** `tracing-oslog` maps the target string to a
//!    `Console.app` subsystem / category pair, so these constants determine
//!    how the app appears in the system log.
//!
//! ## Redaction policy
//!
//! Log calls **must not** include any of the following:
//!
//! * Pixel buffer bytes or base64-encoded image data.
//! * OCR result text or annotation `Text` content.
//! * Filenames or filesystem paths inside the user's `$HOME`.
//! * Any user-typed string (preferences values, hotkey definitions, …).
//!
//! Log calls **may** include: dimensions in pixels, durations, error
//! discriminants, display ids (which are opaque OS handles), and version
//! strings. When in doubt, log a synthetic identifier (UUID, hash) instead
//! of the real value.

/// Root tracing target — equal to the macOS bundle id so OSLog sees a stable
/// subsystem name and Linux/Windows log filters can use the same string.
pub const LOG_TARGET: &str = "np.com.pawanpaudel.readshot";

/// Per-component sub-targets. Each value is `LOG_TARGET` followed by a
/// double-colon component name. Use `tracing::info!(target: cat::CAPTURE, …)`.
pub mod cat {
    pub const CAPTURE: &str = concat!("np.com.pawanpaudel.readshot", "::capture");
    pub const EDITOR: &str = concat!("np.com.pawanpaudel.readshot", "::editor");
    pub const OCR: &str = concat!("np.com.pawanpaudel.readshot", "::ocr");
    pub const PERMS: &str = concat!("np.com.pawanpaudel.readshot", "::permissions");
    pub const HISTORY: &str = concat!("np.com.pawanpaudel.readshot", "::history");
    pub const UPDATER: &str = concat!("np.com.pawanpaudel.readshot", "::updater");
    pub const CLI: &str = concat!("np.com.pawanpaudel.readshot", "::cli");
    pub const MCP: &str = concat!("np.com.pawanpaudel.readshot", "::mcp");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_target_matches_bundle_id() {
        assert_eq!(LOG_TARGET, "np.com.pawanpaudel.readshot");
    }

    #[test]
    fn every_category_starts_with_root_target() {
        for c in [
            cat::CAPTURE,
            cat::EDITOR,
            cat::OCR,
            cat::PERMS,
            cat::HISTORY,
            cat::UPDATER,
            cat::CLI,
            cat::MCP,
        ] {
            assert!(
                c.starts_with(LOG_TARGET),
                "category `{c}` does not start with `{LOG_TARGET}`",
            );
            assert!(
                c.contains("::"),
                "category `{c}` is missing the `::` separator",
            );
        }
    }
}
