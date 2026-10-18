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

/// The Resize dialog's fields. `base` is the size it opened
/// at (the displayed, cropped image), which percentages refer to.
///
/// The fields show whole numbers, but a value the dialog worked out
/// itself (a unit switch, the locked side) keeps its exact size in px
/// while its field still shows it: switching px → % → px gives back the
/// same px, and the locked side follows the original aspect, never a
/// rounded field.
#[derive(Clone, Debug, PartialEq)]
pub struct ResizeDraft {
    pub base: (u32, u32),
    pub width: String,
    pub height: String,
    pub unit: ResizeUnit,
    pub lock: bool,
    /// The exact px behind the width field while it shows this text.
    exact_width: Option<(String, f64)>,
    /// The exact px behind the height field while it shows this text.
    exact_height: Option<(String, f64)>,
}

impl ResizeDraft {
    pub fn new(base: (u32, u32)) -> Self {
        Self {
            base,
            width: base.0.to_string(),
            height: base.1.to_string(),
            unit: ResizeUnit::Pixels,
            lock: true,
            exact_width: None,
            exact_height: None,
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

    /// A field's value in px: its exact value while it shows what the
    /// dialog put there, else what it says in the current unit.
    fn field_px(&self, shown: &str, exact: &Option<(String, f64)>, base: u32) -> Option<f64> {
        if let Some((text, px)) = exact {
            if text == shown {
                return Some(*px);
            }
        }
        let v = Self::parse(shown)?;
        Some(match self.unit {
            ResizeUnit::Pixels => v,
            ResizeUnit::Percent => v * base as f64 / 100.0,
        })
    }

    fn width_px(&self) -> Option<f64> {
        self.field_px(&self.width, &self.exact_width, self.base.0)
    }

    fn height_px(&self) -> Option<f64> {
        self.field_px(&self.height, &self.exact_height, self.base.1)
    }

    /// `px` (along an axis of `base` px) as the current unit shows it.
    fn shown(&self, px: f64, base: u32) -> String {
        match self.unit {
            ResizeUnit::Pixels => Self::fmt(px),
            ResizeUnit::Percent => Self::fmt(px * 100.0 / base.max(1) as f64),
        }
    }

    /// Set the height from the width at the original aspect, never
    /// below 1 px (an extreme aspect would round it to 0, like
    /// `document::scaled_size` keeps every side at least 1 px).
    fn sync_height(&mut self) {
        if let Some(w) = self.width_px() {
            let h = (w * self.base.1 as f64 / self.base.0.max(1) as f64).max(1.0);
            self.height = self.shown(h, self.base.1);
            self.exact_height = Some((self.height.clone(), h));
        }
    }

    /// Set the width from the height at the original aspect, never
    /// below 1 px.
    fn sync_width(&mut self) {
        if let Some(h) = self.height_px() {
            let w = (h * self.base.0 as f64 / self.base.1.max(1) as f64).max(1.0);
            self.width = self.shown(w, self.base.0);
            self.exact_width = Some((self.width.clone(), w));
        }
    }

    pub fn set_width(&mut self, raw: &str) {
        self.width = raw.to_string();
        self.exact_width = None;
        if self.lock {
            self.sync_height();
        }
    }

    pub fn set_height(&mut self, raw: &str) {
        self.height = raw.to_string();
        self.exact_height = None;
        if self.lock {
            self.sync_width();
        }
    }

    /// Lock or unlock the aspect; locking sets the height from the width.
    pub fn set_lock(&mut self, on: bool) {
        self.lock = on;
        if on {
            self.sync_height();
        }
    }

    /// Switch units, converting the current values (exactly: switching
    /// back shows what was there).
    pub fn set_unit(&mut self, unit: ResizeUnit) {
        if unit == self.unit {
            return;
        }
        let (w, h) = (self.width_px(), self.height_px());
        self.unit = unit;
        if let Some(w) = w {
            self.width = self.shown(w, self.base.0);
            self.exact_width = Some((self.width.clone(), w));
        }
        if let Some(h) = h {
            self.height = self.shown(h, self.base.1);
            self.exact_height = Some((self.height.clone(), h));
        }
    }

    /// Why there is no [`Self::target`], if there isn't: a field that
    /// isn't a positive number, a side under 1 px, or one too large.
    pub fn problem(&self) -> Option<&'static str> {
        if self.target().is_some() {
            return None;
        }
        let (Some(w), Some(h)) = (self.width_px(), self.height_px()) else {
            return Some("Enter the width and height as numbers.");
        };
        Some(if w.round() < 1.0 {
            "The width comes to less than 1\u{a0}px. Enter a larger size."
        } else if h.round() < 1.0 {
            "The height comes to less than 1\u{a0}px. Enter a larger size."
        } else {
            "That size is too large. Enter a smaller size."
        })
    }

    /// Target size in px (≥ 1), or `None` while a field isn't a number.
    pub fn target(&self) -> Option<(u32, u32)> {
        let (w, h) = (self.width_px()?, self.height_px()?);
        let (w, h) = (w.round(), h.round());
        (w >= 1.0 && h >= 1.0 && w <= u32::MAX as f64 && h <= u32::MAX as f64)
            .then_some((w as u32, h as u32))
    }
}

/// What an eyedropper pick sets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EyedropperTarget {
    /// The tool / selection color.
    #[default]
    Annotation,
    /// Beautify's solid background.
    BeautifyBackground,
}

/// Eyedropper mode state. The loupe updates as the cursor moves.
#[derive(Clone, Debug, Default)]
pub struct EyedropperState {
    pub loupe: Option<readshot_ui::editor::Loupe>,
    pub target: EyedropperTarget,
}

/// Side-panel tabs: Text and Beautify. A new tab is a
/// variant here, its id, and one arm in `runtime::side_panel::side_panel`.
/// The frame a canvas drag's points are in
/// ([`EditorSession::drag_started_in`]).
#[derive(Clone, Debug)]
pub enum CanvasDragFrame {
    /// Begun in this frame: its commit is kept only while it still is
    /// the present one.
    Live(std::sync::Arc<readshot_core::document::DocGeometry>),
    /// Its press landed a pending step (`compose_jobs::drop_stale_press`):
    /// the drag is aimed at a frame no longer shown and commits nothing.
    Dropped,
}

/// The frame change a remapped Select press was mapped through
/// ([`EditorSession::press_remap`]).
#[derive(Clone, Debug)]
pub struct PressRemap {
    /// The frame the canvas's points are in (the one shown).
    pub old: std::sync::Arc<readshot_core::document::DocGeometry>,
    /// The present frame they are mapped into.
    pub new: std::sync::Arc<readshot_core::document::DocGeometry>,
    /// The one map the press was mapped with (the affine of the part
    /// under it): every same-batch drag point goes through it too, so the
    /// move's delta is in one frame whichever part a point is over.
    pub map: readshot_core::document::Affine,
    /// Display scale (screen px per present document px) the press was
    /// made at: the old view's scale through `map`, for its hit tests.
    pub scale: f32,
    /// `canvas_view_seq` when the press was mapped: once a view is built
    /// after it, the canvas sends points in the new frame.
    pub view_seq: u64,
}

