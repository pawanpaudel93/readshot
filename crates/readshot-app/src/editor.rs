//! Annotation editor — runtime integration layer.
//!
//! [`EditorSession`] wraps the unit-tested model from
//! `readshot_ui::editor::EditorState` with the iced-specific bits the
//! runtime cares about: which `window::Id` the session owns, whether
//! a save/copy/ocr task is in flight, the toast text under the action
//! row, and a few transient interaction-state fields the toolbar /
//! canvas widgets read while the user is mid-drag (live preview rect,
//! freehand polyline accumulator, pin counter).
//!
//! The model itself owns the captured base image, the annotation list
//! with undo/redo, and the active tool / colour / line-width. Rendering
//! is delegated to `readshot_core::render` via `model.flatten()` so
//! Save / Copy bake annotations into the saved PNG instead of emitting
//! the raw capture.

use iced::Rectangle;
use readshot_core::{CaptureRecord, PointLike};
use readshot_ui::editor::EditorState as Model;

/// One open editor window, plus the iced-side state that doesn't
/// belong inside the pure model.
pub struct EditorSession {
    pub model: Model,
    pub status: Option<String>,
    /// `true` while a save / copy / ocr task is in flight; the action
    /// bar buttons disable themselves in that state to avoid double
    /// firing.
    pub busy: bool,
    /// `Some` after iced has acknowledged the window-open request.
    /// Used by Discard so we know which window to close.
    pub window_id: Option<iced::window::Id>,
    /// Live drag preview — shape / line / polyline being dragged but
    /// not yet committed. The canvas draw step uses this to paint a
    /// dashed outline so the user sees what they're about to commit.
    pub preview: Option<Preview>,
    /// Counter for the next `NumberedPin` annotation. Starts at 1 and
    /// monotonically grows; resets when the editor is discarded.
    pub next_pin_number: u32,
    /// Cached iced handle for the currently-flattened image (base +
    /// committed annotations). iced's `view` function only gets `&App`
    /// so we can't run `model.flatten()` inside it (the renderer needs
    /// `&mut`); instead the runtime's `update` rebuilds this handle on
    /// every model mutation via [`refresh_image`].
    pub image_handle: iced::widget::image::Handle,
    /// In-progress text annotation. `Some` after the user clicks with
    /// the Text tool active; the editor view renders an inline input
    /// row letting them type the content. Confirming commits an
    /// `Annotation::Text`; cancelling drops the pending state.
    pub pending_text: Option<PendingText>,
    /// History record this editor is mutating, if any. Fresh captures
    /// and history-opened captures both carry this so annotation edits
    /// can be written back to the JSON sidecar without altering the
    /// original PNG bytes.
    pub source_record: Option<CaptureRecord>,
}

/// Captured text-input state for the Text tool. The image-pixel
/// origin is fixed at the click point so the eventual annotation
/// lands where the user clicked, regardless of how long they spend
/// typing or how the editor resizes.
#[derive(Clone, Debug)]
pub struct PendingText {
    pub origin: PointLike,
    pub content: String,
}

/// Transient drag preview the canvas emits via `DragMoved` and the
/// editor view re-paints over the image. Shape variants mirror the
/// tool classifications in `readshot_ui::editor::tool_state`.
#[derive(Clone, Debug)]
pub enum Preview {
    /// Rectangular tools: Rectangle, Ellipse, Blur, Pixelate, Crop.
    Rect { tool: PreviewKind, rect: Rectangle },
    /// Segment tools: Line, Arrow.
    Segment {
        tool: PreviewKind,
        anchor: iced::Point,
        cursor: iced::Point,
    },
    /// Freehand tools: Pen, Highlighter — the accumulated polyline.
    Freehand {
        tool: PreviewKind,
        points: Vec<PointLike>,
    },
}

/// Lightweight tag the canvas attaches to a preview so the editor's
/// draw step knows what shape to outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Blur,
    Pixelate,
    Crop,
    Pen,
    Highlighter,
}

