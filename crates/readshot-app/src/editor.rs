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

/// Which floating popover is open over the editor (at most one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorPopover {
    Color,
    Width,
    /// The title bar's Image menu (resize, rotate, flip).
    Image,
    /// The Resize dialog (opened from the Image menu or ⌥⌘I).
    Resize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResizeUnit {
    #[default]
    Pixels,
    Percent,
}

/// The Resize dialog's fields (spec §3.10). `base` is the size it opened
/// at (the displayed, cropped image), which percentages refer to.
#[derive(Clone, Debug, PartialEq)]
pub struct ResizeDraft {
    pub base: (u32, u32),
    pub width: String,
    pub height: String,
    pub unit: ResizeUnit,
    pub lock: bool,
}

impl ResizeDraft {
    pub fn new(base: (u32, u32)) -> Self {
        Self {
            base,
            width: base.0.to_string(),
            height: base.1.to_string(),
            unit: ResizeUnit::Pixels,
            lock: true,
        }
    }

    fn parse(s: &str) -> Option<f64> {
        s.trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v > 0.0)
    }

    fn fmt(v: f64) -> String {
        format!("{}", v.round() as u64)
    }

    pub fn set_width(&mut self, raw: &str) {
        self.width = raw.to_string();
        if self.lock {
            if let Some(w) = Self::parse(raw) {
                self.height = match self.unit {
                    ResizeUnit::Percent => Self::fmt(w),
                    ResizeUnit::Pixels => {
                        Self::fmt(w * self.base.1 as f64 / self.base.0.max(1) as f64)
                    }
                };
            }
        }
    }

    pub fn set_height(&mut self, raw: &str) {
        self.height = raw.to_string();
        if self.lock {
            if let Some(h) = Self::parse(raw) {
                self.width = match self.unit {
                    ResizeUnit::Percent => Self::fmt(h),
                    ResizeUnit::Pixels => {
                        Self::fmt(h * self.base.0 as f64 / self.base.1.max(1) as f64)
                    }
                };
            }
        }
    }

    /// Switch units, converting the current values.
    pub fn set_unit(&mut self, unit: ResizeUnit) {
        if unit == self.unit {
            return;
        }
        if let Some((w, h)) = self.target() {
            (self.width, self.height) = match unit {
                ResizeUnit::Percent => (
                    Self::fmt(w as f64 * 100.0 / self.base.0.max(1) as f64),
                    Self::fmt(h as f64 * 100.0 / self.base.1.max(1) as f64),
                ),
                ResizeUnit::Pixels => (w.to_string(), h.to_string()),
            };
        }
        self.unit = unit;
    }

    /// Target size in px (≥ 1), or `None` while a field isn't a number.
    pub fn target(&self) -> Option<(u32, u32)> {
        let (w, h) = (Self::parse(&self.width)?, Self::parse(&self.height)?);
        let (w, h) = match self.unit {
            ResizeUnit::Pixels => (w, h),
            ResizeUnit::Percent => (
                w * self.base.0 as f64 / 100.0,
                h * self.base.1 as f64 / 100.0,
            ),
        };
        let (w, h) = (w.round(), h.round());
        (w >= 1.0 && h >= 1.0 && w <= u32::MAX as f64 && h <= u32::MAX as f64)
            .then_some((w as u32, h as u32))
    }
}

/// What an eyedropper pick sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EyedropperTarget {
    /// The tool / selection color (spec §3.5).
    #[default]
    Annotation,
    /// Beautify's solid background (spec §3.9).
    BeautifyBackground,
}

/// Eyedropper mode state. The loupe updates as the cursor moves.
#[derive(Clone, Debug, Default)]
pub struct EyedropperState {
    pub loupe: Option<readshot_ui::editor::Loupe>,
    pub target: EyedropperTarget,
}

/// Side-panel tabs (spec §2.3): Text and Beautify. A new tab is a
/// variant here, its id, and one arm in `runtime::side_panel::side_panel`.
/// Which message of an Escape that ended a crop re-drag was handled
/// first ([`EditorSession::escape_ended_crop_drag`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeSeenBy {
    /// The canvas's `DragEscaped`; the key press is still to come.
    Canvas,
    /// The key press; the canvas's `DragEscaped` is still to come.
    Key,
}

