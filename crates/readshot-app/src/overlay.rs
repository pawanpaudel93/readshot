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
//! 2. On mouse-up the rect is *committed* into `OverlayState::selection`
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
    Action, Cache, Event, Fill, Frame, Geometry, Path, Program, Stroke, Text as CanvasText,
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
    /// When set, the initial drag release immediately confirms the
    /// selection with this intent instead of committing an editable
    /// selection and showing the quick-action toolbar.
    pub auto_confirm_intent: Option<CaptureIntent>,
    /// Runtime-tracked Shift state. iced 0.14 does not route keyboard
    /// events into canvas widgets, so the daemon listens globally and
    /// passes the current modifier state in here on each view().
    pub shift_held: bool,
}

#[derive(Default, Debug)]
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
    /// Set when the last drag was released below the minimum committable
    /// size. Keeps the overlay open and swaps the hint pill to a "drag a
    /// larger area" prompt so the discard isn't silent. Cleared as soon
    /// as the user starts another drag.
    pub(crate) too_small_hint: bool,
    /// Static full-window dim veil. It only changes when the overlay
    /// size changes, so keep it out of the cursor-move redraw path.
    veil_cache: Cache<Renderer>,
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

/// Hit-test half-side. Roughly a fingertip target so handles are
/// easy to grab on a trackpad — the visual square stays small but
/// the active grab area expands around it.
const HANDLE_HIT: f32 = 14.0;
/// Half-length of the drawn crosshair arms. Kept small so it reads
/// like a precise selector, not a full-screen guide.
const CROSSHAIR_ARM: f32 = 9.0;
/// Minimum width and height (logical px) a released drag must reach to
/// be committed. Anything smaller is treated as an accidental click
/// rather than a 1×1 selection.
const MIN_DRAG_SIZE: f32 = 4.0;

/// Whether a released drag rect is large enough to commit as a
/// selection. Sub-threshold drags are almost always accidental clicks.
fn drag_meets_min_size(r: Rectangle) -> bool {
    r.width >= MIN_DRAG_SIZE && r.height >= MIN_DRAG_SIZE
}

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

fn crosshair_point(state: &OverlayState, cursor: Option<Point>) -> Option<Point> {
    let point = cursor?;
    match state.active {
        Some(Active::InitialDrag { .. }) => Some(point),
        Some(Active::Resize { .. }) | Some(Active::Move { .. }) => None,
        None => {
            if let Some(selection) = state.selection {
                if handle_at(selection, point).is_some() || rect_contains(selection, point) {
                    return None;
                }
            }
            Some(point)
        }
    }
}

fn draw_crosshair(frame: &mut Frame<Renderer>, bounds: Rectangle, point: Point) {
    let left = 0.0;
    let right = bounds.width.max(0.0);
    let top = 0.0;
    let bottom = bounds.height.max(0.0);
    let x = point.x.clamp(left, right);
    let y = point.y.clamp(top, bottom);
    let segments = [
        (
            Point::new((x - CROSSHAIR_ARM).clamp(left, right), y),
            Point::new((x + CROSSHAIR_ARM).clamp(left, right), y),
        ),
        (
            Point::new(x, (y - CROSSHAIR_ARM).clamp(top, bottom)),
            Point::new(x, (y + CROSSHAIR_ARM).clamp(top, bottom)),
        ),
    ];

    for (from, to) in segments {
        let path = Path::line(from, to);
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.35))
                .with_width(2.0),
        );
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.35))
                .with_width(1.0),
        );
    }
}