impl EditorSession {
    pub fn new(image: image::RgbaImage) -> Self {
        let mut model = Model::new(image);
        let image_handle = build_handle(&mut model);
        Self {
            model,
            status: None,
            busy: false,
            window_id: None,
            preview: None,
            next_pin_number: 1,
            image_handle,
            pending_text: None,
            source_record: None,
        }
    }

    pub fn from_history(image: image::RgbaImage, record: CaptureRecord) -> Self {
        let mut model = Model::with_annotations(image, record.annotation_model.clone());
        let image_handle = build_handle(&mut model);
        Self {
            model,
            status: None,
            busy: false,
            window_id: None,
            preview: None,
            next_pin_number: 1,
            image_handle,
            pending_text: None,
            source_record: Some(record),
        }
    }

    /// Rebuild [`image_handle`] from the model's currently-flattened
    /// pixels. Call after any mutation that affects the rendered
    /// output: `commit_annotation`, `undo`, `redo`, `discard`, or
    /// when the base image changes (Crop replaces it).
    pub fn refresh_image(&mut self) {
        self.image_handle = build_handle(&mut self.model);
    }

    /// Pixel size of the *base* image (pre-crop). Useful for hint
    /// text and not much else; the canvas should use
    /// [`effective_image_size`] so cursor mapping respects an
    /// active Crop annotation.
    pub fn image_size(&self) -> (u32, u32) {
        let base = self.model.base();
        (base.width(), base.height())
    }

    /// Effective displayed-image dimensions — base dimensions, or
    /// the most recent `Annotation::Crop` rect (clamped to the
    /// base) if one exists. Mirrors what the renderer outputs and
    /// what the user actually sees in the editor.
    pub fn effective_image_size(&self) -> (u32, u32) {
        let (bw, bh) = self.image_size();
        if let Some(r) = self.last_crop() {
            let (rx, ry, rw, rh) = (r.x, r.y, r.width, r.height);
            let bw_f = bw as f32;
            let bh_f = bh as f32;
            let x = rx.max(0.0).min(bw_f);
            let y = ry.max(0.0).min(bh_f);
            let w = (rw + rx - x).min(bw_f - x).max(1.0);
            let h = (rh + ry - y).min(bh_f - y).max(1.0);
            (w as u32, h as u32)
        } else {
            (bw, bh)
        }
    }

    /// Offset of the effective displayed image inside the base
    /// image's coordinate frame — `(0, 0)` if no Crop, else the
    /// most recent Crop rect's top-left clamped to the base.
    /// Cursor positions get this added to them before they're
    /// stored on annotations so the renderer's "translate by crop
    /// offset" pass produces pixels under the cursor.
    pub fn crop_offset(&self) -> (f32, f32) {
        let (bw, bh) = self.image_size();
        if let Some(r) = self.last_crop() {
            (r.x.max(0.0).min(bw as f32), r.y.max(0.0).min(bh as f32))
        } else {
            (0.0, 0.0)
        }
    }

    fn last_crop(&self) -> Option<readshot_core::RectLike> {
        self.model.annotations().iter().rev().find_map(|a| match a {
            readshot_core::Annotation::Crop { rect } => Some(*rect),
            _ => None,
        })
    }
}

fn build_handle(model: &mut Model) -> iced::widget::image::Handle {
    let img = model.flatten();
    iced::widget::image::Handle::from_rgba(img.width(), img.height(), img.as_raw().clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32) -> image::RgbaImage {
        let mut img = image::RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 255, 255, 255]);
        }
        img
    }

    #[test]
    fn new_session_starts_idle() {
        let s = EditorSession::new(solid(8, 8));
        assert!(!s.busy);
        assert!(s.window_id.is_none());
        assert!(s.preview.is_none());
        assert_eq!(s.next_pin_number, 1);
    }

    #[test]
    fn handle_reflects_image_size() {
        let s = EditorSession::new(solid(16, 32));
        // Just exercises that the eager handle was built — Handle
        // isn't introspectable beyond identity.
        let _ = s.image_handle.clone();
        assert_eq!(s.image_size(), (16, 32));
    }
}