/// The stored preference is the tab's `id()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PanelTab {
    #[default]
    Text,
    Beautify,
}

impl PanelTab {
    pub const ALL: [Self; 2] = [Self::Text, Self::Beautify];

    pub fn id(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Beautify => "beautify",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Beautify => "Beautify",
        }
    }

    /// Unknown ids (another build's tab) fall back to Text.
    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|t| t.id() == id)
            .unwrap_or_default()
    }
}

/// Text-panel recognition of the session's base image.
#[derive(Clone, Debug, Default)]
pub enum TextRecognition {
    #[default]
    Idle,
    Recognizing {
        since: std::time::Instant,
    },
    Ready(std::sync::Arc<readshot_core::text_lines::TextDocument>),
    Failed(String),
}

/// Everything the Text tab shows or edits.
#[derive(Clone, Debug, Default)]
pub struct TextPanelState {
    pub recognition: TextRecognition,
    /// Line under the pointer in the panel (outlined on the image).
    pub hovered: Option<usize>,
    /// Selected line indices (Copy N Lines).
    pub selected: std::collections::BTreeSet<usize>,
    /// ⇧-click range anchor.
    pub anchor: Option<usize>,
    pub redact_style: readshot_core::redact::RedactStyle,
    /// Sensitive spans found when recognition returned positions.
    pub matches: Vec<readshot_core::redact::RedactMatch>,
    /// Recognition was asked for through `Message::EditorTextRecognize`
    /// (panel, Retry) since the last result. A failure toasts only then
    /// or while the panel is open; a background (history) OCR failure
    /// with the panel closed just marks the panel Failed.
    pub user_requested: bool,
    /// Each part's own recognition, in that part's pixels. The document
    /// shown is derived from these for the current geometry
    /// (`refresh_text_for_geometry`); a part is recognized once, however
    /// often the geometry changes.
    pub part_docs: std::collections::BTreeMap<
        readshot_core::document::PartId,
        std::sync::Arc<readshot_core::text_lines::TextDocument>,
    >,
    /// The part the running recognition is for.
    pub recognizing_part: Option<readshot_core::document::PartId>,
    /// The geometry `recognition`'s document was built for; a different
    /// one (rotate, combine, undo across either) means its boxes are
    /// stale and must be rebuilt before anything uses them.
    pub built_for: Option<std::sync::Arc<readshot_core::document::DocGeometry>>,
    /// Parts made by baking a transformed document before a combine
    /// (decision D4), each with the geometry it was baked from. A baked
    /// part's text is its source parts' recognitions mapped through that
    /// geometry, never a recognition of the turned pixels (M-5).
    pub bake_sources: std::collections::BTreeMap<
        readshot_core::document::PartId,
        std::sync::Arc<readshot_core::document::DocGeometry>,
    >,
}

impl TextPanelState {
    pub fn document(&self) -> Option<&readshot_core::text_lines::TextDocument> {
        match &self.recognition {
            TextRecognition::Ready(doc) => Some(doc),
            _ => None,
        }
    }

    /// The document, only when the engine reported line positions.
    pub fn positioned_document(&self) -> Option<&readshot_core::text_lines::TextDocument> {
        self.document().filter(|d| d.positioned)
    }
}

/// Output size from which a resize, alignment or gap preview composes the
/// document off the UI thread: below it a compose takes a few ms; near the
/// 256 MP budget it allocates and fills ~1 GB, twice (compose, flatten).
pub const ASYNC_COMPOSE_MIN_PIXELS: u64 = 16_000_000;

/// What an off-thread compose is for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ComposeKind {
    /// Image › Resize… to `width` × `height`, a scale of `sx` × `sy`
    /// (one undo step).
    Resize {
        width: u32,
        height: u32,
        sx: f32,
        sy: f32,
    },
    /// Layout alignment (one undo step).
    Align(readshot_core::document::Align),
    /// A gap slider preview (no undo step; release commits).
    GapPreview(u32),
}

