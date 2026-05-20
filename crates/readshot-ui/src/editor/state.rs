//! Editor model — the pure-logic state the iced view binds against.
//!
//! [`EditorState`] owns the captured base image, the annotation
//! [`History`] stack, the active [`ToolState`], the current colour,
//! and the line-width slider. It exposes a small surface
//! ([`commit_annotation`], [`set_tool`], [`set_color`], etc.) plus
//! `undo`/`redo` and `flatten` (re-renders via [`readshot_core::render`]).
//!
//! Splitting the model from the iced view (the Canvas widget in
//! `canvas.rs`) means every state transition is unit-testable without
//! a windowing system. Task 16's composition root holds an
//! `EditorState` and feeds it iced messages.

use image::RgbaImage;
use readshot_core::{render, Annotation, Rgba};

use super::tool_state::ToolState;
use super::undo::History;

const DEFAULT_LINE_WIDTH: f32 = 3.0;

#[derive(Clone, Debug)]
pub struct EditorState {
    base: RgbaImage,
    history: History,
    active_tool: ToolState,
    current_color: Rgba,
    current_line_width: f32,
    /// Cached flattened image. Invalidated whenever `history`,
    /// `current_color`, or `current_line_width` change.
    flattened_cache: Option<RgbaImage>,
}

impl EditorState {
    pub fn new(base: RgbaImage) -> Self {
        Self {
            base,
            history: History::new(),
            active_tool: ToolState::default(),
            current_color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            current_line_width: DEFAULT_LINE_WIDTH,
            flattened_cache: None,
        }
    }

    pub fn with_annotations(base: RgbaImage, annotations: Vec<Annotation>) -> Self {
        Self {
            base,
            history: History::from_present(annotations),
            active_tool: ToolState::default(),
            current_color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            current_line_width: DEFAULT_LINE_WIDTH,
            flattened_cache: None,
        }
    }

    pub fn base(&self) -> &RgbaImage {
        &self.base
    }

    pub fn active_tool(&self) -> ToolState {
        self.active_tool
    }

    pub fn current_color(&self) -> Rgba {
        self.current_color
    }

    pub fn current_line_width(&self) -> f32 {
        self.current_line_width
    }

    pub fn annotations(&self) -> &[Annotation] {
        self.history.current()
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    /// Depth of the undo stack — surfaced in the editor toolbar
    /// tooltip so the user knows how far back they can go.
    pub fn undo_depth(&self) -> usize {
        self.history.past_len()
    }

    /// Depth of the redo stack.
    pub fn redo_depth(&self) -> usize {
        self.history.future_len()
    }

    pub fn set_tool(&mut self, tool: ToolState) {
        self.active_tool = tool;
    }

    pub fn set_color(&mut self, color: Rgba) {
        self.current_color = color;
    }

    pub fn set_line_width(&mut self, width: f32) {
        // Floor at 0.5 to keep tiny-skia happy; cap at 64 px because
        // anything larger doesn't fit on the toolbar's slider in the
        // editor and is almost always a mistake.
        self.current_line_width = width.clamp(0.5, 64.0);
    }

    /// Append a new annotation, snapshotting the previous state to
    /// the undo stack.
    pub fn commit_annotation(&mut self, annotation: Annotation) {
        let mut next = self.history.current().to_vec();
        next.push(annotation);
        self.history.push(next);
        self.flattened_cache = None;
    }

    pub fn undo(&mut self) -> bool {
        let changed = self.history.undo().is_some();
        if changed {
            self.flattened_cache = None;
        }
        changed
    }

    pub fn redo(&mut self) -> bool {
        let changed = self.history.redo().is_some();
        if changed {
            self.flattened_cache = None;
        }
        changed
    }

    /// Discard all annotations and reset undo/redo. Used by the
    /// "Discard" action button before the editor closes.
    pub fn discard(&mut self) {
        self.history.clear();
        self.flattened_cache = None;
    }

    /// Render base + annotations into a single flat `RgbaImage`,
    /// cached so successive views during a single state don't re-render.
    /// The cache is cleared on every state change.
    pub fn flatten(&mut self) -> RgbaImage {
        if let Some(img) = &self.flattened_cache {
            return img.clone();
        }
        let img = render(&self.base, self.history.current());
        self.flattened_cache = Some(img.clone());
        img
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba as ImgRgba;
    use readshot_core::{PointLike, RectLike};

    fn solid_base(w: u32, h: u32) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = ImgRgba([255, 255, 255, 255]);
        }
        img
    }

