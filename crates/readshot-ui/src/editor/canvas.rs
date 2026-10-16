//! Editor canvas — the iced widget that lets the user *make* an
//! annotation by clicking and dragging on the captured image.
//!
//! The widget translates mouse events into [`CanvasMessage`] variants
//! the runtime maps onto [`super::EditorState`] mutations. Three interaction
//! modes are supported, keyed off the active [`ToolState`]:
//!
//! * **Rect tools** (Rectangle, Ellipse, Blur, Pixelate, Crop) and
//!   **segment tools** (Line, Arrow) — drag from anchor to cursor;
//!   release commits a single annotation.
//! * **Freehand tools** (Pen, Highlighter) — accumulate a polyline as
//!   the cursor moves; release commits a `Pen` or `Highlighter`
//!   annotation with all the recorded points.
//! * **Point tools** (NumberedPin) — single click commits a pin at
//!   the click position. Text is handled separately (it needs a text
//!   input modality the canvas can't host on its own).
//!
//! In addition to event translation, the canvas draws a *live preview*
//! of the in-progress shape using the active tool's colour and line
//! width — without it the user has no idea what they're about to
//! commit.

use iced::widget::canvas::{
    self, Event, Frame, Geometry, LineCap, LineJoin, Path, Stroke, Text as CanvasText,
};
use iced::{mouse::Cursor, Color, Point, Rectangle, Renderer, Theme};

use readshot_core::render::{contrast_ink, BOTH_FILL_ALPHA, FONT_FAMILY};
use readshot_core::{
    line_geom, Annotation, ArrowHeads, FillMode, LineStyle, PointLike, RectLike, Rgba as CoreRgba,
    ShapeStyle, SpotlightShape,
};

use super::state::{
    annotation_bounds, annotation_hit_test, hit_tolerance_base, ResizeHandle,
    HANDLE_HIT_RADIUS_SCREEN,
};
use super::style::ToolStyle;
use super::tool_state::ToolState;

/// Selection / crop chrome hue. Mirrors the app's accent teal
/// (`READSHOT_ACCENT` = rgb8(20, 184, 166)); readshot-ui can't depend on
/// readshot-app, so the value is duplicated here — keep them in sync.
/// Everywhere else in the product "selected" reads teal, so the editor's
/// selection handles, crop frame, and blur/pixelate guides match.
const SELECTION_TEAL: Color = Color::from_rgb8(20, 184, 166);

/// Font size for on-canvas chrome (the live measurement badge). Matches
/// the app's caption step so the badge agrees with the rest of the UI.
const CANVAS_BADGE_TEXT: f32 = 12.0;

/// Canvas state: tracks the kind of in-progress interaction so the
/// `draw` step can paint the preview and the `update` step knows what
/// to commit on button-release.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DrawState {
    #[default]
    Idle,
    /// Two-point drag — Rectangle, Ellipse, Line, Arrow, Blur,
    /// Pixelate, Crop.
    Dragging {
        anchor: Point,
        cursor: Point,
        tool_at_press: ToolState,
        /// The editor's drag-cancel sequence at the moment this drag
        /// started. If the program is later rebuilt with a bumped
        /// sequence (Escape pressed while the drag is live), the next
        /// canvas event drops the drag instead of committing it. The
        /// canvas also handles Escape directly (see `update`); the
        /// sequence covers an Escape that reaches the runtime instead.
        cancel_seq: u64,
    },
    /// Polyline accumulator — Pen, Highlighter. Each `CursorMoved` on
    /// a held button appends a point. The preview renders the entire
    /// list as a continuous stroke.
    Drawing {
        points: Vec<Point>,
        tool_at_press: ToolState,
        /// See [`DrawState::Dragging::cancel_seq`].
        cancel_seq: u64,
    },
    /// Select-tool drag in base-image coordinates. The runtime owns
    /// hit testing and live move preview; the canvas only streams
    /// points that have already been transformed out of canvas space.
    Selecting { last: PointLike },
}

/// Radius of the eyedropper loupe grid in source pixels (9 × 9 cells).
pub const LOUPE_RADIUS: u32 = 4;
const LOUPE_CELL: f32 = 10.0;

/// Pixels around the eyedropper cursor. The runtime samples them from
/// the source image (the canvas has no image access) on each hover.
#[derive(Clone, Debug, PartialEq)]
pub struct Loupe {
    /// Sample centre, base-image pixels.
    pub center: PointLike,
    /// `(2 * LOUPE_RADIUS + 1)²` samples, row-major; `None` off the image.
    pub pixels: Vec<Option<CoreRgba>>,
}

/// Messages the editor canvas publishes upward.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasMessage {
    /// User pressed the mouse button — preview drawing begins.
    DragStarted,
    /// User is dragging a rect / segment tool — the editor may want
    /// to repaint the preview overlay.
    DragMoved(Rectangle),
    /// User is freehand-drawing — the preview is a polyline. Carries
    /// the latest list of points so the editor's redraw loop can
    /// rebuild the dashed stroke.
    PolylineMoved(Vec<PointLike>),
    /// User finished a drag with a non-degenerate rect/segment, or
    /// finished a freehand stroke, or single-clicked a point tool;
    /// caller should append the annotation and snapshot history.
    CommitAnnotation(Annotation),
    /// User clicked while the Text tool was active. The editor
    /// should open a text-input UI anchored at this image-pixel
    /// position; the eventual `Annotation::Text` is built from the
    /// user's typed content.
    RequestText(PointLike),
    /// Select tool pressed on the image. The editor model decides
    /// whether this hits an annotation or clears selection.
    SelectPressed(PointLike),
    /// Select tool is dragging across the image.
    SelectDragged(PointLike),
    /// Select tool released.
    SelectReleased,
    /// Eyedropper mode: the cursor moved over the image (base coords).
    EyedropperHover(PointLike),
    /// Eyedropper mode: the user clicked to pick the color at this point.
    EyedropperPick(PointLike),
    /// User pressed Escape or released a zero-area drag — drop
    /// in-progress preview without committing anything.
    Cancelled,
}

/// `iced::widget::canvas::Program` impl. Holds the styling parameters
/// (colour + line width) the editor wants to use when an annotation
/// gets committed, plus the next pin number for `NumberedPin`, and
/// the size of the underlying base image so cursor-in-canvas can be
/// converted to image-pixel coordinates.
///
/// The canvas widget is laid out at the *container* size, not the
/// image's natural size — it's bigger than the displayed image
/// whenever the editor window doesn't match the image's aspect
/// ratio. We translate cursor positions into image-pixel space at
/// commit time so the renderer paints annotations onto the right
/// pixels regardless of zoom / letterbox.
pub struct EditorCanvas {
    pub active_tool: ToolState,
    pub color: CoreRgba,
    pub line_width: f32,
    /// Number to stamp on the next NumberedPin. The editor session
    /// increments this on commit.
    pub next_pin_number: u32,
    /// Effective displayed-image dimensions (post-crop if any). Used
    /// to compute the letterbox transform from canvas-local coords to
    /// the displayed image's pixel coords.
    pub image_size: (u32, u32),
    /// Origin of the displayed image inside the underlying *base*
    /// image's coordinate frame. Annotations are stored in base
    /// coords because that's what the renderer expects, so we add
    /// this offset before publishing CommitAnnotation. `(0, 0)` when
    /// no Crop annotation is in flight.
    pub image_offset: (f32, f32),
    /// Explicit display scale supplied by the app. `None` means fit
    /// large images down and never upscale small captures.
    pub display_scale: Option<f32>,
    /// Bounds of the currently selected annotation in base-image
    /// coordinates. The canvas paints this as a lightweight selection
    /// outline over the flattened image.
    pub selected_bounds: Option<RectLike>,
    /// The currently selected annotation (base coords), used only to
    /// decide the hover/move cursor. We hit-test against the real
    /// geometry — not just `selected_bounds` — so a thin line/pen shows
    /// the move cursor over its stroke, not its empty bounding box.
    pub selected_annotation: Option<Annotation>,
    /// Editable handle centers in base-image coordinates. Rectangular
    /// annotations expose eight handles; line / arrow expose the two
    /// endpoints.
    pub selected_handles: Vec<(ResizeHandle, PointLike)>,
    /// Optional live preview for the selected annotation while Select
    /// move / resize is in progress. The app can keep the flattened
    /// background texture stable and ask the canvas to draw only this
    /// transient overlay.
    pub selected_preview: Option<Annotation>,
    /// Runtime-tracked Shift state. The editor listens to modifiers
    /// globally (the canvas only handles Escape itself) and passes the
    /// current state in here on each `view()`. When
    /// set, an in-progress shape drag is constrained: Rectangle/Ellipse
    /// snap to a square, Line/Arrow snap to 45° increments.
    pub shift_held: bool,
    /// The editor's drag-cancel sequence. The editor bumps this when
    /// Escape is pressed during a live drag; the canvas compares it
    /// against the value captured at drag start and cancels the drag on
    /// the next event when they differ. The canvas also captures Escape
    /// itself and publishes `Cancelled`; this threaded-in sequence is the
    /// path for an Escape the runtime sees uncaptured.
    pub cancel_seq: u64,
    /// Options the next annotation gets (fill, dashes, curve, text…).
    pub style: ToolStyle,
    /// Eyedropper mode: presses pick a color instead of drawing.
    pub eyedropper: bool,
    /// Loupe to draw near the cursor while the eyedropper is active.
    pub loupe: Option<Loupe>,
    /// Text-panel line under the pointer (base coordinates): outlined.
    pub text_hover: Option<RectLike>,
    /// Text-panel selected lines (base coordinates): tinted.
    pub text_selected: Vec<RectLike>,
    /// Shared tessellated-geometry cache, owned by the editor session.
    /// `draw` reuses the stored geometry unless the canvas size changed
    /// or the session explicitly cleared it (on editor messages). This
    /// keeps the background polling ticks from re-tessellating the
    /// annotations on every idle redraw.
    pub cache: std::rc::Rc<canvas::Cache>,
}

/// Display scale for the editor image. Fit mode scales the image to
/// the viewport in either direction.
fn display_scale_for_bounds(
    bounds: Rectangle,
    image_size: (u32, u32),
    explicit_scale: Option<f32>,
) -> Option<f32> {
    let (iw, ih) = (image_size.0 as f32, image_size.1 as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return None;
    }
    if let Some(scale) = explicit_scale {
        return Some(scale.max(f32::EPSILON));
    }
    Some(
        (bounds.width / iw)
            .min(bounds.height / ih)
            .max(f32::EPSILON),
    )
}

