//! Editor canvas — the iced widget that lets the user *make* an
//! annotation by clicking and dragging on the captured image.
//!
//! The widget is a thin event-to-message translator: it reduces a
//! drag into a `CanvasMessage::CommitAnnotation(...)` once the user
//! releases the mouse button. The composition root (Task 16) routes
//! that message into [`EditorState::commit_annotation`]. Drawing the
//! flat image-plus-annotations is done outside this widget — typically
//! by a sibling `image()` widget showing
//! [`EditorState::flatten`]'s output — because reusing
//! `readshot-core`'s renderer is more accurate than redoing the work
//! in iced primitives.
//!
//! The current widget covers the **rect tools** (Rectangle, Ellipse,
//! Blur, Pixelate, Crop) and **segment tools** (Line, Arrow). Pen,
//! Highlighter, Text, and NumberedPin land in a follow-up — they need
//! either a polyline accumulator (pen / highlighter) or a separate
//! input modality (text dialog) that doesn't fit the same drag model.

use iced::widget::canvas::{self, Event, Frame, Geometry};
use iced::{mouse::Cursor, Point, Rectangle, Renderer, Theme};

use readshot_core::{Annotation, PointLike, RectLike, Rgba as CoreRgba};

use super::tool_state::ToolState;

/// Canvas state: which corner the user grabbed, where the cursor is
/// now, and which tool was active when the drag began (so a tool
/// change mid-drag doesn't change semantics).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DrawState {
    #[default]
    Idle,
    Dragging {
        anchor: Point,
        cursor: Point,
        tool_at_press: ToolState,
    },
}

/// Messages the editor canvas publishes upward.
#[derive(Clone, Debug, PartialEq)]
pub enum CanvasMessage {
    /// User pressed the mouse button — preview drawing begins.
    DragStarted,
    /// User is dragging — the editor may want to repaint the
    /// preview overlay.
    DragMoved(Rectangle),
    /// User finished a drag with a non-degenerate rect/segment;
    /// caller should append the annotation and snapshot history.
    CommitAnnotation(Annotation),
    /// User pressed Escape or released a zero-area drag — drop
    /// in-progress preview without committing anything.
    Cancelled,
}

/// `iced::widget::canvas::Program` impl. Holds the styling parameters
/// (colour + line width) the editor wants to use when an annotation
/// gets committed.
pub struct EditorCanvas {
    pub active_tool: ToolState,
    pub color: CoreRgba,
    pub line_width: f32,
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
                if let Some(point) = cursor.position_in(bounds) {
                    *state = DrawState::Dragging {
                        anchor: point,
                        cursor: point,
                        tool_at_press: self.active_tool,
                    };
                    return Some(canvas::Action::publish(CanvasMessage::DragStarted).and_capture());
                }
                None
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { .. }) => {
                if let DrawState::Dragging {
                    anchor,
                    cursor: cur,
                    ..
                } = state
                {
                    if let Some(point) = cursor.position_in(bounds) {
                        *cur = point;
                        let preview_rect = rect_from_points(*anchor, point);
                        return Some(
                            canvas::Action::publish(CanvasMessage::DragMoved(preview_rect))
                                .and_capture(),
                        );
                    }
                }
                None
            }
            Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                if let DrawState::Dragging {
                    anchor,
                    cursor: cur,
                    tool_at_press,
                } = *state
                {
                    *state = DrawState::Idle;
                    let dx = anchor.x - cur.x;
                    let dy = anchor.y - cur.y;
                    let len_sq = dx * dx + dy * dy;
                    if len_sq < 0.5 {
                        return Some(
                            canvas::Action::publish(CanvasMessage::Cancelled).and_capture(),
                        );
                    }

                    let annotation = annotation_for_drag(
                        tool_at_press,
                        anchor,
                        cur,
                        self.color,
                        self.line_width,
                    );
                    return match annotation {
                        Some(a) => Some(
                            canvas::Action::publish(CanvasMessage::CommitAnnotation(a))
                                .and_capture(),
                        ),
                        None => {
                            Some(canvas::Action::publish(CanvasMessage::Cancelled).and_capture())
                        }
                    };
                }
                None
            }
            _ => None,
        }
    }

    /// No-op draw — the editor renders the *flattened* base+annotations
    /// image via a sibling `iced::widget::image` widget powered by
    /// [`crate::editor::EditorState::flatten`]. Reusing
    /// `readshot-core::render` is more accurate than rebuilding the
    /// scene in iced canvas primitives, so this widget is event-only.
    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        // Allocate an empty frame at the bounds size so the widget
        // still occupies its layout slot. No paint commands are
        // issued.
        let frame = Frame::new(renderer, bounds.size());
        vec![frame.into_geometry()]
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
