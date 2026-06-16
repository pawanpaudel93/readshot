//! Annotation editor — runtime integration layer.
//!
//! [`EditorSession`] wraps the unit-tested model from
//! `readshot_ui::editor::EditorState` with the iced-specific bits the
//! runtime cares about: which `window::Id` the session owns, whether
//! a save/copy/ocr task is in flight, the toast text under the action
//! row, and a few transient interaction-state fields the toolbar /
//! canvas widgets read while the user is mid-drag (selected-annotation
//! drag baselines, line-width preview baselines, pin counter).
//!
//! The model itself owns the captured base image, the annotation list
//! with undo/redo, and the active tool / colour / line-width. Rendering
//! is delegated to `readshot_core::render` via `model.flatten()` so
//! Save / Copy bake annotations into the saved PNG instead of emitting
//! the raw capture.

use readshot_core::{Annotation, CaptureRecord, PointLike};
use readshot_ui::editor::EditorState as Model;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditorFrameStyle {
    #[default]
    None,
    Soft,
    Light,
    Dark,
    Minimal,
    Transparent,
}

impl EditorFrameStyle {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Soft,
        Self::Light,
        Self::Dark,
        Self::Minimal,
        Self::Transparent,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No Frame",
            Self::Soft => "Soft Shadow",
            Self::Light => "Light Card",
            Self::Dark => "Dark Card",
            Self::Minimal => "Minimal Border",
            Self::Transparent => "Transparent",
        }
    }
}

impl std::fmt::Display for EditorFrameStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

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
    /// Counter for the next `NumberedPin` annotation. Starts at 1 and
    /// monotonically grows; resets when the editor is discarded.
    pub next_pin_number: u32,
    /// Cached iced handle for the currently-flattened image (base +
    /// committed annotations). iced's `view` function only gets `&App`
    /// so we can't run `model.flatten()` inside it (the renderer needs
    /// `&mut`); instead the runtime's `update` rebuilds this handle on
    /// every model mutation via [`EditorSession::refresh_image`].
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
    /// Editor preview zoom. `Fit` scales captures to the available
    /// viewport; `Percent` is an explicit user zoom.
    pub zoom: EditorZoom,
    /// Screen scale factor for the editor window. Actual-pixels mode
    /// uses this so one image pixel maps to one physical screen pixel
    /// on HiDPI displays.
    pub display_scale: f32,
    /// Display bounds that produced this editor session, when known.
    /// Follow-up windows opened from the editor use this to stay on
    /// the same monitor as the captured region.
    pub source_display_bounds: Option<(f32, f32, f32, f32)>,
    /// `Some` after the user has clicked Discard / pressed ⌘W once on
    /// an editor with unsaved work. The next Discard click inside
    /// [`DISCARD_CONFIRM_WINDOW`] commits; otherwise the flag expires
    /// and the user is back to a single click. Prevents accidental
    /// data loss without forcing a modal dialog.
    pub discard_pending_at: Option<std::time::Instant>,
    /// Wall-clock instant the current `status` toast was set. The
    /// runtime uses this to auto-dismiss successful status messages
    /// (e.g. "Saved to …") after a few seconds so the chrome doesn't
    /// stay loud forever.
    pub status_set_at: Option<std::time::Instant>,
    /// Transient Select-tool move. The model previews movement
    /// without pushing undo history on every mouse move; release
    /// commits the final state against this baseline as one undoable
    /// edit.
    pub move_drag: Option<MoveDrag>,
    /// Baseline for the line-width slider while it previews selected
    /// annotation sizing. Release commits the preview as one undoable
    /// edit.
    pub width_drag_baseline: Option<Vec<Annotation>>,
    /// Optional presentation frame applied to image outputs from this
    /// editor window. `No Frame` keeps Save / Copy / Pin pixel-exact.
    pub frame_style: EditorFrameStyle,
    /// Tessellated-geometry cache for the annotation canvas. Shared
    /// (cheap-clone `Rc`) into the freshly-built `EditorCanvas` program
    /// each `view()`. Background polling subscriptions (hotkey / tray /
    /// url drains) force iced to redraw every window on each tick; with
    /// this cache those idle redraws reuse the previous geometry instead
    /// of re-tessellating every annotation. The runtime clears it on
    /// editor messages (which is the only path that changes what the
    /// canvas paints), so live drags still re-render.
    pub canvas_cache: std::rc::Rc<iced::widget::canvas::Cache>,
    /// Last editor output state that was successfully produced for
    /// the user (saved/copied). Kept separate from undo history so a
    /// saved editor can still undo while closing without a stale
    /// "unsaved edits" warning.
    output_checkpoint: EditorOutputCheckpoint,
    /// Cached uncropped flatten used while a Crop annotation is being
    /// dragged. Cropping changes output geometry but no pixels, so the
    /// annotation stack (blur included) renders once per drag and each
    /// tick serves a sub-rect copy instead of a full re-render per
    /// mouse-move event. Cleared by [`refresh_image`].
    crop_drag_flat: Option<image::RgbaImage>,
}