/// Map a canvas-local point to the underlying image's pixel space
/// using the editor display rule.
/// Returns `None` if the click sits in the dead band outside the
/// displayed image.
pub(crate) fn canvas_to_image(
    point: Point,
    bounds: Rectangle,
    image_size: (u32, u32),
    explicit_scale: Option<f32>,
) -> Option<Point> {
    let (iw, ih) = (image_size.0 as f32, image_size.1 as f32);
    let scale = display_scale_for_bounds(bounds, image_size, explicit_scale)?;
    let displayed_w = iw * scale;
    let displayed_h = ih * scale;
    let offset_x = (bounds.width - displayed_w) * 0.5;
    let offset_y = (bounds.height - displayed_h) * 0.5;
    let dx = point.x - offset_x;
    let dy = point.y - offset_y;
    if dx < 0.0 || dy < 0.0 || dx > displayed_w || dy > displayed_h {
        return None;
    }
    Some(Point::new(
        (dx / scale).clamp(0.0, iw),
        (dy / scale).clamp(0.0, ih),
    ))
}

/// Convert a canvas-local point to *base*-image pixel coordinates,
/// accounting for any active Crop annotation. The `image_size` is
/// the displayed (post-crop) size; `image_offset` is the crop's
/// top-left in base coords. Returns `None` when the click sits in
/// the letterbox dead band.
pub(crate) fn canvas_to_base(
    point: Point,
    bounds: Rectangle,
    image_size: (u32, u32),
    image_offset: (f32, f32),
    explicit_scale: Option<f32>,
) -> Option<Point> {
    let local = canvas_to_image(point, bounds, image_size, explicit_scale)?;
    Some(Point::new(
        local.x + image_offset.0,
        local.y + image_offset.1,
    ))
}

fn canvas_to_base_clamped(
    point: Point,
    bounds: Rectangle,
    image_size: (u32, u32),
    image_offset: (f32, f32),
    explicit_scale: Option<f32>,
) -> Option<Point> {
    let (iw, ih) = (image_size.0 as f32, image_size.1 as f32);
    let scale = display_scale_for_bounds(bounds, image_size, explicit_scale)?;
    let displayed_w = iw * scale;
    let displayed_h = ih * scale;
    let offset_x = (bounds.width - displayed_w) * 0.5;
    let offset_y = (bounds.height - displayed_h) * 0.5;
    let dx = (point.x - offset_x).clamp(0.0, displayed_w);
    let dy = (point.y - offset_y).clamp(0.0, displayed_h);
    Some(Point::new(
        (dx / scale).clamp(0.0, iw) + image_offset.0,
        (dy / scale).clamp(0.0, ih) + image_offset.1,
    ))
}

fn base_rect_to_canvas(
    rect: RectLike,
    bounds: Rectangle,
    image_size: (u32, u32),
    image_offset: (f32, f32),
    explicit_scale: Option<f32>,
) -> Option<Rectangle> {
    let scale = display_scale_for_bounds(bounds, image_size, explicit_scale)?;
    let displayed_w = image_size.0 as f32 * scale;
    let displayed_h = image_size.1 as f32 * scale;
    let offset_x = (bounds.width - displayed_w) * 0.5;
    let offset_y = (bounds.height - displayed_h) * 0.5;
    let local_x = rect.x - image_offset.0;
    let local_y = rect.y - image_offset.1;
    Some(Rectangle {
        x: offset_x + local_x * scale,
        y: offset_y + local_y * scale,
        width: rect.width * scale,
        height: rect.height * scale,
    })
}

fn base_point_to_canvas(
    point: PointLike,
    bounds: Rectangle,
    image_size: (u32, u32),
    image_offset: (f32, f32),
    explicit_scale: Option<f32>,
) -> Option<Point> {
    let scale = display_scale_for_bounds(bounds, image_size, explicit_scale)?;
    let displayed_w = image_size.0 as f32 * scale;
    let displayed_h = image_size.1 as f32 * scale;
    let offset_x = (bounds.width - displayed_w) * 0.5;
    let offset_y = (bounds.height - displayed_h) * 0.5;
    Some(Point::new(
        offset_x + (point.x - image_offset.0) * scale,
        offset_y + (point.y - image_offset.1) * scale,
    ))
}

