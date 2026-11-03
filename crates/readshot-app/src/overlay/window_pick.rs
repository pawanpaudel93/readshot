//! Window picking in the capture overlay (tray "Capture Window…" and the
//! window-capture shortcut).
//!
//! The runtime lists the windows first (front to back), then opens the
//! usual one overlay per display with this program instead of the region
//! selector. The window under the pointer is lit (the veil is cut away
//! over it and it gets an outline and its name); a click captures it.
//! From the keyboard, Tab / ⇧Tab and the arrow keys walk every pickable
//! window on this display, covered ones included (the pointer can only
//! reach the frontmost; the hint says so), ↵ or Space captures the lit one, esc cancels (WCAG
//! 2.1.1). The captured image goes through the normal editor and History
//! flow ([`CaptureMsg::WindowPicked`]).

use super::*;
use readshot_capture::WindowId;

/// One window as this display's overlay sees it: its id, its visible
/// part in window-local logical pixels, the name to show, and whether it
/// can be picked (a panel, the Dock or the menu bar only covers what is
/// behind it).
#[derive(Clone, Debug, PartialEq)]
pub struct PickableWindow {
    pub id: WindowId,
    pub rect: Rectangle,
    pub label: String,
    pub pickable: bool,
}

/// The windows of one display, front to back.
#[derive(Clone, Debug, Default)]
pub struct WindowPickProgram {
    pub windows: Vec<PickableWindow>,
}

#[derive(Debug, Default)]
pub struct WindowPickState {
    /// Index into `windows` of the lit window.
    hovered: Option<usize>,
}

/// This display's pickable windows from the global front-to-back list:
/// each window's part inside the display (`display` is its global
/// logical `(x, y, width, height)`), in window-local coordinates.
/// Windows not on the display are dropped; the order is kept.
pub(crate) fn pickable_windows(
    windows: &[readshot_capture::StackedWindow],
    display: (f32, f32, f32, f32),
) -> Vec<PickableWindow> {
    let (dx, dy, dw, dh) = display;
    windows
        .iter()
        .filter_map(|stacked| {
            let w = &stacked.info;
            let b = w.bounds;
            let x0 = b.x().max(dx);
            let y0 = b.y().max(dy);
            let x1 = b.right().min(dx + dw);
            let y1 = b.bottom().min(dy + dh);
            (x1 > x0 && y1 > y0).then(|| PickableWindow {
                id: w.id.clone(),
                rect: Rectangle {
                    x: x0 - dx,
                    y: y0 - dy,
                    width: x1 - x0,
                    height: y1 - y0,
                },
                label: window_label(&w.app_name, &w.title),
                pickable: stacked.pickable,
            })
        })
        .collect()
}

/// "App — Title", or whichever of the two is known.
fn window_label(app: &str, title: &str) -> String {
    match (app.trim(), title.trim()) {
        ("", "") => "Window".to_string(),
        (a, "") => a.to_string(),
        ("", t) => t.to_string(),
        (a, t) => format!("{a} — {t}"),
    }
}

/// The window picked at `p`: the frontmost window there, if it can be
/// picked. A panel, the Dock or the menu bar in front blocks the pick of
/// whatever is behind it.
pub(crate) fn window_index_at(windows: &[PickableWindow], p: Point) -> Option<usize> {
    windows
        .iter()
        .position(|w| rect_contains(w.rect, p))
        .filter(|&i| windows[i].pickable)
}

/// The next (`forward`) or previous pickable window after `from`,
/// wrapping; `None` when none can be picked.
fn step_pickable(from: Option<usize>, windows: &[PickableWindow], forward: bool) -> Option<usize> {
    let len = windows.len();
    if len == 0 {
        return None;
    }
    let start = match (from, forward) {
        (None, true) => len - 1,
        (None, false) => 0,
        (Some(i), _) => i,
    };
    (1..=len)
        .map(|k| {
            if forward {
                (start + k) % len
            } else {
                (start + len * k - k) % len
            }
        })
        .find(|&i| windows[i].pickable)
}

/// The picker's hint pill.
pub(crate) fn window_pick_hint_segments() -> Vec<HintSeg> {
    use HintSeg::*;
    vec![
        Text("Click a window, or press Tab to reach every window"),
        Key("Tab"),
        Muted("Next window"),
        Key(crate::runtime::RETURN_KEY),
        Muted("Capture"),
        Key(crate::runtime::ESC_KEY),
        Muted("Cancel"),
    ]
}

impl WindowPickProgram {
    fn pick(&self, index: usize) -> Option<Action<Message>> {
        let w = self.windows.get(index).filter(|w| w.pickable)?;
        Some(
            Action::publish(Message::Capture(CaptureMsg::WindowPicked(w.id.clone()))).and_capture(),
        )
    }
}

