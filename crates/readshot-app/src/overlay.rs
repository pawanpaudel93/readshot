//! Region-selection overlay (Flameshot-style).
//!
//! When the user triggers "Capture region…" (welcome button, tray
//! menu, or global hotkey), the runtime opens one transparent
//! borderless `AlwaysOnTop` window covering each display and lets
//! [`OverlayProgram`] render a darkened veil with an editable
//! selection rectangle.
//!
//! ## UX flow
//!
//! 1. Empty state — drag with the mouse to define an initial rect
//!    (Shift = constrain to a square).
//! 2. On mouse-up the rect is *committed* into [`OverlayState::selection`]
//!    but **not** confirmed. Eight resize handles (4 corners, 4
//!    edges) appear and the cursor becomes hover-aware.
//! 3. Refine: drag a handle to resize, or drag the body to translate.
//!    Click outside the selection to discard it and start a fresh drag.
//! 4. Press Enter / Space (or with no selection, just Enter) to confirm
//!    — emits [`Message::OverlaySelected`]. Esc emits
//!    [`Message::OverlayCancelled`].
//!
//! Selection rect is in *window-local* logical pixels. On macOS this
//! equals display-local logical pixels because the overlay window is
//! positioned at the display's origin. The capture path passes them
//! straight through to `CaptureRequest.rect`.

use iced::keyboard::{self, key::Named, Key};
use iced::mouse;
use iced::widget::canvas::{
    Action, Event, Frame, Geometry, LineDash, Path, Program, Stroke, Text as CanvasText,
};
use iced::{Color, Point, Rectangle, Renderer, Theme};

use crate::app::{CaptureIntent, Message};
use readshot_capture::DisplayId;
use readshot_core::geom::Rect;