impl canvas::Program<CanvasMessage, Theme, Renderer> for EditorCanvas {
    type State = DrawState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<canvas::Action<CanvasMessage>> {
        if self.eyedropper {
            match event {
                Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                    let point = cursor.position_in(bounds)?;
                    let base = canvas_to_base(
                        point,
                        bounds,
                        self.image_size,
                        self.image_offset,
                        self.display_scale,
                    )?;
                    return Some(
                        canvas::Action::publish(CanvasMessage::EyedropperPick(PointLike::new(
                            base.x, base.y,
                        )))
                        .and_capture(),
                    );
                }
                Event::Mouse(iced::mouse::Event::CursorMoved { .. }) => {
                    let base = cursor.position_in(bounds).and_then(|p| {
                        canvas_to_base(
                            p,
                            bounds,
                            self.image_size,
                            self.image_offset,
                            self.display_scale,
                        )
                    })?;
                    return Some(canvas::Action::publish(CanvasMessage::EyedropperHover(
                        PointLike::new(base.x, base.y),
                    )));
                }
                // Escape still falls through to the arm below (Cancelled).
                _ => {}
            }
        }
        match event {
            Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                // Reject clicks in the letterbox dead band — the
                // user clearly didn't mean to annotate empty
                // background.
                let local = canvas_to_image(point, bounds, self.image_size, self.display_scale)?;
                // Translate from displayed-image pixels to the
                // underlying base-image pixels so the renderer's
                // crop-offset translation maps annotations back to
                // where the user clicked.
                let image_point =
                    Point::new(local.x + self.image_offset.0, local.y + self.image_offset.1);
                let base = PointLike::new(image_point.x, image_point.y);
                let scale = display_scale_for_bounds(bounds, self.image_size, self.display_scale)
                    .unwrap_or(1.0);
                let pressed_selected_handle = self.selected_handle_at(base, scale).is_some();
                if self.active_tool == ToolState::Select || pressed_selected_handle {
                    *state = DrawState::Selecting { last: base };
                    return Some(
                        canvas::Action::publish(CanvasMessage::SelectPressed(base)).and_capture(),
                    );
                }
                // Text is a point tool but doesn't commit immediately
                // — the runtime opens a text-input UI in response, and
                // the typed content drives the eventual Annotation::Text.
                if self.active_tool == ToolState::Text {
                    return Some(
                        canvas::Action::publish(CanvasMessage::RequestText(PointLike::new(
                            image_point.x,
                            image_point.y,
                        )))
                        .and_capture(),
                    );
                }
                // Other point tools commit on press, no drag required.
                if self.active_tool.is_point_tool() {
                    if let Some(annotation) = annotation_for_point(
                        self.active_tool,
                        image_point,
                        self.color,
                        self.line_width,
                        self.next_pin_number,
                        &self.style,
                    ) {
                        return Some(
                            canvas::Action::publish(CanvasMessage::CommitAnnotation(annotation))
                                .and_capture(),
                        );
                    }
                }
                // Freehand tools start a polyline. The first point is
                // the press location; cursor moves append more.
                if self.active_tool.is_freehand_tool() {
                    *state = DrawState::Drawing {
                        points: vec![point],
                        tool_at_press: self.active_tool,
                        cancel_seq: self.cancel_seq,
                    };
                    return Some(canvas::Action::publish(CanvasMessage::DragStarted).and_capture());
                }
                // Default: rect / segment drag.
                *state = DrawState::Dragging {
                    anchor: point,
                    cursor: point,
                    tool_at_press: self.active_tool,
                    cancel_seq: self.cancel_seq,
                };
                Some(canvas::Action::publish(CanvasMessage::DragStarted).and_capture())
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { .. }) => match state {
                DrawState::Dragging {
                    anchor,
                    cursor: cur,
                    tool_at_press,
                    cancel_seq,
                } => {
                    if *cancel_seq != self.cancel_seq {
                        *state = DrawState::Idle;
                        return Some(
                            canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                        );
                    }
                    if let Some(point) = cursor.position_in(bounds) {
                        *cur = point;
                        let constrained =
                            constrain_drag(*tool_at_press, *anchor, point, self.shift_held);
                        let preview_rect = rect_from_points(*anchor, constrained);
                        return Some(
                            canvas::Action::publish(CanvasMessage::DragMoved(preview_rect))
                                .and_capture(),
                        );
                    }
                    None
                }
                DrawState::Drawing {
                    points, cancel_seq, ..
                } => {
                    if *cancel_seq != self.cancel_seq {
                        *state = DrawState::Idle;
                        return Some(
                            canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                        );
                    }
                    if let Some(point) = cursor.position_in(bounds) {
                        // Drop near-duplicate points so the polyline
                        // doesn't blow up to thousands of nodes for a
                        // slow drag — the renderer is fine with sparse
                        // polylines.
                        let last = points.last().copied();
                        let should_keep = match last {
                            Some(p) => (p.x - point.x).hypot(p.y - point.y) >= 1.5,
                            None => true,
                        };
                        if should_keep {
                            points.push(point);
                            let snapshot: Vec<PointLike> =
                                points.iter().map(|p| PointLike::new(p.x, p.y)).collect();
                            return Some(
                                canvas::Action::publish(CanvasMessage::PolylineMoved(snapshot))
                                    .and_capture(),
                            );
                        }
                    }
                    None
                }
                DrawState::Selecting { last } => {
                    if let Some(point) = cursor.position_in(bounds).and_then(|p| {
                        canvas_to_base(
                            p,
                            bounds,
                            self.image_size,
                            self.image_offset,
                            self.display_scale,
                        )
                    }) {
                        let base = PointLike::new(point.x, point.y);
                        if (base.x - last.x).hypot(base.y - last.y) >= 0.5 {
                            *last = base;
                            return Some(
                                canvas::Action::publish(CanvasMessage::SelectDragged(base))
                                    .and_capture(),
                            );
                        }
                    }
                    None
                }
                DrawState::Idle => None,
            },
            Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                let prev = std::mem::take(state);
                match prev {
                    DrawState::Dragging {
                        anchor,
                        cursor: cur,
                        tool_at_press,
                        cancel_seq,
                    } => {
                        if cancel_seq != self.cancel_seq {
                            return Some(
                                canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                            );
                        }
                        // Apply the Shift constraint (square / 45°) to the
                        // release point so the committed shape matches the
                        // constrained preview the user saw.
                        let cur = constrain_drag(tool_at_press, anchor, cur, self.shift_held);
                        let dx = anchor.x - cur.x;
                        let dy = anchor.y - cur.y;
                        let len_sq = dx * dx + dy * dy;
                        if len_sq < 0.5 {
                            return Some(
                                canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                            );
                        }
                        // Convert canvas-local anchor + cursor into
                        // base-image pixels before building the
                        // annotation — the renderer paints in base
                        // coords and translates for the active crop.
                        let anchor_img = canvas_to_base(
                            anchor,
                            bounds,
                            self.image_size,
                            self.image_offset,
                            self.display_scale,
                        )
                        .unwrap_or(anchor);
                        let cur_img = canvas_to_base_clamped(
                            cur,
                            bounds,
                            self.image_size,
                            self.image_offset,
                            self.display_scale,
                        )
                        .unwrap_or(cur);
                        let annotation = annotation_for_drag(
                            tool_at_press,
                            anchor_img,
                            cur_img,
                            self.color,
                            self.line_width,
                            &self.style,
                        );
                        match annotation {
                            Some(a) => Some(
                                canvas::Action::publish(CanvasMessage::CommitAnnotation(a))
                                    .and_capture(),
                            ),
                            None => Some(
                                canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                            ),
                        }
                    }
                    DrawState::Drawing {
                        points,
                        tool_at_press,
                        cancel_seq,
                    } => {
                        if cancel_seq != self.cancel_seq {
                            return Some(
                                canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                            );
                        }
                        if points.len() < 2 {
                            return Some(
                                canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                            );
                        }
                        // Convert each canvas-local polyline node to
                        // base-image pixels.
                        let pts: Vec<PointLike> = points
                            .iter()
                            .map(|p| {
                                let q = canvas_to_base_clamped(
                                    *p,
                                    bounds,
                                    self.image_size,
                                    self.image_offset,
                                    self.display_scale,
                                )
                                .unwrap_or(*p);
                                PointLike::new(q.x, q.y)
                            })
                            .collect();
                        let annotation = match tool_at_press {
                            ToolState::Pen => Some(Annotation::Pen {
                                points: pts,
                                color: self.color,
                                line_width: self.line_width,
                            }),
                            ToolState::Highlighter => Some(Annotation::Highlighter {
                                points: pts,
                                // Highlighter is semi-transparent so the
                                // text underneath still reads.
                                color: CoreRgba::new(
                                    self.color.r,
                                    self.color.g,
                                    self.color.b,
                                    0.45,
                                ),
                                // Highlighter is intentionally fat —
                                // grow the stroke if the slider is at a
                                // small value so it actually highlights.
                                line_width: self.line_width.max(12.0),
                            }),
                            _ => None,
                        };
                        match annotation {
                            Some(a) => Some(
                                canvas::Action::publish(CanvasMessage::CommitAnnotation(a))
                                    .and_capture(),
                            ),
                            None => Some(
                                canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                            ),
                        }
                    }
                    DrawState::Selecting { .. } => {
                        Some(canvas::Action::publish(CanvasMessage::SelectReleased).and_capture())
                    }
                    DrawState::Idle => None,
                }
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            }) => {
                *state = DrawState::Idle;
                Some(canvas::Action::publish(CanvasMessage::Cancelled).and_capture())
            }
            _ => None,
        }
    }

    /// Paints a *live preview* of the in-progress shape so the user
    /// sees what they're about to commit. Committed annotations are
    /// rendered into the flattened base via `readshot-core::render`
    /// and shown by a sibling `iced::widget::image`; the canvas only
    /// draws the transient drag.
    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Vec<Geometry> {
        // Cache the tessellated geometry. iced redraws every window on
        // each background tick (hotkey/tray/url drains); without this
        // the annotation set would re-tessellate ~20×/sec while idle.
        // The cache auto-invalidates on size change; the editor session
        // clears it on any editor message, so live drags still update.
        let geometry = self.cache.draw(renderer, bounds.size(), |frame| {
            let preview_scale =
                display_scale_for_bounds(bounds, self.image_size, self.display_scale)
                    .unwrap_or(1.0);
            let preview_width = preview_stroke_width(self.line_width, preview_scale);
            let stroke_color = Color::from_rgba(self.color.r, self.color.g, self.color.b, 1.0);
            let preview_stroke = Stroke::default()
                .with_color(stroke_color)
                .with_width(preview_width)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round);

            match state {
                DrawState::Idle => {}
                DrawState::Selecting { .. } => {}
                DrawState::Dragging {
                    anchor,
                    cursor: raw_cur,
                    tool_at_press,
                    ..
                } => {
                    // Draw the preview at the Shift-constrained cursor so the
                    // hint matches the shape that will be committed.
                    let cur = &constrain_drag(*tool_at_press, *anchor, *raw_cur, self.shift_held);
                    let rect = rect_from_points(*anchor, *cur);
                    if rect.width.abs() < 0.5 || rect.height.abs() < 0.5 {
                        // Don't draw anything for sub-pixel drags; iced's
                        // canvas can't represent them and they'd flash on
                        // the first cursor-moved tick anyway.
                    } else {
                        match tool_at_press {
                            ToolState::Crop => {
                                let path = Path::rectangle(
                                    Point::new(rect.x, rect.y),
                                    iced::Size::new(rect.width, rect.height),
                                );
                                shade_crop_outside(frame, bounds, rect);
                                frame.stroke(&path, preview_stroke);
                            }
                            ToolState::Rectangle
                            | ToolState::Ellipse
                            | ToolState::Line
                            | ToolState::Arrow
                            | ToolState::Spotlight => {
                                let anchor_base = canvas_to_base(
                                    *anchor,
                                    bounds,
                                    self.image_size,
                                    self.image_offset,
                                    self.display_scale,
                                );
                                let cur_base = canvas_to_base_clamped(
                                    *cur,
                                    bounds,
                                    self.image_size,
                                    self.image_offset,
                                    self.display_scale,
                                );
                                if let (Some(a0), Some(c0)) = (anchor_base, cur_base) {
                                    if let Some(preview) = annotation_for_drag(
                                        *tool_at_press,
                                        a0,
                                        c0,
                                        self.color,
                                        self.line_width,
                                        &self.style,
                                    ) {
                                        draw_annotation_preview(
                                            frame,
                                            &preview,
                                            bounds,
                                            self.image_size,
                                            self.image_offset,
                                            self.display_scale,
                                        );
                                    }
                                }
                            }
                            ToolState::Blur | ToolState::Pixelate => {
                                // Visualise the area that will be blurred /
                                // pixelated as a translucent rectangle so
                                // the user sees what they'd hide.
                                let path = Path::rectangle(
                                    Point::new(rect.x, rect.y),
                                    iced::Size::new(rect.width, rect.height),
                                );
                                frame.fill(
                                    &path,
                                    Color::from_rgba(
                                        self.color.r,
                                        self.color.g,
                                        self.color.b,
                                        0.18,
                                    ),
                                );
                                frame.stroke(&path, preview_stroke);
                            }
                            _ => {}
                        }
                        if let Some(label) = preview_drag_label(
                            *tool_at_press,
                            *anchor,
                            *cur,
                            bounds,
                            self.image_size,
                            self.display_scale,
                        ) {
                            draw_preview_badge(frame, bounds, rect, label);
                        }
                    }
                }
                DrawState::Drawing {
                    points,
                    tool_at_press,
                    ..
                } => {
                    if points.len() >= 2 {
                        let path = Path::new(|builder| {
                            builder.move_to(points[0]);
                            for p in points.iter().skip(1) {
                                builder.line_to(*p);
                            }
                        });
                        let stroke = match tool_at_press {
                            ToolState::Highlighter => {
                                // Wider, semi-transparent so it reads as a
                                // highlight even at preview time.
                                Stroke::default()
                                    .with_color(Color::from_rgba(
                                        self.color.r,
                                        self.color.g,
                                        self.color.b,
                                        0.45,
                                    ))
                                    .with_width(highlighter_preview_stroke_width(
                                        self.line_width,
                                        preview_scale,
                                    ))
                                    .with_line_cap(LineCap::Round)
                                    .with_line_join(LineJoin::Round)
                            }
                            _ => preview_stroke,
                        };
                        frame.stroke(&path, stroke);
                    }
                }
            }

            if let Some(annotation) = &self.selected_preview {
                draw_annotation_preview(
                    frame,
                    annotation,
                    bounds,
                    self.image_size,
                    self.image_offset,
                    self.display_scale,
                );
            }

            if self.selected_preview.is_none() {
                if let Some(rect) = self.selected_bounds.and_then(|r| {
                    base_rect_to_canvas(
                        r,
                        bounds,
                        self.image_size,
                        self.image_offset,
                        self.display_scale,
                    )
                }) {
                    draw_selection_bounds(frame, rect);
                }
                for (_, handle) in &self.selected_handles {
                    if let Some(point) = base_point_to_canvas(
                        *handle,
                        bounds,
                        self.image_size,
                        self.image_offset,
                        self.display_scale,
                    ) {
                        draw_selection_handle(frame, point);
                    }
                }
            }
        });

        // The loupe is drawn outside the cache: it must vanish as soon
        // as the cursor leaves the image, which changes the mouse
        // interaction (and so redraws) without any editor message.
        let mut layers = vec![geometry];
        // Text-panel line marks (spec §3.7) live in their own uncached
        // layer, like the loupe: hover changes per pointer move and must
        // not force the cached annotation geometry to re-tessellate.
        // Selected lines are tinted, the hovered line outlined on top,
        // all clipped to the visible crop.
        if self.text_hover.is_some() || !self.text_selected.is_empty() {
            let visible = RectLike::new(
                self.image_offset.0,
                self.image_offset.1,
                self.image_size.0 as f32,
                self.image_size.1 as f32,
            );
            let marks = self
                .text_selected
                .iter()
                .map(|r| (*r, false))
                .chain(self.text_hover.map(|r| (r, true)));
            let mut frame = Frame::new(renderer, bounds.size());
            for (r, hovered) in marks {
                let Some(r) = intersect_rect(r, visible) else {
                    continue;
                };
                if let Some(rect) = base_rect_to_canvas(
                    r,
                    bounds,
                    self.image_size,
                    self.image_offset,
                    self.display_scale,
                ) {
                    let path = Path::rectangle(
                        Point::new(rect.x, rect.y),
                        iced::Size::new(rect.width, rect.height),
                    );
                    let (fill, stroke) = text_mark_colors(hovered);
                    frame.fill(&path, fill);
                    if let Some(color) = stroke {
                        frame.stroke(&path, Stroke::default().with_color(color).with_width(2.0));
                    }
                }
            }
            layers.push(frame.into_geometry());
        }
        if let Some(loupe) = self.loupe.as_ref() {
            if self.loupe_visible(bounds, cursor) {
                if let Some(anchor) = base_point_to_canvas(
                    loupe.center,
                    bounds,
                    self.image_size,
                    self.image_offset,
                    self.display_scale,
                ) {
                    let mut frame = Frame::new(renderer, bounds.size());
                    draw_loupe(&mut frame, bounds, anchor, loupe);
                    layers.push(frame.into_geometry());
                }
            }
        }
        layers
    }

    /// Tool-aware cursor — crosshair for shape / line / drag tools,
    /// text I-beam for the Text tool, and the default arrow for
    /// Select. Returned cursors only apply while the canvas has the
    /// pointer; the iced runtime falls back to platform default
    /// otherwise.
    fn mouse_interaction(
        &self,
        _state: &Self::State,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> iced::mouse::Interaction {
        let Some(point) = cursor.position_in(bounds) else {
            return iced::mouse::Interaction::default();
        };
        if canvas_to_image(point, bounds, self.image_size, self.display_scale).is_none() {
            return iced::mouse::Interaction::default();
        }
        if self.eyedropper {
            return iced::mouse::Interaction::Crosshair;
        }
        let base = canvas_to_base(
            point,
            bounds,
            self.image_size,
            self.image_offset,
            self.display_scale,
        )
        .map(|p| PointLike::new(p.x, p.y));

        let scale =
            display_scale_for_bounds(bounds, self.image_size, self.display_scale).unwrap_or(1.0);
        if let Some(base) = base {
            // Resize handles win over the body so corner/edge grabs stay
            // discoverable even where they overlap the selection bounds.
            if let Some(handle) = self.selected_handle_at(base, scale) {
                return resize_handle_cursor(handle);
            }
            // Hovering the selected annotation's body offers a move
            // affordance, mirroring the handle cursors above. Hit-test the
            // real geometry (same rule as `select_at`) so the cursor only
            // promises a move where a press would actually grab it — a thin
            // line shows the move cursor over its stroke, not its bbox.
            if self.active_tool == ToolState::Select
                && self
                    .selected_annotation
                    .as_ref()
                    .is_some_and(|a| annotation_hit_test(a, base, scale))
            {
                return iced::mouse::Interaction::Move;
            }
        }

        match self.active_tool {
            ToolState::Select => iced::mouse::Interaction::default(),
            ToolState::Text => iced::mouse::Interaction::Text,
            _ => iced::mouse::Interaction::Crosshair,
        }
    }
}