/// Whether two frames are the same: one geometry, or equal ones (a copy).
pub fn same_frame(
    a: &std::sync::Arc<readshot_core::document::DocGeometry>,
    b: &std::sync::Arc<readshot_core::document::DocGeometry>,
) -> bool {
    std::sync::Arc::ptr_eq(a, b) || **a == **b
}

/// Which message of an Escape that ended a canvas drag (or a slider
/// preview) was handled first ([`EditorSession::escape_ended_drag`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscapeSeenBy {
    /// The canvas's `DragEscaped`, or its `Cancelled` that cancelled a
    /// slider preview; the key press is still to come.
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

/// A keyboard focus zone of the editor (WCAG 2.1.1). iced 0.14 buttons,
/// sliders and pick-lists can't take focus, so the editor keeps its own
/// focus: `None` is the canvas (tools, nudge, Tab through annotations);
/// a zone is a group of pointer-only controls the keys walk through
/// (`runtime/key_focus.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusZone {
    /// The title bar's Image menu (and its Layout rows).
    ImageMenu,
    /// The options bar under the tool bar.
    OptionsBar,
    /// The side panel: its tabs, then the current tab's controls.
    Panel,
    /// The Resize dialog.
    Resize,
}

/// One keyboard-focusable control inside a [`FocusZone`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FocusItem {
    MenuResize,
    MenuRotateLeft,
    MenuRotateRight,
    MenuFlipH,
    MenuFlipV,
    MenuAddBelow,
    MenuAddBeside,
    MenuAddImage,
    MenuGap,
    MenuAlign,
    Option(readshot_ui::editor::OptionControl),
    PanelTabs,
    BeautifyPreset(readshot_core::beautify::BuiltinPreset),
    BeautifySaved(usize),
    BeautifyRemoveSaved(usize),
    BeautifyBackground,
    BeautifyHex,
    BeautifyPick,
    BeautifyPadding,
    BeautifyCorners,
    BeautifyShadow,
    BeautifyCanvas,
    BeautifyBalance,
    BeautifySave,
    TextRetry,
    TextOpenSettings,
    TextRedactStyle,
    TextRedactAll,
    TextLines,
    TextLineBreaks,
    TextCopyLines,
    TextCopyAll,
    ResizeWidth,
    ResizeHeight,
    ResizeUnit,
    ResizeLock,
    ResizeCancel,
    ResizeApply,
}

/// Where the editor's keyboard focus is when it is not on the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct KeyFocus {
    pub zone: FocusZone,
    pub item: FocusItem,
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
    /// The Text panel's keyboard line cursor (↑/↓ while the panel has
    /// keyboard focus): outlined on the image and shows the line's Redact
    /// button, as hover does.
    pub cursor: Option<usize>,
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
    /// Parts made by baking a transformed document into one part before
    /// a combine (a part is never added to a turned document), each
    /// with the geometry it was baked from. A baked
    /// part's text is its source parts' recognitions mapped through that
    /// geometry, never a recognition of the turned pixels.
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

/// Output size from which document work runs off the UI thread: a
/// resize, alignment or gap preview, a rotate or flip, an added part,
/// undo / redo, and re-rendering the annotations over the document after
/// an edit. Below it a compose takes a few ms; near the 256 MP budget it
/// allocates and fills ~1 GB, twice (compose, flatten).
pub const ASYNC_COMPOSE_MIN_PIXELS: u64 = 16_000_000;

/// Below this many document pixels a dragged redaction is rendered where
/// it is on every drag tick (a full render of the document), so its real
/// effect shows while it moves; at and above it the canvas covers it
/// opaquely until the release renders it.
pub const LIVE_REDACTION_MAX_PIXELS: u64 = 4_000_000;

#[cfg(test)]
pub(crate) use crate::test_probe::ui_renders_on_this_thread;

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
    /// A gap slider drag released while its preview composed: the value
    /// kept, as one undo step.
    Gap(u32),
    /// Rotate a quarter (one undo step).
    Rotate { clockwise: bool },
    /// Flip (one undo step).
    Flip(readshot_core::document::FlipAxis),
    /// Add a part, `what` being "capture" or "image" (one undo step).
    AddPart {
        join: readshot_core::document::Join,
        what: &'static str,
    },
    /// Undo one step.
    Undo,
    /// Redo one step.
    Redo,
    /// Re-render the annotations over the present document for display
    /// (no history change): see [`EditorSession::refresh_image`].
    Flatten,
}

impl ComposeKind {
    /// A history step: queued, applied in order, never dropped.
    pub fn is_step(self) -> bool {
        !matches!(self, Self::GapPreview(_) | Self::Flatten)
    }
}

/// One queued history step ([`ComposeKind::is_step`]): its history move
/// and what landing it needs besides.
#[derive(Clone, Debug)]
pub struct QueuedStep {
    pub kind: ComposeKind,
    pub op: readshot_ui::editor::DocOp,
    /// An added part's image (to redo the step on a moved document).
    pub part: Option<std::sync::Arc<image::RgbaImage>>,
    /// An added part baked the transformed document first: the baked
    /// part's id and the geometry it was baked from (the Text panel maps
    /// the old recognitions through it).
    pub bake: Option<(
        readshot_core::document::PartId,
        std::sync::Arc<readshot_core::document::DocGeometry>,
    )>,
}

/// One off-thread compose: `next` is the state to show once composed.
/// It applies only while it is the newest request and the editor is
/// still in `present` at `generation` (and, for a gap preview, the same
/// slider drag; for a flatten, the same annotations over the same base):
/// anything else made it stale, and it is dropped.
///
/// History steps queue in `steps` (in order; one compose in flight, for
/// the first `composed` of them): a step that arrives while one composes
/// is applied with it or composed next, never dropped.
#[derive(Clone, Debug)]
pub struct ComposeRequest {
    pub seq: u64,
    /// The newest queued step's kind (or the preview / flatten).
    pub kind: ComposeKind,
    pub generation: u64,
    pub present: readshot_ui::editor::Snapshot,
    pub drag_baseline: Option<readshot_ui::editor::Snapshot>,
    pub next: readshot_ui::editor::Snapshot,
    pub steps: Vec<QueuedStep>,
    /// How many of `steps` the compose in flight covers.
    pub composed: usize,
    /// `next`'s composed base when it was cached (the worker then only
    /// flattens; for a flatten, always the present base).
    pub base: Option<std::sync::Arc<image::RgbaImage>>,
    /// The worker's result slot, shared with its message, so settling
    /// can wait for a compose already running instead of redoing it.
    pub output: ComposedOutput,
    /// Whether its worker was started (`compose_jobs::launch`).
    pub launched: bool,
}

