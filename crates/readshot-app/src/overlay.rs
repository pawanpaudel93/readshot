//! Region-selection overlay (Phase B, single-display).
//!
//! When the user triggers "Capture region…" (welcome button, tray
//! menu, or global hotkey), the runtime opens one transparent
//! borderless `AlwaysOnTop` window covering the primary display and
//! lets [`OverlayProgram`] render a darkened veil with a click-drag
//! rectangle. On mouse-up the program emits [`Message::OverlaySelected`]
//! with the chosen rect in window-local logical pixels; ESC emits
//! [`Message::OverlayCancelled`]. The runtime then closes the
//! overlay window, runs the capture, and saves the PNG.
//!
//! ## Phase B scope
//!
//! * Single display only (the primary one). Multi-display means
//!   spawning one overlay per monitor and coordinating cancel — not
//!   in this cut.
//! * Selection rect is in *window-local* logical pixels, which on
//!   macOS equal display-local logical pixels because the overlay
//!   window is positioned at the display's origin. The capture path
//!   passes them straight through to `CaptureRequest.rect`.
//! * No edge-snap, no zoom-loupe, no marching-ants animation. The
//!   border is a solid white stroke. Those polishes can land later.

use iced::keyboard::{self, key::Named, Key};
use iced::mouse;
use iced::widget::canvas::{Action, Event, Frame, Geometry, Path, Program, Stroke};
use iced::{Color, Point, Rectangle, Renderer, Theme};

use crate::app::Message;
use readshot_core::geom::Rect;

/// Stateless drawing program. The transient drag state is held in
/// [`OverlayState`] and owned by the canvas widget.
#[derive(Debug, Default, Clone, Copy)]
pub struct OverlayProgram;

#[derive(Default, Clone, Debug)]
pub struct OverlayState {
    drag_start: Option<Point>,
    drag_current: Option<Point>,
}

impl OverlayState {
    fn rect(&self) -> Option<Rectangle> {
        let (a, b) = (self.drag_start?, self.drag_current?);
        Some(rectangle_from_two_points(a, b))
    }
}

impl Program<Message> for OverlayProgram {
    type State = OverlayState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(p) = cursor.position_in(bounds) {
                    state.drag_start = Some(p);
                    state.drag_current = Some(p);
                    return Some(Action::request_redraw().and_capture());
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let (true, Some(p)) =
                    (state.drag_start.is_some(), cursor.position_in(bounds))
                {
                    state.drag_current = Some(p);
                    return Some(Action::request_redraw());
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let (Some(start), Some(current)) =
                    (state.drag_start.take(), state.drag_current.take())
                {
                    let rect = rectangle_from_two_points(start, current);
                    // Discard sub-pixel "clicks" — the user almost
                    // certainly didn't mean to capture a 1x1 region.
                    if rect.width >= 4.0 && rect.height >= 4.0 {
                        if let Some(domain) = rect_to_domain(rect) {
                            return Some(
                                Action::publish(Message::OverlaySelected(domain)).and_capture(),
                            );
                        }
                    }
                    return Some(Action::publish(Message::OverlayCancelled).and_capture());
                }
            }
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::Escape),
                ..
            }) => {
                state.drag_start = None;
                state.drag_current = None;
                return Some(Action::publish(Message::OverlayCancelled).and_capture());
            }
            // Flameshot-style: Enter / Return / Space captures the
            // entire overlay (i.e. the whole primary display in
            // logical pixels) without the user needing to drag.
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::Enter | Named::Space),
                ..
            }) => {
                state.drag_start = None;
                state.drag_current = None;
                if let Some(domain) = rect_to_domain(Rectangle {
                    x: 0.0,
                    y: 0.0,
                    width: bounds.width,
                    height: bounds.height,
                }) {
                    return Some(
                        Action::publish(Message::OverlaySelected(domain)).and_capture(),
                    );
                }
                return Some(Action::publish(Message::OverlayCancelled).and_capture());
            }
            _ => {}
        }
        None
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry<Renderer>> {
        let mut frame = Frame::new(renderer, bounds.size());

        // Dim veil over the entire screen. 50% black so the user can
        // still see what they're selecting through the overlay.
        let full = Path::rectangle(Point::ORIGIN, bounds.size());
        frame.fill(&full, Color::from_rgba(0.0, 0.0, 0.0, 0.5));

        // If we have a drag in progress, "punch out" the selection
        // by overdrawing it with a near-transparent fill so it
        // appears brighter than the surrounding veil.
        if let Some(rect) = state.rect() {
            let path = Path::rectangle(Point::new(rect.x, rect.y), rect.size());
            frame.fill(&path, Color::from_rgba(1.0, 1.0, 1.0, 0.05));
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(Color::WHITE)
                    .with_width(2.0),
            );
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &Self::State,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        mouse::Interaction::Crosshair
    }
}

fn rectangle_from_two_points(a: Point, b: Point) -> Rectangle {
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

fn rect_to_domain(r: Rectangle) -> Option<Rect> {
    Rect::from_xywh(r.x, r.y, r.width, r.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangle_from_two_points_handles_top_left_to_bottom_right() {
        let r = rectangle_from_two_points(Point::new(10.0, 20.0), Point::new(60.0, 100.0));
        assert_eq!(r.x, 10.0);
        assert_eq!(r.y, 20.0);
        assert_eq!(r.width, 50.0);
        assert_eq!(r.height, 80.0);
    }

    #[test]
    fn rectangle_from_two_points_normalises_reversed_drag() {
        let r = rectangle_from_two_points(Point::new(60.0, 100.0), Point::new(10.0, 20.0));
        assert_eq!(r.x, 10.0);
        assert_eq!(r.y, 20.0);
        assert_eq!(r.width, 50.0);
        assert_eq!(r.height, 80.0);
    }

    #[test]
    fn rect_to_domain_rejects_negative_dimensions() {
        // tiny-skia accepts zero-sized rects, so the canvas program's
        // 4×4 minimum-size guard is what blocks accidental clicks.
        // We still verify NaN/negative paths through Rect::from_xywh.
        assert!(rect_to_domain(Rectangle {
            x: 0.0,
            y: 0.0,
            width: -10.0,
            height: 50.0
        })
        .is_none());
    }

    #[test]
    fn overlay_state_rect_combines_drag_endpoints() {
        let state = OverlayState {
            drag_start: Some(Point::new(5.0, 5.0)),
            drag_current: Some(Point::new(20.0, 25.0)),
        };
        let rect = state.rect().unwrap();
        assert_eq!(rect.width, 15.0);
        assert_eq!(rect.height, 20.0);
    }

    #[test]
    fn overlay_state_rect_is_none_when_not_dragging() {
        assert!(OverlayState::default().rect().is_none());
    }
}