fn resize_handle_cursor(handle: ResizeHandle) -> iced::mouse::Interaction {
    match handle {
        ResizeHandle::North | ResizeHandle::South => iced::mouse::Interaction::ResizingVertically,
        ResizeHandle::East | ResizeHandle::West => iced::mouse::Interaction::ResizingHorizontally,
        ResizeHandle::NorthWest | ResizeHandle::SouthEast => {
            iced::mouse::Interaction::ResizingDiagonallyDown
        }
        ResizeHandle::NorthEast | ResizeHandle::SouthWest => {
            iced::mouse::Interaction::ResizingDiagonallyUp
        }
        ResizeHandle::Start | ResizeHandle::End | ResizeHandle::Bend => {
            iced::mouse::Interaction::Move
        }
    }
}

impl EditorCanvas {
    /// Hit-test the selected annotation's handles. `scale` is the
    /// canvas→base display scale so the grab radius is a constant number
    /// of screen pixels at any zoom.
    fn selected_handle_at(&self, point: PointLike, scale: f32) -> Option<ResizeHandle> {
        let radius = hit_tolerance_base(HANDLE_HIT_RADIUS_SCREEN, scale);
        // Nearest handle wins, so Bend stays grabbable on short curved
        // lines where Start/End also fall inside the radius.
        self.selected_handles
            .iter()
            .map(|(handle, center)| (*handle, (point.x - center.x).hypot(point.y - center.y)))
            .filter(|(_, d)| *d <= radius)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(handle, _)| handle)
    }
}

fn preview_stroke_width(line_width: f32, scale: f32) -> f32 {
    (line_width.max(1.0) * scale.max(f32::EPSILON)).max(1.0)
}

fn highlighter_preview_stroke_width(line_width: f32, scale: f32) -> f32 {
    preview_stroke_width(line_width.max(12.0), scale)
}

/// Build an ellipse path approximation using cubic beziers — iced's
/// canvas builder doesn't ship a native ellipse primitive (only
/// circles), but four arcs reconstruct one cleanly enough for a
/// preview.
fn ellipse_path(builder: &mut canvas::path::Builder, center: Point, (rx, ry): (f32, f32)) {
    let kappa = 0.552_284_8_f32;
    let cx = center.x;
    let cy = center.y;
    let ox = rx * kappa;
    let oy = ry * kappa;
    builder.move_to(Point::new(cx - rx, cy));
    builder.bezier_curve_to(
        Point::new(cx - rx, cy - oy),
        Point::new(cx - ox, cy - ry),
        Point::new(cx, cy - ry),
    );
    builder.bezier_curve_to(
        Point::new(cx + ox, cy - ry),
        Point::new(cx + rx, cy - oy),
        Point::new(cx + rx, cy),
    );
    builder.bezier_curve_to(
        Point::new(cx + rx, cy + oy),
        Point::new(cx + ox, cy + ry),
        Point::new(cx, cy + ry),
    );
    builder.bezier_curve_to(
        Point::new(cx - ox, cy + ry),
        Point::new(cx - rx, cy + oy),
        Point::new(cx - rx, cy),
    );
}

/// Two short strokes converging on the arrow tip — a preview-quality
/// arrowhead. The committed `Annotation::Arrow` gets a more polished
/// head from `readshot-core::arrowhead`.
fn arrowhead_path(_from: Point, to: Point, line_width: f32) -> Path {
    Path::new(|builder| {
        let dx = to.x - _from.x;
        let dy = to.y - _from.y;
        let len = (dx * dx + dy * dy).sqrt().max(1.0);
        let ux = dx / len;
        let uy = dy / len;
        // Perpendicular unit vector.
        let px = -uy;
        let py = ux;
        let head_len = (line_width * 4.0).clamp(8.0, 28.0);
        let head_w = head_len * 0.55;
        let base_x = to.x - ux * head_len;
        let base_y = to.y - uy * head_len;
        let left = Point::new(base_x + px * head_w, base_y + py * head_w);
        let right = Point::new(base_x - px * head_w, base_y - py * head_w);
        builder.move_to(left);
        builder.line_to(to);
        builder.line_to(right);
    })
}

fn shade_crop_outside(frame: &mut Frame, bounds: Rectangle, rect: Rectangle) {
    let shade = Color::from_rgba(0.0, 0.0, 0.0, 0.38);
    for r in [
        Rectangle {
            x: 0.0,
            y: 0.0,
            width: bounds.width,
            height: rect.y.max(0.0),
        },
        Rectangle {
            x: 0.0,
            y: rect.y + rect.height,
            width: bounds.width,
            height: (bounds.height - rect.y - rect.height).max(0.0),
        },
        Rectangle {
            x: 0.0,
            y: rect.y.max(0.0),
            width: rect.x.max(0.0),
            height: rect.height,
        },
        Rectangle {
            x: rect.x + rect.width,
            y: rect.y.max(0.0),
            width: (bounds.width - rect.x - rect.width).max(0.0),
            height: rect.height,
        },
    ] {
        if r.width > 0.0 && r.height > 0.0 {
            let path = Path::rectangle(Point::new(r.x, r.y), iced::Size::new(r.width, r.height));
            frame.fill(&path, shade);
        }
    }
}

fn preview_drag_label(
    tool: ToolState,
    anchor: Point,
    cursor: Point,
    bounds: Rectangle,
    image_size: (u32, u32),
    display_scale: Option<f32>,
) -> Option<String> {
    let label_prefix = match tool {
        ToolState::Crop => "Crop ",
        ToolState::Blur => "Blur ",
        ToolState::Pixelate => "Pixelate ",
        ToolState::Spotlight => "Spotlight ",
        ToolState::Rectangle | ToolState::Ellipse | ToolState::Line | ToolState::Arrow => "",
        _ => return None,
    };
    let a = canvas_to_image(anchor, bounds, image_size, display_scale).unwrap_or(anchor);
    let b = canvas_to_image(cursor, bounds, image_size, display_scale).unwrap_or(cursor);
    let width = (a.x - b.x).abs().round().max(1.0) as u32;
    let height = (a.y - b.y).abs().round().max(1.0) as u32;
    Some(format!("{label_prefix}{width} × {height}px"))
}

impl EditorCanvas {
    /// The loupe shows only while the eyedropper is on, the cursor is
    /// over the image (not the letterbox), and the loupe still describes
    /// the pixel under the cursor. A wheel scroll or re-layout without a
    /// mouse move would otherwise leave it showing a pixel a click would
    /// not pick.
    fn loupe_visible(&self, bounds: Rectangle, cursor: Cursor) -> bool {
        if !self.eyedropper {
            return false;
        }
        let Some(base) = cursor.position_in(bounds).and_then(|p| {
            canvas_to_base(
                p,
                bounds,
                self.image_size,
                self.image_offset,
                self.display_scale,
            )
        }) else {
            return false;
        };
        self.loupe.as_ref().is_none_or(|l| {
            l.center.x.floor() == base.x.floor() && l.center.y.floor() == base.y.floor()
        })
    }
}

fn draw_loupe(frame: &mut Frame, bounds: Rectangle, anchor: Point, loupe: &Loupe) {
    let n = (LOUPE_RADIUS * 2 + 1) as usize;
    let side = LOUPE_CELL * n as f32;
    let x = (anchor.x + 18.0).min(bounds.width - side - 2.0).max(2.0);
    let y = (anchor.y + 18.0).min(bounds.height - side - 26.0).max(2.0);
    for (i, px) in loupe.pixels.iter().enumerate().take(n * n) {
        let (cx, cy) = ((i % n) as f32, (i / n) as f32);
        let cell = Path::rectangle(
            Point::new(x + cx * LOUPE_CELL, y + cy * LOUPE_CELL),
            iced::Size::new(LOUPE_CELL, LOUPE_CELL),
        );
        frame.fill(
            &cell,
            px.map(core_rgba_to_color)
                .unwrap_or(Color::from_rgba(0.0, 0.0, 0.0, 0.35)),
        );
    }
    let outline = Path::rectangle(Point::new(x, y), iced::Size::new(side, side));
    frame.stroke(
        &outline,
        Stroke::default().with_color(Color::BLACK).with_width(3.0),
    );
    frame.stroke(
        &outline,
        Stroke::default().with_color(Color::WHITE).with_width(1.5),
    );
    let mid = (n / 2) as f32 * LOUPE_CELL;
    let centre = Path::rectangle(
        Point::new(x + mid, y + mid),
        iced::Size::new(LOUPE_CELL, LOUPE_CELL),
    );
    frame.stroke(
        &centre,
        Stroke::default().with_color(Color::WHITE).with_width(1.5),
    );
    if let Some(Some(c)) = loupe.pixels.get(n * n / 2) {
        draw_preview_badge(
            frame,
            bounds,
            Rectangle {
                x,
                y,
                width: side,
                height: side,
            },
            c.to_hex(),
        );
    }
}