#[derive(Clone, Debug, PartialEq)]
struct EditorOutputCheckpoint {
    annotations: Vec<Annotation>,
    frame_style: EditorFrameStyle,
}

impl EditorOutputCheckpoint {
    fn new(annotations: &[Annotation], frame_style: EditorFrameStyle) -> Self {
        Self {
            annotations: annotations.to_vec(),
            frame_style,
        }
    }
}

/// How long a "Click Discard again to confirm" prompt stays armed
/// before reverting to a fresh single-click state.
pub const DISCARD_CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(4);

/// Successful status toasts auto-dismiss after this window so they
/// don't linger forever. Error / in-progress messages stay until
/// they're overwritten.
pub const STATUS_AUTO_DISMISS: std::time::Duration = std::time::Duration::from_secs(4);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EditorZoom {
    Fit,
    Percent(f32),
}

impl EditorZoom {
    pub const MIN: f32 = 0.25;
    pub const MAX: f32 = 4.0;
    pub const STEP: f32 = 1.25;

    pub fn label(self) -> String {
        self.label_for_display_scale(1.0)
    }

    pub fn label_for_display_scale(self, display_scale: f32) -> String {
        match self {
            Self::Fit => "Fit".into(),
            Self::Percent(scale) => {
                format!("{:.0}%", scale * display_scale.max(f32::EPSILON) * 100.0)
            }
        }
    }

    pub fn explicit_scale(self) -> Option<f32> {
        match self {
            Self::Fit => None,
            Self::Percent(scale) => Some(scale),
        }
    }

    pub fn zoom_in(self) -> Self {
        self.zoom_in_from_display_scale(1.0)
    }

    pub fn zoom_out(self) -> Self {
        self.zoom_out_from_display_scale(1.0)
    }

    pub fn zoom_in_from_display_scale(self, display_scale: f32) -> Self {
        let current = self.current_display_scale(display_scale);
        Self::Percent((current * Self::STEP).clamp(Self::MIN, Self::MAX))
    }

    pub fn zoom_out_from_display_scale(self, display_scale: f32) -> Self {
        let current = self.current_display_scale(display_scale);
        Self::Percent((current / Self::STEP).clamp(Self::MIN, Self::MAX))
    }

    pub fn can_zoom_in(self) -> bool {
        self.can_zoom_in_from_display_scale(1.0)
    }

    pub fn can_zoom_out(self) -> bool {
        self.can_zoom_out_from_display_scale(1.0)
    }

    pub fn can_zoom_in_from_display_scale(self, display_scale: f32) -> bool {
        self.current_display_scale(display_scale) < Self::MAX
    }

    pub fn can_zoom_out_from_display_scale(self, display_scale: f32) -> bool {
        self.current_display_scale(display_scale) > Self::MIN
    }

    pub fn is_fit(self) -> bool {
        matches!(self, Self::Fit)
    }

    pub fn is_actual_size(self) -> bool {
        self.is_actual_size_for_display_scale(1.0)
    }

    pub fn is_actual_size_for_display_scale(self, display_scale: f32) -> bool {
        matches!(self, Self::Percent(scale) if (scale - Self::actual_scale(display_scale)).abs() < f32::EPSILON)
    }

    pub fn actual_size(display_scale: f32) -> Self {
        Self::Percent(Self::actual_scale(display_scale))
    }