/// A composed document: `base` is `next`'s geometry composed, and
/// `flattened` is `render(base, next.annotations)`. The model takes the
/// flattened pixels for both its cache and the display (no copy).
pub struct ComposedDoc {
    pub next: readshot_ui::editor::Snapshot,
    pub base: std::sync::Arc<image::RgbaImage>,
    pub flattened: image::RgbaImage,
}

/// A compose result slot shared by the worker, its message and the
/// request; taken once. `None` inside when the worker failed.
#[derive(Clone)]
pub struct ComposedOutput(std::sync::Arc<OutputSlot>);

pub struct OutputSlot {
    state: std::sync::Mutex<SlotState>,
    done: std::sync::Condvar,
}

enum SlotState {
    /// The worker hasn't started.
    Queued,
    /// The worker is composing.
    Running,
    /// The worker is composing for a request that was replaced: it frees
    /// its result itself.
    Abandoned,
    /// The worker finished (`None`: it failed).
    Done(Option<ComposedDoc>),
    /// Taken, or claimed by the UI thread before the worker started.
    Taken,
}

impl ComposedOutput {
    /// A slot holding `doc` already.
    pub fn new(doc: Option<ComposedDoc>) -> Self {
        Self::with(SlotState::Done(doc))
    }

    /// A slot for a worker that hasn't started.
    pub fn queued() -> Self {
        Self::with(SlotState::Queued)
    }

    fn with(state: SlotState) -> Self {
        Self(std::sync::Arc::new(OutputSlot {
            state: std::sync::Mutex::new(state),
            done: std::sync::Condvar::new(),
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The worker's result, once.
    pub fn take(&self) -> Option<ComposedDoc> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Done(doc) => doc,
            other @ (SlotState::Queued | SlotState::Running | SlotState::Abandoned) => {
                // Not finished: leave it for whoever waits on it.
                *state = other;
                None
            }
            SlotState::Taken => None,
        }
    }

    /// Whether the worker finished (or failed): claiming won't wait or
    /// compose.
    pub fn is_done(&self) -> bool {
        matches!(*self.lock(), SlotState::Done(_))
    }

    /// Run `work` as the worker, unless the slot was claimed first.
    /// The slot is always finished, a panic included.
    pub fn run(&self, work: impl FnOnce() -> ComposedDoc) {
        {
            let mut state = self.lock();
            if !matches!(*state, SlotState::Queued) {
                return;
            }
            *state = SlotState::Running;
        }
        let doc = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).ok();
        let mut state = self.lock();
        if matches!(*state, SlotState::Abandoned) {
            *state = SlotState::Taken;
            drop(state);
            // Freed here, on the worker, never handed to the UI thread.
            drop(doc);
            return;
        }
        *state = SlotState::Done(doc);
        drop(state);
        self.0.done.notify_all();
    }

    /// Its request was replaced: a worker not started yet never runs, a
    /// running one frees its result itself, a finished result is
    /// returned for freeing off the UI thread.
    pub fn abandon(&self) -> Option<ComposedDoc> {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, SlotState::Taken) {
            SlotState::Running => {
                *state = SlotState::Abandoned;
                None
            }
            SlotState::Done(doc) => doc,
            SlotState::Queued | SlotState::Abandoned | SlotState::Taken => None,
        }
    }

    /// The UI thread needs the result now: a worker not started yet is
    /// cancelled (`None`: compose it here), a running one is waited for
    /// (no second compose of the same document), a finished one taken.
    pub fn claim(&self) -> Option<ComposedDoc> {
        let mut state = self.lock();
        loop {
            match std::mem::replace(&mut *state, SlotState::Taken) {
                SlotState::Running => {
                    *state = SlotState::Running;
                    state = self
                        .0
                        .done
                        .wait(state)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                }
                SlotState::Done(doc) => return doc,
                other @ SlotState::Abandoned => {
                    *state = other;
                    return None;
                }
                SlotState::Queued | SlotState::Taken => return None,
            }
        }
    }
}

impl std::fmt::Debug for ComposedOutput {
    // Never the pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ComposedOutput")
    }
}

/// What [`EditorSession::image_handle`] shows: `annotations` rendered
/// over `base`, cut to its frame (`offset`, `size` in document px).
#[derive(Clone, Debug)]
pub struct ShownImage {
    pub base: std::sync::Arc<image::RgbaImage>,
    /// `None`: not a whole render (a crop drag's sub-image).
    pub annotations: Option<Vec<Annotation>>,
    pub offset: (f32, f32),
    pub size: (u32, u32),
    /// Image px per document px: 1 for a render, less for the scaled-down
    /// placeholder a large editor opens with (`placeholder_display`).
    pub scale: f32,
}

impl ShownImage {
    pub fn of(base: std::sync::Arc<image::RgbaImage>, annotations: Vec<Annotation>) -> Self {
        let (offset, size) = frame_of(base.dimensions(), &annotations);
        Self {
            base,
            annotations: Some(annotations),
            offset,
            size,
            scale: 1.0,
        }
    }

    /// Whether this is `annotations` rendered over `base` (a placeholder
    /// never is).
    pub fn shows(
        &self,
        base: &std::sync::Arc<image::RgbaImage>,
        annotations: &[Annotation],
    ) -> bool {
        self.scale == 1.0
            && std::sync::Arc::ptr_eq(&self.base, base)
            && self.annotations.as_deref() == Some(annotations)
    }
}

/// Longest side of the placeholder a large editor opens with.
pub const PLACEHOLDER_MAX_SIDE: u32 = 1600;

/// What a large editor shows until its first render lands: the base
/// scaled down (nearest pixel, so a few ms whatever the size) with no
/// annotations; the canvas draws them over it
/// ([`EditorSession::pending_overlay`]). Never an output: outputs and
/// History use the model.
fn placeholder_display(
    base: &std::sync::Arc<image::RgbaImage>,
) -> (iced::widget::image::Handle, ShownImage) {
    let (w, h) = base.dimensions();
    let s = (PLACEHOLDER_MAX_SIDE as f32 / w.max(h).max(1) as f32).min(1.0);
    let pw = ((w as f32 * s).round() as u32).clamp(1, w.max(1));
    let ph = ((h as f32 * s).round() as u32).clamp(1, h.max(1));
    let small = image::RgbaImage::from_fn(pw, ph, |x, y| {
        let sx = (u64::from(x) * u64::from(w) / u64::from(pw)) as u32;
        let sy = (u64::from(y) * u64::from(h) / u64::from(ph)) as u32;
        *base.get_pixel(sx.min(w.saturating_sub(1)), sy.min(h.saturating_sub(1)))
    });
    let handle = iced::widget::image::Handle::from_rgba(pw, ph, small.into_raw());
    let shown = ShownImage {
        base: std::sync::Arc::clone(base),
        annotations: Some(Vec::new()),
        offset: (0.0, 0.0),
        size: (w, h),
        scale: pw as f32 / w.max(1) as f32,
    };
    (handle, shown)
}