fn draw_preview_badge(frame: &mut Frame, bounds: Rectangle, rect: Rectangle, label: String) {
    let badge_w = 14.0 + label.chars().count() as f32 * 7.0;
    let badge_h = 20.0;
    let pad = 6.0;
    let mut x = rect.x + rect.width - badge_w;
    let mut y = rect.y + rect.height + pad;
    if y + badge_h > bounds.height {
        y = rect.y - badge_h - pad;
    }
    if y < 0.0 {
        y = rect.y + pad;
    }
    x = x.clamp(0.0, (bounds.width - badge_w).max(0.0));

    let path = Path::rectangle(Point::new(x, y), iced::Size::new(badge_w, badge_h));
    frame.fill(&path, Color::from_rgba(0.02, 0.025, 0.035, 0.82));
    frame.stroke(
        &path,
        Stroke::default()
            .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.22))
            .with_width(1.0),
    );
    frame.fill_text(CanvasText {
        content: label,
        position: Point::new(x + 7.0, y + 3.0),
        color: Color::WHITE,
        size: iced::Pixels(CANVAS_BADGE_TEXT),
        font: iced::Font::with_name(FONT_FAMILY),
        ..Default::default()
    });
}

fn draw_annotation_preview(
    frame: &mut Frame,
    annotation: &Annotation,
    bounds: Rectangle,
    image_size: (u32, u32),
    image_offset: (f32, f32),
    display_scale: Option<f32>,
) {
    let Some(scale) = display_scale_for_bounds(bounds, image_size, display_scale) else {
        return;
    };
    match annotation {
        Annotation::Rectangle {
            rect,
            color,
            line_width,
            style,
        } => {
            if let Some(r) =
                base_rect_to_canvas(*rect, bounds, image_size, image_offset, display_scale)
            {
                let path = Path::rounded_rectangle(
                    Point::new(r.x, r.y),
                    iced::Size::new(r.width, r.height),
                    (style.corner_radius * scale).into(),
                );
                paint_shape_preview(frame, &path, *color, *line_width, scale, *style);
            }
        }
        Annotation::Ellipse {
            rect,
            color,
            line_width,
            style,
        } => {
            if let Some(r) =
                base_rect_to_canvas(*rect, bounds, image_size, image_offset, display_scale)
            {
                let center = Point::new(r.x + r.width * 0.5, r.y + r.height * 0.5);
                let path = Path::new(|b| ellipse_path(b, center, (r.width * 0.5, r.height * 0.5)));
                paint_shape_preview(frame, &path, *color, *line_width, scale, *style);
            }
        }
        Annotation::Line {
            a,
            b,
            color,
            line_width,
            style,
        }
        | Annotation::Arrow {
            a,
            b,
            color,
            line_width,
            style,
        } => {
            let points = line_geom::path_points(*a, *b, style.path, *line_width);
            draw_preview_polyline(
                frame,
                &points,
                *color,
                *line_width,
                bounds,
                image_size,
                image_offset,
                display_scale,
                true,
                style.dashed,
            );
            if matches!(annotation, Annotation::Arrow { .. }) {
                let mut heads = vec![line_geom::end_tangent(*a, *b, style.path)];
                if style.heads == ArrowHeads::Both {
                    heads.push(line_geom::start_tangent(*a, *b, style.path));
                }
                for (from, to) in heads {
                    if let (Some(from), Some(to)) = (
                        base_point_to_canvas(from, bounds, image_size, image_offset, display_scale),
                        base_point_to_canvas(to, bounds, image_size, image_offset, display_scale),
                    ) {
                        let head = arrowhead_path(from, to, (*line_width * scale).max(2.0));
                        frame.stroke(
                            &head,
                            Stroke::default()
                                .with_color(core_rgba_to_color(*color))
                                .with_width((*line_width * scale).max(1.0))
                                .with_line_cap(LineCap::Round)
                                .with_line_join(LineJoin::Round),
                        );
                    }
                }
            }
        }
        Annotation::Pen {
            points,
            color,
            line_width,
        } => draw_preview_polyline(
            frame,
            points,
            *color,
            *line_width,
            bounds,
            image_size,
            image_offset,
            display_scale,
            true,
            false,
        ),
        Annotation::Highlighter {
            points,
            color,
            line_width,
        } => draw_preview_polyline(
            frame,
            points,
            *color,
            *line_width,
            bounds,
            image_size,
            image_offset,
            display_scale,
            false,
            false,
        ),
        Annotation::Text {
            content,
            origin,
            color,
            size,
            style,
            ..
        } => {
            let mut ink = core_rgba_to_color(*color);
            if style.pill {
                if let Some(r) = annotation_bounds(annotation).and_then(|rb| {
                    base_rect_to_canvas(rb, bounds, image_size, image_offset, display_scale)
                }) {
                    let radius = (r.height * 0.5).min(*size * 0.8 * scale);
                    frame.fill(
                        &Path::rounded_rectangle(
                            Point::new(r.x, r.y),
                            iced::Size::new(r.width, r.height),
                            radius.into(),
                        ),
                        ink,
                    );
                    ink = core_rgba_to_color(contrast_ink(*color));
                }
            }
            if let Some(point) =
                base_point_to_canvas(*origin, bounds, image_size, image_offset, display_scale)
            {
                frame.fill_text(CanvasText {
                    content: content.clone(),
                    position: point,
                    color: ink,
                    size: iced::Pixels((*size * scale).max(8.0)),
                    font: iced::Font {
                        weight: if style.bold {
                            iced::font::Weight::Bold
                        } else {
                            iced::font::Weight::Normal
                        },
                        ..iced::Font::with_name(FONT_FAMILY)
                    },
                    ..Default::default()
                });
            }
        }
        Annotation::Blur { rect, .. } | Annotation::Pixelate { rect, .. } => {
            if let Some(rect) =
                base_rect_to_canvas(*rect, bounds, image_size, image_offset, display_scale)
            {
                let path = Path::rectangle(
                    Point::new(rect.x, rect.y),
                    iced::Size::new(rect.width, rect.height),
                );
                frame.fill(
                    &path,
                    Color {
                        a: 0.14,
                        ..SELECTION_TEAL
                    },
                );
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(Color {
                            a: 0.72,
                            ..SELECTION_TEAL
                        })
                        .with_width(1.5),
                );
            }
        }
        Annotation::Spotlight { rect, shape, dim } => {
            if let Some(r) =
                base_rect_to_canvas(*rect, bounds, image_size, image_offset, display_scale)
            {
                let hole = Path::new(|b| {
                    b.rectangle(Point::ORIGIN, bounds.size());
                    match shape {
                        SpotlightShape::Rectangle => {
                            b.rectangle(Point::new(r.x, r.y), iced::Size::new(r.width, r.height))
                        }
                        SpotlightShape::Ellipse => ellipse_path(
                            b,
                            Point::new(r.x + r.width * 0.5, r.y + r.height * 0.5),
                            (r.width * 0.5, r.height * 0.5),
                        ),
                    }
                });
                frame.fill(
                    &hole,
                    canvas::Fill {
                        style: canvas::Style::Solid(Color::from_rgba(0.0, 0.0, 0.0, *dim)),
                        rule: canvas::fill::Rule::EvenOdd,
                    },
                );
            }
        }
        Annotation::NumberedPin {
            origin,
            number,
            color,
        } => {
            if let Some(point) =
                base_point_to_canvas(*origin, bounds, image_size, image_offset, display_scale)
            {
                let radius = 14.0 * scale;
                let path = Path::circle(point, radius);
                frame.fill(&path, core_rgba_to_color(*color));
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(Color::WHITE)
                        .with_width((2.0 * scale).max(1.0)),
                );
                frame.fill_text(CanvasText {
                    content: number.to_string(),
                    position: Point::new(point.x - radius * 0.32, point.y - radius * 0.48),
                    color: Color::WHITE,
                    size: iced::Pixels((12.0 * scale).max(9.0)),
                    font: iced::Font::with_name(FONT_FAMILY),
                    ..Default::default()
                });
            }
        }
        Annotation::Stamp {
            origin,
            stamp,
            color,
        } => {
            if let Some(point) =
                base_point_to_canvas(*origin, bounds, image_size, image_offset, display_scale)
            {
                let radius = readshot_core::stamp::STAMP_RADIUS * scale;
                let symbol = stamp.emoji().is_none();
                if symbol {
                    let disc = Path::circle(point, radius);
                    frame.fill(&disc, core_rgba_to_color(*color));
                    frame.stroke(
                        &disc,
                        Stroke::default()
                            .with_color(Color::WHITE)
                            .with_width((2.0 * scale).max(1.0)),
                    );
                }
                frame.fill_text(CanvasText {
                    content: stamp.glyph().to_string(),
                    position: point,
                    color: Color::WHITE,
                    size: iced::Pixels((radius * if symbol { 1.2 } else { 1.8 }).max(9.0)),
                    align_x: iced::widget::text::Alignment::Center,
                    align_y: iced::alignment::Vertical::Center,
                    shaping: iced::widget::text::Shaping::Advanced,
                    ..Default::default()
                });
            }
        }
        Annotation::Crop { rect } => {
            if let Some(rect) =
                base_rect_to_canvas(*rect, bounds, image_size, image_offset, display_scale)
            {
                shade_crop_outside(frame, bounds, rect);
                let path = Path::rectangle(
                    Point::new(rect.x, rect.y),
                    iced::Size::new(rect.width, rect.height),
                );
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(Color {
                            a: 0.95,
                            ..SELECTION_TEAL
                        })
                        .with_width(1.5),
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_preview_polyline(
    frame: &mut Frame,
    points: &[PointLike],
    color: CoreRgba,
    line_width: f32,
    bounds: Rectangle,
    image_size: (u32, u32),
    image_offset: (f32, f32),
    display_scale: Option<f32>,
    round_caps: bool,
    dashed: bool,
) {
    if points.len() < 2 {
        return;
    }
    let Some(scale) = display_scale_for_bounds(bounds, image_size, display_scale) else {
        return;
    };
    let mapped: Vec<Point> = points
        .iter()
        .filter_map(|p| base_point_to_canvas(*p, bounds, image_size, image_offset, display_scale))
        .collect();
    if mapped.len() < 2 {
        return;
    }
    let path = Path::new(|builder| {
        builder.move_to(mapped[0]);
        for point in mapped.iter().skip(1) {
            builder.line_to(*point);
        }
    });
    let mut stroke = Stroke::default()
        .with_color(core_rgba_to_color(color))
        .with_width((line_width * scale).max(1.0))
        .with_line_join(LineJoin::Round);
    if round_caps {
        stroke = stroke.with_line_cap(LineCap::Round);
    }
    let d = line_geom::dash_pattern(line_width);
    let dash = [d[0] * scale, d[1] * scale];
    if dashed {
        stroke.line_dash = canvas::LineDash {
            segments: &dash,
            offset: 0,
        };
    }
    frame.stroke(&path, stroke);
}

fn paint_shape_preview(
    frame: &mut Frame,
    path: &Path,
    color: CoreRgba,
    line_width: f32,
    scale: f32,
    style: ShapeStyle,
) {
    let c = core_rgba_to_color(color);
    match style.fill {
        FillMode::Outline => {}
        FillMode::Filled => frame.fill(path, c),
        FillMode::Both => frame.fill(
            path,
            Color {
                a: c.a * BOTH_FILL_ALPHA,
                ..c
            },
        ),
    }
    if style.fill == FillMode::Filled {
        return;
    }
    let d = line_geom::dash_pattern(line_width);
    let dash = [d[0] * scale, d[1] * scale];
    let mut stroke = Stroke::default()
        .with_color(c)
        .with_width((line_width * scale).max(1.0))
        .with_line_join(LineJoin::Round);
    if style.dashed {
        stroke.line_dash = canvas::LineDash {
            segments: &dash,
            offset: 0,
        };
    }
    frame.stroke(path, stroke);
}

fn core_rgba_to_color(color: CoreRgba) -> Color {
    Color::from_rgba(color.r, color.g, color.b, color.a)
}

/// Fill and optional outline of a Text-panel mark (mockup `.ocrbox` /
/// `.ocrsel`): the hovered line gets a 12 % teal fill and a 2 px teal
/// outline, selected lines a 16 % teal fill. Teal = selection.
fn text_mark_colors(hovered: bool) -> (Color, Option<Color>) {
    if hovered {
        (
            Color {
                a: 0.12,
                ..SELECTION_TEAL
            },
            Some(SELECTION_TEAL),
        )
    } else {
        (
            Color {
                a: 0.16,
                ..SELECTION_TEAL
            },
            None,
        )
    }
}

/// `a ∩ b` in base coordinates, `None` when they don't overlap. Line
/// marks are clipped to the visible (cropped) image.
fn intersect_rect(a: RectLike, b: RectLike) -> Option<RectLike> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| RectLike::new(x0, y0, x1 - x0, y1 - y0))
}