/// One off-thread compose: `next` is the state to show once composed.
/// It applies only while it is the newest request and the editor is
/// still in `present` at `generation` (and, for a gap preview, the same
/// slider drag): anything else made it stale, and it is dropped.
#[derive(Clone, Debug)]
pub struct ComposeRequest {
    pub seq: u64,
    pub kind: ComposeKind,
    pub generation: u64,
    pub present: readshot_ui::editor::Snapshot,
    pub drag_baseline: Option<readshot_ui::editor::Snapshot>,
    pub next: readshot_ui::editor::Snapshot,
}

/// A composed document: `base` is `next`'s geometry composed, and
/// `flattened` is `render(base, next.annotations)` (also in `handle`).
pub struct ComposedDoc {
    pub next: readshot_ui::editor::Snapshot,
    pub base: std::sync::Arc<image::RgbaImage>,
    pub flattened: image::RgbaImage,
    pub handle: iced::widget::image::Handle,
}

/// A compose result carried by a message; taken once by the handler.
/// `None` inside when the worker failed.
#[derive(Clone)]
pub struct ComposedOutput(std::sync::Arc<std::sync::Mutex<Option<ComposedDoc>>>);

impl ComposedOutput {
    pub fn new(doc: Option<ComposedDoc>) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(doc)))
    }

    pub fn take(&self) -> Option<ComposedDoc> {
        self.0.lock().ok().and_then(|mut d| d.take())
    }
}

impl std::fmt::Debug for ComposedOutput {
    // Never the pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComposedOutput")
    }
}

/// What History's image for this capture is known to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryFrame {
    /// `None` = the original capture; `Some(id)` = that document frame.
    Known(Option<String>),
    /// A write failed: the next sync rewrites the record in full.
    Unknown,
}

/// Button in the status toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorToastAction {
    /// "Redacted N items" → Undo ⌘Z. `undo_depth` is the model's undo
    /// depth right after the redaction: the button shows only while it
    /// still matches, so it can never undo a later edit.
    UndoRedaction { undo_depth: usize },
    /// Recognition failed → Retry.
    RetryRecognition,
}

