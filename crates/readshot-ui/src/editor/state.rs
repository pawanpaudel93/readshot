//! Editor model — the pure-logic state the iced view binds against.
//!
//! [`EditorState`] owns the captured base image, the annotation
//! [`History`] stack, the active [`ToolState`], the current colour,
//! and the line-width slider. It exposes a small surface
//! (`commit_annotation`, `set_tool`, `set_color`, etc.) plus
//! `undo`/`redo` and `flatten` (re-renders via [`readshot_core::render()`]).
//!
//! Splitting the model from the iced view (the Canvas widget in
//! `canvas.rs`) means every state transition is unit-testable without
//! a windowing system. Task 16's composition root holds an
//! `EditorState` and feeds it iced messages.

use std::sync::Arc;

use image::RgbaImage;
use readshot_core::line_geom::{bend_after_drag, curve_apex, path_points};
use readshot_core::{render, Annotation, LinePath, PointLike, RectLike, Rgba};

use super::style::{apply_style_to_annotation, StyleMessage, TextSize, ToolStyle};
use super::tool_state::ToolState;
use super::undo::{DocState, History, Snapshot};
use readshot_core::beautify::BeautifySettings;
use readshot_core::document::{
    remap_annotations, Align, DocGeometry, FlipAxis, GeometryError, Join, Part, PartId,
    ORIGINAL_PART,
};

const DEFAULT_LINE_WIDTH: f32 = 3.0;

/// Resize-handle grab radius in *screen* pixels. Hit-testing scales this
/// by the canvas→base display scale so the grab target stays a constant
/// size on screen at any zoom (mirrors the overlay's `HANDLE_HIT = 14.0`).
/// The old fixed base-pixel radius grabbed only ~2 screen px when a 4K
/// capture was fitted (scale ~0.25) and a huge area at 400% zoom.
pub(crate) const HANDLE_HIT_RADIUS_SCREEN: f32 = 14.0;

/// Annotation body / stroke hit tolerance in *screen* pixels, scaled the
/// same way as the handle radius so clicking near an edge or thin stroke
/// stays equally forgiving regardless of zoom.
pub(crate) const HIT_TOLERANCE_SCREEN: f32 = 6.0;

