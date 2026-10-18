//! Pure-Rust domain core for Readshot.
//!
//! This crate has no platform-specific code (no `cfg(target_os = …)`) and
//! depends only on `serde`, `thiserror`, `tiny-skia`, and `image`. Every
//! other crate in the workspace either re-exports types from here or
//! implements one of the traits declared elsewhere using these primitives.

// No unsafe code here: platform FFI lives in readshot-app, -capture and
// -ocr (AGENTS.md, "Things to avoid").
#![forbid(unsafe_code)]

pub mod annotation;
pub mod arrowhead;
pub mod beautify;
pub mod document;
pub mod error;
pub mod eyedropper;
pub mod filename;
pub mod filters;
pub(crate) mod fs_atomic;
#[cfg(feature = "test-fixtures")]
pub use fs_atomic::skip_fsync_in_this_process;
pub mod geom;
pub mod history;
pub mod line_geom;
pub mod log;
pub mod magnifier;
pub mod ocr_layout;
pub mod ocr_text;
pub mod png;
pub mod preferences;
pub mod redact;
pub mod render;
pub mod scroll_stitch;
pub mod scrub;
pub mod stamp;
pub mod text_lines;

pub use annotation::{
    Annotation, ArrowHeads, FillMode, LinePath, LineStyle, MagnifierShape, Rgba, ShapeStyle,
    SpotlightShape, StampKind, TextStyle,
};
pub use error::{CaptureError, CoreError, ExportError, HistoryError, OCRError, PreferencesError};
pub use filename::expand as expand_filename_template;
pub use geom::{clamp_rect, InvalidRect, Point, PointLike, Rect, RectLike};
pub use history::{
    CaptureRecord, FsHistoryStore, HistoryIndex, HistoryIndexEntry, HistoryStore, StagedImage,
    HISTORY_INDEX_FILENAME, HISTORY_SCHEMA_VERSION,
};
pub use png::{
    encode as encode_png, encode_fast as encode_png_fast, save as save_png, write as write_png,
};
// Downstream callers need `Uuid` to address records by id (delete,
// open-from-history, etc.). Re-export so they don't need a separate
// dependency on `uuid`.
pub use preferences::{
    ExportFormat, HistoryRetention, OcrEngineChoice, Preferences, UpdateChannel,
    PREFERENCES_SCHEMA_VERSION,
};
pub use render::render;
pub use uuid::Uuid;