/// A fresh, process-unique editor session id. Normally assigned by the
/// constructors; a fresh capture allocates one first so its history OCR
/// can be tagged for the editor before the editor exists.
pub(crate) fn next_session_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
    /// Baseline for an options-bar slider drag; see [`StyleDrag`].
    pub style_drag_baseline: Option<StyleDrag>,
    /// Like `snapshot_drag_cancelled`, for the options-bar style slider:
    /// set when Esc, undo or another edit dropped a drag whose slider is
    /// still held; its ticks are ignored and release clears it.
    pub style_drag_cancelled: bool,
    /// Baseline while a snapshot slider (Beautify padding/radius/shadow,
    /// combine gap) is dragged. Release commits one undo step; esc
    /// restores it.
    pub snapshot_drag_baseline: Option<readshot_ui::editor::Snapshot>,
    /// Set when Esc, undo or another edit dropped a snapshot drag whose
    /// slider is still held: iced keeps sending ticks until release, and
    /// they must not start a new preview. Only raised while the mouse is
    /// down ([`Self::pointer_held`]); cleared on release and on any new
    /// press.
    pub snapshot_drag_cancelled: bool,
    /// The left mouse button is down (tracked globally). A cancelled
    /// slider drag only raises the `*_drag_cancelled` flags while it is:
    /// a key or Ctrl+wheel preview has no held slider to ignore, and a
    /// flag raised for it would swallow the next real drag.
    /// Set through [`Self::set_pointer_held`]; cleared also when the
    /// window the press began in loses focus or closes
    /// ([`Self::pointer_window_gone`]), whose release may never come. A
    /// new session starts with it clear.
    pub pointer_held: bool,
    /// The window the held press began in (`None`: not held, or not
    /// known).
    pub pointer_window: Option<iced::window::Id>,
    /// One Escape just ended a live crop re-drag, seen by one of its two
    /// messages (the canvas's `DragEscaped`, the key subscription's
    /// `EditorKeyPressed`); the other, in whichever order it comes, must
    /// not also drop the draft the drag was hiding: one Esc, one step.
    /// Cleared by its counterpart, and by any other key, press or drag
    /// start, so it can't swallow a later Escape.
    pub escape_ended_crop_drag: Option<EscapeSeenBy>,
    /// The newest combine gap slider value not previewed yet: gap ticks
    /// are throttled (each preview recomposes the document), and the
    /// last one is applied on the trailing edge, on release, or before
    /// any output (`commit_snapshot_drag`).
    pub layout_gap_pending: Option<u32>,
    /// When the last gap preview was applied (throttle clock).
    pub layout_gap_previewed_at: Option<std::time::Instant>,
    /// The document compose running off the UI thread (a large resize,
    /// alignment or gap preview), if any. Only the newest request's
    /// result is applied, and only while the editor is still in the
    /// state it was requested from.
    pub compose_job: Option<ComposeRequest>,
    /// Source of [`ComposeRequest::seq`].
    pub compose_seq: u64,
    /// The newest gap value that arrived while a gap compose was running:
    /// previewed when that one lands (one compose in flight at a time).
    pub gap_queued: Option<u32>,
    /// The gap slider was released while its compose was still running:
    /// commit the drag once the newest value has landed.
    pub gap_commit_on_land: bool,
    /// A history step was taken where no History sync could run (a
    /// released gap drag settled inside `cancel_editor_previews`); synced
    /// once the message that caused it is handled.
    pub history_sync_owed: bool,
    /// Documents of at least this many output pixels compose off the UI
    /// thread ([`ASYNC_COMPOSE_MIN_PIXELS`]; tests lower it).
    pub async_compose_min_pixels: u64,
    /// What the user typed into the Beautify tab's background hex field
    /// and hasn't applied yet. `None` while they aren't editing it: the
    /// field then shows the current solid background
    /// ([`Self::beautify_hex_shown`]), so it follows the tab opening, a
    /// preset, undo or the eyedropper.
    pub beautify_hex_draft: Option<String>,
    /// Open color / line-width popover, if any. Esc and a
    /// click outside close it before any other Esc behaviour runs.
    pub popover: Option<EditorPopover>,
    /// The Resize dialog's fields while it is open.
    pub resize_draft: Option<ResizeDraft>,
    /// Draft text of the color popover's hex field.
    pub hex_draft: String,
    /// Some while the eyedropper is active (spec §3.5).
    pub eyedropper: Option<EyedropperState>,
    /// Crop tool: the dragged rect waiting for Apply ↵ (spec §3.10).
    pub crop_draft: Option<readshot_core::RectLike>,
    /// True while a shape/freehand drag is in progress on the canvas
    /// (between `DragStarted` and its `Cancelled`/`CommitAnnotation`).
    /// Escape uses this to decide whether to cancel the drag or fall
    /// through to the discard shortcut.
    pub canvas_drag_active: bool,
    /// Monotonic counter bumped when Escape cancels a live canvas drag.
    /// Threaded into the `EditorCanvas` program so the widget can drop the
    /// drag on its next event — iced 0.14 never delivers the Escape key to
    /// the canvas itself.
    pub drag_cancel_seq: u64,
    /// The displayed-pixels-per-base-pixel scale used by the most recent
    /// `view()`. The canvas hit-tests handle/body grabs in base coords, so
    /// the runtime feeds this back into `select_at_scaled` /
    /// `resize_handle_at_scaled` to keep grab targets constant on screen.
    /// Interior-mutable so `view()` (which takes `&self`) can record it.
    pub render_scale: std::rc::Rc<std::cell::Cell<f32>>,
    /// The stage scrollable's offset (content px) as last reported by
    /// its `on_scroll`; `view()` resets it while the stage doesn't scroll
    /// (a scrollable shown again starts at the top-left). Where a new
    /// magnifier bubble may go depends on what is on screen.
    pub stage_offset: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
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
    /// Backing store for the inline multi-line text editor widget. Text
    /// annotations support `\n`, so the inline editor is an
    /// `iced::widget::text_editor` whose `Content` must persist across
    /// `view()` calls. It mirrors [`PendingText::content`] (the String
    /// remains the source of truth for commit / bounds); this field only
    /// exists while `pending_text` is `Some`. Not `Clone`, which is why
    /// [`EditorSession`] itself is not `Clone`.
    pub text_edit_content: Option<iced::widget::text_editor::Content>,
    /// Unique per session. Tags background recognition results so one
    /// that lands after the editor was replaced is dropped.
    pub session_id: u64,
    /// Text panel: recognition, hover, line selection, redact.
    pub text_panel: TextPanelState,
    /// Button shown in the status toast. Cleared by every `set_status`.
    pub toast_action: Option<EditorToastAction>,
    /// Current editor window width (logical px): `window::size` when the
    /// window is ready, then resize events. Decides dock vs collapse.
    pub window_width: f32,
    /// Narrow windows only: the temporary overlay panel is open. Never
    /// persisted; esc, ⌥⌘P or a click outside closes it.
    pub panel_overlay_open: bool,
    /// History may hold a scrubbed (redacted, flattened) copy of this
    /// capture written by this session, or its state is unknown after a
    /// failed write. While set, an edit that leaves no redaction rewrites
    /// the record in full (restoring the original) instead of the plain
    /// sidecar sync, which can't change a scrubbed record.
    pub history_scrubbed: bool,
    /// Prefix of this session's History frame ids, so a frame id from one
    /// session never matches another session's (or another capture's)
    /// frame with the same geometry fingerprint.
    pub history_nonce: readshot_core::Uuid,
    /// The frame id of the image the editor opened with: `None` for a
    /// fresh capture, the record's `geometry_id` when reopened from
    /// History (its stored image may itself be a transformed document).
    pub history_geometry_base: Option<String>,
    /// The frame History's image was last written in by this session.
    pub history_frame_written: HistoryFrame,
    /// Outcome of the newest queued History rewrite, taken by the runtime
    /// and turned into `Message::EditorHistoryWritten`.
    pub history_write:
        Option<iced::futures::channel::oneshot::Receiver<crate::coordinator::HistoryRewriteResult>>,
}

