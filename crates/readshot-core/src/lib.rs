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
pub mod filters;
pub mod geom;
pub mod log;
pub mod render;

pub use annotation::{Annotation, Rgba};
pub use error::{CaptureError, CoreError, ExportError, OCRError};
pub use geom::{clamp_rect, InvalidRect, Point, PointLike, Rect, RectLike};
pub use render::render;
