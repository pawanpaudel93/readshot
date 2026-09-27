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

use image::RgbaImage;
use readshot_core::{render, Annotation, PointLike, RectLike, Rgba};

use super::tool_state::ToolState;
use super::undo::History;

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
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextEdit {
    pub index: usize,
    pub origin: PointLike,
    pub content: String,
}

#[derive(Clone, Debug)]
pub struct EditorState {
    base: RgbaImage,
    history: History,
    active_tool: ToolState,
    current_color: Rgba,
    current_line_width: f32,
    selected_annotation: Option<usize>,
    /// Cached flattened image. Invalidated (reset to `None`) whenever the
    /// annotation `history` changes — commit, undo/redo, delete, discard,
    /// nudge, drag previews, and the selected-annotation colour/width/text
    /// edits (which all push a new history entry). Changing the *current*
    /// tool colour (`set_color`) or line width (`set_line_width`) does not
    /// touch it: those only style the next annotation, not the rendered
    /// stack, so the flattened image is unchanged.
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
            selected_annotation: None,
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
            selected_annotation: None,
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

    pub fn render_snapshot(&self) -> (RgbaImage, Vec<Annotation>) {
        (self.base.clone(), self.history.current().to_vec())
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
        let mut annotation = annotation;
        self.clamp_crop_annotation_to_base(&mut annotation, CropClampMode::Resize);
        let mut next = self.history.current().to_vec();
        next.push(annotation);
        self.selected_annotation = next.len().checked_sub(1);
        self.history.push(next);
        self.flattened_cache = None;
    }

    pub fn undo(&mut self) -> bool {
        let changed = self.history.undo().is_some();
        if changed {
            self.clamp_selection();
            self.flattened_cache = None;
        }
        changed
    }

    pub fn redo(&mut self) -> bool {
        let changed = self.history.redo().is_some();
        if changed {
            self.clamp_selection();
            self.flattened_cache = None;
        }
        changed
    }