/// Convert a screen-pixel tolerance into base-image pixels for hit-testing
/// against annotation geometry (which is stored in base coords). `scale`
/// is displayed-pixels per base-pixel; a smaller scale (fitted-down large
/// capture) yields a larger base-pixel tolerance so the on-screen target
/// is constant.
pub(crate) fn hit_tolerance_base(screen: f32, scale: f32) -> f32 {
    (screen / scale.max(f32::EPSILON)).max(0.5)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeHandle {
    NorthWest,
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    Start,
    End,
    /// Apex of a curved line or arrow; dragging it changes the bend.
    Bend,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub index: usize,
    pub origin: PointLike,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct EditorState {
    /// Shared so background work (OCR) can take the image without a
    /// deep copy on the UI thread.
    base: Arc<RgbaImage>,
    /// The geometry `base` was composed from.
    base_geometry: Arc<DocGeometry>,
    next_part_id: PartId,
    /// Present snapshot's baseline while a snapshot preview is open.
    preview_baseline: Option<(Snapshot, u64)>,
    /// The baseline of the open annotation preview (move, resize, width,
    /// style) and the history generation it opened at: a cancel or commit
    /// after the history moved (a text commit, say) is stale and would
    /// clobber the newer edit.
    annotation_preview: Option<(Vec<Annotation>, u64)>,
    history: History,
    active_tool: ToolState,
    current_color: Rgba,
    current_line_width: f32,
    tool_style: ToolStyle,
    selected_annotation: Option<usize>,
    /// Cached flattened image. Invalidated (reset to `None`) whenever the
    /// annotation `history` changes — commit, undo/redo, delete, discard,
    /// nudge, drag previews, and the selected-annotation colour/width/text
    /// edits (which all push a new history entry). Changing the *current*
    /// tool colour (`set_color`) or line width (`set_line_width`) does not
    /// touch it: those only style the next annotation, not the rendered
    /// stack, so the flattened image is unchanged.
    flattened_cache: Option<Flattened>,
    /// Composed bases other than the present's, kept so undo, redo and a
    /// preview cancel swap an image in instead of recomposing. Only bases
    /// that were composed anyway are kept (never composed ahead), and only
    /// for an open preview's baseline and the states one undo or one redo
    /// away ([`Self::trim_bases`]); the rest are dropped.
    bases: Vec<(Arc<DocGeometry>, Arc<RgbaImage>)>,
    /// How many bases were composed on this thread (`sync_base`).
    base_composes: u64,
    /// The flattened image of an open snapshot preview's baseline, kept
    /// when the preview opened so a cancel restores it without a render.
    preview_baseline_flat: Option<Flattened>,
}

/// A flattened image, held once for both the display and an output: its
/// pixels live in an iced image handle (shared, reference counted), so
/// showing it copies nothing and it is read back without a render.
#[derive(Clone, Debug)]
pub struct Flattened {
    handle: iced::widget::image::Handle,
    width: u32,
    height: u32,
}

impl Flattened {
    /// Take `image`'s pixels (no copy).
    pub fn new(image: RgbaImage) -> Self {
        let (width, height) = image.dimensions();
        Self {
            handle: iced::widget::image::Handle::from_rgba(width, height, image.into_raw()),
            width,
            height,
        }
    }

    /// The handle to show it with (shares its pixels).
    pub fn handle(&self) -> iced::widget::image::Handle {
        self.handle.clone()
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Its RGBA pixels, row by row.
    pub fn pixels(&self) -> &[u8] {
        match &self.handle {
            iced::widget::image::Handle::Rgba { pixels, .. } => pixels,
            _ => &[],
        }
    }

    /// An owned copy as an image. Its pixels always match its size (it is
    /// only built by [`Self::new`]); were they ever not to, this is `None`
    /// so the caller renders again, never a blank image passed off as the
    /// document.
    pub fn to_image(&self) -> Option<RgbaImage> {
        let img = self.try_image();
        debug_assert!(img.is_some(), "flattened pixels do not match its size");
        img
    }

    fn try_image(&self) -> Option<RgbaImage> {
        RgbaImage::from_raw(self.width, self.height, self.pixels().to_vec())
    }
}

/// One history move of a queued document step, applied with its composed
/// result by [`EditorState::apply_ops_precomposed`].
#[derive(Clone, Debug, PartialEq)]
pub enum DocOp {
    /// Push this state as one undo step (a geometry change).
    Push(Snapshot),
    Undo,
    Redo,
}

fn apply_op(history: &mut History, op: &DocOp) -> bool {
    match op {
        DocOp::Push(next) => {
            history.push_snapshot(next.clone());
            true
        }
        DocOp::Undo => history.undo().is_some(),
        DocOp::Redo => history.redo().is_some(),
    }
}

fn same_geometry(a: &Arc<DocGeometry>, b: &Arc<DocGeometry>) -> bool {
    // `Part::eq` compares image pointers, so this is cheap.
    Arc::ptr_eq(a, b) || **a == **b
}

impl EditorState {
    pub fn new(base: RgbaImage) -> Self {
        Self::with_annotations(base, Vec::new())
    }

    pub fn with_annotations(base: RgbaImage, annotations: Vec<Annotation>) -> Self {
        let base = Arc::new(base);
        let geometry = Arc::new(DocGeometry::original(Arc::clone(&base)));
        Self {
            base,
            base_geometry: Arc::clone(&geometry),
            history: History::from_snapshot(Snapshot {
                annotations,
                doc: DocState {
                    geometry,
                    beautify: BeautifySettings::default(),
                },
            }),
            active_tool: ToolState::default(),
            current_color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            current_line_width: DEFAULT_LINE_WIDTH,
            tool_style: ToolStyle::default(),
            selected_annotation: None,
            flattened_cache: None,
            next_part_id: ORIGINAL_PART + 1,
            preview_baseline: None,
            annotation_preview: None,
            bases: Vec::new(),
            base_composes: 0,
            preview_baseline_flat: None,
        }
    }

    /// The id of every part any held document state uses: the undo and
    /// redo stacks, the present and an open preview's baseline. A part
    /// outside this set can never be shown again (ids are never reused),
    /// so data kept per part elsewhere can be released.
    pub fn referenced_part_ids(&self) -> std::collections::BTreeSet<PartId> {
        self.history
            .states()
            .chain(self.preview_baseline.as_ref().map(|(b, _)| b))
            .flat_map(|s| s.doc.geometry.parts.iter().map(|p| p.id))
            .collect()
    }

    /// The composed document image for the present geometry.
    pub fn base(&self) -> &RgbaImage {
        &self.base
    }

    /// The base image as a cheap shared handle (no pixel copy).
    pub fn shared_base(&self) -> Arc<RgbaImage> {
        Arc::clone(&self.base)
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

    /// The present annotations as drawn over the composed document:
    /// redactions widened to the whole pixels they must hide
    /// ([`DocGeometry::cover_redactions`]). What renders, History bakes
    /// and text masking use; [`Self::annotations`] keeps the exact rects.
    pub fn drawn_annotations(&self) -> Vec<Annotation> {
        self.geometry().cover_redactions(self.history.current())
    }

    fn render_present(&self) -> RgbaImage {
        render(&self.base, &self.drawn_annotations())
    }

    pub fn render_snapshot(&self) -> (RgbaImage, Vec<Annotation>) {
        (
            RgbaImage::clone(&self.base),
            self.history.current().to_vec(),
        )
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

    pub fn tool_style(&self) -> ToolStyle {
        self.tool_style
    }

    /// Change the style the next annotation gets. Does not touch the
    /// selection — see [`Self::apply_style_to_selected`].
    pub fn set_style(&mut self, msg: StyleMessage) {
        self.tool_style.apply(msg);
    }

    /// Restore a saved tool style (a cancelled options-bar slider).
    pub fn set_tool_style(&mut self, style: ToolStyle) {
        self.tool_style = style;
    }

    /// The tool style with the selected annotation's settings layered on
    /// top — what the options bar displays.
    pub fn effective_style(&self) -> ToolStyle {
        match self.selected_annotation() {
            Some(idx) => self
                .tool_style
                .with_annotation(&self.history.current()[idx]),
            None => self.tool_style,
        }
    }

    /// The selected annotation's width (stroke width, blur radius or
    /// pixel block size) when it has one, else the tool width — what the
    /// options bar's Width control shows and steps from.
    pub fn effective_line_width(&self) -> f32 {
        self.selected_annotation()
            .and_then(|idx| annotation_width(&self.history.current()[idx]))
            .unwrap_or(self.current_line_width)
    }

    /// The selected annotation's color when it has one, else the tool color.
    pub fn effective_color(&self) -> Rgba {
        self.selected_annotation()
            .and_then(|idx| annotation_color(&self.history.current()[idx]))
            .unwrap_or(self.current_color)
    }

    pub fn apply_style_to_selected(&mut self, msg: StyleMessage) -> bool {
        let Some(idx) = self.selected_annotation() else {
            return false;
        };
        let mut next = self.history.current().to_vec();
        if !apply_style_to_annotation(&mut next[idx], msg) {
            return false;
        }
        self.push_annotations(next);
        self.selected_annotation = Some(idx);
        self.flattened_cache = None;
        true
    }

    /// Slider preview: restyle the selection from `baseline` without
    /// pushing history; `commit_preview_from_baseline` makes it one step.
    pub fn preview_style_selected_from(
        &mut self,
        baseline: &[Annotation],
        msg: StyleMessage,
    ) -> bool {
        let Some(idx) = self.selected_annotation else {
            return false;
        };
        if idx >= baseline.len() {
            self.selected_annotation = None;
            return false;
        }
        if !self.annotation_preview_may_go_on(baseline) {
            return false;
        }
        let mut next = baseline.to_vec();
        let changed = apply_style_to_annotation(&mut next[idx], msg);
        self.open_annotation_preview(baseline);
        self.history.replace_present(next);
        self.flattened_cache = None;
        changed
    }

    /// Append a new annotation, snapshotting the previous state to
    /// the undo stack.
    pub fn commit_annotation(&mut self, annotation: Annotation) {
        let mut annotation = annotation;
        self.clamp_crop_annotation_to_base(&mut annotation, CropClampMode::Resize);
        let mut next = self.history.current().to_vec();
        next.push(annotation);
        self.selected_annotation = next.len().checked_sub(1);
        self.push_annotations(next);
        self.flattened_cache = None;
    }

    /// Append several annotations as **one** undo step (smart redact's
    /// Redact All). Clears the selection. Returns how many were added.
    pub fn commit_annotations(&mut self, annotations: Vec<Annotation>) -> usize {
        if annotations.is_empty() {
            return 0;
        }
        let n = annotations.len();
        let mut next = self.history.current().to_vec();
        next.extend(annotations);
        self.selected_annotation = None;
        self.push_annotations(next);
        self.flattened_cache = None;
        n
    }

    pub fn undo(&mut self) -> bool {
        // Closing a preview moved the present, so that alone is a change.
        let closed = self.close_preview();
        let changed = self.history.undo().is_some() || closed;
        if changed {
            self.sync_base();
            self.clamp_selection();
            self.flattened_cache = None;
        }
        changed
    }

    pub fn redo(&mut self) -> bool {
        // Closing a preview moved the present, so that alone is a change.
        let closed = self.close_preview();
        let changed = self.history.redo().is_some() || closed;
        if changed {
            self.sync_base();
            self.clamp_selection();
            self.flattened_cache = None;
        }
        changed
    }

    /// Recompose `base` when the present snapshot's geometry differs from
    /// the one it was built from (undo/redo across a transform).
    fn sync_base(&mut self) {
        let g = Arc::clone(&self.history.doc().geometry);
        if Arc::ptr_eq(&g, &self.base_geometry) {
            self.trim_bases();
            return;
        }
        // Equal by value (cheap: `Part::eq` compares pointers) keeps the
        // composed image; only a real change swaps it, from the cache when
        // it was composed before.
        if *g != *self.base_geometry {
            let base = match self.cached_base(&g) {
                Some(base) => base,
                None => {
                    self.base_composes += 1;
                    g.compose()
                }
            };
            self.swap_base(Arc::clone(&g), base);
            self.flattened_cache = None;
        } else {
            self.base_geometry = g;
        }
        self.trim_bases();
    }

    /// Make `base` (composed from `geometry`) the present base, keeping
    /// the outgoing one in the cache ([`Self::trim_bases`] decides).
    fn swap_base(&mut self, geometry: Arc<DocGeometry>, base: Arc<RgbaImage>) {
        let old_geometry = std::mem::replace(&mut self.base_geometry, geometry);
        let old_base = std::mem::replace(&mut self.base, base);
        if !self
            .bases
            .iter()
            .any(|(g, _)| same_geometry(g, &old_geometry))
        {
            self.bases.push((old_geometry, old_base));
        }
    }

    /// Keep only the cached bases a cheap move can need: an open preview's
    /// baseline and the states one undo or one redo away. The present's
    /// own base is `self.base`.
    fn trim_bases(&mut self) {
        let keep: Vec<Arc<DocGeometry>> = self
            .preview_baseline
            .as_ref()
            .map(|(b, _)| &b.doc.geometry)
            .into_iter()
            .chain(self.history.peek_undo().map(|s| &s.doc.geometry))
            .chain(self.history.peek_redo().map(|s| &s.doc.geometry))
            .cloned()
            .collect();
        let present = Arc::clone(&self.base_geometry);
        self.bases.retain(|(g, _)| {
            !same_geometry(g, &present) && keep.iter().any(|k| same_geometry(g, k))
        });
    }

    /// The composed base of `geometry`, when it is the present's or one
    /// kept in the cache (no compose).
    pub fn cached_base(&self, geometry: &Arc<DocGeometry>) -> Option<Arc<RgbaImage>> {
        if same_geometry(geometry, &self.base_geometry) {
            return Some(Arc::clone(&self.base));
        }
        self.bases
            .iter()
            .find(|(g, _)| same_geometry(g, geometry))
            .map(|(_, b)| Arc::clone(b))
    }

    /// How many bases were composed by this model on the calling thread
    /// (a cache hit composes nothing). Off-thread composes are counted by
    /// whoever runs them.
    pub fn base_composes(&self) -> u64 {
        self.base_composes
    }

    /// An open snapshot preview's baseline.
    pub fn preview_baseline(&self) -> Option<&Snapshot> {
        self.preview_baseline.as_ref().map(|(b, _)| b)
    }

    /// The cached flattened image of the present, shared (no copy).
    pub fn flattened_shared(&self) -> Option<Flattened> {
        self.flattened_cache.clone()
    }

    /// The flattened image of the present as a display handle sharing
    /// the cached pixels, rendered here when there is none.
    pub fn flattened_handle(&mut self) -> iced::widget::image::Handle {
        if self.flattened_cache.is_none() {
            let img = self.render_present();
            self.flattened_cache = Some(Flattened::new(img));
        }
        self.flattened_cache
            .as_ref()
            .map(Flattened::handle)
            .expect("filled above")
    }

    /// Whether a snapshot preview (slider drag) is open.
    pub fn preview_is_open(&self) -> bool {
        self.preview_baseline.is_some()
    }

    /// The state [`Self::undo`] would move to (no preview open).
    pub fn peek_undo(&self) -> Option<&Snapshot> {
        self.history.peek_undo()
    }

    /// The state [`Self::redo`] would move to (no preview open).
    pub fn peek_redo(&self) -> Option<&Snapshot> {
        self.history.peek_redo()
    }

    /// The present state after `ops`, without applying them or composing
    /// anything. `None` while a snapshot preview is open, or when an op
    /// can't apply (an undo with nothing to undo).
    pub fn plan_ops(&self, ops: &[DocOp]) -> Option<Snapshot> {
        if self.preview_baseline.is_some() {
            return None;
        }
        let mut history = self.history.clone();
        ops.iter()
            .all(|op| apply_op(&mut history, op))
            .then(|| history.snapshot().clone())
    }

    /// Whether undo and redo are possible once `ops` are applied (what the
    /// toolbar shows while they are queued). `None` like [`Self::plan_ops`].
    pub fn planned_undo_redo(&self, ops: &[DocOp]) -> Option<(bool, bool)> {
        if self.preview_baseline.is_some() {
            return None;
        }
        let mut history = self.history.clone();
        ops.iter()
            .all(|op| apply_op(&mut history, op))
            .then(|| (history.can_undo(), history.can_redo()))
    }

    /// Apply `ops` (each one undo step, or one undo / redo) with the end
    /// state's document already composed (`base`) and flattened (`flat`,
    /// `render(base, annotations)`), so nothing is composed here; the
    /// states in between get their bases when they are next shown. The
    /// caller checks the history has not moved since the ops were
    /// planned. `Ok(false)` (nothing changed) while a preview is open or
    /// when an op can't apply.
    pub fn apply_ops_precomposed(
        &mut self,
        ops: &[DocOp],
        base: Arc<RgbaImage>,
        flat: RgbaImage,
    ) -> Result<bool, GeometryError> {
        if self.preview_baseline.is_some() {
            return Ok(false);
        }
        let mut history = self.history.clone();
        if !ops.iter().all(|op| apply_op(&mut history, op)) {
            return Ok(false);
        }
        history.doc().geometry.validate()?;
        self.history = history;
        let geometry = Arc::clone(&self.history.doc().geometry);
        self.install_base(geometry, base, flat);
        Ok(true)
    }

    /// Install `flat`, rendered off the UI thread as `render(base,
    /// annotations)`, as the flattened image, if the present still is
    /// `annotations` over `base`. Returns whether it was installed.
    pub fn install_flattened(
        &mut self,
        base: &Arc<RgbaImage>,
        annotations: &[Annotation],
        flat: RgbaImage,
    ) -> bool {
        if !Arc::ptr_eq(base, &self.base) || self.history.current() != annotations {
            return false;
        }
        self.flattened_cache = Some(Flattened::new(flat));
        true
    }

    /// Every image this model holds (the base, the cached bases, the
    /// flattened images), shared: a caller can keep them across a change
    /// so the big ones it releases are freed elsewhere.
    pub fn held_images(&self) -> (Vec<Arc<RgbaImage>>, Vec<Flattened>) {
        (
            std::iter::once(Arc::clone(&self.base))
                .chain(self.bases.iter().map(|(_, b)| Arc::clone(b)))
                .collect(),
            self.flattened_cache
                .iter()
                .chain(self.preview_baseline_flat.iter())
                .cloned()
                .collect(),
        )
    }

    /// Whether [`Self::flatten`] would return a cached image (no render).
    pub fn has_flattened(&self) -> bool {
        self.flattened_cache.is_some()
    }

    /// New annotations on the same document as one undo step. The state
    /// one redo away is gone, so its cached base is too.
    fn push_annotations(&mut self, next: Vec<Annotation>) {
        self.history.push(next);
        self.trim_bases();
    }

    fn alloc_part_id(&mut self) -> PartId {
        let id = self.next_part_id;
        self.next_part_id += 1;
        id
    }

    pub fn geometry(&self) -> &Arc<DocGeometry> {
        &self.history.doc().geometry
    }

    pub fn part_ids(&self) -> Vec<PartId> {
        self.geometry().parts.iter().map(|p| p.id).collect()
    }

    pub fn beautify(&self) -> BeautifySettings {
        self.history.doc().beautify
    }

    pub fn snapshot(&self) -> Snapshot {
        self.history.snapshot().clone()
    }

    /// New Beautify settings as one undo step; `false` when unchanged.
    pub fn set_beautify(&mut self, beautify: BeautifySettings) -> bool {
        if self.beautify() == beautify {
            return false;
        }
        let mut next = self.snapshot();
        next.doc.beautify = beautify;
        self.history.push_snapshot(next);
        self.trim_bases();
        true
    }

    /// Live preview (slider drags): replaces the present, no undo step.
    /// The snapshot's annotations must already be in its own geometry's
    /// frame. An invalid geometry is refused and nothing changes.
    pub fn preview_snapshot(&mut self, snapshot: Snapshot) -> Result<(), GeometryError> {
        snapshot.doc.geometry.validate()?;
        self.open_snapshot_preview();
        // Beautify is drawn around the flattened image, not into it: a
        // preview with the same annotations on the same document keeps it.
        let same_pixels = snapshot.annotations == self.history.current()
            && same_geometry(&snapshot.doc.geometry, &self.history.doc().geometry);
        self.history.replace_present_snapshot(snapshot);
        self.sync_base();
        self.clamp_selection();
        if !same_pixels {
            self.flattened_cache = None;
        }
        Ok(())
    }

    /// Open a snapshot preview over the present (if none is open), keeping
    /// the present's flattened image for a cancel.
    fn open_snapshot_preview(&mut self) {
        if self.preview_baseline.is_none() {
            self.preview_baseline = Some((self.snapshot(), self.history.generation()));
            self.preview_baseline_flat = self.flattened_cache.clone();
        }
    }

    /// The preview is closed: what it kept for a cancel goes.
    fn take_preview_baseline(&mut self) -> Option<(Snapshot, u64)> {
        self.preview_baseline_flat = None;
        self.preview_baseline.take()
    }

    /// Keep the previewed state as one undo step over `baseline`. Does
    /// nothing when no preview is open.
    pub fn commit_preview_snapshot(&mut self, baseline: Snapshot) -> bool {
        // Refuse when no preview is open or the history moved since it
        // opened: the baseline is stale and would clobber a newer edit.
        match self.take_preview_baseline() {
            Some((b, g)) if g == self.history.generation() && b == baseline => {}
            _ => return false,
        }
        let current = self.snapshot();
        if current == baseline {
            return false;
        }
        self.history.replace_present_snapshot(baseline);
        self.history.push_snapshot(current);
        self.sync_base();
        self.flattened_cache = None;
        true
    }

    /// Restore `baseline`, but only while the preview it belongs to is
    /// open and the history has not moved; a stale baseline would
    /// clobber newer edits.
    pub fn cancel_preview_snapshot(&mut self, baseline: Snapshot) {
        let flat = self.preview_baseline_flat.take();
        match self.preview_baseline.take() {
            Some((b, g)) if g == self.history.generation() && b == baseline => {}
            _ => return,
        }
        // `baseline` came from `snapshot()`, so its geometry is valid. Its
        // base is cached and its flattened image was kept: no render.
        self.history.replace_present_snapshot(baseline);
        self.sync_base();
        self.clamp_selection();
        self.flattened_cache = flat;
    }

    /// Close an open snapshot preview, restoring its baseline.
    /// `true` when a baseline was restored.
    fn close_preview(&mut self) -> bool {
        // Restore only while the history has not moved; otherwise the
        // baseline is stale and the present already holds newer edits.
        let flat = self.preview_baseline_flat.take();
        if let Some((b, g)) = self.preview_baseline.take() {
            if g == self.history.generation() {
                self.history.replace_present_snapshot(b);
                self.sync_base();
                self.flattened_cache = flat;
                return true;
            }
        }
        false
    }

    /// Changes on every push, undo and redo; never on a preview. A result
    /// prepared for one generation is stale under any other.
    pub fn history_generation(&self) -> u64 {
        self.history.generation()
    }

    /// The undo step [`Self::apply_geometry`] would push for `next`,
    /// without pushing it or composing anything: `None` when `next` is
    /// the present geometry. Land it as [`DocOp::Push`] through
    /// [`Self::apply_ops_precomposed`] to compose the document off the
    /// UI thread.
    pub fn geometry_step(&self, next: DocGeometry) -> Result<Option<Snapshot>, GeometryError> {
        Self::step_from(self.history.snapshot(), next)
    }

    /// [`Self::geometry_step`] from any state `from` (a queued step's).
    pub fn step_from(
        from: &Snapshot,
        next: DocGeometry,
    ) -> Result<Option<Snapshot>, GeometryError> {
        next.validate()?;
        if *from.doc.geometry == next {
            return Ok(None);
        }
        let annotations = remap_annotations(&from.doc.geometry, &next, &from.annotations);
        Ok(Some(Snapshot {
            annotations,
            doc: DocState {
                geometry: Arc::new(next),
                beautify: from.doc.beautify,
            },
        }))
    }

    /// The undo step [`Self::add_part`] would push from `from`, without
    /// pushing it or composing anything; allocates the new part's id
    /// (and a bake's), which are never reused even if the step isn't.
    pub fn add_part_step(
        &mut self,
        from: &Snapshot,
        image: Arc<RgbaImage>,
        join: Join,
    ) -> Result<(Snapshot, PartId), GeometryError> {
        let old = Arc::clone(&from.doc.geometry);
        old.validate()?;
        // Two steps when a bake is involved: old -> baked, then
        // baked -> baked + part. Never old -> baked + part directly.
        let (staged, annotations) = if old.transform.is_identity() {
            ((*old).clone(), from.annotations.clone())
        } else {
            let baked_id = self.alloc_part_id();
            // The bake's image is the document as composed: the cached base
            // when there is one (no compose).
            let baked = match self.cached_base(&old) {
                Some(image) => old.baked_from_image(baked_id, image)?,
                None => old.baked(baked_id)?,
            };
            // The bake keeps the composed pixels, a resize's fringe
            // around each redaction included: the redactions become what
            // they cover there (exact rects would leave the fringe out).
            let covered = old.cover_redactions(&from.annotations);
            let annotations = remap_annotations(&old, &baked, &covered);
            (baked, annotations)
        };
        let id = self.alloc_part_id();
        let next = staged.with_part(Part { id, image, join });
        next.validate()?;
        let annotations = remap_annotations(&staged, &next, &annotations);
        Ok((
            Snapshot {
                annotations,
                doc: DocState {
                    geometry: Arc::new(next),
                    beautify: from.doc.beautify,
                },
            },
            id,
        ))
    }

    /// The state [`Self::preview_geometry`] would show for `next` over
    /// `baseline`, without showing it or composing anything.
    pub fn geometry_preview_step(
        baseline: &Snapshot,
        next: DocGeometry,
    ) -> Result<Snapshot, GeometryError> {
        next.validate()?;
        let annotations = remap_annotations(&baseline.doc.geometry, &next, &baseline.annotations);
        let geometry = if *baseline.doc.geometry == next {
            Arc::clone(&baseline.doc.geometry)
        } else {
            Arc::new(next)
        };
        Ok(Snapshot {
            annotations,
            doc: DocState {
                geometry,
                beautify: baseline.doc.beautify,
            },
        })
    }

    /// The geometry [`Self::scale_by`] would move to; `None` for a
    /// factor it ignores.
    pub fn scaled_geometry(&self, sx: f32, sy: f32) -> Option<DocGeometry> {
        if !(sx.is_finite() && sy.is_finite() && sx > 0.0 && sy > 0.0) {
            return None;
        }
        let g = self.geometry();
        Some(g.with_transform(g.transform.scaled(sx, sy)))
    }

    /// [`Self::preview_snapshot`] with the document already composed
    /// and flattened (see [`Self::apply_ops_precomposed`]).
    pub fn preview_precomposed(
        &mut self,
        snapshot: Snapshot,
        base: Arc<RgbaImage>,
        flattened: RgbaImage,
    ) -> Result<(), GeometryError> {
        snapshot.doc.geometry.validate()?;
        self.open_snapshot_preview();
        let geometry = Arc::clone(&snapshot.doc.geometry);
        self.history.replace_present_snapshot(snapshot);
        self.install_base(geometry, base, flattened);
        Ok(())
    }

    fn install_base(&mut self, geometry: Arc<DocGeometry>, base: Arc<RgbaImage>, flat: RgbaImage) {
        debug_assert_eq!(base.dimensions(), geometry.size());
        if Arc::ptr_eq(&base, &self.base) {
            self.base_geometry = geometry;
        } else {
            self.swap_base(geometry, base);
        }
        self.trim_bases();
        self.clamp_selection();
        self.flattened_cache = Some(Flattened::new(flat));
    }

    /// Move to `next` as one undo step; annotations follow their parts.
    pub fn apply_geometry(&mut self, next: DocGeometry) -> Result<bool, GeometryError> {
        next.validate()?;
        if **self.geometry() == next {
            return Ok(false);
        }
        let annotations = remap_annotations(self.geometry(), &next, self.annotations());
        let beautify = self.beautify();
        self.history.push_snapshot(Snapshot {
            annotations,
            doc: DocState {
                geometry: Arc::new(next),
                beautify,
            },
        });
        self.sync_base();
        self.clamp_selection();
        self.flattened_cache = None;
        Ok(true)
    }

    /// Preview `next` relative to `baseline` (gap slider), no undo step.
    pub fn preview_geometry(
        &mut self,
        baseline: &Snapshot,
        next: DocGeometry,
    ) -> Result<(), GeometryError> {
        next.validate()?;
        let annotations = remap_annotations(&baseline.doc.geometry, &next, &baseline.annotations);
        // Reuse the baseline's Arc when nothing changed: no recompose.
        let geometry = if *baseline.doc.geometry == next {
            Arc::clone(&baseline.doc.geometry)
        } else {
            Arc::new(next)
        };
        self.preview_snapshot(Snapshot {
            annotations,
            doc: DocState {
                geometry,
                beautify: baseline.doc.beautify,
            },
        })
    }

    pub fn rotate(&mut self, clockwise: bool) -> Result<bool, GeometryError> {
        let next = self.geometry().rotated(clockwise);
        // Any transform drops the untouched-original exemption from the
        // size limits, so this can fail on a huge capture.
        self.apply_geometry(next)
    }

    pub fn flip(&mut self, axis: FlipAxis) -> Result<bool, GeometryError> {
        let next = self.geometry().flipped(axis);
        self.apply_geometry(next)
    }

    pub fn scale_by(&mut self, sx: f32, sy: f32) -> Result<bool, GeometryError> {
        match self.scaled_geometry(sx, sy) {
            Some(next) => self.apply_geometry(next),
            None => Ok(false),
        }
    }

    /// Append `image` joined `join` (decision D4: a transformed document
    /// is baked into one part first). One undo step; returns the new id.
    pub fn add_part(&mut self, image: RgbaImage, join: Join) -> Result<PartId, GeometryError> {
        let from = self.snapshot();
        let (step, id) = self.add_part_step(&from, Arc::new(image), join)?;
        self.history.push_snapshot(step);
        self.sync_base();
        self.clamp_selection();
        self.flattened_cache = None;
        Ok(id)
    }

    pub fn set_layout(&mut self, gap: u32, align: Align) -> Result<bool, GeometryError> {
        let next = self.geometry().with_layout(gap, align);
        self.apply_geometry(next)
    }

    /// Discard all annotations and reset undo/redo. Used by the
    /// "Discard" action button before the editor closes.
    pub fn discard(&mut self) {
        self.preview_baseline_flat = None;
        self.history.clear();
        self.selected_annotation = None;
        self.flattened_cache = None;
    }

    pub fn selected_annotation(&self) -> Option<usize> {
        self.selected_annotation
            .filter(|idx| *idx < self.history.current().len())
    }

    pub fn clear_selection(&mut self) {
        self.selected_annotation = None;
    }

    pub fn selected_bounds(&self) -> Option<RectLike> {
        let idx = self.selected_annotation()?;
        annotation_bounds(&self.history.current()[idx])
    }

    pub fn selected_handles(&self) -> Vec<(ResizeHandle, PointLike)> {
        let Some(idx) = self.selected_annotation() else {
            return Vec::new();
        };
        annotation_resize_handles(&self.history.current()[idx])
    }

    /// Hit-test the selected annotation's resize handles at the default
    /// 1:1 display scale. Prefer [`Self::resize_handle_at_scaled`] from the
    /// interactive canvas so the grab radius stays constant on screen.
    pub fn resize_handle_at(&self, point: PointLike) -> Option<ResizeHandle> {
        self.resize_handle_at_scaled(point, 1.0)
    }

    /// Hit-test resize handles with the current canvas→base display
    /// `scale` (displayed px per base px) so the grab radius is constant
    /// in screen pixels regardless of zoom.
    pub fn resize_handle_at_scaled(&self, point: PointLike, scale: f32) -> Option<ResizeHandle> {
        let idx = self.selected_annotation()?;
        annotation_resize_handle_at(&self.history.current()[idx], point, scale)
    }

    pub fn select_at(&mut self, point: PointLike) -> Option<usize> {
        self.select_at_scaled(point, 1.0)
    }

    /// Select the topmost annotation under `point`, using the current
    /// display `scale` to keep the hit tolerance constant on screen.
    ///
    /// "Topmost" follows paint order: render draws every magnifier
    /// bubble after all other annotations, so a press inside a bubble
    /// picks that magnifier first (newest first) and never a shape the
    /// bubble hides. Next, the selected magnifier's leader line keeps it
    /// selected (a press there moves the whole magnifier; an unselected
    /// leader stays click-through). Elsewhere (including a magnifier's
    /// source disc, which only paints a thin ring) everything is tried
    /// newest first.
    pub fn select_at_scaled(&mut self, point: PointLike, scale: f32) -> Option<usize> {
        let annotations = self.history.current();
        let in_bubble = annotations
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, a)| magnifier_bubble_grab(a, point).map(|_| idx));
        let on_selected_leader = || {
            self.selected_annotation.filter(|&idx| {
                annotations
                    .get(idx)
                    .is_some_and(|a| magnifier_leader_hit(a, point, scale))
            })
        };
        let hit = in_bubble.or_else(on_selected_leader).or_else(|| {
            annotations
                .iter()
                .enumerate()
                .rev()
                .find_map(|(idx, annotation)| {
                    annotation_hit_test(annotation, point, scale).then_some(idx)
                })
        });
        self.selected_annotation = hit;
        hit
    }

    pub fn delete_selected_annotation(&mut self) -> bool {
        let Some(idx) = self.selected_annotation() else {
            return false;
        };
        let mut next = self.history.current().to_vec();
        if idx >= next.len() {
            self.selected_annotation = None;
            return false;
        }
        next.remove(idx);
        self.selected_annotation = None;
        self.push_annotations(next);
        self.flattened_cache = None;
        true
    }

    pub fn selected_kind_label(&self) -> Option<&'static str> {
        let idx = self.selected_annotation()?;
        Some(annotation_kind_label(&self.history.current()[idx]))
    }

    pub fn selected_text_edit(&self) -> Option<TextEdit> {
        let idx = self.selected_annotation()?;
        match &self.history.current()[idx] {
            Annotation::Text {
                content, origin, ..
            } => Some(TextEdit {
                index: idx,
                origin: *origin,
                content: content.clone(),
            }),
            _ => None,
        }
    }

    pub fn replace_selected_text(&mut self, content: String) -> bool {
        let Some(idx) = self.selected_annotation() else {
            return false;
        };
        self.replace_text_at(idx, content)
    }

    pub fn replace_text_at(&mut self, idx: usize, content: String) -> bool {
        let mut next = self.history.current().to_vec();
        let Some(Annotation::Text {
            content: existing, ..
        }) = next.get_mut(idx)
        else {
            return false;
        };
        if *existing == content {
            return false;
        }
        *existing = content;
        self.push_annotations(next);
        self.selected_annotation = Some(idx);
        self.flattened_cache = None;
        true
    }

    pub fn apply_color_to_selected(&mut self, color: Rgba) -> bool {
        let Some(idx) = self.selected_annotation() else {
            return false;
        };
        let mut next = self.history.current().to_vec();
        if !set_annotation_color(&mut next[idx], color) {
            return false;
        }
        self.push_annotations(next);
        self.selected_annotation = Some(idx);
        self.flattened_cache = None;
        true
    }

    pub fn apply_line_width_to_selected(&mut self, width: f32) -> bool {
        let Some(idx) = self.selected_annotation() else {
            return false;
        };
        let mut next = self.history.current().to_vec();
        if !set_annotation_width(&mut next[idx], width.clamp(0.5, 64.0)) {
            return false;
        }
        self.push_annotations(next);
        self.selected_annotation = Some(idx);
        self.flattened_cache = None;
        true
    }

    /// Move the selected annotation by `(dx, dy)` base-image pixels as a
    /// single undoable edit. Used by arrow-key nudging. Returns false
    /// when nothing is selected.
    pub fn nudge_selected(&mut self, dx: f32, dy: f32) -> bool {
        let Some(idx) = self.selected_annotation() else {
            return false;
        };
        let mut next = self.history.current().to_vec();
        if idx >= next.len() {
            self.selected_annotation = None;
            return false;
        }
        translate_annotation(&mut next[idx], dx, dy);
        self.clamp_crop_annotation_to_base(&mut next[idx], CropClampMode::Move);
        // Keep non-Crop annotations at least partly on the image so a
        // burst of nudges can't strand one entirely off-canvas where it
        // can no longer be selected. Crop is already clamped above.
        self.clamp_annotation_into_base(&mut next[idx]);
        self.push_annotations(next);
        self.selected_annotation = Some(idx);
        self.flattened_cache = None;
        true
    }

    /// Select the annotation at `index` (keyboard Tab), or nothing.
    /// Returns whether `index` names an annotation.
    pub fn select_index(&mut self, index: Option<usize>) -> bool {
        let valid = index.filter(|i| *i < self.history.current().len());
        self.selected_annotation = valid;
        valid.is_some() || index.is_none()
    }

    /// Drag the selected annotation's `handle` by `(dx, dy)` base-image
    /// pixels as one undoable edit: the keyboard alternative to dragging
    /// a handle (⌥ + arrow). Returns false when nothing is selected or the
    /// annotation has no such handle.
    pub fn resize_selected_by(&mut self, handle: ResizeHandle, dx: f32, dy: f32) -> bool {
        let Some(idx) = self.selected_annotation() else {
            return false;
        };
        let mut next = self.history.current().to_vec();
        if !resize_annotation(&mut next[idx], handle, dx, dy) {
            return false;
        }
        self.clamp_crop_annotation_to_base(&mut next[idx], CropClampMode::Resize);
        self.clamp_annotation_into_base(&mut next[idx]);
        self.push_annotations(next);
        self.selected_annotation = Some(idx);
        self.flattened_cache = None;
        true
    }

    pub fn preview_move_selected_from(
        &mut self,
        baseline: &[Annotation],
        dx: f32,
        dy: f32,
    ) -> bool {
        let Some(idx) = self.selected_annotation else {
            return false;
        };
        if idx >= baseline.len() {
            self.selected_annotation = None;
            return false;
        }
        if !self.annotation_preview_may_go_on(baseline) {
            return false;
        }
        let mut next = baseline.to_vec();
        translate_annotation(&mut next[idx], dx, dy);
        self.clamp_crop_annotation_to_base(&mut next[idx], CropClampMode::Move);
        self.open_annotation_preview(baseline);
        self.history.replace_present(next);
        self.flattened_cache = None;
        true
    }

    pub fn preview_resize_selected_from(
        &mut self,
        baseline: &[Annotation],
        handle: ResizeHandle,
        dx: f32,
        dy: f32,
    ) -> bool {
        let Some(idx) = self.selected_annotation else {
            return false;
        };
        if idx >= baseline.len() {
            self.selected_annotation = None;
            return false;
        }
        if !self.annotation_preview_may_go_on(baseline) {
            return false;
        }
        let mut next = baseline.to_vec();
        if !resize_annotation(&mut next[idx], handle, dx, dy) {
            return false;
        }
        self.clamp_crop_annotation_to_base(&mut next[idx], CropClampMode::Resize);
        self.open_annotation_preview(baseline);
        self.history.replace_present(next);
        self.flattened_cache = None;
        true
    }

    pub fn preview_line_width_selected_from(
        &mut self,
        baseline: &[Annotation],
        width: f32,
    ) -> bool {
        let Some(idx) = self.selected_annotation else {
            return false;
        };
        if idx >= baseline.len() {
            self.selected_annotation = None;
            return false;
        }
        if !self.annotation_preview_may_go_on(baseline) {
            return false;
        }
        let mut next = baseline.to_vec();
        if !set_annotation_width(&mut next[idx], width.clamp(0.5, 64.0)) {
            self.open_annotation_preview(baseline);
            self.history.replace_present(next);
            self.flattened_cache = None;
            return false;
        }
        self.open_annotation_preview(baseline);
        self.history.replace_present(next);
        self.flattened_cache = None;
        true
    }

    /// Whether a preview from `baseline` may go on (checked before each
    /// tick). Fails safe when the history moved while this preview was
    /// open — an entry for the same baseline at an older generation:
    /// every handler that moves history cancels open previews first, so
    /// this only happens if one forgets to. The present then holds a
    /// newer edit made on top of the preview, so it is not re-opened:
    /// the tick changes nothing, the release commits nothing
    /// ([`Self::commit_preview_from_baseline`] needs it open) and a cancel
    /// restores nothing ([`Self::cancel_preview_from_baseline`]).
    ///
    /// An entry is only opened by a tick that changed the present
    /// ([`Self::open_annotation_preview`]), and every drag that changed it
    /// ends with a commit or cancel, so a leftover entry can't block a
    /// later drag.
    fn annotation_preview_may_go_on(&self, baseline: &[Annotation]) -> bool {
        !self
            .annotation_preview
            .as_ref()
            .is_some_and(|(b, g)| b.as_slice() == baseline && *g != self.history.generation())
    }

    /// Record the generation an annotation preview from `baseline` opened
    /// at: called by the first tick that replaces the present.
    fn open_annotation_preview(&mut self, baseline: &[Annotation]) {
        if !self.annotation_preview_is_open(baseline) {
            self.annotation_preview = Some((baseline.to_vec(), self.history.generation()));
        }
    }

    /// Whether `baseline`'s preview is the open one, opened at the
    /// present generation.
    fn annotation_preview_is_open(&self, baseline: &[Annotation]) -> bool {
        self.annotation_preview
            .as_ref()
            .is_some_and(|(b, g)| b.as_slice() == baseline && *g == self.history.generation())
    }

    /// Whether `baseline`'s preview opened before the history last moved:
    /// the present then holds newer edits made on top of the preview, and
    /// restoring `baseline` would drop them. Ends the preview.
    fn annotation_preview_is_stale(&mut self, baseline: &[Annotation]) -> bool {
        self.annotation_preview
            .take()
            .is_some_and(|(b, g)| b.as_slice() == baseline && g != self.history.generation())
    }

    /// Forget the open annotation preview without touching the present
    /// (a drag released before anything changed).
    pub fn end_annotation_preview(&mut self) {
        self.annotation_preview = None;
    }

    /// Keep the open preview from `baseline` as one undo step. Only the
    /// preview that is open now, at the present generation, commits: a
    /// stale or foreign baseline would push an edit over newer history.
    pub fn commit_preview_from_baseline(&mut self, baseline: Vec<Annotation>) -> bool {
        let open = self.annotation_preview_is_open(&baseline);
        self.annotation_preview = None;
        if !open {
            return false;
        }
        let final_state = self.history.current().to_vec();
        if final_state == baseline {
            self.history.replace_present(baseline);
            return false;
        }
        self.history.replace_present(baseline);
        self.push_annotations(final_state);
        self.clamp_selection();
        self.flattened_cache = None;
        true
    }

    /// Restore `baseline`, unless the history moved since its preview
    /// opened (a stale baseline would clobber the newer edits).
    pub fn cancel_preview_from_baseline(&mut self, baseline: Vec<Annotation>) {
        if self.annotation_preview_is_stale(&baseline) {
            return;
        }
        self.history.replace_present(baseline);
        self.sync_base();
        self.clamp_selection();
        self.flattened_cache = None;
    }

    /// Render base + annotations into a single flat `RgbaImage`,
    /// cached so successive views during a single state don't re-render.
    /// The cache is cleared on every state change.
    pub fn flatten(&mut self) -> RgbaImage {
        if let Some(img) = self.flattened_cache.as_ref().and_then(Flattened::to_image) {
            return img;
        }
        let img = self.render_present();
        self.flattened_cache = Some(Flattened::new(img.clone()));
        img
    }

    fn clamp_selection(&mut self) {
        if self
            .selected_annotation
            .is_some_and(|idx| idx >= self.history.current().len())
        {
            self.selected_annotation = None;
        }
    }

    /// Nudge a non-Crop annotation back toward the image if a move has
    /// pushed its bounding box fully off the base, so at least
    /// `MIN_ON_IMAGE` base pixels of it remain grabbable. Crop is
    /// handled by its own edge/position clamps and is skipped here.
    fn clamp_annotation_into_base(&self, annotation: &mut Annotation) {
        if matches!(annotation, Annotation::Crop { .. }) {
            return;
        }
        let Some(bounds) = annotation_bounds(annotation) else {
            return;
        };
        let base_w = self.base.width() as f32;
        let base_h = self.base.height() as f32;
        if base_w <= 0.0 || base_h <= 0.0 {
            return;
        }
        const MIN_ON_IMAGE: f32 = 8.0;
        let keep_x = MIN_ON_IMAGE.min(bounds.width);
        let keep_y = MIN_ON_IMAGE.min(bounds.height);
        // Allowed range for the bounding box's top-left corner such that a
        // sliver of width/height `keep_*` always overlaps the image.
        let x_lo = keep_x - bounds.width;
        let x_hi = base_w - keep_x;
        let y_lo = keep_y - bounds.height;
        let y_hi = base_h - keep_y;
        let clamped_x = bounds.x.clamp(x_lo.min(x_hi), x_lo.max(x_hi));
        let clamped_y = bounds.y.clamp(y_lo.min(y_hi), y_lo.max(y_hi));
        let dx = clamped_x - bounds.x;
        let dy = clamped_y - bounds.y;
        if dx != 0.0 || dy != 0.0 {
            translate_annotation(annotation, dx, dy);
        }
    }

    fn clamp_crop_annotation_to_base(&self, annotation: &mut Annotation, mode: CropClampMode) {
        let Annotation::Crop { rect } = annotation else {
            return;
        };
        let bounds = (self.base.width() as f32, self.base.height() as f32);
        match mode {
            CropClampMode::Move => clamp_crop_position(rect, bounds),
            CropClampMode::Resize => clamp_crop_edges(rect, bounds),
        }
    }
}

#[derive(Clone, Copy)]
enum CropClampMode {
    Move,
    Resize,
}

/// True when `point` (base-image coords) lands on the annotation per the
/// same rule `select_at` uses to pick it: stroke distance for line/pen
/// kinds, a radius for pins, bbox-with-tolerance for everything else. The
/// canvas reuses this so the hover/move cursor matches what a press does.
pub(crate) fn annotation_hit_test(annotation: &Annotation, point: PointLike, scale: f32) -> bool {
    // Tolerance is a constant number of *screen* pixels; convert to base
    // pixels for the current display scale. The stroke half-width part
    // (`line_width * 0.5`) stays in base coords because the drawn stroke
    // scales with the image; only the extra grab slack is screen-constant.
    let tolerance = hit_tolerance_base(HIT_TOLERANCE_SCREEN, scale);
    match annotation {
        Annotation::Line {
            a,
            b,
            line_width,
            style,
            ..
        }
        | Annotation::Arrow {
            a,
            b,
            line_width,
            style,
            ..
        } => {
            let reach = (*line_width * 0.5).max(tolerance);
            path_points(*a, *b, style.path, *line_width)
                .windows(2)
                .any(|pair| distance_to_segment(point, pair[0], pair[1]) <= reach)
        }
        Annotation::Pen {
            points, line_width, ..
        }
        | Annotation::Highlighter {
            points, line_width, ..
        } => points.windows(2).any(|pair| {
            distance_to_segment(point, pair[0], pair[1]) <= (*line_width * 0.5).max(tolerance)
        }),
        Annotation::NumberedPin { origin, .. } | Annotation::Stamp { origin, .. } => {
            distance(point, *origin) <= 16.0
        }
        // Border only: a spotlight must not block selecting what it contains.
        Annotation::Spotlight {
            rect,
            shape: readshot_core::SpotlightShape::Ellipse,
            ..
        } => distance_to_ellipse_rim(*rect, point) <= tolerance,
        Annotation::Spotlight { rect, .. } => {
            let inside = point.x > rect.x + tolerance
                && point.x < rect.x + rect.width - tolerance
                && point.y > rect.y + tolerance
                && point.y < rect.y + rect.height - tolerance;
            rect_contains(*rect, point, tolerance) && !inside
        }
        // The source disc and the bubble, not the empty box between them.
        Annotation::Magnifier {
            source,
            radius,
            bubble,
            zoom,
            shape,
            ..
        } => readshot_core::magnifier::hit(
            *source, *radius, *bubble, *zoom, *shape, point, tolerance,
        )
        .is_some(),
        _ => {
            annotation_bounds(annotation).is_some_and(|rect| rect_contains(rect, point, tolerance))
        }
    }
}

/// Distance from `point` to the outline of the ellipse inscribed in
/// `rect`, measured along the ray from the centre. Exact for circles and
/// close enough for grabbing an elongated rim.
fn distance_to_ellipse_rim(rect: RectLike, point: PointLike) -> f32 {
    let (rx, ry) = (rect.width * 0.5, rect.height * 0.5);
    if rx <= f32::EPSILON || ry <= f32::EPSILON {
        return f32::INFINITY;
    }
    let (dx, dy) = (point.x - (rect.x + rx), point.y - (rect.y + ry));
    let k = ((dx / rx).powi(2) + (dy / ry).powi(2)).sqrt();
    if k <= f32::EPSILON {
        return rx.min(ry);
    }
    (dx * dx + dy * dy).sqrt() * (1.0 - 1.0 / k).abs()
}

fn annotation_kind_label(annotation: &Annotation) -> &'static str {
    match annotation {
        Annotation::Rectangle { .. } => "Rectangle",
        Annotation::Ellipse { .. } => "Ellipse",
        Annotation::Line { .. } => "Line",
        Annotation::Arrow { .. } => "Arrow",
        Annotation::Pen { .. } => "Pen",
        Annotation::Highlighter { .. } => "Highlighter",
        Annotation::Text { .. } => "Text",
        Annotation::Blur { .. } => "Blur",
        Annotation::Pixelate { .. } => "Pixelate",
        Annotation::Spotlight { .. } => "Spotlight",
        Annotation::NumberedPin { .. } => "Pin",
        Annotation::Stamp { .. } => "Stamp",
        Annotation::Magnifier { .. } => "Magnifier",
        Annotation::Crop { .. } => "Crop",
    }
}

fn set_annotation_color(annotation: &mut Annotation, color: Rgba) -> bool {
    match annotation {
        Annotation::Rectangle { color: c, .. }
        | Annotation::Ellipse { color: c, .. }
        | Annotation::Line { color: c, .. }
        | Annotation::Arrow { color: c, .. }
        | Annotation::Pen { color: c, .. }
        | Annotation::Text { color: c, .. }
        | Annotation::NumberedPin { color: c, .. }
        | Annotation::Stamp { color: c, .. }
        | Annotation::Magnifier { color: c, .. } => {
            if *c == color {
                return false;
            }
            *c = color;
            true
        }
        Annotation::Highlighter { color: c, .. } => {
            let next = Rgba::new(color.r, color.g, color.b, c.a);
            if *c == next {
                return false;
            }
            *c = next;
            true
        }
        Annotation::Blur { .. }
        | Annotation::Pixelate { .. }
        | Annotation::Spotlight { .. }
        | Annotation::Crop { .. } => false,
    }
}

fn annotation_color(annotation: &Annotation) -> Option<Rgba> {
    match annotation {
        Annotation::Rectangle { color, .. }
        | Annotation::Ellipse { color, .. }
        | Annotation::Line { color, .. }
        | Annotation::Arrow { color, .. }
        | Annotation::Pen { color, .. }
        | Annotation::Highlighter { color, .. }
        | Annotation::Text { color, .. }
        | Annotation::NumberedPin { color, .. }
        | Annotation::Stamp { color, .. }
        | Annotation::Magnifier { color, .. } => Some(*color),
        Annotation::Blur { .. }
        | Annotation::Pixelate { .. }
        | Annotation::Spotlight { .. }
        | Annotation::Crop { .. } => None,
    }
}

fn annotation_width(annotation: &Annotation) -> Option<f32> {
    match annotation {
        Annotation::Rectangle { line_width, .. }
        | Annotation::Ellipse { line_width, .. }
        | Annotation::Line { line_width, .. }
        | Annotation::Arrow { line_width, .. }
        | Annotation::Pen { line_width, .. }
        | Annotation::Highlighter { line_width, .. } => Some(*line_width),
        Annotation::Blur { radius, .. } => Some(*radius),
        Annotation::Pixelate { block_size, .. } => Some(*block_size),
        Annotation::Text { .. }
        | Annotation::NumberedPin { .. }
        | Annotation::Stamp { .. }
        | Annotation::Magnifier { .. }
        | Annotation::Spotlight { .. }
        | Annotation::Crop { .. } => None,
    }
}

fn set_annotation_width(annotation: &mut Annotation, width: f32) -> bool {
    match annotation {
        Annotation::Rectangle { line_width, .. }
        | Annotation::Ellipse { line_width, .. }
        | Annotation::Line { line_width, .. }
        | Annotation::Arrow { line_width, .. }
        | Annotation::Pen { line_width, .. } => {
            if (*line_width - width).abs() < f32::EPSILON {
                return false;
            }
            *line_width = width;
            true
        }
        Annotation::Highlighter { line_width, .. } => {
            let width = width.max(12.0);
            if (*line_width - width).abs() < f32::EPSILON {
                return false;
            }
            *line_width = width;
            true
        }
        Annotation::Text { size, .. } => {
            let next = TextSize::nearest((width * 4.0 + 8.0).clamp(12.0, 96.0)).px();
            if (*size - next).abs() < f32::EPSILON {
                return false;
            }
            *size = next;
            true
        }
        Annotation::Blur { radius, .. } => {
            let next = width.max(2.0);
            if (*radius - next).abs() < f32::EPSILON {
                return false;
            }
            *radius = next;
            true
        }
        Annotation::Pixelate { block_size, .. } => {
            let next = width.max(4.0);
            if (*block_size - next).abs() < f32::EPSILON {
                return false;
            }
            *block_size = next;
            true
        }
        Annotation::NumberedPin { .. }
        | Annotation::Stamp { .. }
        | Annotation::Magnifier { .. }
        | Annotation::Spotlight { .. }
        | Annotation::Crop { .. } => false,
    }
}

pub(crate) fn annotation_bounds(annotation: &Annotation) -> Option<RectLike> {
    match annotation {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Spotlight { rect, .. }
        | Annotation::Crop { rect } => Some(*rect),
        Annotation::Line {
            a,
            b,
            line_width,
            style,
            ..
        }
        | Annotation::Arrow {
            a,
            b,
            line_width,
            style,
            ..
        } => bounds_for_points(
            &path_points(*a, *b, style.path, *line_width),
            (*line_width * 0.5).max(6.0),
        ),
        Annotation::Pen {
            points, line_width, ..
        }
        | Annotation::Highlighter {
            points, line_width, ..
        } => bounds_for_points(points, (*line_width * 0.5).max(6.0)),
        Annotation::Text {
            content,
            origin,
            size,
            style,
            ..
        } => readshot_core::render::text_bounds(content, *size, *origin, *style),
        Annotation::NumberedPin { origin, .. } | Annotation::Stamp { origin, .. } => {
            Some(RectLike::new(origin.x - 16.0, origin.y - 16.0, 32.0, 32.0))
        }
        Annotation::Magnifier {
            source,
            radius,
            bubble,
            zoom,
            ..
        } => Some(readshot_core::magnifier::bounds(
            *source, *radius, *bubble, *zoom,
        )),
    }
}

fn annotation_resize_handle_at(
    annotation: &Annotation,
    point: PointLike,
    scale: f32,
) -> Option<ResizeHandle> {
    let radius = hit_tolerance_base(HANDLE_HIT_RADIUS_SCREEN, scale);
    // Nearest handle wins, so Bend stays grabbable on short curved lines
    // where Start/End also fall inside the radius.
    let nearest = annotation_resize_handles(annotation)
        .into_iter()
        .map(|(handle, p)| (handle, distance(point, p)))
        .filter(|(_, d)| *d <= radius)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(handle, _)| handle);
    nearest.or_else(|| magnifier_bubble_grab(annotation, point))
}

