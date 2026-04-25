//! Minimal annotation editor (Phase C — basic actions only).
//!
//! When a region capture finishes, the runtime opens an editor
//! window with the captured RGBA image and four buttons:
//!
//! * **Save** — write the PNG to `~/Desktop/Readshot-<timestamp>.png`.
//! * **Copy** — push the image to the system clipboard via `arboard`.
//! * **Copy Text** — run OCR via the existing coordinator and push
//!   the recognised text to the clipboard.
//! * **Discard** — close the editor window without saving.
//!
//! What is *not* in this cut: pen / arrow / rectangle / text
//! annotation tools, undo/redo, the toolbar/canvas/action-bar
//! widget split from `readshot-ui::editor`. Those need careful
//! interactive design and are deferred to their own session.
//!
//! For the runtime story this module is dumb data + a single
//! `view` function. Async work (save, copy, OCR) lives in `runtime`
//! so the per-message Task wiring stays in one place.

use image::RgbaImage;

/// One editor session — a captured image plus a small status string
/// for the toast under the action row.
pub struct EditorState {
    pub image: RgbaImage,
    pub status: Option<String>,
    /// `true` while a save / copy / ocr task is in flight; the
    /// buttons are disabled in that state to avoid double-firing.
    pub busy: bool,
    /// `Some` after iced has acknowledged the window-open request.
    /// We need it to know which window to close on Discard.
    pub window_id: Option<iced::window::Id>,
}

impl EditorState {
    pub fn new(image: RgbaImage) -> Self {
        Self {
            image,
            status: None,
            busy: false,
            window_id: None,
        }
    }

    /// Iced image handle backed by the captured RGBA pixels.
    pub fn handle(&self) -> iced::widget::image::Handle {
        iced::widget::image::Handle::from_rgba(
            self.image.width(),
            self.image.height(),
            self.image.as_raw().clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_derived_from_image_dimensions() {
        let img = RgbaImage::new(8, 12);
        let state = EditorState::new(img);
        let _h = state.handle();
        // Just exercises the path — Handle isn't introspectable
        // beyond identity, but the call must not panic and the size
        // round-trip via the From impl is verified by the iced
        // widget in real-app tests.
        assert!(state.status.is_none());
        assert!(!state.busy);
    }

    #[test]
    fn new_starts_idle() {
        let state = EditorState::new(RgbaImage::new(2, 2));
        assert!(!state.busy);
        assert!(state.window_id.is_none());
    }
}
