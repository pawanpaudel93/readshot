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

use readshot_core::{Annotation, PointLike, RectLike, Rgba as CoreRgba};

use super::state::{ResizeHandle, HANDLE_HIT_RADIUS};
use super::tool_state::ToolState;

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
    },
    /// Polyline accumulator — Pen, Highlighter. Each `CursorMoved` on
    /// a held button appends a point. The preview renders the entire
    /// list as a continuous stroke.
    Drawing {
        points: Vec<Point>,
        tool_at_press: ToolState,
    },
    /// Select-tool drag in base-image coordinates. The runtime owns
    /// hit testing and live move preview; the canvas only streams
    /// points that have already been transformed out of canvas space.
    Selecting { last: PointLike },
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
    /// Editable handle centers in base-image coordinates. Rectangular
    /// annotations expose eight handles; line / arrow expose the two
    /// endpoints.
    pub selected_handles: Vec<(ResizeHandle, PointLike)>,
    /// Optional live preview for the selected annotation while Select
    /// move / resize is in progress. The app can keep the flattened
    /// background texture stable and ask the canvas to draw only this
    /// transient overlay.
    pub selected_preview: Option<Annotation>,
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
                let pressed_selected_handle = self.selected_handle_at(base).is_some();
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
                    };
                    return Some(canvas::Action::publish(CanvasMessage::DragStarted).and_capture());
                }
                // Default: rect / segment drag.
                *state = DrawState::Dragging {
                    anchor: point,
                    cursor: point,
                    tool_at_press: self.active_tool,
                };
                Some(canvas::Action::publish(CanvasMessage::DragStarted).and_capture())
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { .. }) => match state {
                DrawState::Dragging {
                    anchor,
                    cursor: cur,
                    ..
                } => {
                    if let Some(point) = cursor.position_in(bounds) {
                        *cur = point;
                        let preview_rect = rect_from_points(*anchor, point);
                        return Some(
                            canvas::Action::publish(CanvasMessage::DragMoved(preview_rect))
                                .and_capture(),
                        );
                    }
                    None
                }
                DrawState::Drawing { points, .. } => {
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
                    } => {
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
                    } => {
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
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let preview_scale =
            display_scale_for_bounds(bounds, self.image_size, self.display_scale).unwrap_or(1.0);
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
                cursor: cur,
                tool_at_press,
            } => {
                let rect = rect_from_points(*anchor, *cur);
                if rect.width.abs() < 0.5 || rect.height.abs() < 0.5 {
                    // Don't draw anything for sub-pixel drags; iced's
                    // canvas can't represent them and they'd flash on
                    // the first cursor-moved tick anyway.
                } else {
                    match tool_at_press {
                        ToolState::Rectangle | ToolState::Crop => {
                            let path = Path::rectangle(
                                Point::new(rect.x, rect.y),
                                iced::Size::new(rect.width, rect.height),
                            );
                            if *tool_at_press == ToolState::Crop {
                                shade_crop_outside(&mut frame, bounds, rect);
                            }
                            frame.stroke(&path, preview_stroke);
                        }
                        ToolState::Ellipse => {
                            let center =
                                Point::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
                            let radii = (rect.width * 0.5, rect.height * 0.5);
                            let path = Path::new(|builder| {
                                ellipse_path(builder, center, radii);
                            });
                            frame.stroke(&path, preview_stroke);
                        }
                        ToolState::Line => {
                            let path = Path::line(*anchor, *cur);
                            frame.stroke(&path, preview_stroke);
                        }
                        ToolState::Arrow => {
                            // Shaft + simple arrowhead. The committed
                            // annotation gets a polished arrowhead via
                            // readshot-core::arrowhead; the preview is
                            // a hint, not the final stroke.
                            let path = Path::line(*anchor, *cur);
                            frame.stroke(&path, preview_stroke);
                            let head = arrowhead_path(*anchor, *cur, preview_width.max(2.0));
                            frame.stroke(&head, preview_stroke);
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
                                Color::from_rgba(self.color.r, self.color.g, self.color.b, 0.18),
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
                        draw_preview_badge(&mut frame, bounds, rect, label);
                    }
                }
            }
            DrawState::Drawing {
                points,
                tool_at_press,
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
                &mut frame,
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
                draw_selection_bounds(&mut frame, rect);
            }
            for (_, handle) in &self.selected_handles {
                if let Some(point) = base_point_to_canvas(
                    *handle,
                    bounds,
                    self.image_size,
                    self.image_offset,
                    self.display_scale,
                ) {
                    draw_selection_handle(&mut frame, point);
                }
            }
        }

        vec![frame.into_geometry()]
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
        if let Some(handle) = canvas_to_base(
            point,
            bounds,
            self.image_size,
            self.image_offset,
            self.display_scale,
        )
        .map(|p| PointLike::new(p.x, p.y))
        .and_then(|base| self.selected_handle_at(base))
        {
            return resize_handle_cursor(handle);
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
        ResizeHandle::Start | ResizeHandle::End => iced::mouse::Interaction::Move,
    }
}