/// Approximate advance width of one glyph in the hint pill, in logical
/// pixels. Used to size the pill and the key chips.
const HINT_TEXT_SIZE: f32 = 13.0;
const HINT_KEY_SIZE: f32 = 11.0;
/// JetBrains Mono advance (600/1000 upm) per pixel of font size.
const MONO_ADVANCE: f32 = 0.6;
const HINT_TEXT_W: f32 = HINT_TEXT_SIZE * MONO_ADVANCE;
const HINT_KEY_W: f32 = HINT_KEY_SIZE * MONO_ADVANCE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HintSeg {
    Text(&'static str),
    Muted(&'static str),
    Key(&'static str),
}

/// Hint pill copy: the instruction first, then key chips. Key chips never
/// truncate, so the old "Esc first" ordering is no longer needed.
pub(crate) fn hint_segments(cli_interactive: bool, too_small: bool) -> Vec<HintSeg> {
    use HintSeg::*;
    if too_small {
        vec![
            Text("Drag to select a larger area"),
            Key("esc"),
            Muted("Cancel"),
        ]
    } else if cli_interactive {
        vec![
            Text("Drag to capture"),
            Muted("Release to capture"),
            Key("⇧"),
            Muted("Square"),
            Key("esc"),
            Muted("Cancel"),
        ]
    } else {
        vec![
            Text("Drag to select an area"),
            Key("⇧"),
            Muted("Square"),
            Key("↵"),
            Muted("Full screen"),
            Key("esc"),
            Muted("Cancel"),
        ]
    }
}

const HINT_GAP: f32 = 10.0;
const HINT_KEY_PAD: f32 = 12.0;
const HINT_H: f32 = 34.0;
const HINT_TOP: f32 = 40.0;

fn seg_width(s: &HintSeg) -> f32 {
    match s {
        HintSeg::Text(t) | HintSeg::Muted(t) => t.chars().count() as f32 * HINT_TEXT_W,
        HintSeg::Key(k) => k.chars().count().max(2) as f32 * HINT_KEY_W + HINT_KEY_PAD,
    }
}

fn segs_width(segs: &[HintSeg]) -> f32 {
    segs.iter().map(seg_width).sum::<f32>() + HINT_GAP * segs.len().saturating_sub(1) as f32 + 32.0
}

/// Drop optional key/label pairs (Square, Full screen, Release) from the
/// middle until the pill fits; the instruction and esc always stay.
fn fit_hint_segments(mut segs: Vec<HintSeg>, max_w: f32) -> Vec<HintSeg> {
    while segs_width(&segs) > max_w {
        let drop = segs
            .iter()
            .position(|s| matches!(s, HintSeg::Key(k) if *k != "esc"))
            .or_else(|| {
                segs.iter()
                    .skip(1)
                    .position(|s| matches!(s, HintSeg::Muted(m) if *m != "Cancel"))
                    .map(|i| i + 1)
            });
        match drop {
            Some(i) => {
                segs.remove(i);
                if matches!(segs.get(i), Some(HintSeg::Muted(m)) if *m != "Cancel") {
                    segs.remove(i);
                }
            }
            None => break,
        }
    }
    segs
}

fn draw_overlay_hint(
    frame: &mut Frame<Renderer>,
    bounds: Rectangle,
    cli_interactive: bool,
    too_small: bool,
) {
    let segs = fit_hint_segments(
        hint_segments(cli_interactive, too_small),
        bounds.width - 48.0,
    );
    let box_w = segs_width(&segs);
    let x0 = ((bounds.width - box_w) / 2.0).max(0.0);
    let pill = Path::rounded_rectangle(
        Point::new(x0, HINT_TOP),
        iced::Size::new(box_w, HINT_H),
        (HINT_H / 2.0).into(),
    );
    frame.fill(&pill, Color::from_rgba8(28, 28, 30, 0.90));
    frame.stroke(
        &pill,
        Stroke::default()
            .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.10))
            .with_width(1.0),
    );

    let font = iced::Font::with_name(readshot_core::render::FONT_FAMILY);
    let mid_y = HINT_TOP + HINT_H / 2.0;
    let mut x = x0 + 16.0;
    for seg in &segs {
        let w = seg_width(seg);
        match seg {
            HintSeg::Text(t) | HintSeg::Muted(t) => {
                let color = if matches!(seg, HintSeg::Text(_)) {
                    Color::from_rgb8(0xED, 0xED, 0xED)
                } else {
                    Color::from_rgb8(0xA1, 0xA1, 0xAA)
                };
                frame.fill_text(CanvasText {
                    content: (*t).to_string(),
                    position: Point::new(x, mid_y),
                    color,
                    size: iced::Pixels(HINT_TEXT_SIZE),
                    font,
                    align_y: iced::alignment::Vertical::Center,
                    ..Default::default()
                });
            }
            HintSeg::Key(k) => {
                let chip_h = 20.0;
                let chip = Path::rounded_rectangle(
                    Point::new(x, mid_y - chip_h / 2.0),
                    iced::Size::new(w, chip_h),
                    5.0.into(),
                );
                frame.fill(&chip, Color::from_rgba(1.0, 1.0, 1.0, 0.08));
                frame.fill_text(CanvasText {
                    content: (*k).to_string(),
                    position: Point::new(x + w / 2.0, mid_y),
                    color: Color::from_rgb8(0xB4, 0xB4, 0xB4),
                    size: iced::Pixels(HINT_KEY_SIZE),
                    font,
                    align_x: iced::alignment::Horizontal::Center.into(),
                    align_y: iced::alignment::Vertical::Center,
                    ..Default::default()
                });
            }
        }
        x += w + HINT_GAP;
    }
}

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

/// Badge sits above the selection's top-left corner; inside the selection
/// when there is no room above.
fn size_badge_origin(
    bounds: Rectangle,
    selection: Rectangle,
    badge_w: f32,
    badge_h: f32,
) -> (f32, f32) {
    let gap = 6.0;
    let max_x = (bounds.width - badge_w).max(0.0);
    let x = selection.x.clamp(0.0, max_x);
    let above = selection.y - badge_h - gap;
    let y = if above >= 0.0 {
        above
    } else {
        selection.y + gap
    };
    (x, y)
}

/// Estimated size of the floating action bar (`runtime/view.rs`
/// `overlay_toolbar_layer`). Used for edge-aware placement without
/// measuring real layout. Edit (~74) + 6 icon buttons (32) + separator
/// + gaps + padding.
pub(crate) const OVERLAY_TOOLBAR_HEIGHT: f32 = 42.0;
pub(crate) const OVERLAY_TOOLBAR_GAP: f32 = 10.0;
pub(crate) const OVERLAY_TOOLBAR_WIDTH: f32 = 312.0;

/// Keyboard shortcuts the overlay handles while a selection exists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum OverlayShortcut {
    Intent(CaptureIntent),
    Nudge { dx: f32, dy: f32 },
}

