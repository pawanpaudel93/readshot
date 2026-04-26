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
use iced::widget::canvas::{
    Action, Event, Frame, Geometry, LineDash, Path, Program, Stroke, Text as CanvasText,
};
use iced::{Color, Point, Rectangle, Renderer, Theme};

use crate::app::Message;
use readshot_capture::DisplayId;
use readshot_core::geom::Rect;

/// Stateless drawing program. The transient drag state is held in
/// [`OverlayState`] and owned by the canvas widget.
///
/// Each overlay window gets its own [`OverlayProgram`] instance whose
/// `display_id` identifies the monitor the user is dragging on. The
/// canvas only surfaces events when the cursor is in `bounds`, so
/// the resulting [`Message::OverlaySelected`] always carries the
/// display id of the screen the drag actually started on.
///
/// `dash_offset` is bumped by the runtime's `OverlayTick` subscription
/// so the selection border animates as marching ants. `scale` is the
/// display's HiDPI factor — used to render the live size badge in
/// physical pixels (which is what the captured image will be).
#[derive(Debug, Default, Clone)]
pub struct OverlayProgram {
    pub display_id: DisplayId,
    pub dash_offset: usize,
    pub scale: f32,
}

#[derive(Default, Clone, Debug)]
pub struct OverlayState {
    drag_start: Option<Point>,
    drag_current: Option<Point>,
    /// `true` while the Shift key is held. Mouse events in iced
    /// 0.14 don't carry modifier state, so the canvas tracks Shift
    /// itself via `KeyPressed` / `KeyReleased` events. While set
    /// the live preview rect is forced to a perfect square (drag
    /// gets constrained to its longer axis).
    shift_held: bool,
}

impl OverlayState {
    fn rect(&self) -> Option<Rectangle> {
        let a = self.drag_start?;
        let b = self.drag_current?;
        Some(rectangle_from_two_points(
            a,
            constrain_target(a, b, self.shift_held),
        ))
    }
}

