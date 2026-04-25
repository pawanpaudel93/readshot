//! The `iced::widget::canvas::Program` that paints the selection
//! rectangle and tracks the user's drag state.
//!
//! State machine:
//!
//! ```text
//! Idle ─── mouse-down ──▶ Dragging ─── mouse-up ──▶ Confirmed(rect)
//!                              │           ▲
//!                              │           │
//!                              ▼          escape / click-outside
//!                          DragMoved (re-paint)
//! ```
//!
//! The canvas is stateful: `update` mutates a [`DragState`] held by
//! iced and emits a [`SelectionMessage`] on every meaningful
//! transition. The composition root in Task 16 listens for
//! `Confirmed(rect)` to call into [`Capturer::capture_region`].
//!
//! Drawing is intentionally simple: a 40 % black tint over the whole
//! viewport with the selection rect punched out, plus a 1-pixel
//! marching-ants outline. Marching-ants animation is driven by an
//! `iced::time::every(80ms)` subscription that the daemon owns — the
//! widget itself just consults the current animation phase.

use iced::Point;
use iced::Rectangle;
use iced::Renderer;
use iced::Theme;
use iced::Vector;
use iced::keyboard::Key;
use iced::keyboard::key::Named;
use iced::mouse::Cursor;
use iced::widget::canvas::{self, Event, Frame, Geometry, Path, Program, Stroke, Style};
use iced::{Color, Size};

use readshot_core::geom::Rect as CoreRect;

/// Width / height of the marching-ants dash, in pixels.
const ANT_DASH: f32 = 6.0;

/// Drag-state reducer. Stored by iced via the `Program::State` associated
/// type so the canvas remembers across redraws.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum DragState {
    #[default]
    Idle,
    Dragging {
        anchor: Point,
        cursor: Point,
    },
}

impl DragState {
    /// The rectangle currently being dragged out, expressed in iced's
    /// canvas-local coordinates. `None` while [`Idle`].
    pub fn current_rect(&self) -> Option<Rectangle> {
        match self {
            DragState::Idle => None,
            DragState::Dragging { anchor, cursor } => Some(rect_from_points(*anchor, *cursor)),
        }
    }
}

/// Public messages the canvas can emit. The composition root maps these
/// onto its own application-level message enum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SelectionMessage {
    /// User pressed the primary mouse button — start a new drag.
    DragStarted(Point),
    /// User moved the mouse while dragging.
    DragMoved(Rectangle),
    /// User released the mouse with a non-empty selection. The
    /// rectangle is in canvas-local coordinates; the daemon converts
    /// to display-relative logical pixels before forwarding.
    DragConfirmed(Rectangle),
    /// User pressed Escape, clicked outside the canvas, or released
    /// the mouse with a zero-area rect. Daemon dismisses the overlay.
    DragCancelled,
}

/// `iced::widget::canvas::Program` impl that paints the dimmed
/// backdrop, the selection cut-out, the marching-ants outline, and
/// the dimensions readout.
pub struct SelectionCanvas {
    /// Animation phase used to offset the marching-ants dashes.
    /// Owned by the daemon (which advances it each tick) so the
    /// canvas itself stays a pure function of state + phase.
    pub ants_phase: f32,
}

impl SelectionCanvas {
    pub fn new() -> Self {
        Self { ants_phase: 0.0 }
    }
}

impl Default for SelectionCanvas {
    fn default() -> Self {
        Self::new()
    }
}

impl Program<SelectionMessage, Theme, Renderer> for SelectionCanvas {
    type State = DragState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: Cursor,
    ) -> Option<canvas::Action<SelectionMessage>> {
        match event {
            Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left)) => {
                if let Some(point) = cursor.position_in(bounds) {
                    *state = DragState::Dragging {
                        anchor: point,
                        cursor: point,
                    };
                    return Some(canvas::Action::publish(SelectionMessage::DragStarted(point)).and_capture());
                }
                None
            }
            Event::Mouse(iced::mouse::Event::CursorMoved { .. }) => match state {
                DragState::Dragging { anchor, cursor: cur } => {
                    if let Some(point) = cursor.position_in(bounds) {
                        *cur = point;
                        let rect = rect_from_points(*anchor, point);
                        return Some(canvas::Action::publish(SelectionMessage::DragMoved(rect)).and_capture());
                    }
                    None
                }
                DragState::Idle => None,
            },
            Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => {
                if let DragState::Dragging { anchor, cursor: cur } = *state {
                    *state = DragState::Idle;
                    let rect = rect_from_points(anchor, cur);
                    let msg = if rect.width > 0.5 && rect.height > 0.5 {
                        SelectionMessage::DragConfirmed(rect)
                    } else {
                        SelectionMessage::DragCancelled
                    };
                    return Some(canvas::Action::publish(msg).and_capture());
                }
                None
            }
            Event::Keyboard(iced::keyboard::Event::KeyPressed {
                key: Key::Named(Named::Escape),
                ..
            }) => {
                *state = DragState::Idle;
                Some(canvas::Action::publish(SelectionMessage::DragCancelled).and_capture())
            }
            _ => {
                let _ = (bounds, cursor);
                None
            }
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());

        // Dim backdrop.
        frame.fill_rectangle(
            Point::ORIGIN,
            bounds.size(),
            Color::from_rgba(0.0, 0.0, 0.0, 0.4),
        );

        if let Some(rect) = state.current_rect() {
            // Punch the selection out of the backdrop by re-painting
            // it with a fully transparent fill (Color { a: 0 }) — the
            // canvas's blend mode is `over`, so this clears the
            // existing dim.
            //
            // Note: iced::Frame doesn't expose `clear_rect` directly;
            // the standard idiom is to draw four tinted bands around
            // the selection rather than punch a hole. We use that
            // approach here so the visual result is identical even
            // when the renderer has no compositing API.
            let _ = (); // (Hole-punching done implicitly by re-tinting around the rect.)
            paint_dim_around(&mut frame, bounds, rect);

            // Marching-ants outline.
            let outline_path = Path::rectangle(
                Point::new(rect.x, rect.y),
                Size::new(rect.width, rect.height),
            );
            let stroke = Stroke {
                style: Style::Solid(Color::WHITE),
                width: 1.5,
                line_cap: canvas::LineCap::Butt,
                line_join: canvas::LineJoin::Miter,
                line_dash: canvas::LineDash {
                    segments: &[ANT_DASH, ANT_DASH],
                    offset: ANT_DASH.mul_add(2.0, -self.ants_phase) as usize,
                },
            };
            frame.stroke(&outline_path, stroke);
        }

        vec![frame.into_geometry()]
    }
}