pub(crate) fn overlay_shortcut(key: &Key, m: keyboard::Modifiers) -> Option<OverlayShortcut> {
    use CaptureIntent as I;
    if m.command() && !m.alt() {
        let Key::Character(c) = key else { return None };
        return match (c.to_ascii_lowercase().as_str(), m.shift()) {
            ("c", false) => Some(OverlayShortcut::Intent(I::CopyToClipboard)),
            ("c", true) => Some(OverlayShortcut::Intent(I::CopyTextDirect)),
            ("s", false) => Some(OverlayShortcut::Intent(I::SaveDirect)),
            ("p", false) => Some(OverlayShortcut::Intent(I::Pin)),
            _ => None,
        };
    }
    if m.command() || m.alt() || m.control() {
        return None;
    }
    let step = if m.shift() { 10.0 } else { 1.0 };
    match key {
        Key::Named(Named::ArrowLeft) => Some(OverlayShortcut::Nudge { dx: -step, dy: 0.0 }),
        Key::Named(Named::ArrowRight) => Some(OverlayShortcut::Nudge { dx: step, dy: 0.0 }),
        Key::Named(Named::ArrowUp) => Some(OverlayShortcut::Nudge { dx: 0.0, dy: -step }),
        Key::Named(Named::ArrowDown) => Some(OverlayShortcut::Nudge { dx: 0.0, dy: step }),
        _ => None,
    }
}