    /// Discard all annotations and reset undo/redo. Used by the
    /// "Discard" action button before the editor closes.
    pub fn discard(&mut self) {
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
    /// 1:1 display scale. Prefer [`resize_handle_at_scaled`] from the
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
    pub fn select_at_scaled(&mut self, point: PointLike, scale: f32) -> Option<usize> {
        let hit = self
            .history
            .current()
            .iter()
            .enumerate()
            .rev()
            .find_map(|(idx, annotation)| {
                annotation_hit_test(annotation, point, scale).then_some(idx)
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
        self.history.push(next);
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
        self.history.push(next);
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
        self.history.push(next);
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
        self.history.push(next);
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
        self.history.push(next);
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
        let mut next = baseline.to_vec();
        translate_annotation(&mut next[idx], dx, dy);
        self.clamp_crop_annotation_to_base(&mut next[idx], CropClampMode::Move);
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
        let mut next = baseline.to_vec();
        if !resize_annotation(&mut next[idx], handle, dx, dy) {
            return false;
        }
        self.clamp_crop_annotation_to_base(&mut next[idx], CropClampMode::Resize);
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
        let mut next = baseline.to_vec();
        if !set_annotation_width(&mut next[idx], width.clamp(0.5, 64.0)) {
            self.history.replace_present(next);
            self.flattened_cache = None;
            return false;
        }
        self.history.replace_present(next);
        self.flattened_cache = None;
        true
    }

    pub fn commit_preview_from_baseline(&mut self, baseline: Vec<Annotation>) -> bool {
        let final_state = self.history.current().to_vec();
        if final_state == baseline {
            self.history.replace_present(baseline);
            return false;
        }
        self.history.replace_present(baseline);
        self.history.push(final_state);
        self.clamp_selection();
        self.flattened_cache = None;
        true
    }

    pub fn cancel_preview_from_baseline(&mut self, baseline: Vec<Annotation>) {
        self.history.replace_present(baseline);
        self.clamp_selection();
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
    /// [`MIN_ON_IMAGE`] base pixels of it remain grabbable. Crop is
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
            a, b, line_width, ..
        }
        | Annotation::Arrow {
            a, b, line_width, ..
        } => distance_to_segment(point, *a, *b) <= (*line_width * 0.5).max(tolerance),
        Annotation::Pen {
            points, line_width, ..
        }
        | Annotation::Highlighter {
            points, line_width, ..
        } => points.windows(2).any(|pair| {
            distance_to_segment(point, pair[0], pair[1]) <= (*line_width * 0.5).max(tolerance)
        }),
        Annotation::NumberedPin { origin, .. } => distance(point, *origin) <= 16.0,
        _ => {
            annotation_bounds(annotation).is_some_and(|rect| rect_contains(rect, point, tolerance))
        }
    }
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
        Annotation::NumberedPin { .. } => "Pin",
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
        | Annotation::NumberedPin { color: c, .. } => {
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
        Annotation::Blur { .. } | Annotation::Pixelate { .. } | Annotation::Crop { .. } => false,
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
            let next = (width * 4.0 + 8.0).clamp(12.0, 96.0);
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
        Annotation::NumberedPin { .. } | Annotation::Crop { .. } => false,
    }
}

fn annotation_bounds(annotation: &Annotation) -> Option<RectLike> {
    match annotation {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Crop { rect } => Some(*rect),
        Annotation::Line {
            a, b, line_width, ..
        }
        | Annotation::Arrow {
            a, b, line_width, ..
        } => bounds_for_points(&[*a, *b], (*line_width * 0.5).max(6.0)),
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
            ..
        } => {
            // Multi-line text: width is the widest line, height grows
            // one line per `\n`. `origin` is the first line's baseline,
            // so the box starts `size` above it (cap height) and each
            // line adds `size * 1.35` (matches the render line height).
            let line_height = *size * 1.35;
            let mut line_count = 0usize;
            let mut max_chars = 0usize;
            for line in content.split('\n') {
                line_count += 1;
                max_chars = max_chars.max(line.chars().count());
            }
            let line_count = line_count.max(1) as f32;
            let width = (max_chars as f32 * *size * 0.6).max(*size);
            Some(RectLike::new(
                origin.x,
                origin.y - *size,
                width,
                line_height * line_count,
            ))
        }
        Annotation::NumberedPin { origin, .. } => {
            Some(RectLike::new(origin.x - 16.0, origin.y - 16.0, 32.0, 32.0))
        }
    }
}

fn annotation_resize_handle_at(
    annotation: &Annotation,
    point: PointLike,
    scale: f32,
) -> Option<ResizeHandle> {
    let radius = hit_tolerance_base(HANDLE_HIT_RADIUS_SCREEN, scale);
    annotation_resize_handles(annotation)
        .into_iter()
        .find_map(|(handle, p)| (distance(point, p) <= radius).then_some(handle))
}

fn annotation_resize_handles(annotation: &Annotation) -> Vec<(ResizeHandle, PointLike)> {
    match annotation {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Crop { rect } => rect_resize_handles(*rect),
        Annotation::Line { a, b, .. } | Annotation::Arrow { a, b, .. } => {
            vec![(ResizeHandle::Start, *a), (ResizeHandle::End, *b)]
        }
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
        Annotation::Text { origin, .. } | Annotation::NumberedPin { origin, .. } => {
            translate_point(origin, dx, dy);
        }
    }
}

fn resize_annotation(annotation: &mut Annotation, handle: ResizeHandle, dx: f32, dy: f32) -> bool {
    match annotation {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Crop { rect } => {
            if matches!(handle, ResizeHandle::Start | ResizeHandle::End) {
                return false;
            }
            resize_rect(rect, handle, dx, dy);
            true
        }
        Annotation::Line { a, b, .. } | Annotation::Arrow { a, b, .. } => match handle {
            ResizeHandle::Start => {
                translate_point(a, dx, dy);
                true
            }
            ResizeHandle::End => {
                translate_point(b, dx, dy);
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
        ResizeHandle::Start | ResizeHandle::End => return,
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
    fn select_at_returns_topmost_annotation_containing_point() {
        let mut s = EditorState::new(solid_base(64, 64));
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 30.0, 30.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
        });
        s.commit_annotation(Annotation::Ellipse {
            rect: RectLike::new(10.0, 10.0, 30.0, 30.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
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
        });
        s.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(40.0, 40.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
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
}