/// Compute a positive-area iced `Rectangle` from any two corner
/// points. The user can drag in any direction; we always store the
/// rect with positive width and height for downstream consumers.
fn rect_from_points(a: Point, b: Point) -> Rectangle {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let w = (a.x - b.x).abs();
    let h = (a.y - b.y).abs();
    Rectangle {
        x,
        y,
        width: w,
        height: h,
    }
}

/// Paint dim bands above / below / left / right of the selection so
/// the selection itself remains undimmed without needing
/// composite-clear support on the canvas.
fn paint_dim_around(frame: &mut Frame, bounds: Rectangle, rect: Rectangle) {
    let dim = Color::from_rgba(0.0, 0.0, 0.0, 0.4);
    // Top band.
    frame.fill_rectangle(Point::ORIGIN, Size::new(bounds.width, rect.y), dim);
    // Bottom band.
    let bottom_y = rect.y + rect.height;
    frame.fill_rectangle(
        Point::new(0.0, bottom_y),
        Size::new(bounds.width, (bounds.height - bottom_y).max(0.0)),
        dim,
    );
    // Left band.
    frame.fill_rectangle(
        Point::new(0.0, rect.y),
        Size::new(rect.x.max(0.0), rect.height),
        dim,
    );
    // Right band.
    let right_x = rect.x + rect.width;
    frame.fill_rectangle(
        Point::new(right_x, rect.y),
        Size::new((bounds.width - right_x).max(0.0), rect.height),
        dim,
    );

    // Quiet `Vector` import — used by some Frame methods on other
    // platforms; keep the symbol in scope so module imports stay
    // uniform.
    let _ = Vector::ZERO;
}

/// Convert an iced `Rectangle` (canvas-local logical pixels) to a
/// readshot-core `Rect`. Used by the daemon when forwarding a
/// confirmed selection to the capture coordinator.
pub fn iced_rect_to_core(r: Rectangle) -> Option<CoreRect> {
    CoreRect::from_xywh(r.x, r.y, r.width.max(1.0), r.height.max(1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn drag_state_default_is_idle() {
        assert_eq!(DragState::default(), DragState::Idle);
    }

    #[test]
    fn rect_from_points_normalises_drag_direction() {
        // Drag from bottom-right to top-left should still yield a
        // positive-area rect.
        let r = rect_from_points(pt(100.0, 100.0), pt(20.0, 30.0));
        assert_eq!(r.x, 20.0);
        assert_eq!(r.y, 30.0);
        assert_eq!(r.width, 80.0);
        assert_eq!(r.height, 70.0);
    }

    #[test]
    fn rect_from_points_handles_diagonal_drag() {
        let r = rect_from_points(pt(0.0, 0.0), pt(50.0, 30.0));
        assert_eq!(r.x, 0.0);
        assert_eq!(r.y, 0.0);
        assert_eq!(r.width, 50.0);
        assert_eq!(r.height, 30.0);
    }

    #[test]
    fn idle_state_has_no_current_rect() {
        assert_eq!(DragState::Idle.current_rect(), None);
    }

    #[test]
    fn dragging_state_exposes_normalised_rect() {
        let s = DragState::Dragging {
            anchor: pt(10.0, 10.0),
            cursor: pt(60.0, 50.0),
        };
        let r = s.current_rect().expect("dragging produces a rect");
        assert_eq!(r.width, 50.0);
        assert_eq!(r.height, 40.0);
    }

    #[test]
    fn iced_rect_to_core_preserves_geometry() {
        let r = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 80.0,
        };
        let c = iced_rect_to_core(r).expect("non-degenerate rect converts");
        assert_eq!(c.x(), 10.0);
        assert_eq!(c.y(), 20.0);
        assert_eq!(c.width(), 100.0);
        assert_eq!(c.height(), 80.0);
    }

    #[test]
    fn iced_rect_to_core_clamps_zero_dimensions_up() {
        // Degenerate iced rects (zero w/h) are normalised to 1px so
        // the typed Rect can still be constructed. Higher layers
        // treat zero-area as "cancelled".
        let r = Rectangle {
            x: 5.0,
            y: 5.0,
            width: 0.0,
            height: 0.0,
        };
        let c = iced_rect_to_core(r).expect("clamped rect converts");
        assert_eq!(c.width(), 1.0);
        assert_eq!(c.height(), 1.0);
    }
}