/// True when the action bar flips above a selection too narrow to
/// shield the size badge, so the badge must move inside the selection.
fn badge_clashes_with_toolbar(bounds: Rectangle, selection: Rectangle) -> bool {
    selection.y + selection.height + OVERLAY_TOOLBAR_GAP + OVERLAY_TOOLBAR_HEIGHT > bounds.height
        && selection.width < OVERLAY_TOOLBAR_WIDTH
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
        // iced 0.14 does not deliver keyboard events to canvas widgets,
        // so the runtime tracks Shift globally and threads the current
        // state into `self.shift_held`. Sync it into the program state
        // on every event so the rest of the draw path (which reads
        // state.shift_held) can stay unchanged. A direct
        // `KeyPressed { modifiers, .. }` branch is kept below as a
        // fallback for the rare case where iced does deliver them.
        if state.shift_held != self.shift_held {
            state.shift_held = self.shift_held;
            if state.active.is_some() {
                return Some(Action::request_redraw());
            }
        }
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
        if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event {
            match key {
                Key::Named(Named::Escape) => {
                    state.active = None;
                    state.selection = None;
                    return Some(Action::publish(Message::OverlayCancelled).and_capture());
                }
                Key::Named(Named::Enter | Named::Space) => {
                    // Confirm: existing selection if any, else fall
                    // back to "capture the whole overlay" (the
                    // original Phase B Enter semantics). Enter routes
                    // through the editor by default; one-shot CLI mode
                    // overrides this to capture directly.
                    let r = state.selection.unwrap_or(Rectangle {
                        x: 0.0,
                        y: 0.0,
                        width: bounds.width,
                        height: bounds.height,
                    });
                    let intent = self.auto_confirm_intent.unwrap_or(CaptureIntent::Editor);
                    state.active = None;
                    state.selection = None;
                    if let Some(domain) = rect_to_domain(r) {
                        return Some(
                            Action::publish(Message::OverlaySelected {
                                display_id: self.display_id.clone(),
                                rect: domain,
                                intent,
                            })
                            .and_capture(),
                        );
                    }
                    return Some(Action::publish(Message::OverlayCancelled).and_capture());
                }
                _ => {}
            }
            if let (Some(shortcut), Some(sel)) =
                (overlay_shortcut(key, *modifiers), state.selection)
            {
                if state.active.is_none() {
                    match shortcut {
                        OverlayShortcut::Intent(intent) => {
                            if let Some(domain) = rect_to_domain(sel) {
                                state.selection = None;
                                return Some(
                                    Action::publish(Message::OverlaySelected {
                                        display_id: self.display_id.clone(),
                                        rect: domain,
                                        intent,
                                    })
                                    .and_capture(),
                                );
                            }
                        }
                        OverlayShortcut::Nudge { dx, dy } => {
                            let moved = move_rect(sel, (dx, dy), bounds);
                            state.selection = Some(moved);
                            return Some(
                                Action::publish(Message::OverlaySelectionChanged {
                                    display_id: self.display_id.clone(),
                                    rect: rect_to_domain(moved),
                                })
                                .and_capture(),
                            );
                        }
                    }
                }
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
                    state.too_small_hint = false;
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
                state.too_small_hint = false;
                state.active = Some(Active::InitialDrag {
                    anchor: p,
                    current: p,
                });
                Some(Action::request_redraw().and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let p = cursor.position_in(bounds)?;
                let mut updated_selection: Option<Rectangle> = None;
                let Some(active) = state.active.as_mut() else {
                    return Some(Action::request_redraw());
                };
                match active {
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
                        // Discard sub-threshold "clicks" — the user almost
                        // certainly didn't mean to commit a tiny region.
                        if drag_meets_min_size(r) {
                            state.too_small_hint = false;
                            if let Some(intent) = self.auto_confirm_intent {
                                state.selection = None;
                                if let Some(domain) = rect_to_domain(r) {
                                    return Some(
                                        Action::publish(Message::OverlaySelected {
                                            display_id: self.display_id.clone(),
                                            rect: domain,
                                            intent,
                                        })
                                        .and_capture(),
                                    );
                                }
                                return Some(
                                    Action::publish(Message::OverlayCancelled).and_capture(),
                                );
                            }
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
                        if self.auto_confirm_intent.is_some() {
                            return Some(Action::publish(Message::OverlayCancelled).and_capture());
                        }
                        // Too-small drag with no auto-confirm: keep the
                        // overlay open and flag the hint so the user sees
                        // *why* nothing was selected instead of guessing.
                        if state.selection.is_none() {
                            state.too_small_hint = true;
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
        cursor: mouse::Cursor,
    ) -> Vec<Geometry<Renderer>> {
        let mut frame = Frame::new(renderer, bounds.size());
        let cursor_point = cursor.position_in(bounds);

        let current_rect = state.current_rect();
        if current_rect.is_none() {
            draw_overlay_hint(
                &mut frame,
                bounds,
                self.auto_confirm_intent == Some(CaptureIntent::CliInteractive),
                state.too_small_hint,
            );
        }

        let Some(rect) = current_rect else {
            let veil = state.veil_cache.draw(renderer, bounds.size(), |frame| {
                frame.fill(
                    &Path::rectangle(Point::ORIGIN, bounds.size()),
                    Color::from_rgba(0.0, 0.0, 0.0, 0.5),
                );
            });
            if let Some(point) = crosshair_point(state, cursor_point) {
                draw_crosshair(&mut frame, bounds, point);
            }
            return vec![veil, frame.into_geometry()];
        };

        // Punch the selection out of the veil (even-odd) so it shows the
        // screen at full brightness, then draw a clean two-tone edge.
        let mut veil_frame = Frame::new(renderer, bounds.size());
        let veil_path = Path::new(|b| {
            b.rectangle(Point::ORIGIN, bounds.size());
            b.rectangle(Point::new(rect.x, rect.y), rect.size());
        });
        veil_frame.fill(
            &veil_path,
            Fill {
                style: Color::from_rgba(0.0, 0.0, 0.0, 0.5).into(),
                rule: iced::widget::canvas::fill::Rule::EvenOdd,
            },
        );
        let path = Path::rectangle(Point::new(rect.x, rect.y), rect.size());
        let interaction_active = state.active.is_some();
        frame.stroke(
            &path,
            Stroke::default()
                .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))
                .with_width(3.0),
        );
        frame.stroke(
            &path,
            Stroke::default().with_color(Color::WHITE).with_width(1.0),
        );

        if !interaction_active {
            // Size badge in physical pixels (what the captured PNG
            // will be), plus the selection's top-left origin. Text
            // layout is intentionally skipped during pointer drags so
            // the hot path stays just vector strokes.
            let phys_w = ((rect.width * self.scale).round() as i32).max(0);
            let phys_h = ((rect.height * self.scale).round() as i32).max(0);
            let label = format!("{phys_w} × {phys_h}");
            let badge_w = 16.0 + label.chars().count() as f32 * 7.0;
            let badge_h = 22.0;
            let (bx, mut by) = size_badge_origin(bounds, rect, badge_w, badge_h);
            if badge_clashes_with_toolbar(bounds, rect) {
                by = rect.y + 6.0;
            }
            // Rounded corners (radius 6) match the hint pill; the badge
            // text already uses the bundled monospace family.
            let badge_path = Path::rounded_rectangle(
                Point::new(bx, by),
                iced::Size::new(badge_w, badge_h),
                6.0.into(),
            );
            frame.fill(&badge_path, Color::from_rgba8(28, 28, 30, 0.90));
            frame.stroke(
                &badge_path,
                Stroke::default()
                    .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.10))
                    .with_width(1.0),
            );
            frame.fill_text(CanvasText {
                content: label,
                position: Point::new(bx + 8.0, by + 4.0),
                color: Color::from_rgb8(0xED, 0xED, 0xED),
                size: iced::Pixels(11.5),
                font: iced::Font::with_name(readshot_core::render::FONT_FAMILY),
                ..Default::default()
            });
        }

        // Resize handles — only when the selection is committed (i.e.
        // we're not mid-initial-drag). White squares with a thin black
        // outline so they read on any wallpaper.
        if !interaction_active && state.selection.is_some() {
            let centers = corner_centers(rect).into_iter().chain(edge_centers(rect));
            for (_h, c) in centers {
                let p = Path::circle(c, 5.0);
                frame.fill(&p, Color::WHITE);
                frame.stroke(
                    &p,
                    Stroke::default()
                        .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.35))
                        .with_width(1.0),
                );
            }
        }

        if let Some(point) = crosshair_point(state, cursor_point) {
            draw_crosshair(&mut frame, bounds, point);
        }

        vec![veil_frame.into_geometry(), frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        overlay_mouse_interaction(state, bounds, cursor)
    }
}