/// A press anywhere inside a magnifier's bubble grabs the bubble (End),
/// not just its centre handle.
pub(crate) fn magnifier_bubble_grab(
    annotation: &Annotation,
    point: PointLike,
) -> Option<ResizeHandle> {
    let Annotation::Magnifier {
        source,
        radius,
        bubble,
        zoom,
        shape,
        ..
    } = annotation
    else {
        return None;
    };
    (readshot_core::magnifier::hit(*source, *radius, *bubble, *zoom, *shape, point, 0.0)
        == Some(readshot_core::magnifier::MagnifierHit::Bubble))
    .then_some(ResizeHandle::End)
}

/// Whether `point` is on a magnifier's leader line (within the hit
/// tolerance for the display `scale`).
pub(crate) fn magnifier_leader_hit(annotation: &Annotation, point: PointLike, scale: f32) -> bool {
    let Annotation::Magnifier {
        source,
        radius,
        bubble,
        zoom,
        ..
    } = annotation
    else {
        return false;
    };
    let br = readshot_core::magnifier::bubble_radius(*radius, *zoom);
    readshot_core::magnifier::leader_endpoints(*source, *radius, *bubble, br).is_some_and(
        |(a, b)| {
            distance_to_segment(point, a, b) <= hit_tolerance_base(HIT_TOLERANCE_SCREEN, scale)
        },
    )
}