    fn current_display_scale(self, display_scale: f32) -> f32 {
        self.explicit_scale()
            .unwrap_or_else(|| display_scale.max(f32::EPSILON))
    }

    fn actual_scale(display_scale: f32) -> f32 {
        1.0 / display_scale.max(f32::EPSILON)
    }
}

/// Captured text-input state for the Text tool. The image-pixel
/// origin is fixed at the click point so the eventual annotation
/// lands where the user clicked, regardless of how long they spend
/// typing or how the editor resizes.
#[derive(Clone, Debug)]
pub struct PendingText {
    pub origin: PointLike,
    pub content: String,
    pub edit_index: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct MoveDrag {
    pub baseline: Vec<Annotation>,
    pub selected_index: usize,
    pub start: PointLike,
    pub moved: bool,
    pub kind: MoveDragKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveDragKind {
    Move,
    Resize(readshot_ui::editor::ResizeHandle),
}

impl EditorSession {
    pub fn new(image: image::RgbaImage) -> Self {
        Self::new_with_display_scale(image, 1.0)
    }

    pub fn new_with_display_scale(image: image::RgbaImage, display_scale: f32) -> Self {
        let mut model = Model::new(image);
        let image_handle = build_handle(&mut model);
        let output_checkpoint =
            EditorOutputCheckpoint::new(model.annotations(), EditorFrameStyle::None);
        Self {
            model,
            status: None,
            busy: false,
            window_id: None,
            next_pin_number: 1,
            image_handle,
            pending_text: None,
            source_record: None,
            zoom: EditorZoom::Fit,
            display_scale: display_scale.max(f32::EPSILON),
            source_display_bounds: None,
            discard_pending_at: None,
            status_set_at: None,
            move_drag: None,
            width_drag_baseline: None,
            frame_style: EditorFrameStyle::None,
            canvas_cache: std::rc::Rc::new(iced::widget::canvas::Cache::default()),
            output_checkpoint,
            crop_drag_flat: None,
        }
    }

    pub fn from_history(image: image::RgbaImage, record: CaptureRecord) -> Self {
        Self::from_history_with_display_scale(image, record, 1.0)
    }

    pub fn from_history_with_display_scale(
        image: image::RgbaImage,
        record: CaptureRecord,
        display_scale: f32,
    ) -> Self {
        let next_pin_number = next_pin_number_after(&record.annotation_model);
        let mut model = Model::with_annotations(image, record.annotation_model.clone());
        let image_handle = build_handle(&mut model);
        let output_checkpoint =
            EditorOutputCheckpoint::new(model.annotations(), EditorFrameStyle::None);
        Self {
            model,
            status: None,
            busy: false,
            window_id: None,
            next_pin_number,
            image_handle,
            pending_text: None,
            source_record: Some(record),
            zoom: EditorZoom::Fit,
            display_scale: display_scale.max(f32::EPSILON),
            source_display_bounds: None,
            discard_pending_at: None,
            status_set_at: None,
            move_drag: None,
            width_drag_baseline: None,
            frame_style: EditorFrameStyle::None,
            canvas_cache: std::rc::Rc::new(iced::widget::canvas::Cache::default()),
            output_checkpoint,
            crop_drag_flat: None,
        }
    }

    pub fn actual_size_zoom(&self) -> EditorZoom {
        EditorZoom::actual_size(self.display_scale)
    }

    /// Set the toast text under the action row and stamp the
    /// `status_set_at` clock so auto-dismiss can run on the runtime
    /// tick. Use this everywhere `status = Some(…)` was set inline so
    /// the auto-dismiss is uniform across save / copy / OCR / pin
    /// paths.
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.status = Some(msg.into());
        self.status_set_at = Some(std::time::Instant::now());
    }

    pub fn mark_output_clean(&mut self) {
        self.output_checkpoint =
            EditorOutputCheckpoint::new(self.model.annotations(), self.frame_style);
        self.clear_discard_confirmation();
    }

    pub fn has_output_changes(&self) -> bool {
        self.output_checkpoint
            != EditorOutputCheckpoint::new(self.model.annotations(), self.frame_style)
    }

    pub fn clear_discard_confirmation(&mut self) {
        self.discard_pending_at = None;
    }

    /// "In-progress" status strings the auto-dismiss logic must
    /// leave alone — clearing "Saving…" before the save actually
    /// completes would look broken. Any message that ends with `…`
    /// or starts with one of these prefixes counts as in-progress.
    pub fn status_is_in_progress(&self) -> bool {
        match self.status.as_deref() {
            None => false,
            Some(s) => {
                s.ends_with('…')
                    || s.starts_with("Saving")
                    || s.starts_with("Copying")
                    || s.starts_with("Recognising")
                    || s.starts_with("Choose")
            }
        }
    }

    pub fn zoom_label(&self) -> String {
        self.zoom.label_for_display_scale(self.display_scale)
    }

    pub fn zoom_is_actual_size(&self) -> bool {
        self.zoom
            .is_actual_size_for_display_scale(self.display_scale)
    }

    /// Rebuild [`Self::image_handle`] from the model's currently-flattened
    /// pixels. Call after any mutation that affects the rendered
    /// output: `commit_annotation`, `undo`, `redo`, `discard`, or
    /// when the base image changes (Crop replaces it).
    pub fn refresh_image(&mut self) {
        self.crop_drag_flat = None;
        self.image_handle = build_handle(&mut self.model);
    }

    /// Cheap image-handle refresh for Crop drag ticks. The full
    /// renderer replays every annotation (blur included) per call;
    /// since a crop changes geometry but no pixels, render the stack
    /// once without any Crop annotation and serve each tick as a
    /// sub-rect copy. Falls back to [`Self::refresh_image`] when no crop
    /// annotation exists.
    pub fn refresh_crop_drag_preview(&mut self) {
        let Some(rect) = self.last_crop() else {
            self.refresh_image();
            return;
        };
        if self.crop_drag_flat.is_none() {
            let uncropped: Vec<_> = self
                .model
                .annotations()
                .iter()
                .filter(|a| !matches!(a, readshot_core::Annotation::Crop { .. }))
                .cloned()
                .collect();
            self.crop_drag_flat = Some(readshot_core::render(self.model.base(), &uncropped));
        }
        let flat = self.crop_drag_flat.as_ref().expect("cache filled above");
        // Mirror `render`'s crop clamping so the preview matches the
        // dimensions the committed render will produce.
        let x0 = rect.x.max(0.0);
        let y0 = rect.y.max(0.0);
        let x1 = (rect.x + rect.width).min(flat.width() as f32);
        let y1 = (rect.y + rect.height).min(flat.height() as f32);
        let w = (x1 - x0).max(1.0).round() as u32;
        let h = (y1 - y0).max(1.0).round() as u32;
        let img = image::imageops::crop_imm(flat, x0 as u32, y0 as u32, w, h).to_image();
        self.image_handle =
            iced::widget::image::Handle::from_rgba(img.width(), img.height(), img.into_raw());
    }

    /// Pixel size of the *base* image (pre-crop). Useful for hint
    /// text and not much else; the canvas should use
    /// [`Self::effective_image_size`] so cursor mapping respects an
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

fn next_pin_number_after(annotations: &[Annotation]) -> u32 {
    annotations
        .iter()
        .filter_map(|annotation| match annotation {
            Annotation::NumberedPin { number, .. } => Some(*number),
            _ => None,
        })
        .max()
        .and_then(|n| n.checked_add(1))
        .unwrap_or(1)
}

fn build_handle(model: &mut Model) -> iced::widget::image::Handle {
    // `flatten` already returns an owned copy of the cached image —
    // hand its buffer to the handle instead of cloning it again.
    let img = model.flatten();
    let (w, h) = (img.width(), img.height());
    iced::widget::image::Handle::from_rgba(w, h, img.into_raw())
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
        assert_eq!(s.next_pin_number, 1);
        assert_eq!(s.zoom, EditorZoom::Fit);
    }

    #[test]
    fn handle_reflects_image_size() {
        let s = EditorSession::new(solid(16, 32));
        // Just exercises that the eager handle was built — Handle
        // isn't introspectable beyond identity.
        let _ = s.image_handle.clone();
        assert_eq!(s.image_size(), (16, 32));
    }

    #[test]
    fn history_session_continues_numbered_pin_sequence() {
        let mut record = CaptureRecord::new(chrono::Utc::now(), 64, 64, "display-main".to_string());
        record.annotation_model = vec![
            Annotation::NumberedPin {
                origin: readshot_core::PointLike::new(5.0, 5.0),
                number: 1,
                color: readshot_core::Rgba::OPAQUE_BLACK,
            },
            Annotation::NumberedPin {
                origin: readshot_core::PointLike::new(12.0, 12.0),
                number: 7,
                color: readshot_core::Rgba::OPAQUE_BLACK,
            },
        ];

        let s = EditorSession::from_history(solid(64, 64), record);

        assert_eq!(s.next_pin_number, 8);
    }

    #[test]
    fn next_pin_number_after_empty_or_saturated_history_starts_at_one() {
        assert_eq!(next_pin_number_after(&[]), 1);
        assert_eq!(
            next_pin_number_after(&[Annotation::NumberedPin {
                origin: readshot_core::PointLike::new(5.0, 5.0),
                number: u32::MAX,
                color: readshot_core::Rgba::OPAQUE_BLACK,
            }]),
            1
        );
    }

    #[test]
    fn frame_style_labels_describe_the_output() {
        let labels: Vec<&str> = EditorFrameStyle::ALL
            .iter()
            .map(|style| style.label())
            .collect();

        assert_eq!(
            labels,
            vec![
                "No Frame",
                "Soft Shadow",
                "Light Card",
                "Dark Card",
                "Minimal Border",
                "Transparent",
            ]
        );
        for label in labels {
            assert!(
                label.len() <= 14,
                "frame labels should stay compact: {label}"
            );
        }
    }

    #[test]
    fn editor_zoom_steps_from_fit_via_actual_size() {
        assert_eq!(EditorZoom::Fit.zoom_in(), EditorZoom::Percent(1.25));
        assert_eq!(EditorZoom::Fit.zoom_out(), EditorZoom::Percent(0.8));
        assert_eq!(
            EditorZoom::Percent(10.0).zoom_in(),
            EditorZoom::Percent(EditorZoom::MAX)
        );
        assert_eq!(
            EditorZoom::Percent(0.01).zoom_out(),
            EditorZoom::Percent(EditorZoom::MIN)
        );
        assert!(EditorZoom::Fit.is_fit());
        assert!(EditorZoom::Percent(1.0).is_actual_size());
        assert!(!EditorZoom::Percent(EditorZoom::MAX).can_zoom_in());
        assert!(!EditorZoom::Percent(EditorZoom::MIN).can_zoom_out());
    }

    #[test]
    fn editor_zoom_steps_from_fit_use_visible_fit_scale() {
        assert_eq!(
            EditorZoom::Fit.zoom_in_from_display_scale(0.5),
            EditorZoom::Percent(0.625)
        );
        assert_eq!(
            EditorZoom::Fit.zoom_out_from_display_scale(0.5),
            EditorZoom::Percent(0.4)
        );
        assert!(!EditorZoom::Fit.can_zoom_out_from_display_scale(0.2));
        assert!(EditorZoom::Fit.can_zoom_in_from_display_scale(0.2));
    }

    #[test]
    fn editor_actual_size_accounts_for_hidpi_display_scale() {
        assert_eq!(EditorZoom::actual_size(2.0), EditorZoom::Percent(0.5));
        assert!(EditorZoom::Percent(0.5).is_actual_size_for_display_scale(2.0));
        assert_eq!(
            EditorZoom::Percent(0.5).label_for_display_scale(2.0),
            "100%"
        );

        let s = EditorSession::new_with_display_scale(solid(8, 8), 2.0);
        assert_eq!(s.actual_size_zoom(), EditorZoom::Percent(0.5));
    }

    #[test]
    fn editor_zoom_label_distinguishes_fit_actual_and_custom_zoom() {
        let mut s = EditorSession::new_with_display_scale(solid(8, 8), 2.0);
        assert_eq!(s.zoom_label(), "Fit");

        s.zoom = s.actual_size_zoom();
        assert_eq!(s.zoom_label(), "100%");

        s.zoom = EditorZoom::Percent(1.0);
        assert_eq!(s.zoom_label(), "200%");
    }
}
