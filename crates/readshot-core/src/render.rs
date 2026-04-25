//! Annotation rendering — declared in Task 3, implemented in Task 4.
//!
//! The signature lives here so the snapshot tests committed in Task 3 can
//! type-check and compile, even though calling [`render`] panics until
//! Task 4 lands the tiny-skia-backed implementation.
//!
//! Why split declaration from implementation? The snapshot tests are part
//! of the design contract — they capture the exact pixel output the
//! renderer is expected to produce. Authoring those tests *before* the
//! implementation forces us to reason about what "correct" rendering looks
//! like for each annotation variant rather than letting the implementation
//! define correctness after the fact.

use crate::Annotation;
use image::RgbaImage;

/// Render `model` over `base` and return the flattened result.
///
/// **Not yet implemented.** Until Task 4 lands the renderer body, this
/// function panics with `unimplemented!()` so callers (and the snapshot
/// suite) surface the missing piece loudly rather than silently rendering
/// nothing.
pub fn render(_base: &RgbaImage, _model: &[Annotation]) -> RgbaImage {
    unimplemented!("AnnotationRenderer is implemented in Task 4 — see docs/superpowers/plans/2026-04-23-readshot.md")
}