/// What the user last copied/saved: annotations, geometry and Beautify.
#[derive(Clone, Debug, PartialEq)]
struct EditorOutputCheckpoint(readshot_ui::editor::Snapshot);

/// How long the "Press Discard again" prompt stays armed
/// before reverting to a fresh single-click state.
pub const DISCARD_CONFIRM_WINDOW: std::time::Duration = std::time::Duration::from_secs(4);

/// Width the editor window opens at (logical px).
pub const DEFAULT_WINDOW_WIDTH: f32 = 1100.0;

/// Successful status toasts auto-dismiss after this window so they
/// don't linger forever. Error / in-progress messages stay until
/// they're dismissed (the toast's close button) or replaced.
pub const STATUS_AUTO_DISMISS: std::time::Duration = std::time::Duration::from_secs(4);

/// A status string is "in progress" when it ends with `…` or starts
/// with one of the known in-flight prefixes ("Saving", "Copying",
/// "Recognizing", "Choose"). Kept as a free function so the view can
/// classify a toast string identically to [`EditorSession`] without
/// re-implementing (and drifting from) the rule.
pub fn status_str_is_in_progress(s: &str) -> bool {
    s.ends_with('…')
        || s.starts_with("Saving")
        || s.starts_with("Copying")
        || s.starts_with("Recognizing")
        || s.starts_with("Choose")
}

/// A status string is an error when it mentions "fail" or "error"
/// (case-insensitive), or when it opens with the friendly "Couldn't …"
/// failure phrasing. Errors are never auto-dismissed.
pub fn status_str_is_error(s: &str) -> bool {
    let lower = s.to_lowercase();
    lower.contains("fail") || lower.contains("error") || lower.starts_with("couldn't")
}

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