fn draw_selection_bounds(frame: &mut Frame, rect: Rectangle) {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return;
    }
    let path = Path::rectangle(
        Point::new(rect.x, rect.y),
        iced::Size::new(rect.width, rect.height),
    );
    frame.stroke(
        &path,
        Stroke::default()
            .with_color(Color {
                a: 0.95,
                ..SELECTION_TEAL
            })
            .with_width(1.5),
    );
}

fn draw_selection_handle(frame: &mut Frame, point: Point) {
    // Round handles (spec §3.2): white fill, 1.5 px selection-teal ring.
    let handle_path = Path::circle(point, 5.0);
    frame.fill(&handle_path, Color::WHITE);
    frame.stroke(
        &handle_path,
        Stroke::default()
            .with_color(Color {
                a: 1.0,
                ..SELECTION_TEAL
            })
            .with_width(1.5),
    );
}

/// Translate a single click for a point tool into an annotation.
/// NumberedPin and Stamp are wired here; Text needs a separate
/// input modality and is handled outside the canvas.
pub fn annotation_for_point(
    tool: ToolState,
    point: Point,
    color: CoreRgba,
    _line_width: f32,
    next_pin_number: u32,
    style: &ToolStyle,
) -> Option<Annotation> {
    match tool {
        ToolState::NumberedPin => Some(Annotation::NumberedPin {
            origin: PointLike::new(point.x, point.y),
            number: next_pin_number,
            color,
        }),
        ToolState::Stamp => Some(Annotation::Stamp {
            origin: PointLike::new(point.x, point.y),
            stamp: style.stamp,
            color,
        }),
        _ => None,
    }
}

/// Smallest side, in base-image px, a rect-tool drag must span to commit.
const MIN_RECT_SIDE: f32 = 2.0;

/// Translate a finished drag into the annotation the active tool
/// produces. Returns `None` for tools that don't produce an annotation
/// from a simple two-point drag (Pen, Highlighter, Text, NumberedPin,
/// Select).
pub fn annotation_for_drag(
    tool: ToolState,
    anchor: Point,
    cursor: Point,
    color: CoreRgba,
    line_width: f32,
    style: &ToolStyle,
) -> Option<Annotation> {
    let p0 = PointLike::new(anchor.x, anchor.y);
    let p1 = PointLike::new(cursor.x, cursor.y);
    let rect = rect_like_from_points(p0, p1);
    // Rect tools need a real area: a sliver commits an invisible shape, and
    // a sliver Spotlight dims almost the whole image.
    let sliver = (p0.x - p1.x).abs() < MIN_RECT_SIDE || (p0.y - p1.y).abs() < MIN_RECT_SIDE;
    if sliver
        && matches!(
            tool,
            ToolState::Rectangle
                | ToolState::Ellipse
                | ToolState::Blur
                | ToolState::Pixelate
                | ToolState::Spotlight
        )
    {
        return None;
    }

    match tool {
        ToolState::Rectangle | ToolState::Ellipse => {
            let shape = style.shape_style(tool);
            Some(if tool == ToolState::Rectangle {
                Annotation::Rectangle {
                    rect,
                    color,
                    line_width,
                    style: shape,
                }
            } else {
                Annotation::Ellipse {
                    rect,
                    color,
                    line_width,
                    style: shape,
                }
            })
        }
        ToolState::Line => Some(Annotation::Line {
            a: p0,
            b: p1,
            color,
            line_width,
            style: LineStyle {
                heads: ArrowHeads::End,
                ..style.line_style(p0)
            },
        }),
        ToolState::Arrow => Some(Annotation::Arrow {
            a: p0,
            b: p1,
            color,
            line_width,
            style: style.line_style(p0),
        }),
        ToolState::Blur => Some(Annotation::Blur {
            rect,
            radius: line_width.max(2.0),
        }),
        ToolState::Pixelate => Some(Annotation::Pixelate {
            rect,
            block_size: line_width.max(4.0),
        }),
        ToolState::Spotlight => Some(Annotation::Spotlight {
            rect,
            shape: style.spotlight_shape,
            dim: style.dim,
        }),
        ToolState::Crop => Some(Annotation::Crop { rect }),
        // Polyline + point + select tools need a richer event model than
        // a two-point drag; deferred to a follow-up.
        ToolState::Pen
        | ToolState::Highlighter
        | ToolState::Text
        | ToolState::NumberedPin
        | ToolState::Stamp
        | ToolState::Select => None,
    }
}

/// Apply the "hold Shift to constrain" rule to a shape drag's far point.
/// Rectangle / Ellipse snap to a perfect square (the longer axis wins);
/// Line / Arrow snap the direction to the nearest 45° increment while
/// keeping the drawn length. All other tools pass through unchanged. iced
/// 0.14 doesn't deliver keyboard events to canvas widgets, so `shift` is
/// threaded in from the runtime's global modifier watcher.
fn constrain_drag(tool: ToolState, anchor: Point, cursor: Point, shift: bool) -> Point {
    if !shift {
        return cursor;
    }
    match tool {
        ToolState::Rectangle | ToolState::Ellipse | ToolState::Spotlight => {
            square_target(anchor, cursor)
        }
        ToolState::Line | ToolState::Arrow => snap_to_45(anchor, cursor),
        _ => cursor,
    }
}

/// Snap the far corner so the resulting rect is a perfect square; the
/// longer axis determines the side length, and the sign per axis keeps the
/// square on the same side of the anchor as the raw cursor.
fn square_target(anchor: Point, cursor: Point) -> Point {
    let dx = cursor.x - anchor.x;
    let dy = cursor.y - anchor.y;
    let size = dx.abs().max(dy.abs());
    let sx = if dx >= 0.0 { 1.0 } else { -1.0 };
    let sy = if dy >= 0.0 { 1.0 } else { -1.0 };
    Point::new(anchor.x + sx * size, anchor.y + sy * size)
}