/// If `shift` is held, snap the drag's far corner so the resulting
/// rect is a perfect square; the longer axis wins. Otherwise pass
/// the cursor through unchanged.
fn constrain_target(anchor: Point, cursor: Point, shift: bool) -> Point {
    if !shift {
        return cursor;
    }
    let dx = cursor.x - anchor.x;
    let dy = cursor.y - anchor.y;
    let size = dx.abs().max(dy.abs());
    let sx = if dx >= 0.0 { 1.0 } else { -1.0 };
    let sy = if dy >= 0.0 { 1.0 } else { -1.0 };
    Point::new(anchor.x + sx * size, anchor.y + sy * size)
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
                if let (true, Some(p)) = (state.drag_start.is_some(), cursor.position_in(bounds)) {
                    state.drag_current = Some(p);
                    return Some(Action::request_redraw());
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let (Some(start), Some(current)) =
                    (state.drag_start.take(), state.drag_current.take())
                {
                    // Apply the same Shift-snap to the committed
                    // rectangle that the live preview uses, so the
                    // captured PNG matches what the user saw at
                    // the moment they released.
                    let target = constrain_target(start, current, state.shift_held);
                    let rect = rectangle_from_two_points(start, target);
                    // Discard sub-pixel "clicks" — the user almost
                    // certainly didn't mean to capture a 1x1 region.
                    if rect.width >= 4.0 && rect.height >= 4.0 {
                        if let Some(domain) = rect_to_domain(rect) {
                            return Some(
                                Action::publish(Message::OverlaySelected {
                                    display_id: self.display_id.clone(),
                                    rect: domain,
                                })
                                .and_capture(),
                            );
                        }
                    }
                    return Some(Action::publish(Message::OverlayCancelled).and_capture());
                }
            }
            // Track Shift state for the constrain-to-square modifier.
            // Iced delivers KeyPressed events while a modifier key
            // is repeated, so this stays correct even on long holds.
            Event::Keyboard(keyboard::Event::KeyPressed { modifiers, .. })
            | Event::Keyboard(keyboard::Event::KeyReleased { modifiers, .. }) => {
                let shift = modifiers.shift();
                if state.shift_held != shift {
                    state.shift_held = shift;
                    if state.drag_start.is_some() {
                        return Some(Action::request_redraw());
                    }
                }
                // Fall through so other keyboard arms still match.
            }
            _ => {}
        }
        // Second pass for keyboard arms that still need to match
        // after the modifier-tracking branch above.
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(Named::Escape),
                ..
            }) => {
                state.drag_start = None;
                state.drag_current = None;
                return Some(Action::publish(Message::OverlayCancelled).and_capture());
            }
            // Flameshot-style: Enter / Return / Space captures the
            // entire overlay (i.e. the whole display this overlay
            // covers, in display-local logical pixels) without the
            // user needing to drag.
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
                        Action::publish(Message::OverlaySelected {
                            display_id: self.display_id.clone(),
                            rect: domain,
                        })
                        .and_capture(),
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
        // appears brighter than the surrounding veil. Border is a
        // marching-ants dashed stroke; the runtime's `OverlayTick`
        // bumps `dash_offset` so the dashes appear to crawl around
        // the rect.
        if let Some(rect) = state.rect() {
            let path = Path::rectangle(Point::new(rect.x, rect.y), rect.size());
            frame.fill(&path, Color::from_rgba(1.0, 1.0, 1.0, 0.05));
            // Outer black halo so the white dashes read on any
            // background.
            frame.stroke(
                &path,
                Stroke::default()
                    .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.7))
                    .with_width(3.0),
            );
            // Marching ants. `usize` offset advances as the runtime
            // ticks so the pattern visibly crawls.
            const DASH_SEGMENTS: &[f32] = &[6.0, 4.0];
            frame.stroke(
                &path,
                Stroke {
                    line_dash: LineDash {
                        segments: DASH_SEGMENTS,
                        offset: self.dash_offset,
                    },
                    ..Stroke::default().with_color(Color::WHITE).with_width(1.5)
                },
            );

            // Live size badge — render in physical pixels (what the
            // captured PNG will be), placed just outside the bottom-
            // right corner of the selection (or inside if there's no
            // room). A semi-opaque black pill behind the text keeps
            // it legible on any wallpaper.
            let phys_w = ((rect.width * self.scale).round() as i32).max(0);
            let phys_h = ((rect.height * self.scale).round() as i32).max(0);
            let label = format!("{phys_w} × {phys_h}");
            let badge_w = 8.0 + label.chars().count() as f32 * 7.5; // rough text-width estimate
            let badge_h = 18.0;
            let pad_x = 4.0;
            let pad_y = 4.0;
            // Prefer just below-right; fall back to inside the
            // selection's bottom-right when off-screen.
            let mut bx = rect.x + rect.width - badge_w;
            let mut by = rect.y + rect.height + pad_y;
            if by + badge_h > bounds.height {
                by = rect.y + rect.height - badge_h - pad_y;
            }
            if bx < 0.0 {
                bx = rect.x + pad_x;
            }
            let badge_path = Path::rectangle(Point::new(bx, by), iced::Size::new(badge_w, badge_h));
            frame.fill(&badge_path, Color::from_rgba(0.0, 0.0, 0.0, 0.7));
            frame.fill_text(CanvasText {
                content: label,
                position: Point::new(bx + 4.0, by + 2.0),
                color: Color::WHITE,
                size: iced::Pixels(11.0),
                ..Default::default()
            });
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
            shift_held: false,
        };
        let rect = state.rect().unwrap();
        assert_eq!(rect.width, 15.0);
        assert_eq!(rect.height, 20.0);
    }

    #[test]
    fn shift_constrains_drag_to_a_square() {
        let state = OverlayState {
            drag_start: Some(Point::new(0.0, 0.0)),
            drag_current: Some(Point::new(60.0, 20.0)),
            shift_held: true,
        };
        let rect = state.rect().unwrap();
        // Longer axis wins — 60×60 square anchored at (0,0).
        assert_eq!(rect.width, 60.0);
        assert_eq!(rect.height, 60.0);
    }

    #[test]
    fn overlay_state_rect_is_none_when_not_dragging() {
        assert!(OverlayState::default().rect().is_none());
    }
}