/// Saved state while an options-bar slider (corner radius, dim) is
/// dragged. Release keeps the new values. Esc restores both the
/// selection (`annotations`, `None` when nothing was selected) and the
/// tool style.
#[derive(Clone, Debug)]
pub struct StyleDrag {
    pub annotations: Option<Vec<Annotation>>,
    pub tool_style: readshot_ui::editor::ToolStyle,
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
        let output_checkpoint = EditorOutputCheckpoint(model.snapshot());
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
            popover: None,
            resize_draft: None,
            hex_draft: String::new(),
            eyedropper: None,
            crop_draft: None,
            status_set_at: None,
            move_drag: None,
            width_drag_baseline: None,
            style_drag_baseline: None,
            style_drag_cancelled: false,
            snapshot_drag_baseline: None,
            snapshot_drag_cancelled: false,
            pointer_held: false,
            pointer_window: None,
            escape_ended_crop_drag: None,
            layout_gap_pending: None,
            layout_gap_previewed_at: None,
            compose_job: None,
            compose_seq: 0,
            gap_queued: None,
            gap_commit_on_land: false,
            history_sync_owed: false,
            async_compose_min_pixels: ASYNC_COMPOSE_MIN_PIXELS,
            beautify_hex_draft: None,
            canvas_drag_active: false,
            drag_cancel_seq: 0,
            render_scale: std::rc::Rc::new(std::cell::Cell::new(1.0)),
            stage_offset: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            canvas_cache: std::rc::Rc::new(iced::widget::canvas::Cache::default()),
            output_checkpoint,
            crop_drag_flat: None,
            text_edit_content: None,
            session_id: next_session_id(),
            text_panel: TextPanelState::default(),
            toast_action: None,
            window_width: DEFAULT_WINDOW_WIDTH,
            panel_overlay_open: false,
            history_scrubbed: false,
            history_nonce: readshot_core::Uuid::new_v4(),
            history_geometry_base: None,
            history_frame_written: HistoryFrame::Known(None),
            history_write: None,
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
        let output_checkpoint = EditorOutputCheckpoint(model.snapshot());
        let history_geometry_base = record.geometry_id.clone();
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
            popover: None,
            resize_draft: None,
            hex_draft: String::new(),
            eyedropper: None,
            crop_draft: None,
            status_set_at: None,
            move_drag: None,
            width_drag_baseline: None,
            style_drag_baseline: None,
            style_drag_cancelled: false,
            snapshot_drag_baseline: None,
            snapshot_drag_cancelled: false,
            pointer_held: false,
            pointer_window: None,
            escape_ended_crop_drag: None,
            layout_gap_pending: None,
            layout_gap_previewed_at: None,
            compose_job: None,
            compose_seq: 0,
            gap_queued: None,
            gap_commit_on_land: false,
            history_sync_owed: false,
            async_compose_min_pixels: ASYNC_COMPOSE_MIN_PIXELS,
            beautify_hex_draft: None,
            canvas_drag_active: false,
            drag_cancel_seq: 0,
            render_scale: std::rc::Rc::new(std::cell::Cell::new(1.0)),
            stage_offset: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            canvas_cache: std::rc::Rc::new(iced::widget::canvas::Cache::default()),
            output_checkpoint,
            crop_drag_flat: None,
            text_edit_content: None,
            session_id: next_session_id(),
            text_panel: TextPanelState::default(),
            toast_action: None,
            window_width: DEFAULT_WINDOW_WIDTH,
            panel_overlay_open: false,
            history_scrubbed: false,
            history_nonce: readshot_core::Uuid::new_v4(),
            history_frame_written: HistoryFrame::Known(history_geometry_base.clone()),
            history_geometry_base,
            history_write: None,
        }
    }

    pub fn actual_size_zoom(&self) -> EditorZoom {
        EditorZoom::actual_size(self.display_scale)
    }

    /// Drop the loupe after the view moves under a still cursor (zoom),
    /// so it never shows a pixel other than the one a click would pick.
    /// The next hover refills it.
    pub fn clear_eyedropper_loupe(&mut self) {
        if let Some(eye) = self.eyedropper.as_mut() {
            eye.loupe = None;
        }
    }

    /// Set the toast text under the action row and stamp the
    /// `status_set_at` clock so auto-dismiss can run on the runtime
    /// tick. Use this everywhere `status = Some(…)` was set inline so
    /// the auto-dismiss is uniform across save / copy / OCR / pin
    /// paths.
    ///
    /// Any status set here replaces the armed-discard prompt, so it also
    /// disarms Discard (the armed prompt itself is written directly by
    /// `EditorDiscardRequested`, not through this method).
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.toast_action = None;
        self.discard_pending_at = None;
        self.status = Some(msg.into());
        self.status_set_at = Some(std::time::Instant::now());
    }

    /// [`Self::set_status`] plus a button in the toast.
    pub fn set_status_with_action(&mut self, msg: impl Into<String>, action: EditorToastAction) {
        self.set_status(msg);
        self.toast_action = Some(action);
    }

    /// The toast button to show: a redaction's Undo only while that
    /// redaction is still the newest history step.
    pub fn visible_toast_action(&self) -> Option<EditorToastAction> {
        self.toast_action.filter(|a| match a {
            EditorToastAction::UndoRedaction { undo_depth } => {
                self.model.undo_depth() == *undo_depth
            }
            EditorToastAction::RetryRecognition => true,
        })
    }

    /// Open the inline text editor for `pending`, seeding the
    /// `text_editor::Content` from its current string so the widget and
    /// [`PendingText::content`] start in sync. Use this everywhere a
    /// text draft/edit begins so the multi-line backing store is never
    /// missing while `pending_text` is `Some`.
    pub fn begin_text_edit(&mut self, pending: PendingText) {
        self.text_edit_content = Some(iced::widget::text_editor::Content::with_text(
            &pending.content,
        ));
        self.pending_text = Some(pending);
    }

    /// Tear down the inline text editor: drop both the draft and its
    /// backing `Content` together so they never disagree.
    pub fn clear_text_edit(&mut self) {
        self.pending_text = None;
        self.text_edit_content = None;
    }

    pub fn mark_output_clean(&mut self) {
        self.output_checkpoint = EditorOutputCheckpoint(self.model.snapshot());
        self.clear_discard_confirmation();
    }

    pub fn has_output_changes(&self) -> bool {
        self.output_checkpoint.0 != self.model.snapshot()
    }

    /// A left press (`Some(window)`, the window when known) or release
    /// (`None`). Either way a raised cancel flag belongs to a drag that
    /// is over: a press starts a new drag, a release ends the held one.
    pub fn set_pointer_held(&mut self, pressed_in: Option<Option<iced::window::Id>>) {
        if pressed_in.is_some() {
            // A new press: an Escape marker still waiting is stale.
            self.escape_ended_crop_drag = None;
        }
        self.pointer_held = pressed_in.is_some();
        self.pointer_window = pressed_in.flatten();
        self.style_drag_cancelled = false;
        self.snapshot_drag_cancelled = false;
    }

    /// Window `id` lost focus or closed: a press held since it began
    /// there counts as released.
    pub fn pointer_window_gone(&mut self, id: iced::window::Id) {
        if self.pointer_held && self.pointer_window == Some(id) {
            self.set_pointer_held(None);
        }
    }

    /// The Beautify hex field's text: the user's draft while they edit
    /// it, else the current solid background (empty for any other).
    pub fn beautify_hex_shown(&self) -> String {
        if let Some(draft) = &self.beautify_hex_draft {
            return draft.clone();
        }
        match self.model.beautify().background {
            readshot_core::beautify::Background::Solid { rgba } => {
                readshot_core::Rgba::from_rgba8(rgba).to_hex()
            }
            _ => String::new(),
        }
    }

    pub fn clear_discard_confirmation(&mut self) {
        self.discard_pending_at = None;
    }

    /// "In-progress" status strings the auto-dismiss logic must
    /// leave alone — clearing "Saving…" before the save actually
    /// completes would look broken.
    pub fn status_is_in_progress(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(status_str_is_in_progress)
    }

    /// Whether the current status toast is an error. Errors stay on
    /// screen (never auto-dismissed) and render with the error style;
    /// this is the single classifier the runtime and view share so the
    /// auto-dismiss window and the toast colour never disagree.
    pub fn status_is_error(&self) -> bool {
        self.status.as_deref().is_some_and(status_str_is_error)
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

    /// [`Self::refresh_image`] with the handle already built (from the
    /// model's flattened image, off the UI thread).
    pub fn install_image(&mut self, handle: iced::widget::image::Handle) {
        self.crop_drag_flat = None;
        self.image_handle = handle;
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
            // Round like the renderer does: a crop remapped by a resize
            // is fractional, and truncating would read 1 px small.
            (w.round() as u32, h.round() as u32)
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

    pub(crate) fn last_crop(&self) -> Option<readshot_core::RectLike> {
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
    fn status_classifiers_agree_across_in_progress_error_and_success() {
        // In-progress: kept on screen, neutral style.
        for s in [
            "Saving…",
            "Choose a save location…",
            "Copying…",
            "Recognizing text…",
        ] {
            assert!(status_str_is_in_progress(s), "{s:?} should be in-progress");
            assert!(!status_str_is_error(s), "{s:?} should not be an error");
        }
        // Errors: kept on screen, error style. Includes the friendly
        // "Couldn't …" phrasing the failure paths now produce, which
        // carries neither "fail" nor "error".
        for s in [
            "Couldn't save the image — the disk is full. Pick a different folder in Settings and try again.",
            "Couldn't copy the text — permission denied. Try again.",
            "Unexpected error",
        ] {
            assert!(status_str_is_error(s), "{s:?} should be an error");
            assert!(
                !status_str_is_in_progress(s),
                "{s:?} should not be in-progress"
            );
        }
        // Success / info: eligible for auto-dismiss, success style.
        for s in ["Saved to ~/Desktop/shot.png", "Added text. ⌘Z to undo."] {
            assert!(!status_str_is_error(s), "{s:?} should not be an error");
            assert!(
                !status_str_is_in_progress(s),
                "{s:?} should not be in-progress"
            );
        }
    }

    #[test]
    fn editor_session_status_helpers_match_free_classifiers() {
        let mut s = EditorSession::new(solid(8, 8));
        s.set_status("Couldn't save the image — the disk is full. Pick a different folder in Settings and try again.");
        assert!(s.status_is_error());
        assert!(!s.status_is_in_progress());
        s.set_status("Saving…");
        assert!(s.status_is_in_progress());
        assert!(!s.status_is_error());
        s.set_status("Saved to disk");
        assert!(!s.status_is_error());
        assert!(!s.status_is_in_progress());
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

    #[test]
    fn set_status_clears_a_toast_action() {
        let mut ed = EditorSession::new(image::RgbaImage::new(4, 4));
        let undo = EditorToastAction::UndoRedaction { undo_depth: 0 };
        ed.set_status_with_action("Redacted 1 item.", undo);
        assert_eq!(ed.toast_action, Some(undo));
        ed.set_status("Saved.");
        assert_eq!(ed.toast_action, None);
    }

    #[test]
    fn redaction_undo_button_hides_after_any_other_history_change() {
        let mut ed = EditorSession::new(image::RgbaImage::new(8, 8));
        let blur = readshot_core::Annotation::Blur {
            rect: readshot_core::RectLike::new(0.0, 0.0, 2.0, 2.0),
            radius: 2.0,
        };
        ed.model.commit_annotation(blur.clone());
        let undo_depth = ed.model.undo_depth();
        ed.toast_action = Some(EditorToastAction::UndoRedaction { undo_depth });
        assert!(ed.visible_toast_action().is_some());
        ed.model.commit_annotation(blur);
        assert!(
            ed.visible_toast_action().is_none(),
            "Undo would undo the newer edit"
        );
        ed.model.undo();
        assert!(
            ed.visible_toast_action().is_some(),
            "the redaction is newest again"
        );
        ed.toast_action = Some(EditorToastAction::RetryRecognition);
        assert!(ed.visible_toast_action().is_some());
    }

    #[test]
    fn panel_tab_ids_round_trip_and_unknown_ids_fall_back_to_text() {
        for tab in PanelTab::ALL {
            assert_eq!(PanelTab::from_id(tab.id()), tab);
        }
        assert_eq!(PanelTab::from_id("beautify"), PanelTab::Beautify);
        assert_eq!(PanelTab::from_id("future"), PanelTab::Text);
        assert_eq!(PanelTab::from_id(""), PanelTab::Text);
    }

    #[test]
    fn sessions_get_distinct_ids_and_start_idle() {
        let a = EditorSession::new(image::RgbaImage::new(4, 4));
        let b = EditorSession::new(image::RgbaImage::new(4, 4));
        assert_ne!(a.session_id, b.session_id);
        assert!(matches!(a.text_panel.recognition, TextRecognition::Idle));
        assert!(a.text_panel.document().is_none());
    }
}