impl Program<Message> for WindowPickProgram {
    type State = WindowPickState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => match key {
                Key::Named(Named::Escape) => Some(
                    Action::publish(Message::Capture(CaptureMsg::OverlayCancelled)).and_capture(),
                ),
                Key::Named(Named::Enter | Named::Space) => state.hovered.and_then(|i| self.pick(i)),
                Key::Named(
                    Named::Tab
                    | Named::ArrowDown
                    | Named::ArrowRight
                    | Named::ArrowUp
                    | Named::ArrowLeft,
                ) => {
                    let forward = match key {
                        Key::Named(Named::Tab) => !modifiers.shift(),
                        Key::Named(Named::ArrowDown | Named::ArrowRight) => true,
                        _ => false,
                    };
                    state.hovered = step_pickable(state.hovered, &self.windows, forward);
                    Some(Action::request_redraw().and_capture())
                }
                _ => None,
            },
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let p = cursor.position_in(bounds)?;
                let hovered = window_index_at(&self.windows, p);
                (hovered != state.hovered).then(|| {
                    state.hovered = hovered;
                    Action::request_redraw()
                })
            }
            // The pointer went to another display: this one's window is
            // no longer the one under it.
            Event::Mouse(mouse::Event::CursorLeft) => {
                state.hovered.take().map(|_| Action::request_redraw())
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                match window_index_at(&self.windows, p) {
                    Some(i) => self.pick(i),
                    // A click on the desktop between windows does nothing.
                    None => Some(Action::request_redraw().and_capture()),
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
        let lit = state.hovered.and_then(|i| self.windows.get(i));
        // The veil, cut away over the lit window (even-odd).
        let veil = Path::new(|b| {
            b.rectangle(Point::ORIGIN, bounds.size());
            if let Some(w) = lit {
                b.rectangle(Point::new(w.rect.x, w.rect.y), w.rect.size());
            }
        });
        frame.fill(
            &veil,
            Fill {
                style: VEIL.into(),
                rule: iced::widget::canvas::fill::Rule::EvenOdd,
            },
        );
        if let Some(w) = lit {
            let outline = Path::rectangle(Point::new(w.rect.x, w.rect.y), w.rect.size());
            frame.stroke(
                &outline,
                Stroke::default()
                    .with_color(Color::from_rgba(0.0, 0.0, 0.0, 0.45))
                    .with_width(4.0),
            );
            frame.stroke(
                &outline,
                Stroke::default().with_color(Color::WHITE).with_width(2.0),
            );
            let hint = hint_pill_rect(bounds, window_pick_hint_segments()).1;
            draw_window_label(&mut frame, bounds, w, hint);
        }
        draw_overlay_hint(&mut frame, bounds, window_pick_hint_segments());
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Self::State,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.hovered.is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

/// Advance of one Latin glyph of the 11.5 px label, and the pill's
/// horizontal padding.
const LABEL_ADVANCE: f32 = 7.0;
const LABEL_PAD: f32 = 16.0;

/// Advances of `c` in the label: a wide glyph (CJK, Hangul, emoji…)
/// draws about one em, twice a Latin one.
/// shortcut: a code-point estimate, not shaping; measure with iced's
/// `Paragraph` if the workspace ever enables iced's `advanced` feature.
fn label_units(c: char) -> f32 {
    if c as u32 >= 0x1100 {
        2.0
    } else {
        1.0
    }
}

/// The label as the pill shows it and the pill's width: cut with "…" to
/// 60 characters and to `max_w`.
fn fit_window_label(label: &str, max_w: f32) -> (String, f32) {
    const MAX_CHARS: usize = 60;
    let width = |units: f32| LABEL_PAD + units * LABEL_ADVANCE;
    let units: f32 = label.chars().take(MAX_CHARS + 1).map(label_units).sum();
    if label.chars().count() <= MAX_CHARS && width(units) <= max_w {
        return (label.to_string(), width(units));
    }
    let mut out = String::new();
    let mut used = 1.0; // the ellipsis
    for c in label.chars().take(MAX_CHARS - 1) {
        if width(used + label_units(c)) > max_w {
            break;
        }
        used += label_units(c);
        out.push(c);
    }
    out.push('…');
    (out, width(used))
}

/// Where the name pill goes: above the window's top-left corner (inside
/// it when there is no room above), but below the hint band when it
/// would sit under the hint pill.
fn window_label_rect(
    bounds: Rectangle,
    window: Rectangle,
    badge_w: f32,
    badge_h: f32,
    hint: Rectangle,
) -> Rectangle {
    let (bx, by) = size_badge_origin(bounds, window, badge_w, badge_h);
    let rect = Rectangle::new(Point::new(bx, by), iced::Size::new(badge_w, badge_h));
    if rect.intersects(&hint) {
        Rectangle::new(Point::new(bx, hint.y + hint.height + 6.0), rect.size())
    } else {
        rect
    }
}

/// The lit window's name in a pill at its top-left corner, inside the
/// window when there is no room above it, never under the hint pill.
fn draw_window_label(
    frame: &mut Frame<Renderer>,
    bounds: Rectangle,
    w: &PickableWindow,
    hint: Rectangle,
) {
    let max_w = w
        .rect
        .width
        .min(bounds.width)
        .max(LABEL_PAD + 2.0 * LABEL_ADVANCE);
    let (label, badge_w) = fit_window_label(&w.label, max_w);
    let badge_h = 22.0;
    let pill = window_label_rect(bounds, w.rect, badge_w, badge_h, hint);
    let (bx, by) = (pill.x, pill.y);
    let badge = Path::rounded_rectangle(pill.position(), pill.size(), 6.0.into());
    frame.fill(&badge, CHROME_FILL);
    frame.stroke(
        &badge,
        Stroke::default()
            .with_color(DARK.float_border)
            .with_width(1.0),
    );
    frame.fill_text(CanvasText {
        content: label,
        position: Point::new(bx + 8.0, by + 4.0),
        color: DARK.t1,
        size: iced::Pixels(11.5),
        font: iced::Font::with_name(readshot_core::render::FONT_FAMILY),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use readshot_capture::{StackedWindow, WindowInfo};
    use readshot_core::geom::Rect;

    fn info(id: &str, x: f32, y: f32, w: f32, h: f32) -> StackedWindow {
        StackedWindow {
            info: WindowInfo {
                id: WindowId(id.into()),
                title: format!("{id} title"),
                app_name: "App".into(),
                display_id: Some("d".into()),
                bounds: Rect::from_xywh(x, y, w, h).unwrap(),
            },
            pickable: true,
        }
    }

    fn panel(id: &str, x: f32, y: f32, w: f32, h: f32) -> StackedWindow {
        StackedWindow {
            pickable: false,
            ..info(id, x, y, w, h)
        }
    }

    #[test]
    fn a_panel_or_dock_over_a_window_blocks_the_pick() {
        let display = (0.0, 0.0, 1000.0, 800.0);
        let windows = [
            panel("dock", 200.0, 740.0, 600.0, 60.0),
            info("app", 0.0, 0.0, 1000.0, 800.0),
        ];
        let picks = pickable_windows(&windows, display);
        // Over the Dock: nothing is lit, nothing is picked.
        assert_eq!(window_index_at(&picks, Point::new(300.0, 760.0)), None);
        // Beside it: the window.
        assert_eq!(window_index_at(&picks, Point::new(100.0, 760.0)), Some(1));
        // The keyboard only walks windows that can be picked.
        assert_eq!(step_pickable(None, &picks, true), Some(1));
        assert_eq!(step_pickable(Some(1), &picks, true), Some(1));
        assert_eq!(step_pickable(Some(1), &picks, false), Some(1));
    }

    #[test]
    fn leaving_a_display_clears_its_highlight() {
        let program = WindowPickProgram {
            windows: pickable_windows(
                &[info("w", 0.0, 0.0, 200.0, 200.0)],
                (0.0, 0.0, 400.0, 400.0),
            ),
        };
        let bounds = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 400.0,
            height: 400.0,
        };
        let mut state = WindowPickState::default();
        let moved = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(50.0, 50.0),
        });
        let _ = program.update(
            &mut state,
            &moved,
            bounds,
            mouse::Cursor::Available(Point::new(50.0, 50.0)),
        );
        assert_eq!(state.hovered, Some(0));
        let left = Event::Mouse(mouse::Event::CursorLeft);
        let action = program.update(&mut state, &left, bounds, mouse::Cursor::Unavailable);
        assert!(action.is_some(), "a redraw takes the highlight down");
        assert_eq!(state.hovered, None);
    }

    #[test]
    fn windows_are_clipped_to_the_display_and_made_local() {
        // A second display at x = 1440.
        let display = (1440.0, 0.0, 1920.0, 1080.0);
        let windows = [
            info("left-only", 100.0, 100.0, 300.0, 200.0),
            info("straddles", 1300.0, 50.0, 400.0, 300.0),
            info("inside", 1500.0, 500.0, 200.0, 100.0),
        ];
        let picks = pickable_windows(&windows, display);
        let ids: Vec<&str> = picks.iter().map(|p| p.id.0.as_str()).collect();
        assert_eq!(ids, ["straddles", "inside"]);
        assert_eq!(
            picks[0].rect,
            Rectangle {
                x: 0.0,
                y: 50.0,
                width: 260.0,
                height: 300.0
            }
        );
        assert_eq!(picks[1].rect.x, 60.0);
        assert_eq!(picks[1].label, "App — inside title");
    }

    #[test]
    fn the_front_window_under_the_pointer_is_the_one_picked() {
        let display = (0.0, 0.0, 1000.0, 800.0);
        let windows = [
            info("front", 100.0, 100.0, 200.0, 200.0),
            info("back", 0.0, 0.0, 600.0, 500.0),
        ];
        let picks = pickable_windows(&windows, display);
        assert_eq!(window_index_at(&picks, Point::new(150.0, 150.0)), Some(0));
        assert_eq!(window_index_at(&picks, Point::new(50.0, 50.0)), Some(1));
        assert_eq!(window_index_at(&picks, Point::new(900.0, 700.0)), None);
    }

    #[test]
    fn the_keyboard_walks_the_windows_and_wraps() {
        let display = (0.0, 0.0, 1000.0, 800.0);
        let three = pickable_windows(
            &[
                info("a", 0.0, 0.0, 10.0, 10.0),
                info("b", 20.0, 0.0, 10.0, 10.0),
                info("c", 40.0, 0.0, 10.0, 10.0),
            ],
            display,
        );
        assert_eq!(step_pickable(None, &three, true), Some(0));
        assert_eq!(step_pickable(None, &three, false), Some(2));
        assert_eq!(step_pickable(Some(2), &three, true), Some(0));
        assert_eq!(step_pickable(Some(0), &three, false), Some(2));
        assert_eq!(step_pickable(Some(0), &[], true), None);
    }

    #[test]
    fn labels_use_what_is_known() {
        assert_eq!(window_label("Safari", "Docs"), "Safari — Docs");
        assert_eq!(window_label("Safari", " "), "Safari");
        assert_eq!(window_label("", "Docs"), "Docs");
        assert_eq!(window_label("", ""), "Window");
    }

    #[test]
    fn the_hint_says_the_keys_reach_every_window() {
        // Tab walks every pickable window, covered ones included, so the
        // instruction itself (never dropped when the pill is narrow)
        // names it, not only clicking.
        let segs = window_pick_hint_segments();
        let HintSeg::Text(instruction) = segs[0] else {
            panic!("the hint starts with its instruction");
        };
        assert!(instruction.contains("Tab"), "{instruction}");
        assert!(instruction.contains("every window"), "{instruction}");
    }

    #[test]
    fn the_hint_names_the_way_out() {
        let segs = window_pick_hint_segments();
        assert!(segs.contains(&HintSeg::Key(crate::runtime::ESC_KEY)));
        assert!(segs.contains(&HintSeg::Muted("Cancel")));
    }

    /// C#2: a wide glyph (CJK, emoji) takes about twice a Latin one, so
    /// the pill is sized for it, and a long name is cut with "…" to the
    /// window's width.
    #[test]
    fn the_name_pill_fits_wide_glyphs_and_the_window() {
        let (_, latin) = fit_window_label("abcd", 1000.0);
        let (_, cjk) = fit_window_label("微信聊天", 1000.0);
        assert_eq!(latin, LABEL_PAD + 4.0 * LABEL_ADVANCE);
        assert_eq!(cjk, LABEL_PAD + 8.0 * LABEL_ADVANCE);
        let (text, w) = fit_window_label(&"微".repeat(40), 200.0);
        assert!(w <= 200.0, "{w}");
        assert!(text.ends_with('…'), "{text}");
    }

    /// C#1: the name pill never sits under the hint pill: one that would
    /// overlap it moves below the hint band.
    #[test]
    fn the_name_pill_never_sits_under_the_hint() {
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(1200.0, 800.0));
        let hint = hint_pill_rect(bounds, window_pick_hint_segments()).1;
        // A centred dialog whose top sits just under the hint band.
        let window = Rectangle::new(Point::new(500.0, 80.0), iced::Size::new(300.0, 200.0));
        let pill = window_label_rect(bounds, window, 120.0, 22.0, hint);
        assert!(!pill.intersects(&hint), "{pill:?} under {hint:?}");
        assert!(pill.y >= hint.y + hint.height);
        // Away from the hint it keeps its place above the window.
        let low = Rectangle::new(Point::new(500.0, 400.0), iced::Size::new(300.0, 200.0));
        let pill = window_label_rect(bounds, low, 120.0, 22.0, hint);
        assert_eq!((pill.x, pill.y), (500.0, 400.0 - 22.0 - 6.0));
    }
}