fn annotation_resize_handles(annotation: &Annotation) -> Vec<(ResizeHandle, PointLike)> {
    match annotation {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Spotlight { rect, .. }
        | Annotation::Crop { rect } => rect_resize_handles(*rect),
        Annotation::Line { a, b, style, .. } | Annotation::Arrow { a, b, style, .. } => {
            let mut handles = vec![(ResizeHandle::Start, *a), (ResizeHandle::End, *b)];
            if let LinePath::Curved { bend } = style.path {
                handles.push((ResizeHandle::Bend, curve_apex(*a, *b, bend)));
            }
            handles
        }
        // Source centre moves what is magnified, the rim resizes it, the
        // bubble centre moves the bubble.
        Annotation::Magnifier {
            source,
            radius,
            bubble,
            ..
        } => vec![
            (ResizeHandle::Start, *source),
            (
                ResizeHandle::East,
                PointLike::new(source.x + radius, source.y),
            ),
            (ResizeHandle::End, *bubble),
        ],
        _ => Vec::new(),
    }
}

fn rect_resize_handles(rect: RectLike) -> Vec<(ResizeHandle, PointLike)> {
    let cx = rect.x + rect.width * 0.5;
    let cy = rect.y + rect.height * 0.5;
    vec![
        (ResizeHandle::NorthWest, PointLike::new(rect.x, rect.y)),
        (ResizeHandle::North, PointLike::new(cx, rect.y)),
        (
            ResizeHandle::NorthEast,
            PointLike::new(rect.x + rect.width, rect.y),
        ),
        (ResizeHandle::East, PointLike::new(rect.x + rect.width, cy)),
        (
            ResizeHandle::SouthEast,
            PointLike::new(rect.x + rect.width, rect.y + rect.height),
        ),
        (
            ResizeHandle::South,
            PointLike::new(cx, rect.y + rect.height),
        ),
        (
            ResizeHandle::SouthWest,
            PointLike::new(rect.x, rect.y + rect.height),
        ),
        (ResizeHandle::West, PointLike::new(rect.x, cy)),
    ]
}

fn translate_annotation(annotation: &mut Annotation, dx: f32, dy: f32) {
    match annotation {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Spotlight { rect, .. }
        | Annotation::Crop { rect } => translate_rect(rect, dx, dy),
        Annotation::Line { a, b, .. } | Annotation::Arrow { a, b, .. } => {
            translate_point(a, dx, dy);
            translate_point(b, dx, dy);
        }
        Annotation::Pen { points, .. } | Annotation::Highlighter { points, .. } => {
            for point in points {
                translate_point(point, dx, dy);
            }
        }
        Annotation::Text { origin, .. }
        | Annotation::NumberedPin { origin, .. }
        | Annotation::Stamp { origin, .. } => {
            translate_point(origin, dx, dy);
        }
        Annotation::Magnifier { source, bubble, .. } => {
            translate_point(source, dx, dy);
            translate_point(bubble, dx, dy);
        }
    }
}

fn resize_annotation(annotation: &mut Annotation, handle: ResizeHandle, dx: f32, dy: f32) -> bool {
    match annotation {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Spotlight { rect, .. }
        | Annotation::Crop { rect } => {
            if matches!(
                handle,
                ResizeHandle::Start | ResizeHandle::End | ResizeHandle::Bend
            ) {
                return false;
            }
            resize_rect(rect, handle, dx, dy);
            true
        }
        Annotation::Line { a, b, style, .. } | Annotation::Arrow { a, b, style, .. } => {
            match handle {
                ResizeHandle::Start => {
                    translate_point(a, dx, dy);
                    true
                }
                ResizeHandle::End => {
                    translate_point(b, dx, dy);
                    true
                }
                ResizeHandle::Bend => match &mut style.path {
                    LinePath::Curved { bend } => {
                        *bend = bend_after_drag(*a, *b, *bend, dx, dy);
                        true
                    }
                    _ => false,
                },
                _ => false,
            }
        }
        Annotation::Magnifier {
            source,
            radius,
            bubble,
            ..
        } => match handle {
            ResizeHandle::Start => {
                translate_point(source, dx, dy);
                true
            }
            ResizeHandle::End => {
                translate_point(bubble, dx, dy);
                true
            }
            ResizeHandle::East => {
                // The rim follows the pointer in any direction: the new
                // radius is the grabbed rim point's distance from the
                // source once moved by the drag.
                let (rx, ry) = (*radius + dx, dy);
                let next = (rx * rx + ry * ry).sqrt();
                *radius = if next.is_finite() {
                    next.max(readshot_core::magnifier::MIN_SOURCE_RADIUS)
                } else {
                    *radius
                };
                true
            }
            _ => false,
        },
        _ => false,
    }
}