fn overlay_mouse_interaction(
    state: &OverlayState,
    bounds: Rectangle,
    cursor: mouse::Cursor,
) -> mouse::Interaction {
    if crosshair_point(state, cursor.position_in(bounds)).is_some() {
        return overlay_crosshair_interaction();
    }

    match &state.active {
        Some(Active::Resize { handle, .. }) => return cursor_for_handle(*handle),
        Some(Active::Move { .. }) => return mouse::Interaction::Grabbing,
        Some(Active::InitialDrag { .. }) => return overlay_crosshair_interaction(),
        None => {}
    }
    if let (Some(sel), Some(p)) = (state.selection, cursor.position_in(bounds)) {
        if let Some(h) = handle_at(sel, p) {
            return cursor_for_handle(h);
        }
        if rect_contains(sel, p) {
            return mouse::Interaction::Grab;
        }
    }
    overlay_crosshair_interaction()
}

fn overlay_crosshair_interaction() -> mouse::Interaction {
    mouse::Interaction::Hidden
}

#[cfg(test)]
mod tests {
    #[test]
    fn badge_moves_inside_when_flipped_toolbar_would_cover_it() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 800.0,
            height: 600.0,
        };
        let narrow_low = Rectangle {
            x: 10.0,
            y: 500.0,
            width: 100.0,
            height: 90.0,
        };
        let wide_low = Rectangle {
            x: 10.0,
            y: 500.0,
            width: 400.0,
            height: 90.0,
        };
        let narrow_high = Rectangle {
            x: 10.0,
            y: 100.0,
            width: 100.0,
            height: 90.0,
        };
        assert!(badge_clashes_with_toolbar(bounds, narrow_low));
        assert!(!badge_clashes_with_toolbar(bounds, wide_low));
        assert!(!badge_clashes_with_toolbar(bounds, narrow_high));
    }

    #[test]
    fn overlay_shortcuts_map_to_intents_and_nudges() {
        use crate::app::CaptureIntent as I;
        use iced::keyboard::{key::Named, Key, Modifiers};
        let ch = |c: &str| Key::Character(c.into());
        assert_eq!(
            overlay_shortcut(&ch("c"), Modifiers::COMMAND),
            Some(OverlayShortcut::Intent(I::CopyToClipboard))
        );
        assert_eq!(
            overlay_shortcut(&ch("c"), Modifiers::COMMAND | Modifiers::SHIFT),
            Some(OverlayShortcut::Intent(I::CopyTextDirect))
        );
        assert_eq!(
            overlay_shortcut(&ch("s"), Modifiers::COMMAND),
            Some(OverlayShortcut::Intent(I::SaveDirect))
        );
        assert_eq!(
            overlay_shortcut(&ch("p"), Modifiers::COMMAND),
            Some(OverlayShortcut::Intent(I::Pin))
        );
        assert_eq!(
            overlay_shortcut(&Key::Named(Named::ArrowLeft), Modifiers::empty()),
            Some(OverlayShortcut::Nudge { dx: -1.0, dy: 0.0 })
        );
        assert_eq!(
            overlay_shortcut(&Key::Named(Named::ArrowDown), Modifiers::SHIFT),
            Some(OverlayShortcut::Nudge { dx: 0.0, dy: 10.0 })
        );
        assert_eq!(overlay_shortcut(&ch("c"), Modifiers::empty()), None);
    }

    use super::*;

    #[test]
    fn hint_leads_with_the_instruction_and_always_offers_esc() {
        for (cli, small) in [(false, false), (true, false), (false, true)] {
            let segs = hint_segments(cli, small);
            assert!(matches!(segs[0], HintSeg::Text(_)), "instruction first");
            assert!(segs.iter().any(|s| matches!(s, HintSeg::Key("esc"))));
        }
        let normal = hint_segments(false, false);
        assert!(normal.contains(&HintSeg::Key("↵")));
        assert!(normal.contains(&HintSeg::Key("⇧")));
    }

    #[test]
    fn hint_variants_match_live_behavior() {
        let small = hint_segments(false, true);
        assert!(small.contains(&HintSeg::Text("Drag to select a larger area")));
        assert!(!small.contains(&HintSeg::Key("↵")));
        assert!(!hint_segments(true, false).contains(&HintSeg::Key("↵")));
    }

    #[test]
    fn seg_width_uses_the_drawn_font_advance() {
        let t = "Drag to select an area";
        let w = seg_width(&HintSeg::Text(t));
        assert!((w - t.chars().count() as f32 * 13.0 * 0.6).abs() < 1e-3);
        let k = seg_width(&HintSeg::Key("esc"));
        assert!((k - (3.0 * 11.0 * 0.6 + HINT_KEY_PAD)).abs() < 1e-3);
    }

    #[test]
    fn narrow_hint_keeps_instruction_and_esc() {
        let segs = fit_hint_segments(hint_segments(false, false), 260.0);
        assert!(matches!(segs[0], HintSeg::Text(_)));
        assert!(segs.contains(&HintSeg::Key("esc")));
        assert!(!segs.contains(&HintSeg::Key("↵")));
    }

    #[test]
    fn drag_meets_min_size_rejects_sub_threshold_drags() {
        // A near-zero "click" drag is discarded.
        assert!(!drag_meets_min_size(rectangle_from_two_points(
            Point::new(10.0, 10.0),
            Point::new(11.0, 11.0),
        )));
        // Tall-but-thin (and its transpose) are also rejected.
        assert!(!drag_meets_min_size(Rectangle {
            x: 0.0,
            y: 0.0,
            width: 2.0,
            height: 50.0,
        }));
        // Exactly at the threshold, and comfortably above it, commit.
        assert!(drag_meets_min_size(Rectangle {
            x: 0.0,
            y: 0.0,
            width: MIN_DRAG_SIZE,
            height: MIN_DRAG_SIZE,
        }));
        assert!(drag_meets_min_size(rectangle_from_two_points(
            Point::new(10.0, 10.0),
            Point::new(60.0, 40.0),
        )));
    }

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
    fn crosshair_is_visible_before_selection() {
        let point = Point::new(24.0, 36.0);
        assert_eq!(
            crosshair_point(&OverlayState::default(), Some(point)),
            Some(point)
        );
    }

    #[test]
    fn crosshair_stays_visible_during_initial_drag() {
        let point = Point::new(24.0, 36.0);
        let state = OverlayState {
            active: Some(Active::InitialDrag {
                anchor: Point::new(10.0, 10.0),
                current: point,
            }),
            ..Default::default()
        };

        assert_eq!(crosshair_point(&state, Some(point)), Some(point));
    }

    #[test]
    fn overlay_hides_os_cursor_where_canvas_crosshair_draws() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };

        assert_eq!(
            overlay_mouse_interaction(
                &OverlayState::default(),
                bounds,
                mouse::Cursor::Available(Point::new(50.0, 40.0)),
            ),
            mouse::Interaction::Hidden
        );
    }

    #[test]
    fn overlay_keeps_os_cursor_hidden_during_initial_drag() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        let state = OverlayState {
            active: Some(Active::InitialDrag {
                anchor: Point::new(10.0, 10.0),
                current: Point::new(50.0, 40.0),
            }),
            ..Default::default()
        };

        assert_eq!(
            overlay_mouse_interaction(
                &state,
                bounds,
                mouse::Cursor::Available(Point::new(50.0, 40.0)),
            ),
            mouse::Interaction::Hidden
        );
    }

    #[test]
    fn overlay_uses_handle_cursor_when_crosshair_is_hidden() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        let state = OverlayState {
            selection: Some(Rectangle {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            }),
            ..Default::default()
        };

        assert_eq!(
            overlay_mouse_interaction(
                &state,
                bounds,
                mouse::Cursor::Available(Point::new(10.0, 20.0)),
            ),
            mouse::Interaction::ResizingDiagonallyDown
        );
    }

    #[test]
    fn crosshair_hides_over_committed_selection_body_and_handles() {
        let state = OverlayState {
            selection: Some(Rectangle {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            }),
            ..Default::default()
        };

        assert_eq!(crosshair_point(&state, Some(Point::new(60.0, 45.0))), None);
        assert_eq!(crosshair_point(&state, Some(Point::new(10.0, 20.0))), None);
        assert_eq!(
            crosshair_point(&state, Some(Point::new(160.0, 90.0))),
            Some(Point::new(160.0, 90.0))
        );
    }

    #[test]
    fn crosshair_hides_while_moving_or_resizing() {
        let selection = Rectangle {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        let moving = OverlayState {
            selection: Some(selection),
            active: Some(Active::Move {
                start_cursor: Point::new(60.0, 45.0),
                original: selection,
            }),
            ..Default::default()
        };
        let resizing = OverlayState {
            selection: Some(selection),
            active: Some(Active::Resize {
                handle: Handle::SE,
                original: selection,
            }),
            ..Default::default()
        };

        assert_eq!(crosshair_point(&moving, Some(Point::new(80.0, 60.0))), None);
        assert_eq!(
            crosshair_point(&resizing, Some(Point::new(110.0, 70.0))),
            None
        );
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
    fn badge_sits_above_the_top_left_corner() {
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 800.0,
        };
        let sel = Rectangle {
            x: 100.0,
            y: 200.0,
            width: 300.0,
            height: 200.0,
        };
        assert_eq!(size_badge_origin(bounds, sel, 80.0, 20.0), (100.0, 174.0));
        let top = Rectangle { y: 4.0, ..sel };
        assert_eq!(size_badge_origin(bounds, top, 80.0, 20.0), (100.0, 10.0));
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