/// Snap the drag direction to the nearest multiple of 45° while preserving
/// the drag length, so lines / arrows lock to horizontal, vertical, or the
/// diagonals.
fn snap_to_45(anchor: Point, cursor: Point) -> Point {
    let dx = cursor.x - anchor.x;
    let dy = cursor.y - anchor.y;
    let len = dx.hypot(dy);
    if len <= f32::EPSILON {
        return cursor;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let snapped = (dy.atan2(dx) / step).round() * step;
    Point::new(
        anchor.x + len * snapped.cos(),
        anchor.y + len * snapped.sin(),
    )
}

fn rect_from_points(a: Point, b: Point) -> Rectangle {
    Rectangle {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        width: (a.x - b.x).abs(),
        height: (a.y - b.y).abs(),
    }
}

fn rect_like_from_points(a: PointLike, b: PointLike) -> RectLike {
    RectLike::new(
        a.x.min(b.x),
        a.y.min(b.y),
        (a.x - b.x).abs().max(1.0),
        (a.y - b.y).abs().max(1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::super::style::ToolStyle;
    use super::*;
    use iced::widget::canvas::Program;

    fn pt(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    fn test_canvas() -> EditorCanvas {
        EditorCanvas {
            active_tool: ToolState::Rectangle,
            color: CoreRgba::OPAQUE_BLACK,
            line_width: 2.0,
            next_pin_number: 1,
            image_size: (200, 100),
            image_offset: (0.0, 0.0),
            display_scale: Some(2.0),
            selected_bounds: None,
            selected_handles: Vec::new(),
            selected_annotation: None,
            selected_preview: None,
            shift_held: false,
            cancel_seq: 0,
            style: ToolStyle::default(),
            eyedropper: false,
            loupe: None,
            text_hover: None,
            text_selected: Vec::new(),
            cache: std::rc::Rc::new(canvas::Cache::default()),
        }
    }

    #[test]
    fn drag_commits_the_tool_style() {
        use readshot_core::{FillMode, ShapeStyle};
        let style = ToolStyle {
            fill: FillMode::Filled,
            dashed: true,
            corner_radius: 8.0,
            ..ToolStyle::default()
        };
        let rect = annotation_for_drag(
            ToolState::Rectangle,
            pt(0.0, 0.0),
            pt(20.0, 20.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
            &style,
        )
        .unwrap();
        assert!(
            matches!(rect, Annotation::Rectangle { style: s, .. } if s == ShapeStyle { fill: FillMode::Filled, dashed: true, corner_radius: 8.0 })
        );
        let ellipse = annotation_for_drag(
            ToolState::Ellipse,
            pt(0.0, 0.0),
            pt(20.0, 20.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
            &style,
        )
        .unwrap();
        assert!(
            matches!(ellipse, Annotation::Ellipse { style: s, .. } if s.corner_radius == 0.0 && s.dashed)
        );
    }

    #[test]
    fn hand_drawn_line_is_seeded_from_its_anchor() {
        use readshot_core::{line_geom::seed_for, LinePath};
        let style = ToolStyle {
            line_path: super::super::style::LinePathKind::HandDrawn,
            heads: readshot_core::ArrowHeads::Both,
            ..ToolStyle::default()
        };
        let line = annotation_for_drag(
            ToolState::Line,
            pt(5.0, 6.0),
            pt(80.0, 30.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
            &style,
        )
        .unwrap();
        match line {
            Annotation::Line { style: s, .. } => {
                assert_eq!(
                    s.path,
                    LinePath::HandDrawn {
                        seed: seed_for(PointLike::new(5.0, 6.0))
                    }
                );
                // Lines have no heads; only arrows keep `Both`.
                assert_eq!(s.heads, readshot_core::ArrowHeads::End);
            }
            other => panic!("{other:?}"),
        }
        let arrow = annotation_for_drag(
            ToolState::Arrow,
            pt(5.0, 6.0),
            pt(80.0, 30.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
            &style,
        )
        .unwrap();
        assert!(
            matches!(arrow, Annotation::Arrow { style: s, .. } if s.heads == readshot_core::ArrowHeads::Both)
        );
    }

    #[test]
    fn bend_handle_shows_the_move_cursor() {
        assert_eq!(
            resize_handle_cursor(ResizeHandle::Bend),
            iced::mouse::Interaction::Move
        );
    }

    #[test]
    fn nearest_selected_handle_wins_on_short_curved_lines() {
        // Start/End/Bend centres all sit within the grab radius of the
        // cursor; the Bend handle is the closest, so it must win.
        let canvas = EditorCanvas {
            selected_handles: vec![
                (ResizeHandle::Start, PointLike::new(10.0, 10.0)),
                (ResizeHandle::End, PointLike::new(14.0, 10.0)),
                (ResizeHandle::Bend, PointLike::new(12.0, 11.0)),
            ],
            ..test_canvas()
        };
        assert_eq!(
            canvas.selected_handle_at(PointLike::new(12.0, 11.0), 2.0),
            Some(ResizeHandle::Bend)
        );
    }

    #[test]
    fn rectangle_drag_produces_rectangle_annotation() {
        let a = annotation_for_drag(
            ToolState::Rectangle,
            pt(10.0, 20.0),
            pt(60.0, 80.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
            &ToolStyle::default(),
        )
        .expect("rectangle tool yields an annotation");
        assert!(matches!(a, Annotation::Rectangle { .. }));
    }

    #[test]
    fn arrow_drag_uses_anchor_and_cursor_directly() {
        let a = annotation_for_drag(
            ToolState::Arrow,
            pt(10.0, 10.0),
            pt(50.0, 30.0),
            CoreRgba::OPAQUE_BLACK,
            3.0,
            &ToolStyle::default(),
        )
        .expect("arrow tool yields an annotation");
        match a {
            Annotation::Arrow { a, b, .. } => {
                assert_eq!(a.x, 10.0);
                assert_eq!(b.x, 50.0);
            }
            other => panic!("expected Arrow, got {other:?}"),
        }
    }

    #[test]
    fn pen_and_text_drags_yield_none() {
        // Pen needs a polyline accumulator; Text needs a separate
        // text-input modality. Both return None from the simple
        // two-point translator.
        for tool in [
            ToolState::Pen,
            ToolState::Highlighter,
            ToolState::Text,
            ToolState::NumberedPin,
            ToolState::Select,
        ] {
            let r = annotation_for_drag(
                tool,
                pt(0.0, 0.0),
                pt(10.0, 10.0),
                CoreRgba::OPAQUE_BLACK,
                1.0,
                &ToolStyle::default(),
            );
            assert!(
                r.is_none(),
                "tool {tool:?} should not produce a drag-annotation"
            );
        }
    }

    #[test]
    fn rect_normalises_inverted_drag() {
        let a = annotation_for_drag(
            ToolState::Rectangle,
            pt(100.0, 100.0),
            pt(20.0, 30.0),
            CoreRgba::OPAQUE_BLACK,
            1.0,
            &ToolStyle::default(),
        )
        .unwrap();
        match a {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.x, 20.0);
                assert_eq!(rect.y, 30.0);
                assert_eq!(rect.width, 80.0);
                assert_eq!(rect.height, 70.0);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn stamp_click_commits_the_selected_stamp() {
        let style = ToolStyle {
            stamp: readshot_core::StampKind::Fire,
            ..ToolStyle::default()
        };
        let a = annotation_for_point(
            ToolState::Stamp,
            pt(10.0, 12.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
            1,
            &style,
        );
        assert!(matches!(
            a,
            Some(Annotation::Stamp { stamp: readshot_core::StampKind::Fire, origin, .. }) if origin == PointLike::new(10.0, 12.0)
        ));
    }

    #[test]
    fn spotlight_drag_commits_shape_and_dim_from_the_style() {
        let style = ToolStyle {
            spotlight_shape: readshot_core::SpotlightShape::Ellipse,
            dim: 0.7,
            ..ToolStyle::default()
        };
        let a = annotation_for_drag(
            ToolState::Spotlight,
            pt(0.0, 0.0),
            pt(40.0, 30.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
            &style,
        )
        .unwrap();
        assert!(
            matches!(a, Annotation::Spotlight { shape: readshot_core::SpotlightShape::Ellipse, dim, .. } if dim == 0.7)
        );
    }

    #[test]
    fn rect_tools_reject_slivers_under_two_pixels() {
        // A 1-px spotlight would dim nearly the whole image; every rect
        // tool drops a drag whose either side is under 2 base px.
        for tool in [
            ToolState::Rectangle,
            ToolState::Ellipse,
            ToolState::Blur,
            ToolState::Pixelate,
            ToolState::Spotlight,
        ] {
            for (a, b) in [
                (pt(10.0, 10.0), pt(10.0, 60.0)),
                (pt(10.0, 10.0), pt(11.5, 60.0)),
                (pt(10.0, 10.0), pt(60.0, 11.0)),
            ] {
                let r = annotation_for_drag(
                    tool,
                    a,
                    b,
                    CoreRgba::OPAQUE_BLACK,
                    2.0,
                    &ToolStyle::default(),
                );
                assert!(r.is_none(), "{tool:?} {a:?}->{b:?} gave {r:?}");
            }
            assert!(annotation_for_drag(
                tool,
                pt(10.0, 10.0),
                pt(12.0, 12.0),
                CoreRgba::OPAQUE_BLACK,
                2.0,
                &ToolStyle::default(),
            )
            .is_some());
        }
        // Lines and arrows keep their own (length) guard.
        for tool in [ToolState::Line, ToolState::Arrow] {
            assert!(annotation_for_drag(
                tool,
                pt(10.0, 10.0),
                pt(10.0, 60.0),
                CoreRgba::OPAQUE_BLACK,
                2.0,
                &ToolStyle::default(),
            )
            .is_some());
        }
    }

    #[test]
    fn preview_drag_label_reports_image_pixel_size() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 200.0,
        };

        assert_eq!(
            preview_drag_label(
                ToolState::Crop,
                pt(20.0, 20.0),
                pt(220.0, 120.0),
                bounds,
                (800, 400),
                None,
            ),
            Some("Crop 400 × 200px".into())
        );
        assert_eq!(
            preview_drag_label(
                ToolState::Text,
                pt(20.0, 20.0),
                pt(220.0, 120.0),
                bounds,
                (800, 400),
                None,
            ),
            None
        );
    }

    #[test]
    fn canvas_mapping_fits_small_images_up_to_bounds() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        // A 200x100 capture should enlarge to 800x400 and center vertically.
        assert_eq!(
            canvas_to_image(pt(0.0, 100.0), bounds, (200, 100), None),
            Some(pt(0.0, 0.0))
        );
        assert_eq!(
            canvas_to_image(pt(800.0, 500.0), bounds, (200, 100), None),
            Some(pt(200.0, 100.0))
        );
        assert_eq!(
            canvas_to_image(pt(400.0, 50.0), bounds, (200, 100), None),
            None
        );
    }

    #[test]
    fn canvas_mapping_still_fits_down_large_images() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };

        // A 1000x400 capture scales down to 500x200 and is vertically centered.
        assert_eq!(
            canvas_to_image(pt(250.0, 200.0), bounds, (1000, 400), None),
            Some(pt(500.0, 200.0))
        );
        assert_eq!(
            canvas_to_image(pt(250.0, 90.0), bounds, (1000, 400), None),
            None
        );
    }

    #[test]
    fn canvas_mapping_uses_explicit_zoom_scale() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        // A 200x100 capture at 200% displays as 400x200 and is centered.
        assert_eq!(
            canvas_to_image(pt(200.0, 200.0), bounds, (200, 100), Some(2.0)),
            Some(pt(0.0, 0.0))
        );
        assert_eq!(
            canvas_to_image(pt(600.0, 400.0), bounds, (200, 100), Some(2.0)),
            Some(pt(200.0, 100.0))
        );
    }

    #[test]
    fn canvas_to_base_clamped_keeps_release_inside_image() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        // The image displays at 800x400 with a 100px top/bottom gutter.
        assert_eq!(
            canvas_to_base_clamped(pt(400.0, 50.0), bounds, (200, 100), (10.0, 20.0), None),
            Some(pt(110.0, 20.0))
        );
        assert_eq!(
            canvas_to_base_clamped(pt(900.0, 650.0), bounds, (200, 100), (10.0, 20.0), None),
            Some(pt(210.0, 120.0))
        );
    }

    #[test]
    fn preview_stroke_width_tracks_display_scale() {
        assert_eq!(preview_stroke_width(4.0, 2.0), 8.0);
        assert_eq!(preview_stroke_width(4.0, 0.5), 2.0);
        assert_eq!(preview_stroke_width(0.2, 0.25), 1.0);
        assert_eq!(highlighter_preview_stroke_width(2.0, 2.0), 24.0);
    }

    #[test]
    fn drawing_cursor_only_applies_over_displayed_image() {
        let canvas = test_canvas();
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(400.0, 300.0))
            ),
            iced::mouse::Interaction::Crosshair
        );
        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(400.0, 120.0))
            ),
            iced::mouse::Interaction::default()
        );
    }

    #[test]
    fn selected_handle_press_routes_to_selection_even_with_drawing_tool_active() {
        let canvas = EditorCanvas {
            selected_bounds: Some(RectLike::new(10.0, 10.0, 40.0, 30.0)),
            selected_handles: vec![(ResizeHandle::SouthEast, PointLike::new(50.0, 40.0))],
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let mut state = DrawState::Idle;

        let action = canvas
            .update(
                &mut state,
                &Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)),
                bounds,
                Cursor::Available(Point::new(300.0, 280.0)),
            )
            .expect("selected handle press should publish selection message");
        let (message, _, _) = action.into_inner();

        assert_eq!(
            state,
            DrawState::Selecting {
                last: PointLike::new(50.0, 40.0)
            }
        );
        assert_eq!(
            message,
            Some(CanvasMessage::SelectPressed(PointLike::new(50.0, 40.0)))
        );
    }

    #[test]
    fn selected_handle_cursor_overrides_drawing_cursor() {
        let canvas = EditorCanvas {
            selected_bounds: Some(RectLike::new(10.0, 10.0, 40.0, 30.0)),
            selected_handles: vec![(ResizeHandle::SouthEast, PointLike::new(50.0, 40.0))],
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(300.0, 280.0))
            ),
            iced::mouse::Interaction::ResizingDiagonallyDown
        );
    }

    #[test]
    fn move_cursor_over_selected_body_with_select_tool() {
        let canvas = EditorCanvas {
            active_tool: ToolState::Select,
            selected_bounds: Some(RectLike::new(10.0, 10.0, 40.0, 30.0)),
            selected_handles: vec![(ResizeHandle::SouthEast, PointLike::new(50.0, 40.0))],
            selected_annotation: Some(Annotation::Rectangle {
                rect: RectLike::new(10.0, 10.0, 40.0, 30.0),
                color: CoreRgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: Default::default(),
            }),
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        // Canvas (260, 250) → base (30, 25), inside the selection body and
        // clear of the handles → move affordance.
        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(260.0, 250.0))
            ),
            iced::mouse::Interaction::Move
        );
        // Canvas (320, 320) → base (60, 60), well outside the rect + the
        // 6px hit tolerance → default arrow.
        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(320.0, 320.0))
            ),
            iced::mouse::Interaction::default()
        );
    }

    #[test]
    fn shift_constrains_rectangle_drag_to_a_square() {
        // Wider-than-tall drag: the longer (x) axis sets the side length.
        let target = constrain_drag(ToolState::Rectangle, pt(0.0, 0.0), pt(100.0, 40.0), true);
        assert_eq!(target, pt(100.0, 100.0));
        // Sign is preserved per-axis so the square follows the cursor's
        // quadrant (here up-and-left).
        let target = constrain_drag(ToolState::Ellipse, pt(50.0, 50.0), pt(10.0, 20.0), true);
        assert_eq!(target, pt(10.0, 10.0));
        // Without Shift the cursor passes straight through.
        assert_eq!(
            constrain_drag(ToolState::Rectangle, pt(0.0, 0.0), pt(100.0, 40.0), false),
            pt(100.0, 40.0)
        );
    }

    #[test]
    fn shift_snaps_line_drag_to_45_degree_increments() {
        // A near-horizontal drag snaps to exactly horizontal, length kept.
        let target = constrain_drag(ToolState::Line, pt(0.0, 0.0), pt(100.0, 10.0), true);
        assert!((target.x - 100.4987).abs() < 0.01, "x = {}", target.x);
        assert!(target.y.abs() < 0.01, "y = {}", target.y);

        // A ~45° drag snaps onto the diagonal.
        let target = constrain_drag(ToolState::Arrow, pt(0.0, 0.0), pt(50.0, 40.0), true);
        let len = (50.0_f32).hypot(40.0);
        let expected = len / std::f32::consts::SQRT_2;
        assert!((target.x - expected).abs() < 0.01, "x = {}", target.x);
        assert!((target.y - expected).abs() < 0.01, "y = {}", target.y);

        // Non-shape tools ignore Shift.
        assert_eq!(
            constrain_drag(ToolState::Blur, pt(0.0, 0.0), pt(100.0, 10.0), true),
            pt(100.0, 10.0)
        );
    }

    #[test]
    fn bumped_cancel_seq_drops_an_in_progress_drag_on_release() {
        // A live drag whose cancel_seq no longer matches the program's
        // (Escape was pressed) must publish Cancelled, not commit.
        let canvas = EditorCanvas {
            cancel_seq: 1,
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        // Drag started under cancel_seq 0; the program now carries 1.
        let mut state = DrawState::Dragging {
            anchor: Point::new(100.0, 120.0),
            cursor: Point::new(300.0, 320.0),
            tool_at_press: ToolState::Rectangle,
            cancel_seq: 0,
        };

        let action = canvas
            .update(
                &mut state,
                &Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )),
                bounds,
                Cursor::Available(Point::new(300.0, 320.0)),
            )
            .expect("release should publish a message");
        let (message, _, _) = action.into_inner();
        assert_eq!(message, Some(CanvasMessage::Cancelled));
    }

    #[test]
    fn move_cursor_uses_stroke_hit_test_not_bbox_for_lines() {
        // A diagonal line: its bounding box has large empty corners that a
        // press would NOT grab. The move cursor must follow the stroke, not
        // the bbox, so it doesn't promise a drag the click won't deliver.
        let canvas = EditorCanvas {
            active_tool: ToolState::Select,
            selected_bounds: Some(RectLike::new(4.0, 4.0, 52.0, 42.0)),
            selected_annotation: Some(Annotation::Line {
                a: PointLike::new(10.0, 10.0),
                b: PointLike::new(50.0, 40.0),
                color: CoreRgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: Default::default(),
            }),
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };

        // Canvas (260, 250) → base (30, 25): on the stroke → move.
        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(260.0, 250.0))
            ),
            iced::mouse::Interaction::Move
        );
        // Canvas (296, 224) → base (48, 12): inside the bbox but far from
        // the stroke → default (the pre-fix bbox test would wrongly show Move).
        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(296.0, 224.0))
            ),
            iced::mouse::Interaction::default()
        );
    }

    #[test]
    fn eyedropper_press_picks_in_base_coordinates_without_drawing() {
        let canvas = EditorCanvas {
            eyedropper: true,
            image_offset: (10.0, 20.0),
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let mut state = DrawState::Idle;
        let action = canvas
            .update(
                &mut state,
                &Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)),
                bounds,
                Cursor::Available(Point::new(300.0, 280.0)),
            )
            .expect("pick");
        let (message, _, _) = action.into_inner();
        // (300, 280) → displayed (100, 80) at 2× → (50, 40) + offset (10, 20).
        assert_eq!(
            message,
            Some(CanvasMessage::EyedropperPick(PointLike::new(60.0, 60.0)))
        );
        assert_eq!(state, DrawState::Idle);
    }

    #[test]
    fn eyedropper_hover_publishes_the_cursor_in_base_coordinates() {
        let canvas = EditorCanvas {
            eyedropper: true,
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let mut state = DrawState::Idle;
        let action = canvas
            .update(
                &mut state,
                &Event::Mouse(iced::mouse::Event::CursorMoved {
                    position: Point::new(300.0, 280.0),
                }),
                bounds,
                Cursor::Available(Point::new(300.0, 280.0)),
            )
            .expect("hover");
        let (message, _, _) = action.into_inner();
        assert_eq!(
            message,
            Some(CanvasMessage::EyedropperHover(PointLike::new(50.0, 40.0)))
        );
    }

    #[test]
    fn eyedropper_ignores_the_letterbox() {
        let canvas = EditorCanvas {
            eyedropper: true,
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let outside = Point::new(50.0, 50.0);
        let mut state = DrawState::Idle;
        for event in [
            Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)),
            Event::Mouse(iced::mouse::Event::CursorMoved { position: outside }),
        ] {
            assert!(canvas
                .update(&mut state, &event, bounds, Cursor::Available(outside))
                .is_none());
        }
        assert_eq!(state, DrawState::Idle);
        assert!(!canvas.loupe_visible(bounds, Cursor::Available(outside)));
        assert!(canvas.loupe_visible(bounds, Cursor::Available(Point::new(300.0, 280.0))));
        let off = EditorCanvas { ..test_canvas() };
        assert!(!off.loupe_visible(bounds, Cursor::Available(Point::new(300.0, 280.0))));
    }

    #[test]
    fn eyedropper_uses_the_crosshair_cursor_even_with_select() {
        let canvas = EditorCanvas {
            eyedropper: true,
            active_tool: ToolState::Select,
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        assert_eq!(
            canvas.mouse_interaction(
                &DrawState::Idle,
                bounds,
                Cursor::Available(Point::new(400.0, 300.0))
            ),
            iced::mouse::Interaction::Crosshair
        );
    }

    #[test]
    fn loupe_hides_when_it_no_longer_matches_the_cursor_pixel() {
        // After a wheel scroll (or any re-layout) without a mouse move,
        // the stored loupe describes another pixel: it must not show.
        let canvas = EditorCanvas {
            eyedropper: true,
            loupe: Some(Loupe {
                center: PointLike::new(50.2, 40.7),
                pixels: Vec::new(),
            }),
            ..test_canvas()
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        // (300, 280) → base (50, 40): same pixel.
        assert!(canvas.loupe_visible(bounds, Cursor::Available(Point::new(301.0, 281.0))));
        // (310, 280) → base (55, 40): stale.
        assert!(!canvas.loupe_visible(bounds, Cursor::Available(Point::new(310.0, 280.0))));
    }

    #[test]
    fn text_marks_use_selection_teal() {
        let (fill, stroke) = text_mark_colors(true);
        assert_eq!(stroke, Some(SELECTION_TEAL));
        assert!((fill.a - 0.12).abs() < 1e-6);
        let (fill, stroke) = text_mark_colors(false);
        assert_eq!(stroke, None);
        assert!((fill.a - 0.16).abs() < 1e-6);
        let c = test_canvas();
        assert!(c.text_hover.is_none() && c.text_selected.is_empty());
    }

    #[test]
    fn marks_are_clipped_to_the_visible_crop() {
        let crop = RectLike::new(10.0, 10.0, 50.0, 30.0);
        assert_eq!(
            intersect_rect(RectLike::new(0.0, 20.0, 30.0, 5.0), crop),
            Some(RectLike::new(10.0, 20.0, 20.0, 5.0))
        );
        assert_eq!(
            intersect_rect(RectLike::new(70.0, 20.0, 5.0, 5.0), crop),
            None
        );
    }
}
