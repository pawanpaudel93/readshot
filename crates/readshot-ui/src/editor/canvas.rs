//! Editor canvas — the iced widget that lets the user *make* an
//! annotation by clicking and dragging on the captured image.
//!
//! The widget translates mouse events into [`CanvasMessage`] variants
//! the runtime maps onto [`EditorState`] mutations. Three interaction
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

use iced::widget::canvas::{self, Event, Frame, Geometry, LineCap, LineJoin, Path, Stroke};
use iced::{mouse::Cursor, Color, Point, Rectangle, Renderer, Theme};

use readshot_core::{Annotation, PointLike, RectLike, Rgba as CoreRgba};

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
}

/// Map a canvas-local point to the underlying image's pixel space
/// using the same `Contain` letterbox the iced image widget uses.
/// Returns `None` if the click sits in the dead band outside the
/// displayed image.
pub(crate) fn canvas_to_image(
    point: Point,
    bounds: Rectangle,
    image_size: (u32, u32),
) -> Option<Point> {
    let (iw, ih) = (image_size.0 as f32, image_size.1 as f32);
    if iw <= 0.0 || ih <= 0.0 {
        return None;
    }
    let scale = (bounds.width / iw)
        .min(bounds.height / ih)
        .max(f32::EPSILON);
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
) -> Option<Point> {
    let local = canvas_to_image(point, bounds, image_size)?;
    Some(Point::new(
        local.x + image_offset.0,
        local.y + image_offset.1,
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
                let local = canvas_to_image(point, bounds, self.image_size)?;
                // Translate from displayed-image pixels to the
                // underlying base-image pixels so the renderer's
                // crop-offset translation maps annotations back to
                // where the user clicked.
                let image_point =
                    Point::new(local.x + self.image_offset.0, local.y + self.image_offset.1);
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
                        let anchor_img =
                            canvas_to_base(anchor, bounds, self.image_size, self.image_offset)
                                .unwrap_or(anchor);
                        let cur_img =
                            canvas_to_base(cur, bounds, self.image_size, self.image_offset)
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
                                let q =
                                    canvas_to_base(*p, bounds, self.image_size, self.image_offset)
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
        let stroke_color = Color::from_rgba(self.color.r, self.color.g, self.color.b, 1.0);
        let preview_stroke = Stroke::default()
            .with_color(stroke_color)
            .with_width(self.line_width.max(1.0))
            .with_line_cap(LineCap::Round)
            .with_line_join(LineJoin::Round);

        match state {
            DrawState::Idle => {}
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
                            let head = arrowhead_path(*anchor, *cur, self.line_width.max(2.0));
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
                                .with_width(self.line_width.max(12.0))
                                .with_line_cap(LineCap::Round)
                                .with_line_join(LineJoin::Round)
                        }
                        _ => preview_stroke,
                    };
                    frame.stroke(&path, stroke);
                }
            }
        }

        vec![frame.into_geometry()]
    }
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
}
