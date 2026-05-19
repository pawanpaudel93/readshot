//! Pure-Rust domain core for Readshot.
//!
//! This crate has no platform-specific code (no `cfg(target_os = …)`) and
//! depends only on `serde`, `thiserror`, `tiny-skia`, and `image`. Every
//! other crate in the workspace either re-exports types from here or
//! implements one of the traits declared elsewhere using these primitives.
//!
//! See `docs/superpowers/specs/2026-04-23-readshot-design.md` §3.8 for the
//! design rationale.

pub mod annotation;
pub mod arrowhead;
pub mod error;
pub mod filename;
pub mod filters;
pub(crate) mod fs_atomic;
pub mod geom;
pub mod history;
pub mod log;
pub mod ocr_layout;
pub mod ocr_text;
pub mod png;
pub mod preferences;
pub mod render;

pub use annotation::{Annotation, Rgba};
pub use error::{CaptureError, CoreError, ExportError, HistoryError, OCRError, PreferencesError};
pub use filename::expand as expand_filename_template;
pub use geom::{clamp_rect, InvalidRect, Point, PointLike, Rect, RectLike};
pub use history::{
    CaptureRecord, FsHistoryStore, HistoryIndex, HistoryIndexEntry, HistoryStore,
    HISTORY_INDEX_FILENAME, HISTORY_SCHEMA_VERSION,
};
pub use png::{encode as encode_png, save as save_png, write as write_png};
// Downstream callers need `Uuid` to address records by id (delete,
// open-from-history, etc.). Re-export so they don't need a separate
// dependency on `uuid`.
pub use preferences::{
    ExportFormat, HistoryRetention, OcrEngineChoice, Preferences, UpdateChannel,
    PREFERENCES_SCHEMA_VERSION,
};
pub use render::render;
pub use uuid::Uuid;