/// Stateless drawing program. The transient drag/resize/move state is
/// held in [`OverlayState`] and owned by the canvas widget.
///
/// Each overlay window gets its own [`OverlayProgram`] instance whose
/// `display_id` identifies the monitor the user is interacting with.
/// The canvas only surfaces events when the cursor is in `bounds`, so
/// the resulting [`Message::OverlaySelected`] always carries the
/// display id of the screen the selection actually lives on.
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
    /// The committed-but-still-editable selection. Populated on the
    /// first drag's mouse-up, mutated by subsequent resize/move ops,
    /// finally consumed by Enter / Space.
    pub(crate) selection: Option<Rectangle>,
    /// The mouse op currently in flight (button held). `None` while
    /// the user is just hovering over a committed selection.
    pub(crate) active: Option<Active>,
    /// `true` while Shift is held. Mouse events in iced 0.14 don't
    /// carry modifier state, so the canvas tracks Shift itself via
    /// `KeyPressed` / `KeyReleased`. Only used to constrain the
    /// initial drag to a perfect square.
    pub(crate) shift_held: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Active {
    /// First drag — no committed selection yet. `current` updates
    /// every CursorMoved; the live rect is computed from
    /// `(anchor, current)` plus the Shift-square constraint.
    InitialDrag { anchor: Point, current: Point },
    /// Resizing a committed selection by dragging one of its handles.
    /// `original` is the rect at the moment the press started; each
    /// CursorMoved recomputes `state.selection` from `original` plus
    /// the live cursor so the math doesn't drift across frames.
    Resize { handle: Handle, original: Rectangle },
    /// Translating a committed selection by dragging its body.
    /// `start_cursor` is the cursor position at press; `original` is
    /// the rect at that moment. Delta = current_cursor - start_cursor.
    Move {
        start_cursor: Point,
        original: Rectangle,
    },
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Handle {
    NW,
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
}

/// Visual half-side of a resize handle, in logical pixels.
const HANDLE_HALF: f32 = 4.0;
/// Hit-test half-side. Slightly larger than visual so handles are
/// easier to grab.
const HANDLE_HIT: f32 = 8.0;

impl OverlayState {
    /// The rect to draw / inspect *right now*. During InitialDrag this
    /// is the live preview (with optional Shift-square snap); otherwise
    /// it's the committed selection (or `None` if neither exists).
    fn current_rect(&self) -> Option<Rectangle> {
        if let Some(Active::InitialDrag { anchor, current }) = self.active {
            return Some(rectangle_from_two_points(
                anchor,
                constrain_target(anchor, current, self.shift_held),
            ));
        }
        self.selection
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

fn corner_centers(r: Rectangle) -> [(Handle, Point); 4] {
    let l = r.x;
    let t = r.y;
    let rt = r.x + r.width;
    let b = r.y + r.height;
    [
        (Handle::NW, Point::new(l, t)),
        (Handle::NE, Point::new(rt, t)),
        (Handle::SE, Point::new(rt, b)),
        (Handle::SW, Point::new(l, b)),
    ]
}

fn edge_centers(r: Rectangle) -> [(Handle, Point); 4] {
    let cx = r.x + r.width / 2.0;
    let cy = r.y + r.height / 2.0;
    let l = r.x;
    let t = r.y;
    let rt = r.x + r.width;
    let b = r.y + r.height;
    [
        (Handle::N, Point::new(cx, t)),
        (Handle::E, Point::new(rt, cy)),
        (Handle::S, Point::new(cx, b)),
        (Handle::W, Point::new(l, cy)),
    ]
}

fn handle_at(r: Rectangle, p: Point) -> Option<Handle> {
    // Corners first so they win at small sizes where the corner and
    // edge hit-boxes overlap.
    for (h, c) in corner_centers(r) {
        if (p.x - c.x).abs() <= HANDLE_HIT && (p.y - c.y).abs() <= HANDLE_HIT {
            return Some(h);
        }
    }
    for (h, c) in edge_centers(r) {
        if (p.x - c.x).abs() <= HANDLE_HIT && (p.y - c.y).abs() <= HANDLE_HIT {
            return Some(h);
        }
    }
    None
}

fn cursor_for_handle(h: Handle) -> mouse::Interaction {
    match h {
        Handle::NW | Handle::SE => mouse::Interaction::ResizingDiagonallyDown,
        Handle::NE | Handle::SW => mouse::Interaction::ResizingDiagonallyUp,
        Handle::N | Handle::S => mouse::Interaction::ResizingVertically,
        Handle::E | Handle::W => mouse::Interaction::ResizingHorizontally,
    }
}

fn rect_contains(r: Rectangle, p: Point) -> bool {
    p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height
}

/// Recompute the rect when a handle is dragged. The cursor position
/// replaces the relevant edge(s); `rectangle_from_two_points`
/// normalises so dragging past the opposite edge "flips" cleanly
/// without producing negative dimensions.
fn resize_rect(orig: Rectangle, handle: Handle, cursor: Point) -> Rectangle {
    let mut left = orig.x;
    let mut right = orig.x + orig.width;
    let mut top = orig.y;
    let mut bottom = orig.y + orig.height;
    match handle {
        Handle::NW => {
            left = cursor.x;
            top = cursor.y;
        }
        Handle::N => {
            top = cursor.y;
        }
        Handle::NE => {
            right = cursor.x;
            top = cursor.y;
        }
        Handle::E => {
            right = cursor.x;
        }
        Handle::SE => {
            right = cursor.x;
            bottom = cursor.y;
        }
        Handle::S => {
            bottom = cursor.y;
        }
        Handle::SW => {
            left = cursor.x;
            bottom = cursor.y;
        }
        Handle::W => {
            left = cursor.x;
        }
    }
    rectangle_from_two_points(Point::new(left, top), Point::new(right, bottom))
}

fn size_badge_origin(
    bounds: Rectangle,
    selection: Rectangle,
    badge_w: f32,
    badge_h: f32,
) -> (f32, f32) {
    let pad = 4.0;
    let max_x = (bounds.width - badge_w).max(0.0);
    let max_y = (bounds.height - badge_h).max(0.0);
    let x = (selection.x + selection.width - badge_w - pad).clamp(0.0, max_x);
    let y = (selection.y + pad).clamp(0.0, max_y);
    (x, y)
}

/// Translate `orig` by `delta`, clamped so the rect stays inside
/// `bounds`. When the rect is larger than `bounds` (shouldn't happen
/// in practice but the math has to be safe) we clamp the available
/// range to `[0, 0]`.
fn move_rect(orig: Rectangle, delta: (f32, f32), bounds: Rectangle) -> Rectangle {
    let max_x = (bounds.width - orig.width).max(0.0);
    let max_y = (bounds.height - orig.height).max(0.0);
    Rectangle {
        x: (orig.x + delta.0).clamp(0.0, max_x),
        y: (orig.y + delta.1).clamp(0.0, max_y),
        width: orig.width,
        height: orig.height,
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
        // Track Shift modifier (mouse events lack modifier state in
        // iced 0.14). Falls through so other keyboard arms still match.
        if let Event::Keyboard(
            keyboard::Event::KeyPressed { modifiers, .. }
            | keyboard::Event::KeyReleased { modifiers, .. },
        ) = event
        {
            let s = modifiers.shift();
            if state.shift_held != s {
                state.shift_held = s;
                if state.active.is_some() {
                    return Some(Action::request_redraw());
                }
            }
        }

        // Esc / Enter / Space.
        if let Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) = event {
            match key {
                Key::Named(Named::Escape) => {
                    state.active = None;
                    state.selection = None;
                    return Some(Action::publish(Message::OverlayCancelled).and_capture());
                }
                Key::Named(Named::Enter | Named::Space) => {
                    // Confirm: existing selection if any, else fall
                    // back to "capture the whole overlay" (the
                    // original Phase B Enter semantics). Enter
                    // routes through the editor by default — the
                    // toolbar buttons publish their own
                    // `OverlaySelected` with a different intent.
                    let r = state.selection.unwrap_or(Rectangle {
                        x: 0.0,
                        y: 0.0,
                        width: bounds.width,
                        height: bounds.height,
                    });
                    state.active = None;
                    state.selection = None;
                    if let Some(domain) = rect_to_domain(r) {
                        return Some(
                            Action::publish(Message::OverlaySelected {
                                display_id: self.display_id.clone(),
                                rect: domain,
                                intent: CaptureIntent::Editor,
                            })
                            .and_capture(),
                        );
                    }
                    return Some(Action::publish(Message::OverlayCancelled).and_capture());
                }
                _ => {}
            }
        }

        // Mouse handling.
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                if let Some(sel) = state.selection {
                    if let Some(handle) = handle_at(sel, p) {
                        state.active = Some(Active::Resize {
                            handle,
                            original: sel,
                        });
                        return Some(Action::request_redraw().and_capture());
                    }
                    if rect_contains(sel, p) {
                        state.active = Some(Active::Move {
                            start_cursor: p,
                            original: sel,
                        });
                        return Some(Action::request_redraw().and_capture());
                    }
                    // Click outside the existing selection — drop it
                    // and start a fresh drag from the click point.
                    // Tell the runtime the toolbar should disappear.
                    state.selection = None;
                    state.active = Some(Active::InitialDrag {
                        anchor: p,
                        current: p,
                    });
                    return Some(
                        Action::publish(Message::OverlaySelectionChanged {
                            display_id: self.display_id.clone(),
                            rect: None,
                        })
                        .and_capture(),
                    );
                }
                state.active = Some(Active::InitialDrag {
                    anchor: p,
                    current: p,
                });
                Some(Action::request_redraw().and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let p = cursor.position_in(bounds)?;
                let mut updated_selection: Option<Rectangle> = None;
                match state.active.as_mut()? {
                    Active::InitialDrag { current, .. } => {
                        *current = p;
                    }
                    Active::Resize { handle, original } => {
                        let new_rect = resize_rect(*original, *handle, p);
                        state.selection = Some(new_rect);
                        updated_selection = Some(new_rect);
                    }
                    Active::Move {
                        start_cursor,
                        original,
                    } => {
                        let dx = p.x - start_cursor.x;
                        let dy = p.y - start_cursor.y;
                        let new_rect = move_rect(*original, (dx, dy), bounds);
                        state.selection = Some(new_rect);
                        updated_selection = Some(new_rect);
                    }
                }
                if let Some(domain) = updated_selection.and_then(rect_to_domain) {
                    // `Action::publish` already produces a redraw,
                    // per iced::widget::canvas::Action docs.
                    return Some(Action::publish(Message::OverlaySelectionChanged {
                        display_id: self.display_id.clone(),
                        rect: Some(domain),
                    }));
                }
                Some(Action::request_redraw())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                match state.active.take() {
                    Some(Active::InitialDrag { anchor, current }) => {
                        let target = constrain_target(anchor, current, state.shift_held);
                        let r = rectangle_from_two_points(anchor, target);
                        // Discard sub-pixel "clicks" — the user almost
                        // certainly didn't mean to commit a 1×1 region.
                        if r.width >= 4.0 && r.height >= 4.0 {
                            state.selection = Some(r);
                            if let Some(domain) = rect_to_domain(r) {
                                return Some(
                                    Action::publish(Message::OverlaySelectionChanged {
                                        display_id: self.display_id.clone(),
                                        rect: Some(domain),
                                    })
                                    .and_capture(),
                                );
                            }
                        }
                        Some(Action::request_redraw().and_capture())
                    }
                    Some(Active::Resize { .. }) | Some(Active::Move { .. }) => {
                        // `state.selection` was kept up to date during
                        // the drag and the toolbar already saw it via
                        // CursorMoved publishes — nothing new to send.
                        Some(Action::request_redraw().and_capture())
                    }
                    None => None,
                }
            }
            _ => None,
        }
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

        let Some(rect) = state.current_rect() else {
            return vec![frame.into_geometry()];
        };

        // "Punch out" the selection by overdrawing it with a near-
        // transparent fill so it appears brighter than the surrounding
        // veil. Border = outer black halo + animated white dashes.
        let path = Path::rectangle(Point::new(rect.x, rect.y), rect.size());
        frame.fill(&path, Color::from_rgba(1.0, 1.0, 1.0, 0.05));
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.7))
                .with_width(3.0),
        );
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

        // Live size badge in physical pixels (what the captured PNG
        // will be). Keep it inside the top edge of the selection so
        // it never competes with the quick-action toolbar.
        let phys_w = ((rect.width * self.scale).round() as i32).max(0);
        let phys_h = ((rect.height * self.scale).round() as i32).max(0);
        let label = format!("{phys_w} × {phys_h}px");
        let badge_w = 8.0 + label.chars().count() as f32 * 7.5;
        let badge_h = 18.0;
        let (bx, by) = size_badge_origin(bounds, rect, badge_w, badge_h);
        let badge_path = Path::rectangle(Point::new(bx, by), iced::Size::new(badge_w, badge_h));
        frame.fill(&badge_path, Color::from_rgba(0.0, 0.0, 0.0, 0.7));
        frame.fill_text(CanvasText {
            content: label,
            position: Point::new(bx + 4.0, by + 2.0),
            color: Color::WHITE,
            size: iced::Pixels(11.0),
            ..Default::default()
        });

        // Resize handles — only when the selection is committed (i.e.
        // we're not mid-initial-drag). White squares with a thin black
        // outline so they read on any wallpaper.
        let in_initial_drag = matches!(state.active, Some(Active::InitialDrag { .. }));
        if !in_initial_drag && state.selection.is_some() {
            let centers = corner_centers(rect).into_iter().chain(edge_centers(rect));
            for (_h, c) in centers {
                let p = Path::rectangle(
                    Point::new(c.x - HANDLE_HALF, c.y - HANDLE_HALF),
                    iced::Size::new(HANDLE_HALF * 2.0, HANDLE_HALF * 2.0),
                );
                frame.fill(&p, Color::WHITE);
                frame.stroke(
                    &p,
                    Stroke::default()
                        .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.8))
                        .with_width(1.0),
                );
            }
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        // While dragging, lock the cursor to the op kind.
        match &state.active {
            Some(Active::Resize { handle, .. }) => return cursor_for_handle(*handle),
            Some(Active::Move { .. }) => return mouse::Interaction::Grabbing,
            Some(Active::InitialDrag { .. }) => return mouse::Interaction::Crosshair,
            None => {}
        }
        // Idle hover: reflect what a click would do.
        if let (Some(sel), Some(p)) = (state.selection, cursor.position_in(bounds)) {
            if let Some(h) = handle_at(sel, p) {
                return cursor_for_handle(h);
            }
            if rect_contains(sel, p) {
                return mouse::Interaction::Grab;
            }
        }
        mouse::Interaction::Crosshair
    }
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
        assert!(rect_to_domain(Rectangle {
            x: 0.0,
            y: 0.0,
            width: -10.0,
            height: 50.0
        })
        .is_none());
    }

    #[test]
    fn current_rect_during_initial_drag() {
        let state = OverlayState {
            active: Some(Active::InitialDrag {
                anchor: Point::new(5.0, 5.0),
                current: Point::new(20.0, 25.0),
            }),
            ..Default::default()
        };
        let r = state.current_rect().unwrap();
        assert_eq!(r.width, 15.0);
        assert_eq!(r.height, 20.0);
    }

    #[test]
    fn shift_constrains_initial_drag_to_a_square() {
        let state = OverlayState {
            active: Some(Active::InitialDrag {
                anchor: Point::ORIGIN,
                current: Point::new(60.0, 20.0),
            }),
            shift_held: true,
            ..Default::default()
        };
        let r = state.current_rect().unwrap();
        assert_eq!(r.width, 60.0);
        assert_eq!(r.height, 60.0);
    }

    #[test]
    fn current_rect_returns_committed_selection_when_idle() {
        let sel = Rectangle {
            x: 1.0,
            y: 2.0,
            width: 30.0,
            height: 40.0,
        };
        let state = OverlayState {
            selection: Some(sel),
            ..Default::default()
        };
        assert_eq!(state.current_rect(), Some(sel));
    }

    #[test]
    fn current_rect_is_none_when_idle_with_no_selection() {
        assert!(OverlayState::default().current_rect().is_none());
    }

    #[test]
    fn handle_at_finds_corners() {
        let r = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(handle_at(r, Point::new(10.0, 20.0)), Some(Handle::NW));
        assert_eq!(handle_at(r, Point::new(110.0, 20.0)), Some(Handle::NE));
        assert_eq!(handle_at(r, Point::new(110.0, 70.0)), Some(Handle::SE));
        assert_eq!(handle_at(r, Point::new(10.0, 70.0)), Some(Handle::SW));
    }

    #[test]
    fn handle_at_finds_edges() {
        let r = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(handle_at(r, Point::new(60.0, 20.0)), Some(Handle::N));
        assert_eq!(handle_at(r, Point::new(110.0, 45.0)), Some(Handle::E));
        assert_eq!(handle_at(r, Point::new(60.0, 70.0)), Some(Handle::S));
        assert_eq!(handle_at(r, Point::new(10.0, 45.0)), Some(Handle::W));
    }

    #[test]
    fn handle_at_returns_none_in_body() {
        let r = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(handle_at(r, Point::new(60.0, 45.0)), None);
    }

    #[test]
    fn resize_se_extends_only_right_and_bottom() {
        let orig = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        let new = resize_rect(orig, Handle::SE, Point::new(200.0, 150.0));
        assert_eq!(new.x, 10.0);
        assert_eq!(new.y, 20.0);
        assert_eq!(new.width, 190.0);
        assert_eq!(new.height, 130.0);
    }

    #[test]
    fn resize_n_only_changes_top_edge() {
        let orig = Rectangle {
            x: 10.0,
            y: 50.0,
            width: 100.0,
            height: 50.0,
        };
        // x=999 must be ignored — N handle only moves the top edge.
        let new = resize_rect(orig, Handle::N, Point::new(999.0, 30.0));
        assert_eq!(new.x, 10.0);
        assert_eq!(new.y, 30.0);
        assert_eq!(new.width, 100.0);
        assert_eq!(new.height, 70.0);
    }

    #[test]
    fn resize_normalises_when_dragged_past_opposite_edge() {
        let orig = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        // Drag SE handle past the top-left of the rect.
        let new = resize_rect(orig, Handle::SE, Point::new(0.0, 0.0));
        assert_eq!(new.x, 0.0);
        assert_eq!(new.y, 0.0);
        assert_eq!(new.width, 10.0);
        assert_eq!(new.height, 20.0);
    }

    #[test]
    fn size_badge_stays_at_selection_top() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        };
        let selection = Rectangle {
            x: 100.0,
            y: 80.0,
            width: 220.0,
            height: 120.0,
        };

        let (x, y) = size_badge_origin(bounds, selection, 80.0, 18.0);

        assert_eq!(x, 236.0);
        assert_eq!(y, 84.0);
    }

    #[test]
    fn move_clamps_to_bounds() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let orig = Rectangle {
            x: 100.0,
            y: 100.0,
            width: 200.0,
            height: 200.0,
        };
        let off_top_left = move_rect(orig, (-9999.0, -9999.0), bounds);
        assert_eq!(off_top_left.x, 0.0);
        assert_eq!(off_top_left.y, 0.0);
        let off_bottom_right = move_rect(orig, (9999.0, 9999.0), bounds);
        assert_eq!(off_bottom_right.x, 800.0);
        assert_eq!(off_bottom_right.y, 600.0);
    }

    #[test]
    fn rect_contains_inclusive_of_edges() {
        let r = Rectangle {
            x: 10.0,
            y: 10.0,
            width: 20.0,
            height: 20.0,
        };
        assert!(rect_contains(r, Point::new(10.0, 10.0)));
        assert!(rect_contains(r, Point::new(30.0, 30.0)));
        assert!(rect_contains(r, Point::new(20.0, 20.0)));
        assert!(!rect_contains(r, Point::new(9.9, 20.0)));
    }
}