fn resize_rect(rect: &mut RectLike, handle: ResizeHandle, dx: f32, dy: f32) {
    let mut left = rect.x;
    let mut right = rect.x + rect.width;
    let mut top = rect.y;
    let mut bottom = rect.y + rect.height;
    match handle {
        ResizeHandle::NorthWest => {
            left += dx;
            top += dy;
        }
        ResizeHandle::North => top += dy,
        ResizeHandle::NorthEast => {
            right += dx;
            top += dy;
        }
        ResizeHandle::East => right += dx,
        ResizeHandle::SouthEast => {
            right += dx;
            bottom += dy;
        }
        ResizeHandle::South => bottom += dy,
        ResizeHandle::SouthWest => {
            left += dx;
            bottom += dy;
        }
        ResizeHandle::West => left += dx,
        ResizeHandle::Start | ResizeHandle::End | ResizeHandle::Bend => return,
    }

    let x = left.min(right);
    let y = top.min(bottom);
    rect.x = x;
    rect.y = y;
    rect.width = (right - left).abs().max(1.0);
    rect.height = (bottom - top).abs().max(1.0);
}

fn clamp_crop_position(rect: &mut RectLike, (base_w, base_h): (f32, f32)) {
    if base_w <= 0.0 || base_h <= 0.0 {
        rect.x = 0.0;
        rect.y = 0.0;
        rect.width = 1.0;
        rect.height = 1.0;
        return;
    }
    rect.width = rect.width.clamp(1.0, base_w);
    rect.height = rect.height.clamp(1.0, base_h);
    rect.x = rect.x.clamp(0.0, (base_w - rect.width).max(0.0));
    rect.y = rect.y.clamp(0.0, (base_h - rect.height).max(0.0));
}

fn clamp_crop_edges(rect: &mut RectLike, (base_w, base_h): (f32, f32)) {
    if base_w <= 0.0 || base_h <= 0.0 {
        rect.x = 0.0;
        rect.y = 0.0;
        rect.width = 1.0;
        rect.height = 1.0;
        return;
    }
    let x0 = rect.x.clamp(0.0, base_w);
    let y0 = rect.y.clamp(0.0, base_h);
    let x1 = (rect.x + rect.width).clamp(0.0, base_w);
    let y1 = (rect.y + rect.height).clamp(0.0, base_h);
    rect.x = x0.min(x1);
    rect.y = y0.min(y1);
    rect.width = (x1 - x0).abs().max(1.0).min(base_w);
    rect.height = (y1 - y0).abs().max(1.0).min(base_h);
    clamp_crop_position(rect, (base_w, base_h));
}

fn translate_rect(rect: &mut RectLike, dx: f32, dy: f32) {
    rect.x += dx;
    rect.y += dy;
}

fn translate_point(point: &mut PointLike, dx: f32, dy: f32) {
    point.x += dx;
    point.y += dy;
}