/// The display an editor opens with: rendered here when small; on a
/// large document a placeholder, the render following off the UI thread
/// (the display starts dirty).
fn initial_display(model: &mut Model) -> (iced::widget::image::Handle, ShownImage, bool) {
    let (w, h) = model.geometry().size();
    if u64::from(w) * u64::from(h) >= ASYNC_COMPOSE_MIN_PIXELS {
        let (handle, shown) = placeholder_display(&model.shared_base());
        return (handle, shown, true);
    }
    let handle = build_handle(model);
    let shown = ShownImage::of(model.shared_base(), model.annotations().to_vec());
    (handle, shown, false)
}

/// A display kept for a snapshot preview's baseline, restored by a
/// cancel without a render.
#[derive(Clone, Debug)]
pub struct KeptDisplay {
    pub shown: ShownImage,
    pub handle: iced::widget::image::Handle,
}

/// The frame `annotations` give a document of `base` size: the offset and
/// size of the last crop (clamped to the base, rounded like the
/// renderer), else the whole document.
pub fn frame_of(base: (u32, u32), annotations: &[Annotation]) -> ((f32, f32), (u32, u32)) {
    let (bw, bh) = base;
    let crop = annotations.iter().rev().find_map(|a| match a {
        Annotation::Crop { rect } => Some(*rect),
        _ => None,
    });
    let Some(r) = crop else {
        return ((0.0, 0.0), base);
    };
    let bw_f = bw as f32;
    let bh_f = bh as f32;
    let x = r.x.max(0.0).min(bw_f);
    let y = r.y.max(0.0).min(bh_f);
    let w = (r.width + r.x - x).min(bw_f - x).max(1.0);
    let h = (r.height + r.y - y).min(bh_f - y).max(1.0);
    // Round like the renderer does: a crop remapped by a resize is
    // fractional, and truncating would read 1 px small.
    ((x, y), (w.round() as u32, h.round() as u32))
}

/// What History's image for this capture is known to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryFrame {
    /// `None` = the original capture; `Some(id)` = that document frame.
    Known(Option<String>),
    /// A write failed: the next sync rewrites the record in full.
    Unknown,
}

/// How a status was set: `Auto` is classified from its wording;
/// `Info` is an instruction; `Warning` a refusal or partial failure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusTone {
    #[default]
    Auto,
    Info,
    Warning,
}

/// Whether the 1 Hz status tick has work: a status that can still
/// auto-dismiss. Errors, warnings, an armed Discard and a live key
/// guide stay, so they need no tick.
pub fn status_tick_needed(ed: &EditorSession) -> bool {
    ed.status.is_some() && !ed.status_is_in_progress() && !ed.status_is_sticky()
}

/// What a keyboard instruction in the status line explains. It stays on
/// screen while that is going on (WCAG 2.2.1), and goes when it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyGuide {
    /// A keyboard crop draft is open.
    CropDraft,
    /// The keyboard is in a focus zone.
    Zone,
    /// Tab selected an annotation, which is still selected.
    Selection,
}