    fn rect(x: f32) -> Annotation {
        Annotation::Rectangle {
            rect: RectLike::new(x, 0.0, 10.0, 10.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
        }
    }

    #[test]
    fn new_editor_has_no_annotations() {
        let s = EditorState::new(solid_base(32, 32));
        assert!(s.annotations().is_empty());
        assert_eq!(s.active_tool(), ToolState::Select);
    }

    #[test]
    fn with_annotations_starts_from_existing_present_state() {
        let s = EditorState::with_annotations(solid_base(32, 32), vec![rect(0.0)]);
        assert_eq!(s.annotations().len(), 1);
        assert!(!s.can_undo());
        assert!(!s.can_redo());
    }

    #[test]
    fn commit_appends_and_records_undo() {
        let mut s = EditorState::new(solid_base(32, 32));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.annotations().len(), 1);
        assert!(s.can_undo());
        assert!(!s.can_redo());
    }

    #[test]
    fn undo_reverts_commit() {
        let mut s = EditorState::new(solid_base(32, 32));
        s.commit_annotation(rect(0.0));
        assert!(s.undo());
        assert!(s.annotations().is_empty());
    }

    #[test]
    fn line_width_is_clamped_to_safe_range() {
        let mut s = EditorState::new(solid_base(32, 32));
        s.set_line_width(0.0);
        assert!(s.current_line_width() >= 0.5);
        s.set_line_width(9999.0);
        assert!(s.current_line_width() <= 64.0);
    }

    #[test]
    fn flatten_reflects_committed_annotations() {
        let mut s = EditorState::new(solid_base(64, 64));
        // Empty annotations: flattened equals base.
        let flat0 = s.flatten();
        assert_eq!(flat0.as_raw(), s.base().as_raw());

        // Add a black rect outline: flattened pixels along the
        // perimeter must differ from the white base.
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(8.0, 8.0, 48.0, 48.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
        });
        let flat1 = s.flatten();
        let pixel_top_edge = flat1.get_pixel(32, 8);
        assert!(
            pixel_top_edge[0] < 200,
            "expected darker pixel on stroke, got {pixel_top_edge:?}"
        );
    }

    #[test]
    fn flatten_uses_cache_when_state_unchanged() {
        let mut s = EditorState::new(solid_base(16, 16));
        s.commit_annotation(rect(0.0));
        let a = s.flatten();
        let b = s.flatten();
        assert_eq!(a.as_raw(), b.as_raw());
    }

    #[test]
    fn discard_clears_history_and_cache() {
        let mut s = EditorState::new(solid_base(16, 16));
        s.commit_annotation(rect(0.0));
        s.discard();
        assert!(s.annotations().is_empty());
        assert!(!s.can_undo());
    }

    #[test]
    fn point_tool_does_not_apply_to_segment_tools() {
        // Sanity check that ToolState classifications (defined in
        // tool_state.rs) plug into the editor model — the model
        // doesn't directly enforce them, but the unit test makes the
        // wiring explicit so a future refactor can't silently lose it.
        let mut s = EditorState::new(solid_base(16, 16));
        s.set_tool(ToolState::Line);
        assert!(s.active_tool().is_segment_tool());
        s.set_tool(ToolState::NumberedPin);
        assert!(s.active_tool().is_point_tool());
        // Suppress unused-import-of-PointLike when only some test
        // bodies reference it; this keeps imports symmetric.
        let _ = PointLike::new(0.0, 0.0);
    }
}