fn bounds_for_points(points: &[PointLike], pad: f32) -> Option<RectLike> {
    let first = points.first()?;
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (first.x, first.y, first.x, first.y);
    for point in points.iter().skip(1) {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    Some(RectLike::new(
        min_x - pad,
        min_y - pad,
        (max_x - min_x) + pad * 2.0,
        (max_y - min_y) + pad * 2.0,
    ))
}

fn rect_contains(rect: RectLike, point: PointLike, tolerance: f32) -> bool {
    point.x >= rect.x - tolerance
        && point.y >= rect.y - tolerance
        && point.x <= rect.x + rect.width + tolerance
        && point.y <= rect.y + rect.height + tolerance
}

fn distance(a: PointLike, b: PointLike) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

fn distance_to_segment(point: PointLike, a: PointLike, b: PointLike) -> f32 {
    let ab_x = b.x - a.x;
    let ab_y = b.y - a.y;
    let len_sq = ab_x * ab_x + ab_y * ab_y;
    if len_sq <= f32::EPSILON {
        return distance(point, a);
    }
    let t = (((point.x - a.x) * ab_x + (point.y - a.y) * ab_y) / len_sq).clamp(0.0, 1.0);
    let projection = PointLike::new(a.x + ab_x * t, a.y + ab_y * t);
    distance(point, projection)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba as ImgRgba;
    use readshot_core::render::text_pill_rect;
    use readshot_core::{PointLike, RectLike};

    fn mismatched_flat() -> Flattened {
        Flattened {
            handle: iced::widget::image::Handle::from_rgba(2, 2, vec![0u8; 16]),
            width: 4,
            height: 4,
        }
    }

    #[test]
    fn a_flattened_image_whose_pixels_do_not_fit_is_none_never_blank() {
        // Before, this came back as a blank 4x4 image passed off as the
        // document.
        assert_eq!(mismatched_flat().try_image(), None);
        let ok = Flattened::new(RgbaImage::from_pixel(3, 2, ImgRgba([9, 8, 7, 255])));
        assert_eq!(
            ok.to_image(),
            Some(RgbaImage::from_pixel(3, 2, ImgRgba([9, 8, 7, 255])))
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "flattened pixels do not match its size")]
    fn a_mismatched_flattened_image_trips_the_debug_assertion() {
        let _ = mismatched_flat().to_image();
    }

    fn magnifier() -> Annotation {
        Annotation::Magnifier {
            source: PointLike::new(20.0, 20.0),
            radius: 10.0,
            bubble: PointLike::new(80.0, 20.0),
            zoom: 2.0,
            shape: readshot_core::MagnifierShape::Circle,
            color: Rgba::OPAQUE_BLACK,
        }
    }

    #[test]
    fn select_index_and_keyboard_resize_are_one_undo_step() {
        let mut s = EditorState::new(RgbaImage::new(200, 100));
        s.commit_annotation(magnifier());
        assert!(s.select_index(Some(0)));
        assert!(!s.select_index(Some(5)));
        assert_eq!(s.selected_annotation(), None);
        assert!(s.select_index(Some(0)));
        let depth = s.undo_depth();
        assert!(s.resize_selected_by(ResizeHandle::End, 0.0, 10.0));
        assert_eq!(s.undo_depth(), depth + 1);
        let Annotation::Magnifier { source, bubble, .. } = s.annotations()[0].clone() else {
            panic!("magnifier");
        };
        assert_eq!(source, PointLike::new(20.0, 20.0));
        assert_eq!(bubble, PointLike::new(80.0, 30.0));
        // A handle the annotation lacks changes nothing.
        assert!(!s.resize_selected_by(ResizeHandle::NorthWest, 1.0, 1.0));
        assert_eq!(s.undo_depth(), depth + 1);
        s.clear_selection();
        assert!(!s.resize_selected_by(ResizeHandle::End, 1.0, 1.0));
    }

    #[test]
    fn magnifier_handles_move_source_bubble_and_radius_independently() {
        let mut s = EditorState::new(RgbaImage::new(200, 100));
        s.commit_annotation(magnifier());
        // A press anywhere in the bubble grabs it (End).
        assert_eq!(
            s.resize_handle_at_scaled(PointLike::new(92.0, 25.0), 1.0),
            Some(ResizeHandle::End)
        );
        assert_eq!(
            s.resize_handle_at_scaled(PointLike::new(30.0, 20.0), 1.0),
            Some(ResizeHandle::East)
        );
        let base = s.annotations().to_vec();
        assert!(s.preview_resize_selected_from(&base, ResizeHandle::End, 10.0, 30.0));
        let Annotation::Magnifier { source, bubble, .. } = s.annotations()[0].clone() else {
            unreachable!()
        };
        assert_eq!(
            (source, bubble),
            (PointLike::new(20.0, 20.0), PointLike::new(90.0, 50.0))
        );
        assert!(s.preview_resize_selected_from(&base, ResizeHandle::Start, 5.0, 0.0));
        let Annotation::Magnifier { source, bubble, .. } = s.annotations()[0].clone() else {
            unreachable!()
        };
        assert_eq!(
            (source, bubble),
            (PointLike::new(25.0, 20.0), PointLike::new(80.0, 20.0))
        );
        assert!(s.preview_resize_selected_from(&base, ResizeHandle::East, -8.0, 0.0));
        let Annotation::Magnifier { radius, .. } = s.annotations()[0].clone() else {
            unreachable!()
        };
        assert_eq!(radius, readshot_core::magnifier::MIN_SOURCE_RADIUS);
    }

    #[test]
    fn the_magnifier_rim_follows_the_pointer() {
        let mut s = EditorState::new(RgbaImage::new(200, 100));
        s.commit_annotation(magnifier());
        let base = s.annotations().to_vec();
        let radius = |s: &EditorState| match s.annotations()[0] {
            Annotation::Magnifier { radius, .. } => radius,
            _ => unreachable!(),
        };
        // Rim handle at (30, 20); dragged straight down to (30, 40): the
        // rim is now ~22.4 px from the source at (20, 20).
        assert!(s.preview_resize_selected_from(&base, ResizeHandle::East, 0.0, 20.0));
        assert!((radius(&s) - 500f32.sqrt()).abs() < 1e-4, "{}", radius(&s));
        // Across the source to (5, 20): 15 px on the other side.
        assert!(s.preview_resize_selected_from(&base, ResizeHandle::East, -25.0, 0.0));
        assert_eq!(radius(&s), 15.0);
        // Onto the source centre: floored.
        assert!(s.preview_resize_selected_from(&base, ResizeHandle::East, -10.0, 0.0));
        assert_eq!(radius(&s), readshot_core::magnifier::MIN_SOURCE_RADIUS);
    }

    #[test]
    fn a_selected_magnifiers_leader_keeps_it_selected_and_an_unselected_one_does_not() {
        // Source (20, 20) r 10, bubble (80, 20) r 20: the leader runs
        // from x 30 to x 60 along y 20.
        let mut s = EditorState::with_annotations(RgbaImage::new(200, 100), vec![magnifier()]);
        let on_leader = PointLike::new(45.0, 21.0);
        assert_eq!(s.select_at(on_leader), None, "unselected: click-through");
        assert_eq!(s.select_at(PointLike::new(80.0, 20.0)), Some(0));
        assert_eq!(s.resize_handle_at(on_leader), None, "not a handle");
        assert_eq!(s.select_at(on_leader), Some(0), "selected: grabbed");
        // Off the leader the selection still clears.
        assert_eq!(s.select_at(PointLike::new(45.0, 40.0)), None);
    }

    #[test]
    fn a_bubble_far_from_its_centre_handle_still_grabs_the_bubble() {
        let mut s = EditorState::new(RgbaImage::new(200, 100));
        let mut m = magnifier();
        if let Annotation::Magnifier { zoom, .. } = &mut m {
            *zoom = 4.0;
        }
        s.commit_annotation(m);
        // 35 px from the bubble centre (radius 40): beyond the handle
        // radius, still inside the bubble.
        assert_eq!(
            s.resize_handle_at_scaled(PointLike::new(115.0, 20.0), 1.0),
            Some(ResizeHandle::End)
        );
        // Outside both: no handle.
        assert_eq!(
            s.resize_handle_at_scaled(PointLike::new(50.0, 90.0), 1.0),
            None
        );
    }

    #[test]
    fn a_press_in_a_bubble_selects_the_magnifier_over_a_later_shape_it_covers() {
        // The rectangle is newer, but render paints magnifiers last, so
        // the bubble hides it where they overlap.
        let mut s = EditorState::with_annotations(
            RgbaImage::new(200, 100),
            vec![
                magnifier(),
                Annotation::Rectangle {
                    rect: RectLike::new(70.0, 10.0, 20.0, 20.0),
                    color: Rgba::OPAQUE_BLACK,
                    line_width: 2.0,
                    style: Default::default(),
                },
            ],
        );
        assert_eq!(s.select_at(PointLike::new(80.0, 20.0)), Some(0));
        // Outside the bubble the rectangle is still selectable.
        assert_eq!(s.select_at(PointLike::new(150.0, 50.0)), None);
        let mut s2 = EditorState::with_annotations(
            RgbaImage::new(200, 100),
            vec![
                magnifier(),
                Annotation::Rectangle {
                    rect: RectLike::new(110.0, 10.0, 20.0, 20.0),
                    color: Rgba::OPAQUE_BLACK,
                    line_width: 2.0,
                    style: Default::default(),
                },
            ],
        );
        assert_eq!(s2.select_at(PointLike::new(120.0, 20.0)), Some(1));
        // Among magnifiers, the newest wins.
        let mut s3 =
            EditorState::with_annotations(RgbaImage::new(200, 100), vec![magnifier(), magnifier()]);
        assert_eq!(s3.select_at(PointLike::new(80.0, 20.0)), Some(1));
    }

    #[test]
    fn a_newer_shape_inside_a_source_disc_stays_selectable() {
        let mut s = EditorState::with_annotations(
            RgbaImage::new(300, 200),
            vec![
                Annotation::Magnifier {
                    source: PointLike::new(100.0, 100.0),
                    radius: 30.0,
                    bubble: PointLike::new(220.0, 100.0),
                    zoom: 2.0,
                    shape: readshot_core::MagnifierShape::Circle,
                    color: Rgba::OPAQUE_BLACK,
                },
                Annotation::Blur {
                    rect: RectLike::new(90.0, 90.0, 20.0, 20.0),
                    radius: 8.0,
                },
            ],
        );
        assert_eq!(s.select_at(PointLike::new(100.0, 100.0)), Some(1));
        // The disc outside the blur still selects the magnifier.
        assert_eq!(s.select_at(PointLike::new(80.0, 100.0)), Some(0));
    }

    #[test]
    fn a_magnifier_is_grabbed_by_its_source_and_bubble_and_moves_whole() {
        let mut m = Annotation::Magnifier {
            source: PointLike::new(20.0, 20.0),
            radius: 5.0,
            bubble: PointLike::new(60.0, 20.0),
            zoom: 2.0,
            shape: readshot_core::MagnifierShape::Circle,
            color: Rgba::OPAQUE_BLACK,
        };
        assert!(annotation_hit_test(&m, PointLike::new(20.0, 20.0), 1.0));
        assert!(annotation_hit_test(&m, PointLike::new(62.0, 22.0), 1.0));
        // The empty gap between them stays clickable for what is below.
        assert!(!annotation_hit_test(&m, PointLike::new(40.0, 5.0), 1.0));
        assert_eq!(
            annotation_bounds(&m),
            Some(RectLike::new(15.0, 10.0, 55.0, 20.0))
        );
        translate_annotation(&mut m, 3.0, -2.0);
        let Annotation::Magnifier { source, bubble, .. } = m else {
            unreachable!()
        };
        assert_eq!(
            (source, bubble),
            (PointLike::new(23.0, 18.0), PointLike::new(63.0, 18.0))
        );
    }

    #[test]
    fn shared_base_hands_out_the_same_image_without_copying() {
        let state = EditorState::new(solid_base(8, 8));
        let a = state.shared_base();
        let b = state.clone().shared_base();
        assert!(std::sync::Arc::ptr_eq(&a, &b));
        assert_eq!(*a, *state.base());
    }

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
            style: Default::default(),
        }
    }

    #[test]
    fn effective_line_width_reads_the_selection_then_the_tool() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.set_line_width(3.0);
        assert_eq!(s.effective_line_width(), 3.0);
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 8.0,
            style: Default::default(),
        });
        assert_eq!(s.effective_line_width(), 8.0);
        s.commit_annotation(Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
            radius: 12.0,
        });
        assert_eq!(s.effective_line_width(), 12.0);
        s.clear_selection();
        assert_eq!(s.effective_line_width(), 3.0);
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
            style: Default::default(),
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
    fn spotlight_selects_by_its_border_not_its_inside() {
        let mut s = EditorState::new(solid_base(200, 200));
        s.commit_annotation(Annotation::Spotlight {
            rect: RectLike::new(20.0, 20.0, 100.0, 100.0),
            shape: readshot_core::SpotlightShape::Rectangle,
            dim: 0.55,
        });
        s.clear_selection();
        assert_eq!(s.select_at(PointLike::new(70.0, 70.0)), None);
        assert_eq!(s.select_at(PointLike::new(20.0, 70.0)), Some(0));
    }

    #[test]
    fn ellipse_spotlight_selects_by_its_rim_not_its_bbox() {
        let mut s = EditorState::new(solid_base(200, 200));
        // Centre (100, 60), radii 80 × 40.
        s.commit_annotation(Annotation::Spotlight {
            rect: RectLike::new(20.0, 20.0, 160.0, 80.0),
            shape: readshot_core::SpotlightShape::Ellipse,
            dim: 0.55,
        });
        s.clear_selection();
        let (c, a, b) = ((100.0_f32, 60.0_f32), 80.0_f32, 40.0_f32);
        let t = std::f32::consts::FRAC_PI_4;
        let rim = PointLike::new(c.0 + a * t.cos(), c.1 + b * t.sin());
        assert_eq!(s.select_at(rim), Some(0), "rim at 45°");
        s.clear_selection();
        // The bbox corner and border midpoint far from the rim miss,
        // and so does the inside.
        assert_eq!(s.select_at(PointLike::new(21.0, 21.0)), None);
        assert_eq!(s.select_at(PointLike::new(178.0, 98.0)), None);
        assert_eq!(s.select_at(PointLike::new(100.0, 60.0)), None);
        // Rim points on the axes still hit.
        assert_eq!(s.select_at(PointLike::new(20.0, 60.0)), Some(0));
    }

    #[test]
    fn stamp_moves_and_recolors_undoably() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Stamp {
            origin: PointLike::new(20.0, 20.0),
            stamp: readshot_core::StampKind::Star,
            color: Rgba::OPAQUE_BLACK,
        });
        let depth = s.undo_depth();
        assert!(s.nudge_selected(5.0, 0.0));
        assert!(s.apply_color_to_selected(Rgba::new(1.0, 0.0, 0.0, 1.0)));
        assert_eq!(s.undo_depth(), depth + 2);
        assert!(matches!(
            &s.annotations()[0],
            Annotation::Stamp { origin, color, .. } if origin.x == 25.0 && color.r == 1.0
        ));
        assert_eq!(s.selected_kind_label(), Some("Stamp"));
        // Stamps have no size, so width changes don't apply.
        assert!(!s.apply_line_width_to_selected(9.0));
    }

    #[test]
    fn select_at_returns_topmost_annotation_containing_point() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 30.0, 30.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });
        s.commit_annotation(Annotation::Ellipse {
            rect: RectLike::new(10.0, 10.0, 30.0, 30.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });

        assert_eq!(s.select_at(PointLike::new(20.0, 20.0)), Some(1));
        assert_eq!(s.selected_annotation(), Some(1));

        assert_eq!(s.select_at(PointLike::new(55.0, 55.0)), None);
        assert_eq!(s.selected_annotation(), None);
    }

    #[test]
    fn delete_selected_annotation_pushes_one_undoable_edit() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        s.commit_annotation(rect(20.0));
        assert_eq!(s.select_at(PointLike::new(22.0, 2.0)), Some(1));

        assert!(s.delete_selected_annotation());

        assert_eq!(s.annotations().len(), 1);
        assert_eq!(s.selected_annotation(), None);
        assert!(s.undo());
        assert_eq!(s.annotations().len(), 2);
    }

    #[test]
    fn switching_drawing_tools_preserves_selection_for_toolbar_edits() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));

        s.set_tool(ToolState::Arrow);

        assert_eq!(s.active_tool(), ToolState::Arrow);
        assert_eq!(s.selected_annotation(), Some(0));
        assert!(s.apply_line_width_to_selected(9.0));
        match &s.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 9.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn selected_annotation_can_preview_move_then_commit_one_undo_step() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));

        let baseline = s.annotations().to_vec();
        assert!(s.preview_move_selected_from(&baseline, 5.0, 7.0));
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.x, 5.0);
                assert_eq!(rect.y, 7.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
        assert!(s.commit_preview_from_baseline(baseline));
        assert!(s.undo());
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.x, 0.0);
                assert_eq!(rect.y, 0.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn nudge_selected_moves_annotation_as_one_undo_step() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));
        let depth_before = s.undo_depth();

        assert!(s.nudge_selected(1.0, -1.0));
        assert!(s.nudge_selected(1.0, 0.0));
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.x, 2.0);
                assert_eq!(rect.y, -1.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
        // Each nudge is its own undoable edit, and selection survives.
        assert_eq!(s.undo_depth(), depth_before + 2);
        assert_eq!(s.selected_annotation(), Some(0));
        assert!(s.undo());
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => assert_eq!(rect.x, 1.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn nudge_selected_is_noop_without_selection() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        s.clear_selection();
        let depth_before = s.undo_depth();
        assert!(!s.nudge_selected(1.0, 1.0));
        assert_eq!(s.undo_depth(), depth_before);
    }

    #[test]
    fn selected_annotation_can_cancel_preview_without_undo_step() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));

        let undo_depth = s.undo_depth();
        let baseline = s.annotations().to_vec();
        assert!(s.preview_move_selected_from(&baseline, 5.0, 7.0));

        s.cancel_preview_from_baseline(baseline.clone());

        assert_eq!(s.annotations(), baseline.as_slice());
        assert_eq!(s.selected_annotation(), Some(0));
        assert_eq!(s.undo_depth(), undo_depth);
    }

    #[test]
    fn selected_rect_reports_resize_handle_at_corner() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(10.0, 20.0, 30.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });
        assert_eq!(s.select_at(PointLike::new(12.0, 22.0)), Some(0));

        assert_eq!(
            s.resize_handle_at(PointLike::new(40.0, 60.0)),
            Some(ResizeHandle::SouthEast)
        );
        assert_eq!(s.resize_handle_at(PointLike::new(25.0, 40.0)), None);
        assert_eq!(s.selected_handles().len(), 8);
    }

    #[test]
    fn selected_rect_can_preview_resize_then_commit_one_undo_step() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(10.0, 20.0, 30.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });
        assert_eq!(s.select_at(PointLike::new(12.0, 22.0)), Some(0));

        let baseline = s.annotations().to_vec();
        assert!(s.preview_resize_selected_from(&baseline, ResizeHandle::SouthEast, 5.0, 7.0));
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.x, 10.0);
                assert_eq!(rect.y, 20.0);
                assert_eq!(rect.width, 35.0);
                assert_eq!(rect.height, 47.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
        assert!(s.commit_preview_from_baseline(baseline));
        assert!(s.undo());
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.width, 30.0);
                assert_eq!(rect.height, 40.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn crop_move_preview_stays_inside_base_without_shrinking() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Crop {
            rect: RectLike::new(10.0, 10.0, 20.0, 20.0),
        });
        assert_eq!(s.select_at(PointLike::new(12.0, 12.0)), Some(0));

        let baseline = s.annotations().to_vec();
        assert!(s.preview_move_selected_from(&baseline, -30.0, 50.0));

        match &s.annotations()[0] {
            Annotation::Crop { rect } => {
                assert_eq!(*rect, RectLike::new(0.0, 44.0, 20.0, 20.0));
            }
            other => panic!("expected crop, got {other:?}"),
        }
    }

    #[test]
    fn crop_resize_preview_clamps_to_base_edges() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Crop {
            rect: RectLike::new(10.0, 10.0, 20.0, 20.0),
        });
        assert_eq!(s.select_at(PointLike::new(12.0, 12.0)), Some(0));

        let baseline = s.annotations().to_vec();
        assert!(s.preview_resize_selected_from(&baseline, ResizeHandle::SouthEast, 100.0, 100.0));

        match &s.annotations()[0] {
            Annotation::Crop { rect } => {
                assert_eq!(*rect, RectLike::new(10.0, 10.0, 54.0, 54.0));
            }
            other => panic!("expected crop, got {other:?}"),
        }
    }

    #[test]
    fn committed_crop_is_clamped_to_base() {
        let mut s = EditorState::new(solid_base(64, 64));

        s.commit_annotation(Annotation::Crop {
            rect: RectLike::new(50.0, -10.0, 40.0, 30.0),
        });

        match &s.annotations()[0] {
            Annotation::Crop { rect } => {
                assert_eq!(*rect, RectLike::new(50.0, 0.0, 14.0, 20.0));
            }
            other => panic!("expected crop, got {other:?}"),
        }
    }

    #[test]
    fn selected_line_endpoint_can_move_independently() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Line {
            a: PointLike::new(10.0, 10.0),
            b: PointLike::new(30.0, 30.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(s.select_at(PointLike::new(20.0, 20.0)), Some(0));
        assert_eq!(
            s.resize_handle_at(PointLike::new(30.0, 30.0)),
            Some(ResizeHandle::End)
        );
        assert_eq!(
            s.selected_handles(),
            vec![
                (ResizeHandle::Start, PointLike::new(10.0, 10.0)),
                (ResizeHandle::End, PointLike::new(30.0, 30.0)),
            ]
        );

        let baseline = s.annotations().to_vec();
        assert!(s.preview_resize_selected_from(&baseline, ResizeHandle::End, 4.0, -6.0));
        match &s.annotations()[0] {
            Annotation::Line { a, b, .. } => {
                assert_eq!(*a, PointLike::new(10.0, 10.0));
                assert_eq!(*b, PointLike::new(34.0, 24.0));
            }
            other => panic!("expected line, got {other:?}"),
        }
    }

    #[test]
    fn a_preview_baseline_older_than_a_commit_never_drops_the_commit() {
        for cancel in [true, false] {
            let mut s = EditorState::new(solid_base(64, 64));
            s.commit_annotation(rect(0.0));
            assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));
            let baseline = s.annotations().to_vec();
            assert!(s.preview_line_width_selected_from(&baseline, 8.0));
            // A text draft is committed while the width preview lingers.
            s.commit_annotation(rect(30.0));
            let committed = s.annotations().to_vec();
            let depth = s.undo_depth();
            if cancel {
                s.cancel_preview_from_baseline(baseline);
            } else {
                assert!(!s.commit_preview_from_baseline(baseline));
            }
            assert_eq!(s.annotations(), committed.as_slice());
            assert_eq!(s.undo_depth(), depth);
        }
        // A fresh preview still cancels back to its baseline.
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));
        let baseline = s.annotations().to_vec();
        assert!(s.preview_line_width_selected_from(&baseline, 12.0));
        s.cancel_preview_from_baseline(baseline.clone());
        assert_eq!(s.annotations(), baseline.as_slice());
    }

    #[test]
    fn selected_annotation_color_updates_undoably() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));
        let blue = Rgba::new(0.0, 0.2, 1.0, 1.0);

        assert!(s.apply_color_to_selected(blue));

        match &s.annotations()[0] {
            Annotation::Rectangle { color, .. } => assert_eq!(*color, blue),
            other => panic!("expected rectangle, got {other:?}"),
        }
        assert!(s.undo());
        match &s.annotations()[0] {
            Annotation::Rectangle { color, .. } => assert_eq!(*color, Rgba::OPAQUE_BLACK),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn selected_annotation_width_updates_undoably() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));

        assert!(s.apply_line_width_to_selected(9.0));

        match &s.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 9.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn selected_annotation_width_preview_commits_one_undo_step() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        assert_eq!(s.select_at(PointLike::new(2.0, 2.0)), Some(0));
        let baseline = s.annotations().to_vec();

        assert!(s.preview_line_width_selected_from(&baseline, 8.0));
        assert!(s.preview_line_width_selected_from(&baseline, 10.0));
        assert!(s.commit_preview_from_baseline(baseline));
        assert_eq!(s.undo_depth(), 2);

        match &s.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 10.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
        assert!(s.undo());
        match &s.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 1.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn multiline_text_bounds_grow_per_line_and_hit_test_lower_lines() {
        let origin = PointLike::new(10.0, 20.0);
        let size = 16.0;

        let mut single = EditorState::new(solid_base(96, 96));
        single.commit_annotation(Annotation::Text {
            content: "one".into(),
            origin,
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size,
            style: Default::default(),
        });
        assert_eq!(single.select_at(origin), Some(0));
        let single_bounds = single.selected_bounds().expect("single-line bounds");

        let mut multi = EditorState::new(solid_base(96, 96));
        multi.commit_annotation(Annotation::Text {
            content: "one\ntwo\nthree".into(),
            origin,
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size,
            style: Default::default(),
        });
        assert_eq!(multi.select_at(origin), Some(0));
        let multi_bounds = multi.selected_bounds().expect("multi-line bounds");

        // Three lines are ~3x the single-line height.
        assert!(
            (multi_bounds.height - single_bounds.height * 3.0).abs() < 0.01,
            "three-line height {} should be 3x single-line height {}",
            multi_bounds.height,
            single_bounds.height
        );
        // Width tracks the widest line ("three" > "one").
        assert!(multi_bounds.width > single_bounds.width);

        // A point on the third line hits the multi-line text but falls
        // outside a single-line box.
        let third_line = PointLike::new(12.0, origin.y + size * 1.35 * 2.0);
        assert_eq!(multi.select_at(third_line), Some(0));
        let mut single_again = EditorState::new(solid_base(96, 96));
        single_again.commit_annotation(Annotation::Text {
            content: "one".into(),
            origin,
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size,
            style: Default::default(),
        });
        assert_eq!(single_again.select_at(third_line), None);
    }

    #[test]
    fn selected_text_can_be_replaced_undoably() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Text {
            content: "old".into(),
            origin: PointLike::new(10.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        assert_eq!(s.select_at(PointLike::new(12.0, 8.0)), Some(0));
        let edit = s.selected_text_edit().expect("selected text is editable");
        assert_eq!(edit.index, 0);
        assert_eq!(edit.content, "old");

        assert!(s.replace_selected_text("new".into()));

        match &s.annotations()[0] {
            Annotation::Text { content, .. } => assert_eq!(content, "new"),
            other => panic!("expected text, got {other:?}"),
        }
        assert!(s.undo());
        match &s.annotations()[0] {
            Annotation::Text { content, .. } => assert_eq!(content, "old"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn text_can_be_replaced_by_original_edit_index() {
        let mut s = EditorState::new(solid_base(96, 96));
        s.commit_annotation(Annotation::Text {
            content: "old".into(),
            origin: PointLike::new(10.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(40.0, 40.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(s.select_at(PointLike::new(12.0, 8.0)), Some(0));
        let edit = s.selected_text_edit().expect("selected text is editable");
        assert_eq!(edit.index, 0);
        assert_eq!(s.select_at(PointLike::new(45.0, 45.0)), Some(1));

        assert!(s.replace_text_at(edit.index, "new".into()));

        match &s.annotations()[0] {
            Annotation::Text { content, .. } => assert_eq!(content, "new"),
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(s.selected_annotation(), Some(0));
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

    #[test]
    fn handle_hit_radius_is_constant_in_screen_pixels() {
        let mut s = EditorState::new(solid_base(256, 256));
        // A large rect so its eight handles are far apart and a probe near
        // one corner can only match that corner.
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(20.0, 20.0, 200.0, 200.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });
        assert_eq!(s.select_at(PointLike::new(30.0, 30.0)), Some(0));

        // SE corner is at (220, 220); probe 12 base px east of it. The next
        // nearest handle (E or S) is ~100 base px away.
        let probe = PointLike::new(232.0, 220.0);

        // Fitted-down 4K-style view (scale 0.25): the 14 screen-px radius is
        // 56 base px, so 12 base px is a comfortable grab.
        assert_eq!(
            s.resize_handle_at_scaled(probe, 0.25),
            Some(ResizeHandle::SouthEast)
        );

        // Zoomed in 4x: the radius shrinks to 3.5 base px, so 12 base px
        // (48 screen px) is out of reach — no accidental grab.
        assert_eq!(s.resize_handle_at_scaled(probe, 4.0), None);
    }

    #[test]
    fn select_tolerance_scales_with_display_scale() {
        let mut s = EditorState::new(solid_base(256, 256));
        s.commit_annotation(Annotation::Line {
            a: PointLike::new(20.0, 20.0),
            b: PointLike::new(200.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });

        // 10 base px off the stroke. At scale 1.0 the 6-screen-px tolerance
        // is 6 base px → miss. At scale 0.25 it becomes 24 base px → hit.
        let probe = PointLike::new(100.0, 30.0);
        assert_eq!(s.select_at_scaled(probe, 1.0), None);
        assert_eq!(s.select_at_scaled(probe, 0.25), Some(0));
    }

    #[test]
    fn nudge_keeps_non_crop_annotation_partly_on_image() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(10.0)); // 10x10 rect at (10, 0)
        assert_eq!(s.select_at(PointLike::new(12.0, 2.0)), Some(0));

        // Shove it far off to the right and down in one big nudge.
        assert!(s.nudge_selected(1000.0, 1000.0));
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                // At least 8 px of the 10 px-wide rect stays on the image.
                assert!(rect.x <= 64.0 - 8.0, "rect.x = {}", rect.x);
                assert!(rect.y <= 64.0 - 8.0, "rect.y = {}", rect.y);
                assert!(rect.x + rect.width >= 8.0);
                assert!(rect.y + rect.height >= 8.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }

        // And far off to the top-left.
        assert!(s.nudge_selected(-1000.0, -1000.0));
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert!(rect.x + rect.width >= 8.0, "rect.x = {}", rect.x);
                assert!(rect.y + rect.height >= 8.0, "rect.y = {}", rect.y);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    use super::super::style::{StyleMessage, TextSize};
    use readshot_core::{FillMode, LinePath, LineStyle, ShapeStyle, TextStyle};

    #[test]
    fn style_applies_to_the_selected_rectangle_as_one_undo_step() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        let depth = s.undo_depth();
        assert!(s.apply_style_to_selected(StyleMessage::Fill(FillMode::Filled)));
        assert_eq!(s.undo_depth(), depth + 1);
        assert!(matches!(
            &s.annotations()[0],
            Annotation::Rectangle { style, .. } if style.fill == FillMode::Filled
        ));
        assert!(s.undo());
        assert!(matches!(
            &s.annotations()[0],
            Annotation::Rectangle { style, .. } if *style == ShapeStyle::default()
        ));
    }

    #[test]
    fn style_for_another_kind_is_a_no_op_on_the_selection() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        let depth = s.undo_depth();
        assert!(!s.apply_style_to_selected(StyleMessage::TextBold(true)));
        assert_eq!(s.undo_depth(), depth);
    }

    #[test]
    fn style_preview_commits_as_one_undo_step() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(rect(0.0));
        let baseline = s.annotations().to_vec();
        let depth = s.undo_depth();
        for r in [4.0, 8.0, 12.0] {
            s.set_style(StyleMessage::CornerRadius(r));
            s.preview_style_selected_from(&baseline, StyleMessage::CornerRadius(r));
        }
        assert!(s.commit_preview_from_baseline(baseline));
        assert_eq!(s.undo_depth(), depth + 1);
        assert_eq!(s.tool_style().corner_radius, 12.0);
        assert!(matches!(
            &s.annotations()[0],
            Annotation::Rectangle { style, .. } if style.corner_radius == 12.0
        ));
    }

    #[test]
    fn effective_style_and_color_follow_the_selection() {
        let mut s = EditorState::new(solid_base(200, 200));
        s.commit_annotation(Annotation::Text {
            content: "hi".into(),
            origin: PointLike::new(20.0, 40.0),
            color: Rgba::new(0.0, 0.0, 1.0, 1.0),
            font_family: "system-ui".into(),
            size: 36.0,
            style: TextStyle {
                pill: true,
                bold: false,
            },
        });
        let eff = s.effective_style();
        assert_eq!(eff.text_size, TextSize::L);
        assert!(eff.text_pill);
        assert_eq!(s.effective_color(), Rgba::new(0.0, 0.0, 1.0, 1.0));
        s.clear_selection();
        assert_eq!(s.effective_style(), s.tool_style());
        assert_eq!(s.effective_color(), s.current_color());
    }

    fn curved_line() -> Annotation {
        Annotation::Line {
            a: PointLike::new(10.0, 50.0),
            b: PointLike::new(110.0, 50.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: LineStyle {
                path: LinePath::Curved { bend: 0.4 },
                ..LineStyle::default()
            },
        }
    }

    #[test]
    fn curved_line_hit_test_follows_the_curve() {
        let mut s = EditorState::new(solid_base(200, 120));
        s.commit_annotation(curved_line());
        s.clear_selection();
        // Apex at y = 50 + 0.5 * 0.4 * 100 = 70; the chord (y = 50) is empty.
        assert_eq!(s.select_at(PointLike::new(60.0, 70.0)), Some(0));
        s.clear_selection();
        assert_eq!(s.select_at(PointLike::new(60.0, 50.0)), None);
    }

    #[test]
    fn bend_handle_is_grabbable_on_a_short_curved_line() {
        let mut s = EditorState::new(solid_base(200, 140));
        s.commit_annotation(Annotation::Line {
            a: PointLike::new(10.0, 50.0),
            b: PointLike::new(14.0, 50.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: LineStyle {
                path: LinePath::Curved { bend: 0.4 },
                ..LineStyle::default()
            },
        });
        let (_, apex) = s
            .selected_handles()
            .into_iter()
            .find(|(h, _)| *h == ResizeHandle::Bend)
            .expect("bend handle");
        // Start and End are within reach too, but Bend is nearest.
        assert_eq!(s.resize_handle_at(apex), Some(ResizeHandle::Bend));
    }

    #[test]
    fn bend_handle_reshapes_a_curved_line_as_one_undo_step() {
        let mut s = EditorState::new(solid_base(200, 140));
        s.commit_annotation(curved_line());
        let handles = s.selected_handles();
        let (_, apex) = handles
            .iter()
            .copied()
            .find(|(h, _)| *h == ResizeHandle::Bend)
            .expect("curved line exposes a bend handle");
        assert_eq!(s.resize_handle_at(apex), Some(ResizeHandle::Bend));
        let baseline = s.annotations().to_vec();
        let depth = s.undo_depth();
        assert!(s.preview_resize_selected_from(&baseline, ResizeHandle::Bend, 0.0, 10.0));
        assert!(s.commit_preview_from_baseline(baseline));
        assert_eq!(s.undo_depth(), depth + 1);
        match &s.annotations()[0] {
            // bend 0.4 + 2 × 10 / 100 = 0.6
            Annotation::Line {
                style:
                    LineStyle {
                        path: LinePath::Curved { bend },
                        ..
                    },
                ..
            } => assert!((bend - 0.6).abs() < 1e-5, "{bend}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn straight_lines_have_no_bend_handle() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Line {
            a: PointLike::new(0.0, 0.0),
            b: PointLike::new(40.0, 0.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: LineStyle::default(),
        });
        assert!(s
            .selected_handles()
            .iter()
            .all(|(h, _)| *h != ResizeHandle::Bend));
    }

    #[test]
    fn pill_text_bounds_are_the_rendered_pill() {
        let pill = Annotation::Text {
            content: "Hi\nthere".into(),
            origin: PointLike::new(50.0, 50.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 20.0,
            style: TextStyle {
                pill: true,
                bold: false,
            },
        };
        assert_eq!(
            annotation_bounds(&pill),
            text_pill_rect("Hi\nthere", 20.0, PointLike::new(50.0, 50.0))
        );
    }

    #[test]
    fn width_changes_snap_text_to_size_steps() {
        let mut s = EditorState::new(solid_base(200, 200));
        s.commit_annotation(Annotation::Text {
            content: "hi".into(),
            origin: PointLike::new(20.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 24.0,
            style: TextStyle::default(),
        });
        for width in [1.0, 3.0, 5.0, 7.0, 9.0, 20.0] {
            s.apply_line_width_to_selected(width);
            match &s.annotations()[0] {
                Annotation::Text { size, .. } => {
                    assert!(
                        [16.0, 24.0, 36.0, 52.0].contains(size),
                        "width {width} → {size}"
                    )
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn commit_annotations_is_one_undo_step() {
        let mut s = EditorState::new(RgbaImage::new(10, 10));
        let blur = |x: f32| Annotation::Blur {
            rect: RectLike::new(x, 0.0, 2.0, 2.0),
            radius: 2.0,
        };
        assert_eq!(s.commit_annotations(Vec::new()), 0);
        assert!(!s.can_undo());
        assert_eq!(s.commit_annotations(vec![blur(0.0), blur(4.0)]), 2);
        assert_eq!(s.annotations().len(), 2);
        assert_eq!(s.selected_annotation(), None);
        assert!(s.undo());
        assert!(s.annotations().is_empty());
    }
    fn px_grid(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x + 10 * y) as u8, 0, 0, 255]))
    }

    fn block(x: f32, y: f32) -> Annotation {
        Annotation::Rectangle {
            rect: RectLike::new(x, y, 1.0, 1.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        }
    }

    #[test]
    fn rotate_turns_the_base_and_its_annotations_and_undo_restores_both() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.commit_annotation(block(2.0, 0.0));
        assert!(s.rotate(true).unwrap());
        assert_eq!(s.base().dimensions(), (2, 3));
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => assert_eq!((rect.x, rect.y), (1.0, 2.0)),
            other => panic!("{other:?}"),
        }
        assert!(s.undo());
        assert_eq!(s.base().dimensions(), (3, 2));
        assert!(s.geometry().is_original());
        match &s.annotations()[0] {
            Annotation::Rectangle { rect, .. } => assert_eq!((rect.x, rect.y), (2.0, 0.0)),
            other => panic!("{other:?}"),
        }
        assert!(s.redo());
        assert_eq!(s.base().dimensions(), (2, 3));
    }

    #[test]
    fn beautify_changes_are_one_undo_step_each() {
        use readshot_core::beautify::BuiltinPreset;
        let mut s = EditorState::new(px_grid(4, 4));
        assert!(s.set_beautify(BuiltinPreset::Dark.settings()));
        assert!(!s.set_beautify(BuiltinPreset::Dark.settings()));
        assert_eq!(s.undo_depth(), 1);
        s.undo();
        assert!(s.beautify().is_identity());
    }

    #[test]
    fn snapshot_preview_commits_as_one_step_and_cancel_restores() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        let mut s = EditorState::new(px_grid(4, 4));
        let baseline = s.snapshot();
        for p in [10, 20, 30] {
            let mut next = baseline.clone();
            next.doc.beautify = BuiltinPreset::Minimal.settings();
            next.doc.beautify.apply(BeautifyEdit::Padding(p));
            s.preview_snapshot(next).unwrap();
        }
        assert_eq!(s.undo_depth(), 0, "previews never push");
        assert!(s.commit_preview_snapshot(baseline.clone()));
        assert_eq!(s.undo_depth(), 1);
        assert_eq!(s.beautify().padding, 30);
        let base2 = s.snapshot();
        let mut next = base2.clone();
        next.doc.beautify.padding = 99;
        s.preview_snapshot(next).unwrap();
        s.cancel_preview_snapshot(base2);
        assert_eq!(s.beautify().padding, 30);
    }

    #[test]
    fn a_precomposed_step_matches_apply_geometry_and_undoes_in_one() {
        let ann = Annotation::Rectangle {
            rect: RectLike::new(1.0, 1.0, 2.0, 2.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        };
        let mut sync = EditorState::with_annotations(px_grid(6, 4), vec![ann.clone()]);
        let mut pre = EditorState::with_annotations(px_grid(6, 4), vec![ann]);
        let next = sync.scaled_geometry(2.0, 1.5).unwrap();
        assert!(sync.apply_geometry(next.clone()).unwrap());
        let step = pre.geometry_step(next).unwrap().expect("a change");
        assert_eq!(pre.undo_depth(), 0, "making a step changes nothing");
        let base = step.doc.geometry.compose();
        let flat = render(&base, &step.annotations);
        assert!(pre
            .apply_ops_precomposed(&[DocOp::Push(step)], base, flat)
            .unwrap());
        assert_eq!(pre.undo_depth(), 1);
        assert_eq!(pre.annotations(), sync.annotations());
        assert_eq!(pre.base().dimensions(), (12, 6));
        assert!(*pre.base() == *sync.base());
        assert!(pre.flatten() == sync.flatten());
        // Undo recomposes the original frame.
        assert!(pre.undo());
        assert_eq!(pre.base().dimensions(), (6, 4));
        // No change, no step.
        let same = (**pre.geometry()).clone();
        assert!(pre.geometry_step(same).unwrap().is_none());
    }

    #[test]
    fn a_precomposed_preview_commits_and_cancels_like_a_preview() {
        let mut s = EditorState::new(px_grid(4, 2));
        s.add_part(px_grid(4, 3), readshot_core::document::Join::Below)
            .unwrap();
        let baseline = s.snapshot();
        let next = baseline
            .doc
            .geometry
            .with_layout(5, baseline.doc.geometry.align);
        let step = EditorState::geometry_preview_step(&baseline, next).unwrap();
        let base = step.doc.geometry.compose();
        let flat = render(&base, &step.annotations);
        s.preview_precomposed(step, base, flat).unwrap();
        assert_eq!(s.base().dimensions(), (4, 10));
        assert_eq!(s.undo_depth(), 1, "a preview never pushes");
        s.cancel_preview_snapshot(baseline.clone());
        assert_eq!(s.base().dimensions(), (4, 5));
        let next = baseline
            .doc
            .geometry
            .with_layout(7, baseline.doc.geometry.align);
        let step = EditorState::geometry_preview_step(&baseline, next).unwrap();
        let base = step.doc.geometry.compose();
        let flat = render(&base, &step.annotations);
        s.preview_precomposed(step, base, flat).unwrap();
        assert!(s.commit_preview_snapshot(baseline));
        assert_eq!(s.undo_depth(), 2);
        assert_eq!(s.base().dimensions(), (4, 12));
    }

    #[test]
    fn add_part_appends_below_and_undo_removes_it() {
        let mut s = EditorState::new(px_grid(4, 2));
        let id = s
            .add_part(px_grid(4, 3), readshot_core::document::Join::Below)
            .unwrap();
        assert_ne!(id, readshot_core::document::ORIGINAL_PART);
        assert_eq!(s.base().dimensions(), (4, 5));
        assert_eq!(s.part_ids().len(), 2);
        s.undo();
        assert_eq!(s.base().dimensions(), (4, 2));
        assert!(s.geometry().is_original());
    }

    #[test]
    fn adding_to_a_rotated_document_bakes_the_rotation_first() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.commit_annotation(block(2.0, 0.0));
        s.rotate(true).unwrap();
        let before = s.annotations().to_vec();
        s.add_part(px_grid(2, 2), readshot_core::document::Join::Below)
            .unwrap();
        let g = s.geometry();
        assert!(g.transform.is_identity());
        assert_eq!(g.parts.len(), 2);
        assert_ne!(g.parts[0].id, readshot_core::document::ORIGINAL_PART);
        assert_eq!(
            s.annotations(),
            &before[..],
            "the first part stays at the origin"
        );
        assert_eq!(s.base().dimensions(), (2, 5));
    }

    #[test]
    fn oversized_scale_is_refused_and_changes_nothing() {
        let mut s = EditorState::new(px_grid(3, 2));
        let err = s.scale_by(20_000.0, 1.0).unwrap_err();
        assert!(matches!(
            err,
            readshot_core::document::GeometryError::TooLarge { .. }
        ));
        assert!(s.geometry().is_original());
        assert_eq!(s.undo_depth(), 0);
    }

    #[test]
    fn noop_geometry_preview_and_cancel_keep_the_same_base() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.rotate(true).unwrap();
        let before = s.shared_base();
        let baseline = s.snapshot();
        s.preview_geometry(&baseline, (*baseline.doc.geometry).clone())
            .unwrap();
        assert!(Arc::ptr_eq(&before, &s.shared_base()));
        s.cancel_preview_snapshot(baseline);
        assert!(Arc::ptr_eq(&before, &s.shared_base()));
    }

    #[test]
    fn undo_during_a_preview_does_not_scramble_history() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.rotate(true).unwrap();
        let baseline = s.snapshot();
        let g = baseline.doc.geometry.with_layout(5, Default::default());
        s.preview_geometry(&baseline, g).unwrap();
        assert!(s.undo());
        assert!(s.geometry().is_original());
        assert!(!s.commit_preview_snapshot(baseline));
        assert_eq!(s.undo_depth(), 0);
        assert!(s.can_redo());
        assert!(s.redo());
        assert_eq!(s.base().dimensions(), (2, 3));
    }

    #[test]
    fn scale_by_ignores_non_finite_and_non_positive_factors() {
        let mut s = EditorState::new(px_grid(3, 2));
        for (x, y) in [
            (0.0, 1.0),
            (-1.0, 1.0),
            (f32::NAN, 1.0),
            (1.0, f32::INFINITY),
        ] {
            assert_eq!(s.scale_by(x, y), Ok(false));
        }
        assert_eq!(s.scale_by(1.0, 1.0), Ok(false));
        assert_eq!(s.undo_depth(), 0);
    }

    #[test]
    fn invalid_snapshot_preview_is_refused() {
        let mut s = EditorState::new(px_grid(3, 2));
        let mut snap = s.snapshot();
        snap.doc.geometry = Arc::new(DocGeometry::default());
        assert!(s.preview_snapshot(snap).is_err());
        assert!(s.geometry().is_original());
    }

    #[test]
    fn an_edit_after_an_abandoned_preview_survives_undo_and_redo() {
        use readshot_core::beautify::BuiltinPreset;
        let mut s = EditorState::new(px_grid(4, 4));
        s.commit_annotation(block(0.0, 0.0));
        let baseline = s.snapshot();
        let mut next = baseline.clone();
        next.doc.beautify = BuiltinPreset::Soft.settings();
        s.preview_snapshot(next).unwrap();
        s.commit_annotation(block(1.0, 1.0));
        assert!(s.undo());
        assert!(s.redo());
        assert_eq!(s.annotations().len(), 2);
    }

    #[test]
    fn a_rotation_after_an_abandoned_preview_stays_redoable() {
        use readshot_core::beautify::BuiltinPreset;
        let mut s = EditorState::new(px_grid(3, 2));
        let mut next = s.snapshot();
        next.doc.beautify = BuiltinPreset::Soft.settings();
        s.preview_snapshot(next).unwrap();
        s.rotate(true).unwrap();
        assert!(s.undo());
        assert!(s.redo());
        assert_eq!(s.base().dimensions(), (2, 3));
    }

    #[test]
    fn undo_that_only_closes_a_preview_reports_a_change() {
        use readshot_core::beautify::BuiltinPreset;
        let mut s = EditorState::new(px_grid(4, 4));
        let mut next = s.snapshot();
        next.doc.beautify = BuiltinPreset::Soft.settings();
        s.preview_snapshot(next).unwrap();
        assert!(s.undo(), "closing the preview is a change");
        assert!(s.beautify().is_identity());
        assert_eq!(s.undo_depth(), 0);
        let mut next = s.snapshot();
        next.doc.beautify = BuiltinPreset::Soft.settings();
        s.preview_snapshot(next).unwrap();
        assert!(s.redo(), "redo closing a preview is a change too");
        assert!(s.beautify().is_identity());
    }

    #[test]
    fn undo_that_only_closes_a_preview_clamps_the_selection_and_redraws() {
        let mut s = EditorState::new(px_grid(4, 4));
        let mut next = s.snapshot();
        next.annotations.push(block(0.0, 0.0));
        s.preview_snapshot(next).unwrap();
        assert_eq!(s.select_at(PointLike::new(0.5, 0.5)), Some(0));
        let previewed = s.flatten();
        assert!(s.undo(), "closing the preview is a change");
        assert_eq!(s.undo_depth(), 0);
        assert!(s.annotations().is_empty());
        assert_eq!(s.selected_annotation(), None, "selection clamped");
        assert_ne!(s.flatten(), previewed, "the cache was reset");
    }

    #[test]
    fn commit_preview_snapshot_refuses_a_different_baseline() {
        use readshot_core::beautify::BuiltinPreset;
        let mut s = EditorState::new(px_grid(4, 4));
        let baseline = s.snapshot();
        let mut next = baseline.clone();
        next.doc.beautify = BuiltinPreset::Soft.settings();
        s.preview_snapshot(next).unwrap();
        let mut other = baseline.clone();
        other.doc.beautify = BuiltinPreset::Dark.settings();
        assert!(!s.commit_preview_snapshot(other));
        assert_eq!(s.undo_depth(), 0);
    }

    #[test]
    fn cancel_preview_snapshot_ignores_a_stale_baseline() {
        use readshot_core::beautify::BuiltinPreset;
        let mut s = EditorState::new(px_grid(4, 4));
        let baseline = s.snapshot();
        let mut next = baseline.clone();
        next.doc.beautify = BuiltinPreset::Soft.settings();
        s.preview_snapshot(next).unwrap();
        s.commit_annotation(block(0.0, 0.0));
        s.cancel_preview_snapshot(baseline);
        assert_eq!(s.annotations().len(), 1, "the newer edit survives");
        assert_eq!(s.beautify(), BuiltinPreset::Soft.settings());
    }

    #[test]
    fn cancel_preview_snapshot_ignores_a_different_baseline() {
        use readshot_core::beautify::BuiltinPreset;
        let mut s = EditorState::new(px_grid(4, 4));
        let baseline = s.snapshot();
        let mut next = baseline.clone();
        next.doc.beautify = BuiltinPreset::Soft.settings();
        s.preview_snapshot(next).unwrap();
        let mut other = baseline.clone();
        other.doc.beautify = BuiltinPreset::Dark.settings();
        s.cancel_preview_snapshot(other);
        assert_eq!(s.beautify(), BuiltinPreset::Soft.settings());
    }

    #[test]
    fn a_leftover_preview_entry_never_passes_for_a_new_preview() {
        let mut s = EditorState::new(RgbaImage::new(100, 100));
        s.commit_annotation(rect(10.0));
        let base = s.annotations().to_vec();
        // A refused resize tick (a rectangle has no Start handle) changes
        // nothing and opens no entry; the drag is released without commit
        // or cancel.
        assert!(!s.preview_resize_selected_from(&base, ResizeHandle::Start, 5.0, 5.0));
        assert!(s.nudge_selected(1.0, 0.0));
        assert!(s.undo());
        assert_eq!(s.annotations(), base.as_slice());
        // A new preview from the same baseline, then Esc: restored.
        assert!(s.preview_move_selected_from(&base, 3.0, 0.0));
        s.cancel_preview_from_baseline(base.clone());
        assert_eq!(s.annotations(), base.as_slice(), "cancel restores");
        // And a commit is one undo step.
        let depth = s.undo_depth();
        assert!(s.preview_move_selected_from(&base, 3.0, 0.0));
        assert!(s.commit_preview_from_baseline(base.clone()));
        assert_eq!(s.undo_depth(), depth + 1);
    }

    #[test]
    fn a_preview_whose_history_moved_mid_drag_is_never_reopened() {
        let mut s = EditorState::new(RgbaImage::new(100, 100));
        s.commit_annotation(rect(10.0));
        let base = s.annotations().to_vec();
        assert!(s.preview_move_selected_from(&base, 3.0, 0.0));
        // A handler moves history without cancelling the preview first.
        assert!(s.nudge_selected(0.0, 7.0));
        let newer = s.annotations().to_vec();
        let depth = s.undo_depth();
        // Further ticks of the stale drag change nothing…
        assert!(!s.preview_move_selected_from(&base, 9.0, 0.0));
        assert_eq!(s.annotations(), newer.as_slice());
        // …its release commits nothing over the newer edit…
        assert!(!s.commit_preview_from_baseline(base.clone()));
        assert_eq!(s.annotations(), newer.as_slice());
        assert_eq!(s.undo_depth(), depth);
        // …and the next drag, from the present, works.
        assert!(s.preview_move_selected_from(&newer, 1.0, 0.0));
        assert!(s.commit_preview_from_baseline(newer.clone()));
        assert_eq!(s.undo_depth(), depth + 1);
        // Same for a cancel (Esc) of a stale drag: nothing restored.
        let base = s.annotations().to_vec();
        assert!(s.preview_line_width_selected_from(&base, 9.0));
        assert!(s.nudge_selected(1.0, 0.0));
        let newer = s.annotations().to_vec();
        assert!(!s.preview_line_width_selected_from(&base, 12.0));
        s.cancel_preview_from_baseline(base);
        assert_eq!(s.annotations(), newer.as_slice());
    }

    #[test]
    fn commit_preview_from_baseline_needs_that_preview_open() {
        let mut s = EditorState::new(RgbaImage::new(100, 100));
        s.commit_annotation(rect(10.0));
        let depth = s.undo_depth();
        // No preview open at all.
        assert!(!s.commit_preview_from_baseline(Vec::new()));
        assert_eq!(s.undo_depth(), depth);
        assert_eq!(s.annotations().len(), 1);
        // A preview from another baseline is open.
        let base = s.annotations().to_vec();
        assert!(s.preview_move_selected_from(&base, 3.0, 0.0));
        let moved = s.annotations().to_vec();
        assert!(!s.commit_preview_from_baseline(Vec::new()));
        assert_eq!(s.undo_depth(), depth);
        assert_eq!(s.annotations(), moved.as_slice());
    }

    #[test]
    fn undo_and_redo_across_a_rotation_swap_cached_bases_in() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.commit_annotation(block(2.0, 0.0));
        let original = s.shared_base();
        s.rotate(true).unwrap();
        assert_eq!(s.base_composes(), 1);
        let rotated = s.shared_base();
        assert!(s.undo());
        assert!(Arc::ptr_eq(&s.shared_base(), &original));
        assert!(s.redo());
        assert!(Arc::ptr_eq(&s.shared_base(), &rotated));
        assert!(s.undo());
        assert_eq!(s.base_composes(), 1, "every move swapped a cached base in");
    }

    #[test]
    fn cached_bases_are_only_the_adjacent_states() {
        let mut s = EditorState::new(px_grid(3, 2));
        for _ in 0..3 {
            s.rotate(true).unwrap();
        }
        // The state one undo away only; the present's is `base`.
        assert_eq!(s.bases.len(), 1);
        assert!(s
            .cached_base(&s.peek_undo().unwrap().doc.geometry)
            .is_some());
        let composes = s.base_composes();
        assert!(s.undo());
        assert_eq!(s.base_composes(), composes, "swapped in");
        // The state one redo away is kept; two undos back was evicted
        // when it stopped being adjacent (nothing is composed ahead).
        assert_eq!(s.bases.len(), 1);
        assert!(s
            .cached_base(&s.peek_redo().unwrap().doc.geometry)
            .is_some());
        assert!(s
            .cached_base(&s.peek_undo().unwrap().doc.geometry)
            .is_none());
        assert!(s.undo());
        assert_eq!(s.base_composes(), composes + 1);
        assert!(s.bases.len() <= 2);
    }

    #[test]
    fn queued_ops_plan_and_apply_as_their_undo_steps() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.commit_annotation(block(2.0, 0.0));
        let start = s.snapshot();
        let turn = |from: &Snapshot| {
            EditorState::step_from(from, from.doc.geometry.rotated(true))
                .unwrap()
                .unwrap()
        };
        let one = turn(&start);
        let two = turn(&one);
        let ops = vec![
            DocOp::Push(one.clone()),
            DocOp::Push(two.clone()),
            DocOp::Undo,
        ];
        assert_eq!(s.plan_ops(&ops), Some(one.clone()));
        assert_eq!(s.snapshot(), start, "planning changes nothing");
        let base = one.doc.geometry.compose();
        let flat = render(&base, &one.annotations);
        assert!(s.apply_ops_precomposed(&ops, base, flat).unwrap());
        assert_eq!(s.snapshot(), one);
        assert_eq!(s.undo_depth(), 2);
        assert_eq!(s.redo_depth(), 1);
        assert_eq!(s.base().dimensions(), (2, 3));
        assert!(s.has_flattened());
        // The same as doing them one by one.
        let mut sync = EditorState::new(px_grid(3, 2));
        sync.commit_annotation(block(2.0, 0.0));
        sync.rotate(true).unwrap();
        sync.rotate(true).unwrap();
        assert!(sync.undo());
        assert_eq!(s.annotations(), sync.annotations());
        assert!(s.flatten() == sync.flatten());
        assert!(s.redo());
        assert_eq!(s.snapshot(), two);
    }

    #[test]
    fn an_op_that_cannot_apply_plans_nothing_and_applies_nothing() {
        let mut s = EditorState::new(px_grid(3, 2));
        assert_eq!(s.plan_ops(&[DocOp::Undo]), None);
        assert_eq!(s.plan_ops(&[DocOp::Redo]), None);
        let base = s.shared_base();
        let flat = s.flatten();
        assert!(!s.apply_ops_precomposed(&[DocOp::Undo], base, flat).unwrap());
        assert_eq!(s.undo_depth(), 0);
        // Not while a snapshot preview is open either.
        let baseline = s.snapshot();
        let mut next = baseline.clone();
        next.doc.beautify.padding = 12;
        s.preview_snapshot(next).unwrap();
        assert_eq!(s.plan_ops(&[]), None);
    }

    #[test]
    fn a_flatten_rendered_elsewhere_is_installed_only_for_the_present() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.commit_annotation(block(2.0, 0.0));
        let base = s.shared_base();
        let annotations = s.annotations().to_vec();
        let flat = render(&base, &annotations);
        s.commit_annotation(block(0.0, 0.0));
        assert!(!s.install_flattened(&base, &annotations, flat.clone()));
        assert!(!s.has_flattened());
        assert!(s.undo());
        assert!(s.install_flattened(&base, &annotations, flat.clone()));
        assert!(s.has_flattened());
        assert!(s.flatten() == flat);
        // Another base (same size) is not this document's.
        let other = Arc::new(px_grid(3, 2));
        assert!(!s.install_flattened(&other, &annotations, flat));
    }

    #[test]
    fn a_cancelled_snapshot_preview_restores_its_flattened_image_without_a_render() {
        let mut s = EditorState::new(px_grid(4, 2));
        s.add_part(px_grid(4, 3), readshot_core::document::Join::Below)
            .unwrap();
        s.commit_annotation(block(1.0, 1.0));
        let _ = s.flatten();
        let kept = s.flattened_shared().unwrap();
        let composes = s.base_composes();
        let baseline = s.snapshot();
        let g = baseline
            .doc
            .geometry
            .with_layout(5, baseline.doc.geometry.align);
        s.preview_geometry(&baseline, g).unwrap();
        assert_eq!(s.base_composes(), composes + 1);
        assert!(!s.has_flattened());
        s.cancel_preview_snapshot(baseline);
        assert_eq!(
            s.base_composes(),
            composes + 1,
            "the baseline's base was cached"
        );
        assert_eq!(
            s.flattened_shared().unwrap().handle().id(),
            kept.handle().id()
        );
        // A Beautify-only preview keeps the flattened image throughout.
        let baseline = s.snapshot();
        let mut next = baseline.clone();
        next.doc.beautify.padding = 20;
        s.preview_snapshot(next).unwrap();
        assert_eq!(
            s.flattened_shared().unwrap().handle().id(),
            kept.handle().id()
        );
        s.cancel_preview_snapshot(baseline);
        assert_eq!(
            s.flattened_shared().unwrap().handle().id(),
            kept.handle().id()
        );
    }

    #[test]
    fn adding_to_a_rotated_document_bakes_its_cached_base_without_a_compose() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.rotate(true).unwrap();
        let composes = s.base_composes();
        let from = s.snapshot();
        let (step, _) = s
            .add_part_step(
                &from,
                Arc::new(px_grid(2, 2)),
                readshot_core::document::Join::Below,
            )
            .unwrap();
        assert!(
            Arc::ptr_eq(&step.doc.geometry.parts[0].image, &s.shared_base()),
            "the bake is the composed base"
        );
        assert_eq!(s.base_composes(), composes);
        let mut sync = EditorState::new(px_grid(3, 2));
        sync.rotate(true).unwrap();
        sync.add_part(px_grid(2, 2), readshot_core::document::Join::Below)
            .unwrap();
        let (a, b) = (&step.doc.geometry, sync.geometry());
        assert_eq!(a.baked_from, b.baked_from);
        assert_eq!(a.parts.len(), b.parts.len());
        for (p, q) in a.parts.iter().zip(&b.parts) {
            assert_eq!(p.id, q.id);
            assert!(*p.image == *q.image, "the same pixels");
        }
        assert_eq!(step.annotations, sync.annotations());
    }

    #[test]
    fn a_new_edit_drops_the_cached_base_of_the_undone_state() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.rotate(true).unwrap();
        let rotated = s.geometry().clone();
        assert!(s.undo());
        assert!(s.cached_base(&rotated).is_some(), "one redo away");
        // An edit drops the redo stack: its base goes with it.
        s.commit_annotation(block(0.0, 0.0));
        assert!(!s.can_redo());
        assert!(s.cached_base(&rotated).is_none());
        assert!(s.bases.is_empty());
    }

    #[test]
    fn the_planned_undo_and_redo_follow_queued_moves() {
        let mut s = EditorState::new(px_grid(3, 2));
        s.commit_annotation(block(0.0, 0.0));
        assert_eq!(s.planned_undo_redo(&[]), Some((true, false)));
        assert_eq!(s.planned_undo_redo(&[DocOp::Undo]), Some((false, true)));
        assert_eq!(s.planned_undo_redo(&[DocOp::Undo, DocOp::Undo]), None);
    }
}