/// Button in the status toast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorToastAction {
    /// "Redacted N items" → Undo ⌘Z. `generation` is the model's
    /// history generation right after the redaction: the button shows
    /// only while it still matches, so it can never undo a later edit.
    /// (Not the undo depth: at the history cap a push drops the oldest
    /// step and the depth stays the same.)
    UndoRedaction { generation: u64 },
    /// Recognition failed → Retry.
    RetryRecognition,
    /// "Removed Preset N." → Undo puts the preset back.
    RestorePreset,
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
    /// an editor with unsaved work. The next Discard commits. It stays
    /// armed until answered (Discard, Keep Editing) or the next edit or
    /// status, with no time limit (WCAG 2.2.1). Prevents accidental data
    /// loss without forcing a modal dialog.
    pub discard_pending_at: Option<std::time::Instant>,
    /// Wall-clock instant the current `status` toast was set. The
    /// runtime uses this to auto-dismiss successful status messages
    /// (e.g. "Saved to …") after a few seconds so the chrome doesn't
    /// stay loud forever.
    pub status_set_at: Option<std::time::Instant>,
    /// The status text and the tone it was set with ([`Self::set_warning`],
    /// [`Self::set_info`]). Compared with `status`, so any other write of
    /// `status` drops the tone with it.
    pub toned_status: Option<(String, StatusTone)>,
    /// The status text set as a keyboard guide ([`Self::set_guide`]), and
    /// what it explains.
    pub key_guide: Option<(String, KeyGuide)>,
    /// A saved Beautify preset just removed, with its list index, while
    /// the toast offers Undo for it.
    pub removed_beautify_preset: Option<(usize, readshot_core::beautify::SavedBeautifyPreset)>,
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
    /// One Escape just ended a live canvas drag (a shape, redaction or
    /// crop draw, a Select move or resize, a magnifier handle), seen by
    /// one of its two messages (the canvas's `DragEscaped`, the key
    /// subscription's `EditorKeyPressed`); the other, in whichever order
    /// it comes, must not also clear the selection, text draft or crop
    /// draft: one Esc, one step.
    /// Cleared by its counterpart, and by any other key, press or drag
    /// start, so it can't swallow a later Escape.
    pub escape_ended_drag: Option<EscapeSeenBy>,
    /// The live canvas drag's frame ([`CanvasDragFrame`]), so a commit of
    /// it is refused when its points are in another frame than the
    /// present one. Cleared by any commit, `Cancelled`, `DragEscaped`,
    /// and when the press's window loses focus or closes (its release may
    /// never come). `None` while no drag is known.
    pub drag_started_in: Option<CanvasDragFrame>,
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
    /// [`LIVE_REDACTION_MAX_PIXELS`] (a field so tests can lower it).
    pub live_redaction_max_pixels: u64,
    /// A part was added by a step that landed off the UI thread: its
    /// recognition is requested once the message is handled.
    pub recognition_owed: bool,
    /// What [`Self::image_handle`] shows.
    pub shown: ShownImage,
    /// The handle lags the present: on a large document the annotations
    /// are re-rendered off the UI thread (`compose_jobs::after_update`),
    /// the previous render staying on screen, positioned in the present
    /// frame by the view, until the new one lands.
    pub display_dirty: bool,
    /// A crop is dragged on a large document: the display wanted is the
    /// document rendered without its crop, which the view cuts to the
    /// live crop (no render per tick).
    pub crop_drag_uncropped: bool,
    /// A Select drag moves the annotation at this index on a large
    /// document: the display wanted is the document without it (the
    /// canvas draws the moving one), rendered once off the UI thread.
    pub display_without: Option<usize>,
    /// The display before a snapshot preview or a Select drag began, so a
    /// cancel restores it without a render.
    pub kept_display: Option<KeptDisplay>,
    /// What the user typed into the Beautify tab's background hex field
    /// and hasn't applied yet. `None` while they aren't editing it: the
    /// field then shows the current solid background
    /// ([`Self::beautify_hex_shown`]), so it follows the tab opening, a
    /// preset, undo or the eyedropper.
    pub beautify_hex_draft: Option<String>,
    /// Key messages in arrival order while a key's Beautify hex field
    /// check is out (`EditorFieldKeyResolved`); the head is the key being
    /// checked. Later keys wait behind it, so a ⌘Z never runs after a key
    /// pressed after it.
    pub key_queue: std::collections::VecDeque<crate::app::Message>,
    /// The keyboard focus outside the canvas ([`KeyFocus`]); `None` is
    /// the canvas. Drawn as a focus ring on that control.
    pub key_focus: Option<KeyFocus>,
    /// The control that opened the current popover from the keyboard:
    /// closing the popover puts the focus back there.
    pub key_focus_return: Option<KeyFocus>,
    /// Open color / line-width popover, if any. Esc and a
    /// click outside close it before any other Esc behaviour runs.
    pub popover: Option<EditorPopover>,
    /// The Resize dialog's fields while it is open.
    pub resize_draft: Option<ResizeDraft>,
    /// Draft text of the color popover's hex field.
    pub hex_draft: String,
    /// Some while the eyedropper is active.
    pub eyedropper: Option<EyedropperState>,
    /// Crop tool: the dragged rect waiting for Apply ↵.
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
    /// The stage's size (screen px) at the last layout; a zoom keeps the
    /// point at its centre fixed with it. `(0, 0)` until laid out.
    pub stage_size: std::rc::Rc<std::cell::Cell<(f32, f32)>>,
    /// Bumped by each `view()`: the canvas maps its points with the frame
    /// of the last view built, so canvas messages handled before the next
    /// one are still in that frame ([`Self::press_remap`]).
    pub canvas_view_seq: std::rc::Rc<std::cell::Cell<u64>>,
    /// The frame the last `view()` built the canvas in: every canvas
    /// position handled before the next view is in it. `None` until a
    /// view is built (the present frame is then the one shown).
    pub canvas_frame: std::rc::Rc<
        std::cell::RefCell<Option<std::sync::Arc<readshot_core::document::DocGeometry>>>,
    >,
    /// A Select press that landed a pending step and was mapped into the
    /// new frame (`compose_jobs::remap_stale_press`): drag points the
    /// canvas sent before the next view are in the old frame too and are
    /// mapped the same way, so the move is in one frame throughout.
    pub press_remap: Option<PressRemap>,
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
    /// mouse-move event. Cleared by [`Self::refresh_image`].
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
    // Typographic or straight apostrophe: the app writes ’, text from
    // elsewhere (a log, a test) may not.
    lower.contains("fail")
        || lower.contains("error")
        || lower.starts_with("couldn’t")
        || lower.starts_with("couldn't")
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
        let (image_handle, shown, display_dirty) = initial_display(&mut model);
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
            key_guide: None,
            popover: None,
            resize_draft: None,
            hex_draft: String::new(),
            eyedropper: None,
            crop_draft: None,
            status_set_at: None,
            toned_status: None,
            removed_beautify_preset: None,
            move_drag: None,
            width_drag_baseline: None,
            style_drag_baseline: None,
            style_drag_cancelled: false,
            snapshot_drag_baseline: None,
            snapshot_drag_cancelled: false,
            pointer_held: false,
            pointer_window: None,
            escape_ended_drag: None,
            drag_started_in: None,
            layout_gap_pending: None,
            layout_gap_previewed_at: None,
            compose_job: None,
            compose_seq: 0,
            gap_queued: None,
            gap_commit_on_land: false,
            history_sync_owed: false,
            async_compose_min_pixels: ASYNC_COMPOSE_MIN_PIXELS,
            live_redaction_max_pixels: LIVE_REDACTION_MAX_PIXELS,
            recognition_owed: false,
            shown,
            display_dirty,
            crop_drag_uncropped: false,
            display_without: None,
            kept_display: None,
            beautify_hex_draft: None,
            key_queue: std::collections::VecDeque::new(),
            key_focus: None,
            key_focus_return: None,
            canvas_drag_active: false,
            drag_cancel_seq: 0,
            render_scale: std::rc::Rc::new(std::cell::Cell::new(1.0)),
            stage_offset: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            stage_size: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            canvas_view_seq: std::rc::Rc::new(std::cell::Cell::new(0)),
            canvas_frame: std::rc::Rc::new(std::cell::RefCell::new(None)),
            press_remap: None,
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
        let (image_handle, shown, display_dirty) = initial_display(&mut model);
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
            key_guide: None,
            popover: None,
            resize_draft: None,
            hex_draft: String::new(),
            eyedropper: None,
            crop_draft: None,
            status_set_at: None,
            toned_status: None,
            removed_beautify_preset: None,
            move_drag: None,
            width_drag_baseline: None,
            style_drag_baseline: None,
            style_drag_cancelled: false,
            snapshot_drag_baseline: None,
            snapshot_drag_cancelled: false,
            pointer_held: false,
            pointer_window: None,
            escape_ended_drag: None,
            drag_started_in: None,
            layout_gap_pending: None,
            layout_gap_previewed_at: None,
            compose_job: None,
            compose_seq: 0,
            gap_queued: None,
            gap_commit_on_land: false,
            history_sync_owed: false,
            async_compose_min_pixels: ASYNC_COMPOSE_MIN_PIXELS,
            live_redaction_max_pixels: LIVE_REDACTION_MAX_PIXELS,
            recognition_owed: false,
            shown,
            display_dirty,
            crop_drag_uncropped: false,
            display_without: None,
            kept_display: None,
            beautify_hex_draft: None,
            key_queue: std::collections::VecDeque::new(),
            key_focus: None,
            key_focus_return: None,
            canvas_drag_active: false,
            drag_cancel_seq: 0,
            render_scale: std::rc::Rc::new(std::cell::Cell::new(1.0)),
            stage_offset: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            stage_size: std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0))),
            canvas_view_seq: std::rc::Rc::new(std::cell::Cell::new(0)),
            canvas_frame: std::rc::Rc::new(std::cell::RefCell::new(None)),
            press_remap: None,
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
        self.toned_status = None;
        self.removed_beautify_preset = None;
        self.discard_pending_at = None;
        self.status = Some(msg.into());
        self.status_set_at = Some(std::time::Instant::now());
    }

    /// [`Self::set_status`] plus a button in the toast.
    pub fn set_status_with_action(&mut self, msg: impl Into<String>, action: EditorToastAction) {
        self.set_status(msg);
        self.toast_action = Some(action);
    }

    /// [`Self::set_status`] for a refusal or a partial failure: the toast
    /// shows the warning kind (never the success check) and is not
    /// auto-dismissed, so an instruction in it stays readable.
    pub fn set_warning(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.set_status(msg.clone());
        self.toned_status = Some((msg, StatusTone::Warning));
    }

    /// [`Self::set_status`] for an instruction ("Press ↵ to crop…"): the
    /// toast shows the neutral Info kind, never the success check.
    pub fn set_info(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        self.set_status(msg.clone());
        self.toned_status = Some((msg, StatusTone::Info));
    }

    /// [`Self::set_info`] for a keyboard instruction that stays while
    /// `guide` is going on (`key_guide_live`), instead of auto-dismissing.
    pub fn set_guide(&mut self, msg: impl Into<String>, guide: KeyGuide) {
        let msg = msg.into();
        self.set_info(msg.clone());
        self.key_guide = Some((msg, guide));
    }

    /// Take the status line down (nothing to report any more). A tone,
    /// action or key guide left with the old text no longer matches any
    /// status, so it is ignored until the next one is set.
    pub fn clear_status(&mut self) {
        self.status = None;
        self.status_set_at = None;
    }

    /// Show the armed-Discard prompt: the toast that asks for a second
    /// Discard. It is written here rather than through
    /// [`Self::set_status`], which disarms Discard; the toast kind comes
    /// from `discard_pending_at`. The old toast's button (a redaction's
    /// Undo, say) must not come back under this text once the arm
    /// expires, so it is dropped.
    pub fn arm_discard_prompt(&mut self, now: std::time::Instant) {
        self.discard_pending_at = Some(now);
        self.toast_action = None;
        self.status = Some("Unsaved edits. Press Discard again to close without saving.".into());
        self.status_set_at = Some(now);
    }

    /// `Some(live)` while the status is a key guide: whether what it
    /// explains is still going on.
    pub fn key_guide_live(&self) -> Option<bool> {
        let (text, guide) = self.key_guide.as_ref()?;
        if self.status.as_deref() != Some(text.as_str()) {
            return None;
        }
        Some(match guide {
            KeyGuide::CropDraft => self.crop_draft.is_some(),
            KeyGuide::Zone => self.key_focus.is_some(),
            KeyGuide::Selection => self.model.selected_annotation().is_some(),
        })
    }

    /// The tone the current status was set with.
    pub fn status_tone(&self) -> StatusTone {
        match (&self.status, &self.toned_status) {
            (Some(s), Some((t, tone))) if s == t => *tone,
            _ => StatusTone::Auto,
        }
    }

    /// [`Self::set_warning`] plus a button in the toast.
    pub fn set_warning_with_action(&mut self, msg: impl Into<String>, action: EditorToastAction) {
        self.set_warning(msg);
        self.toast_action = Some(action);
    }

    /// Whether the current status was set as a warning.
    pub fn status_is_warning(&self) -> bool {
        self.status_tone() == StatusTone::Warning
    }

    /// Errors and warnings stay until the next status replaces them.
    pub fn status_is_sticky(&self) -> bool {
        self.status_is_error()
            || self.status_is_warning()
            || self.discard_pending_at.is_some()
            || self.key_guide_live() == Some(true)
    }

    /// The toast button to show: a redaction's Undo only while that
    /// redaction is still the newest history step.
    pub fn visible_toast_action(&self) -> Option<EditorToastAction> {
        self.toast_action.filter(|a| match a {
            EditorToastAction::UndoRedaction { generation } => {
                self.model.history_generation() == *generation
            }
            EditorToastAction::RetryRecognition => true,
            EditorToastAction::RestorePreset => self.removed_beautify_preset.is_some(),
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

    /// The frame canvas positions handled now are in: the one the last
    /// view showed, else (no view yet) the present one.
    pub fn shown_frame(&self) -> std::sync::Arc<readshot_core::document::DocGeometry> {
        self.canvas_frame
            .borrow()
            .clone()
            .unwrap_or_else(|| std::sync::Arc::clone(self.model.geometry()))
    }

    /// A view was built: the canvas maps positions in the present frame.
    pub fn note_canvas_built(&self) {
        self.canvas_view_seq
            .set(self.canvas_view_seq.get().wrapping_add(1));
        *self.canvas_frame.borrow_mut() = Some(std::sync::Arc::clone(self.model.geometry()));
    }

    /// Whether a canvas drag is live: a draw (shape, redaction, crop,
    /// freehand) or a Select move/resize (handles, a magnifier's parts).
    /// An Escape ends it and nothing else ([`Self::escape_ended_drag`]).
    pub fn canvas_drag_in_progress(&self) -> bool {
        self.canvas_drag_active || self.move_drag.is_some()
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
            self.escape_ended_drag = None;
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
            // That drag's release (and so its commit or cancel) may never
            // come: don't let its frame refuse a later commit. A dropped
            // drag stays dropped (its late commit is still old-frame) until
            // the next drag start, cancel or commit.
            if matches!(self.drag_started_in, Some(CanvasDragFrame::Live(_))) {
                self.drag_started_in = None;
            }
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
    ///
    /// Nothing is rendered when the handle already shows the present, or
    /// (a preview cancelled) the kept baseline display does. A large
    /// document ([`Self::is_large`]) in the frame shown is re-rendered
    /// off the UI thread instead ([`Self::display_dirty`]); a change of
    /// document renders here (geometry changes normally land composed).
    pub fn refresh_image(&mut self) {
        self.crop_drag_flat = None;
        self.crop_drag_uncropped = false;
        self.display_without = None;
        let base = self.model.shared_base();
        if self.shown.shows(&base, self.model.annotations()) {
            self.display_dirty = false;
            self.forget_kept_display();
            return;
        }
        if let Some(kept) = self
            .kept_display
            .take_if(|k| k.shown.shows(&base, self.model.annotations()))
        {
            self.set_display(kept.handle, kept.shown);
            return;
        }
        if self.is_large() && std::sync::Arc::ptr_eq(&self.shown.base, &base) {
            self.keep_baseline_display();
            self.forget_kept_display();
            self.display_dirty = true;
            return;
        }
        let handle = build_handle(&mut self.model);
        let shown = ShownImage::of(base, self.model.annotations().to_vec());
        self.set_display(handle, shown);
    }

    /// [`Self::refresh_image`] with the handle already built (from the
    /// model's flattened image, off the UI thread).
    pub fn install_image(&mut self, handle: iced::widget::image::Handle) {
        let shown = ShownImage::of(self.model.shared_base(), self.model.annotations().to_vec());
        self.set_display(handle, shown);
    }

    /// Show `handle` (`shown`). The display it replaces is kept when it is
    /// an open snapshot preview's baseline's.
    pub fn set_display(&mut self, handle: iced::widget::image::Handle, shown: ShownImage) {
        self.crop_drag_flat = None;
        self.keep_baseline_display();
        self.image_handle = handle;
        self.shown = shown;
        self.display_dirty = false;
        self.forget_kept_display();
    }

    /// Keep the display on screen when it shows an open snapshot preview's
    /// baseline (a cancel puts it back).
    fn keep_baseline_display(&mut self) {
        if self.kept_display.is_some() {
            return;
        }
        let Some(baseline) = self.model.preview_baseline() else {
            return;
        };
        let Some(base) = self.model.cached_base(&baseline.doc.geometry) else {
            return;
        };
        if self.shown.shows(&base, &baseline.annotations) {
            self.kept_display = Some(KeptDisplay {
                shown: self.shown.clone(),
                handle: self.image_handle.clone(),
            });
        }
    }

    /// Drop the kept display once no preview or Select drag could
    /// restore it.
    fn forget_kept_display(&mut self) {
        if !self.model.preview_is_open() && self.move_drag.is_none() {
            self.kept_display = None;
        }
    }

    /// A Select drag of the annotation at `index` began moving: show the
    /// document without it under the canvas's live preview. On a large
    /// document that render is made off the UI thread, the display before
    /// the drag kept for a cancel.
    pub fn show_without_dragged(&mut self, index: usize) {
        let base = self.model.shared_base();
        let mut without = self.model.annotations().to_vec();
        if index < without.len() {
            without.remove(index);
        }
        if self.is_large() && std::sync::Arc::ptr_eq(&self.shown.base, &base) {
            if self.kept_display.is_none() && self.shown.annotations.is_some() {
                self.kept_display = Some(KeptDisplay {
                    shown: self.shown.clone(),
                    handle: self.image_handle.clone(),
                });
            }
            self.display_without = Some(index);
            if !self.shown.shows(&base, &without) {
                self.display_dirty = true;
            }
            return;
        }
        #[cfg(test)]
        crate::test_probe::count_ui_render();
        let flat = readshot_core::render(&base, &self.model.geometry().cover_redactions(&without));
        let handle =
            iced::widget::image::Handle::from_rgba(flat.width(), flat.height(), flat.into_raw());
        self.image_handle = handle;
        self.shown = ShownImage::of(base, without);
    }

    /// Whether the document is large enough that its composes and
    /// renders run off the UI thread ([`Self::async_compose_min_pixels`]).
    /// Whether the image on screen is exactly the present's render (every
    /// annotation drawn, redactions with their real effect, where they
    /// are now).
    pub fn shows_present_render(&self) -> bool {
        self.display_without.is_none()
            && self.crop_drag_flat.is_none()
            && self
                .shown
                .shows(&self.model.shared_base(), self.model.annotations())
    }

    /// Whether a dragged redaction is rendered live on every tick
    /// ([`LIVE_REDACTION_MAX_PIXELS`]).
    pub fn renders_redaction_drags_live(&self) -> bool {
        let (w, h) = self.model.geometry().size();
        !self.is_large() && u64::from(w) * u64::from(h) < self.live_redaction_max_pixels
    }

    pub fn is_large(&self) -> bool {
        let (w, h) = self.model.geometry().size();
        u64::from(w) * u64::from(h) >= self.async_compose_min_pixels
    }

    /// The annotations the display wants but the image on screen doesn't
    /// have yet (its render is in flight), for the canvas to draw over it.
    /// Crops are left out: the view cuts the image to the frame.
    pub fn pending_overlay(&self) -> Vec<Annotation> {
        let base = self.model.shared_base();
        let Some(shown) = self.shown.annotations.as_ref() else {
            return Vec::new();
        };
        if !std::sync::Arc::ptr_eq(&self.shown.base, &base) {
            return Vec::new();
        }
        let wanted = self.wanted_display();
        if wanted == *shown {
            return Vec::new();
        }
        let mut unmatched: Vec<&Annotation> = shown.iter().collect();
        wanted
            .into_iter()
            .filter(|a| !matches!(a, Annotation::Crop { .. }))
            .filter(|a| match unmatched.iter().position(|s| *s == a) {
                Some(i) => {
                    unmatched.swap_remove(i);
                    false
                }
                None => true,
            })
            .collect()
    }

    /// The magnifier bubbles (document px) the canvas must cover
    /// opaquely: a bubble's pixels come from the image on screen, so when
    /// that image lacks a redaction the present has (a Select drag's
    /// render without it, a render still in flight, a lagging flatten),
    /// a bubble whose source it hides magnifies what it hides. Every
    /// magnifier of the image on screen whose sampled source meets such a
    /// redaction (as drawn, [`DocGeometry::redaction_cover`]) — or meets
    /// a bubble already covered (bubbles are composed in order, a later
    /// one can magnify an earlier) — is covered, until a render with the
    /// redaction is shown.
    ///
    /// [`DocGeometry::redaction_cover`]: readshot_core::document::DocGeometry::redaction_cover
    pub fn magnifier_bubble_covers(&self) -> Vec<readshot_core::RectLike> {
        let base = self.model.shared_base();
        let Some(shown) = self.shown.annotations.as_ref() else {
            return Vec::new();
        };
        if !std::sync::Arc::ptr_eq(&self.shown.base, &base) {
            return Vec::new();
        }
        // Redactions the present has (and a Select drag's baseline: where
        // the dragged one hid content before it moved) that the image on
        // screen doesn't have rendered.
        let mut unmatched: Vec<&Annotation> = shown.iter().collect();
        let dragged = self
            .move_drag
            .as_ref()
            .and_then(|drag| drag.baseline.get(drag.selected_index));
        let geometry = self.model.geometry();
        let missing: Vec<readshot_core::RectLike> = self
            .model
            .annotations()
            .iter()
            .chain(dragged)
            .filter(|a| a.is_redaction())
            .filter(|a| match unmatched.iter().position(|s| s == a) {
                Some(i) => {
                    unmatched.swap_remove(i);
                    false
                }
                None => true,
            })
            .filter_map(|a| Some(geometry.redaction_cover(a.redaction_rect()?)))
            .collect();
        if missing.is_empty() {
            return Vec::new();
        }
        // The magnifiers composed into the image on screen: what each
        // samples and the bubble it fills, both widened a pixel.
        let magnifiers: Vec<(readshot_core::RectLike, readshot_core::RectLike)> = shown
            .iter()
            .filter_map(|a| match a {
                Annotation::Magnifier {
                    source,
                    radius,
                    bubble,
                    zoom,
                    ..
                } => {
                    let br = readshot_core::magnifier::bubble_radius(*radius, *zoom);
                    let sampled = br / readshot_core::magnifier::effective_zoom(*zoom) + 1.0;
                    Some((
                        square_around(*source, sampled),
                        square_around(*bubble, br + 1.0),
                    ))
                }
                _ => None,
            })
            .collect();
        let mut covered = vec![false; magnifiers.len()];
        let mut hides = missing;
        loop {
            let mut changed = false;
            for (i, (source, bubble)) in magnifiers.iter().enumerate() {
                if !covered[i] && hides.iter().any(|r| rects_meet(*r, *source)) {
                    covered[i] = true;
                    hides.push(*bubble);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        magnifiers
            .iter()
            .zip(covered)
            .filter(|(_, c)| *c)
            .map(|((_, bubble), _)| *bubble)
            .collect()
    }

    /// The annotations the display should show rendered: the present's,
    /// without its crops while a crop is dragged on a large document.
    pub fn wanted_display(&self) -> Vec<Annotation> {
        let annotations = self.model.annotations();
        if let Some(index) = self.display_without {
            let mut without = annotations.to_vec();
            if index < without.len() {
                without.remove(index);
            }
            return without;
        }
        if self.crop_drag_uncropped {
            annotations
                .iter()
                .filter(|a| !matches!(a, Annotation::Crop { .. }))
                .cloned()
                .collect()
        } else {
            annotations.to_vec()
        }
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
        if self.is_large() && std::sync::Arc::ptr_eq(&self.shown.base, &self.model.shared_base()) {
            // The view cuts whatever render is shown to the live crop; the
            // uncropped render is made once, off the UI thread.
            self.crop_drag_flat = None;
            self.crop_drag_uncropped = true;
            let base = self.model.shared_base();
            if !self.shown.shows(&base, &self.wanted_display()) {
                self.display_dirty = true;
            }
            return;
        }
        if self.crop_drag_flat.is_none() {
            let uncropped: Vec<_> = self
                .model
                .annotations()
                .iter()
                .filter(|a| !matches!(a, readshot_core::Annotation::Crop { .. }))
                .cloned()
                .collect();
            let drawn = self.model.geometry().cover_redactions(&uncropped);
            self.crop_drag_flat = Some(readshot_core::render(self.model.base(), &drawn));
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
        self.shown = ShownImage {
            base: self.model.shared_base(),
            annotations: None,
            offset: ((x0 as u32) as f32, (y0 as u32) as f32),
            size: (w, h),
            scale: 1.0,
        };
        self.display_dirty = false;
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
        frame_of(self.image_size(), self.model.annotations()).1
    }

    /// Offset of the effective displayed image inside the base
    /// image's coordinate frame — `(0, 0)` if no Crop, else the
    /// most recent Crop rect's top-left clamped to the base, at the
    /// whole pixel the displayed (and rendered) image starts at.
    /// Cursor positions get this added to them before they're
    /// stored on annotations so the renderer's "translate by crop
    /// offset" pass produces pixels under the cursor.
    pub fn crop_offset(&self) -> (f32, f32) {
        let (bw, bh) = self.image_size();
        if let Some(r) = self.last_crop() {
            (
                r.x.max(0.0).min(bw as f32).floor(),
                r.y.max(0.0).min(bh as f32).floor(),
            )
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

/// The square of half-side `half` centred on `c`.
fn square_around(c: PointLike, half: f32) -> readshot_core::RectLike {
    readshot_core::RectLike::new(c.x - half, c.y - half, half * 2.0, half * 2.0)
}

/// Whether `a` and `b` share any area (either may have a negative size;
/// a NaN never meets).
fn rects_meet(a: readshot_core::RectLike, b: readshot_core::RectLike) -> bool {
    let span = |p: f32, len: f32| (p.min(p + len), p.max(p + len));
    let (ax0, ax1) = span(a.x, a.width);
    let (ay0, ay1) = span(a.y, a.height);
    let (bx0, bx1) = span(b.x, b.width);
    let (by0, by1) = span(b.y, b.height);
    ax0 < bx1 && bx0 < ax1 && ay0 < by1 && by0 < ay1
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
    if !model.has_flattened() {
        #[cfg(test)]
        crate::test_probe::count_ui_render();
    }
    // Shares the model's cached pixels: one copy for display and output.
    model.flattened_handle()
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
            "Couldn’t save the image — the disk is full. Pick a different folder in Settings and try again.",
            "Couldn’t copy the text — permission denied. Try again.",
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
        s.set_status("Couldn’t save the image — the disk is full. Pick a different folder in Settings and try again.");
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
        let undo = EditorToastAction::UndoRedaction { generation: 0 };
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
        let generation = ed.model.history_generation();
        ed.toast_action = Some(EditorToastAction::UndoRedaction { generation });
        assert!(ed.visible_toast_action().is_some());
        ed.model.commit_annotation(blur.clone());
        assert!(
            ed.visible_toast_action().is_none(),
            "Undo would undo the newer edit"
        );
        // Gone for good: an undo + a different edit could restore the
        // depth, and Undo would then remove that edit.
        ed.model.undo();
        assert!(ed.visible_toast_action().is_none());
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

    fn composed(w: u32) -> ComposedDoc {
        let base = std::sync::Arc::new(image::RgbaImage::new(w, 1));
        let flattened = image::RgbaImage::new(w, 1);
        ComposedDoc {
            next: readshot_ui::editor::Snapshot::default(),
            base,
            flattened,
        }
    }

    #[test]
    fn a_claimed_slot_cancels_a_worker_that_has_not_started() {
        let slot = ComposedOutput::queued();
        assert!(slot.claim().is_none(), "the UI thread composes it itself");
        let ran = std::cell::Cell::new(false);
        slot.run(|| {
            ran.set(true);
            composed(1)
        });
        assert!(!ran.get(), "the worker skips it");
        assert!(slot.take().is_none());
    }

    #[test]
    fn a_claim_waits_for_a_running_worker_instead_of_composing_again() {
        let slot = ComposedOutput::queued();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let worker = slot.clone();
        let thread = std::thread::spawn(move || {
            worker.run(|| {
                started_tx.send(()).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(50));
                composed(3)
            });
        });
        started_rx.recv().unwrap();
        let doc = slot.claim().expect("the running worker's result");
        assert_eq!(doc.base.width(), 3);
        thread.join().unwrap();
        assert!(slot.take().is_none(), "taken once");
    }

    #[test]
    fn a_panicking_worker_still_finishes_its_slot() {
        let slot = ComposedOutput::queued();
        let worker = slot.clone();
        std::thread::spawn(move || worker.run(|| panic!("compose failed")))
            .join()
            .unwrap();
        // Finished with no result: a claim never waits forever.
        assert!(slot.claim().is_none());
    }

    #[test]
    fn a_finished_slot_is_taken_once_by_whichever_asks_first() {
        let slot = ComposedOutput::queued();
        slot.run(|| composed(2));
        assert!(slot.take().is_some());
        assert!(slot.claim().is_none());
        let slot = ComposedOutput::new(Some(composed(2)));
        assert!(slot.claim().is_some());
        assert!(slot.take().is_none());
    }
}