impl EditorCanvas {
    fn selected_handle_at(&self, point: PointLike) -> Option<ResizeHandle> {
        self.selected_handles.iter().find_map(|(handle, center)| {
            let dx = point.x - center.x;
            let dy = point.y - center.y;
            (dx.hypot(dy) <= HANDLE_HIT_RADIUS).then_some(*handle)
        })
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
        ToolState::Rectangle | ToolState::Ellipse | ToolState::Line | ToolState::Arrow => "",
        _ => return None,
    };
    let a = canvas_to_image(anchor, bounds, image_size, display_scale).unwrap_or(anchor);
    let b = canvas_to_image(cursor, bounds, image_size, display_scale).unwrap_or(cursor);
    let width = (a.x - b.x).abs().round().max(1.0) as u32;
    let height = (a.y - b.y).abs().round().max(1.0) as u32;
    Some(format!("{label_prefix}{width} × {height}px"))
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
        size: iced::Pixels(11.0),
        font: iced::Font::with_name(readshot_core::render::FONT_FAMILY),
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
        } => {
            if let Some(rect) =
                base_rect_to_canvas(*rect, bounds, image_size, image_offset, display_scale)
            {
                let path = Path::rectangle(
                    Point::new(rect.x, rect.y),
                    iced::Size::new(rect.width, rect.height),
                );
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(core_rgba_to_color(*color))
                        .with_width((*line_width * scale).max(1.0))
                        .with_line_join(LineJoin::Round),
                );
            }
        }
        Annotation::Ellipse {
            rect,
            color,
            line_width,
        } => {
            if let Some(rect) =
                base_rect_to_canvas(*rect, bounds, image_size, image_offset, display_scale)
            {
                let center = Point::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
                let path = Path::new(|builder| {
                    ellipse_path(builder, center, (rect.width * 0.5, rect.height * 0.5));
                });
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(core_rgba_to_color(*color))
                        .with_width((*line_width * scale).max(1.0))
                        .with_line_join(LineJoin::Round),
                );
            }
        }
        Annotation::Line {
            a,
            b,
            color,
            line_width,
        } => draw_preview_polyline(
            frame,
            &[*a, *b],
            *color,
            *line_width,
            bounds,
            image_size,
            image_offset,
            display_scale,
            true,
        ),
        Annotation::Arrow {
            a,
            b,
            color,
            line_width,
        } => {
            let points = [*a, *b];
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
            );
            if let (Some(from), Some(to)) = (
                base_point_to_canvas(*a, bounds, image_size, image_offset, display_scale),
                base_point_to_canvas(*b, bounds, image_size, image_offset, display_scale),
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
        ),
        Annotation::Text {
            content,
            origin,
            color,
            size,
            ..
        } => {
            if let Some(point) =
                base_point_to_canvas(*origin, bounds, image_size, image_offset, display_scale)
            {
                frame.fill_text(CanvasText {
                    content: content.clone(),
                    position: point,
                    color: core_rgba_to_color(*color),
                    size: iced::Pixels((*size * scale).max(8.0)),
                    font: iced::Font::with_name(readshot_core::render::FONT_FAMILY),
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
                frame.fill(&path, Color::from_rgba(0.0, 0.48, 1.0, 0.14));
                frame.stroke(
                    &path,
                    Stroke::default()
                        .with_color(Color::from_rgba(0.0, 0.48, 1.0, 0.72))
                        .with_width(1.5),
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
                    font: iced::Font::with_name(readshot_core::render::FONT_FAMILY),
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
                        .with_color(Color::from_rgba(0.0, 0.48, 1.0, 0.95))
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
    frame.stroke(&path, stroke);
}

fn core_rgba_to_color(color: CoreRgba) -> Color {
    Color::from_rgba(color.r, color.g, color.b, color.a)
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
            .with_color(Color::from_rgba(0.0, 0.48, 1.0, 0.95))
            .with_width(1.5),
    );
}

fn draw_selection_handle(frame: &mut Frame, point: Point) {
    let handle = 6.0;
    let half = handle * 0.5;
    let handle_path = Path::rectangle(
        Point::new(point.x - half, point.y - half),
        iced::Size::new(handle, handle),
    );
    frame.fill(&handle_path, Color::WHITE);
    frame.stroke(
        &handle_path,
        Stroke::default()
            .with_color(Color::from_rgba(0.0, 0.48, 1.0, 1.0))
            .with_width(1.0),
    );
}

/// Translate a single click for a point tool into an annotation.
/// NumberedPin is the only one wired today; Text needs a separate
/// input modality and is handled outside the canvas.
pub fn annotation_for_point(
    tool: ToolState,
    point: Point,
    color: CoreRgba,
    _line_width: f32,
    next_pin_number: u32,
) -> Option<Annotation> {
    match tool {
        ToolState::NumberedPin => Some(Annotation::NumberedPin {
            origin: PointLike::new(point.x, point.y),
            number: next_pin_number,
            color,
        }),
        _ => None,
    }
}

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
) -> Option<Annotation> {
    let p0 = PointLike::new(anchor.x, anchor.y);
    let p1 = PointLike::new(cursor.x, cursor.y);
    let rect = rect_like_from_points(p0, p1);

    match tool {
        ToolState::Rectangle => Some(Annotation::Rectangle {
            rect,
            color,
            line_width,
        }),
        ToolState::Ellipse => Some(Annotation::Ellipse {
            rect,
            color,
            line_width,
        }),
        ToolState::Line => Some(Annotation::Line {
            a: p0,
            b: p1,
            color,
            line_width,
        }),
        ToolState::Arrow => Some(Annotation::Arrow {
            a: p0,
            b: p1,
            color,
            line_width,
        }),
        ToolState::Blur => Some(Annotation::Blur {
            rect,
            radius: line_width.max(2.0),
        }),
        ToolState::Pixelate => Some(Annotation::Pixelate {
            rect,
            block_size: line_width.max(4.0),
        }),
        ToolState::Crop => Some(Annotation::Crop { rect }),
        // Polyline + point + select tools need a richer event model than
        // a two-point drag; deferred to a follow-up.
        ToolState::Pen
        | ToolState::Highlighter
        | ToolState::Text
        | ToolState::NumberedPin
        | ToolState::Select => None,
    }
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
    use super::*;
    use iced::widget::canvas::Program;

    fn pt(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn rectangle_drag_produces_rectangle_annotation() {
        let a = annotation_for_drag(
            ToolState::Rectangle,
            pt(10.0, 20.0),
            pt(60.0, 80.0),
            CoreRgba::OPAQUE_BLACK,
            2.0,
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
        let canvas = EditorCanvas {
            active_tool: ToolState::Rectangle,
            color: CoreRgba::OPAQUE_BLACK,
            line_width: 2.0,
            next_pin_number: 1,
            image_size: (200, 100),
            image_offset: (0.0, 0.0),
            display_scale: Some(2.0),
            selected_bounds: None,
            selected_handles: Vec::new(),
            selected_preview: None,
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
            active_tool: ToolState::Rectangle,
            color: CoreRgba::OPAQUE_BLACK,
            line_width: 2.0,
            next_pin_number: 1,
            image_size: (200, 100),
            image_offset: (0.0, 0.0),
            display_scale: Some(2.0),
            selected_bounds: Some(RectLike::new(10.0, 10.0, 40.0, 30.0)),
            selected_handles: vec![(ResizeHandle::SouthEast, PointLike::new(50.0, 40.0))],
            selected_preview: None,
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
            active_tool: ToolState::Rectangle,
            color: CoreRgba::OPAQUE_BLACK,
            line_width: 2.0,
            next_pin_number: 1,
            image_size: (200, 100),
            image_offset: (0.0, 0.0),
            display_scale: Some(2.0),
            selected_bounds: Some(RectLike::new(10.0, 10.0, 40.0, 30.0)),
            selected_handles: vec![(ResizeHandle::SouthEast, PointLike::new(50.0, 40.0))],
            selected_preview: None,
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
}
