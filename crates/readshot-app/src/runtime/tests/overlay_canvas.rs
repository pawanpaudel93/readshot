//! The capture overlay's canvas driven with real mouse and key events,
//! each message it publishes fed through `update` as the running app
//! would. The overlay is opened through its real path (a tray click,
//! then the display list) and each canvas program is built from the
//! app's state the way `overlay_view` builds it.

use super::*;
use iced::keyboard::{key::Named, Key, Modifiers};
use iced::widget::canvas::{Event, Program};
use iced::{mouse, Point, Rectangle};

use crate::app::CaptureIntent as I;

fn display(id: &str, x: f32, scale: f32) -> DisplayInfo {
    DisplayInfo {
        id: id.into(),
        bounds: readshot_core::geom::Rect::from_xywh(x, 0.0, 800.0, 600.0).unwrap(),
        scale,
        name: id.into(),
        is_primary: x == 0.0,
    }
}

fn r(x: f32, y: f32, w: f32, h: f32) -> readshot_core::geom::Rect {
    readshot_core::geom::Rect::from_xywh(x, y, w, h).unwrap()
}

fn overlay_count(app: &App) -> usize {
    app.windows
        .kinds()
        .filter(|k| *k == WindowKind::Overlay)
        .count()
}

/// Tray "Capture…", then the display list lands: one overlay per display.
fn open_overlays(app: &mut App, action: crate::tray::TrayAction, displays: Vec<DisplayInfo>) {
    let _ = update(app, Message::Tick(TickMsg::TrayAction(action)));
    assert!(app.overlay_opening, "the tray click lists the displays");
    list_displays(app, displays);
}

fn list_displays(app: &mut App, displays: Vec<DisplayInfo>) {
    let n = displays.len();
    let _ = update(
        app,
        Message::Capture(CaptureMsg::OverlayDisplaysListed(Ok(displays))),
    );
    assert!(!app.overlay_opening);
    assert_eq!(overlay_count(app), n);
}

fn key_event(key: Key, modifiers: Modifiers) -> Event {
    Event::Keyboard(iced::keyboard::Event::KeyPressed {
        modified_key: key.clone(),
        key,
        physical_key: iced::keyboard::key::Physical::Unidentified(
            iced::keyboard::key::NativeCode::Unidentified,
        ),
        location: iced::keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    })
}

/// Deliver `event` to `program` and, as the canvas widget does, pass
/// what it publishes to the app.
fn deliver<P: Program<Message>>(
    app: &mut App,
    program: &P,
    state: &mut P::State,
    bounds: Rectangle,
    event: Event,
    at: Option<Point>,
) -> Option<Message> {
    let cursor = at.map_or(mouse::Cursor::Unavailable, mouse::Cursor::Available);
    let message = program
        .update(state, &event, bounds, cursor)?
        .into_inner()
        .0?;
    let _ = update(app, message.clone());
    Some(message)
}

/// One display's region-selection canvas. Its state lives here (the
/// canvas widget's tree in the app); its program is rebuilt from the
/// app on every event, as `view()` rebuilds it.
struct OverlayCanvas {
    window: window::Id,
    state: crate::overlay::OverlayState,
}

impl OverlayCanvas {
    fn on(app: &App, display_id: &str) -> Self {
        let window = app
            .overlay_displays
            .iter()
            .find(|(_, d)| d.display_id == display_id)
            .map(|(id, _)| *id)
            .expect("an overlay on that display");
        Self {
            window,
            state: Default::default(),
        }
    }

    fn event(&mut self, app: &mut App, event: Event, at: Option<Point>) -> Option<Message> {
        let d = app.overlay_displays[&self.window].clone();
        let program = crate::overlay::OverlayProgram {
            display_id: d.display_id.clone(),
            scale: d.scale,
            auto_confirm_intent: overlay_auto_confirm_intent(app),
            shift_held: app.overlay_shift_held,
        };
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(d.width, d.height));
        deliver(app, &program, &mut self.state, bounds, event, at)
    }

    fn press(&mut self, app: &mut App, at: (f32, f32)) -> Option<Message> {
        let p = Point::new(at.0, at.1);
        self.event(
            app,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Some(p),
        )
    }

    fn move_to(&mut self, app: &mut App, at: (f32, f32)) -> Option<Message> {
        let p = Point::new(at.0, at.1);
        self.event(
            app,
            Event::Mouse(mouse::Event::CursorMoved { position: p }),
            Some(p),
        )
    }

    fn release(&mut self, app: &mut App, at: (f32, f32)) -> Option<Message> {
        let p = Point::new(at.0, at.1);
        self.event(
            app,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            Some(p),
        )
    }

    /// Press, move, release; what the release published.
    fn drag(&mut self, app: &mut App, from: (f32, f32), to: (f32, f32)) -> Option<Message> {
        let _ = self.press(app, from);
        let _ = self.move_to(app, to);
        self.release(app, to)
    }

    fn key(&mut self, app: &mut App, key: Key, modifiers: Modifiers) -> Option<Message> {
        self.event(app, key_event(key, modifiers), None)
    }
}

fn selected(message: Option<Message>) -> (String, readshot_core::geom::Rect, I) {
    match message {
        Some(Message::Capture(CaptureMsg::OverlaySelected {
            display_id,
            rect,
            intent,
        })) => (display_id, rect, intent),
        other => panic!("expected OverlaySelected, got {other:?}"),
    }
}

fn granted_app() -> App {
    build_app(Arc::new(FakePermissions::granted()))
}

/// CAP-02, CAP-04, OV-02, OV-10: a drag on the second display's overlay
/// selects there only; Enter captures it at that display's scale and
/// bounds, and the capture opens the editor on that display.
#[test]
fn a_drag_on_the_second_display_selects_there_and_captures_with_its_scale() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0), display("b", 800.0, 2.0)],
    );
    assert_eq!(app.pending_intent, Some(I::Editor), "tray capture → editor");
    let mut b = OverlayCanvas::on(&app, "b");
    let released = b.drag(&mut app, (100.0, 100.0), (300.0, 250.0));
    assert!(
        matches!(
            released,
            Some(Message::Capture(CaptureMsg::OverlaySelectionChanged { .. }))
        ),
        "mouse-up commits an editable selection: {released:?}"
    );
    assert_eq!(
        app.overlay_selections.get("b"),
        Some(&r(100.0, 100.0, 200.0, 150.0))
    );
    assert!(!app.overlay_selections.contains_key("a"));
    assert_eq!(overlay_count(&app), 2, "the overlays stay open");

    let (display_id, rect, intent) =
        selected(b.key(&mut app, Key::Named(Named::Enter), Modifiers::empty()));
    assert_eq!(
        (display_id.as_str(), rect, intent),
        ("b", r(100.0, 100.0, 200.0, 150.0), I::Editor)
    );
    assert_eq!(overlay_count(&app), 0, "every display's overlay closes");
    assert!(app.overlay_displays.is_empty());
    assert_eq!(app.pending_display_id.as_deref(), Some("b"));
    assert_eq!(app.pending_display_scale, Some(2.0));
    assert_eq!(app.pending_display_bounds, Some((800.0, 0.0, 800.0, 600.0)));
    assert!(app.pending_hide_cursor, "GUI captures hide the cursor");
    assert_eq!(app.last_region_display_id.as_deref(), Some("b"));

    // The runtime's follow-up (`Task::done(RegionRequested)`), then the
    // capture landing.
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionRequested {
            display_id: "b".into(),
            rect,
        }),
    );
    assert!(app.capture_in_flight);
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(400, 300)))),
    );
    assert!(!app.capture_in_flight);
    let ed = app.editor.as_ref().expect("the capture opens the editor");
    assert_eq!(ed.image_size(), (400, 300));
    assert_eq!(ed.source_display_bounds, Some((800.0, 0.0, 800.0, 600.0)));
    assert_eq!(app.pending_intent, None);
}

/// CAP-06, OV-10, OV-11: Enter or Space with no selection captures the
/// whole display of the overlay that has the keyboard.
#[test]
fn enter_or_space_with_no_selection_captures_the_whole_display() {
    for key in [Named::Enter, Named::Space] {
        let mut app = granted_app();
        open_overlays(
            &mut app,
            crate::tray::TrayAction::Capture,
            vec![display("a", 0.0, 2.0), display("b", 800.0, 1.0)],
        );
        let mut a = OverlayCanvas::on(&app, "a");
        let (display_id, rect, intent) =
            selected(a.key(&mut app, Key::Named(key), Modifiers::empty()));
        assert_eq!(
            (display_id.as_str(), rect, intent),
            ("a", r(0.0, 0.0, 800.0, 600.0), I::Editor),
            "{key:?}"
        );
        assert_eq!(overlay_count(&app), 0, "{key:?}");
        assert_eq!(app.pending_display_scale, Some(2.0), "{key:?}");
        assert_eq!(
            app.last_regions.get("a").map(|l| l.rect),
            Some(r(0.0, 0.0, 800.0, 600.0))
        );
    }
}

/// OV-02: with Shift held (tracked by the runtime, not the canvas) the
/// first drag is a square; the longer side wins.
#[test]
fn shift_squares_the_first_drag() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0)],
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayShiftChanged(true)),
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let _ = a.drag(&mut app, (100.0, 100.0), (300.0, 150.0));
    assert_eq!(
        app.overlay_selections.get("a"),
        Some(&r(100.0, 100.0, 200.0, 200.0))
    );
}

/// OV-03: a click (or a drag under 4 px) commits nothing: the overlay
/// stays open and the hint says to drag a larger area. In an
/// auto-confirm overlay (tray scroll capture) it cancels instead.
#[test]
fn a_click_without_a_drag_keeps_the_overlay_open_and_says_why() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let released = a.drag(&mut app, (100.0, 100.0), (102.0, 103.0));
    assert!(released.is_none(), "nothing published: {released:?}");
    assert!(a.state.too_small_hint, "the hint asks for a larger area");
    assert!(a.state.selection.is_none());
    assert_eq!(overlay_count(&app), 1);
    assert!(app.overlay_selections.is_empty(), "no action bar");
    assert_eq!(app.pending_intent, Some(I::Editor), "still capturing");
    // The next drag clears the hint and selects.
    let _ = a.drag(&mut app, (100.0, 100.0), (200.0, 180.0));
    assert!(!a.state.too_small_hint);
    assert_eq!(
        app.overlay_selections.get("a"),
        Some(&r(100.0, 100.0, 100.0, 80.0))
    );

    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::ScrollCapture,
        vec![display("a", 0.0, 1.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let released = a.drag(&mut app, (100.0, 100.0), (101.0, 101.0));
    assert!(matches!(
        released,
        Some(Message::Capture(CaptureMsg::OverlayCancelled))
    ));
    assert_eq!(overlay_count(&app), 0);
    assert!(app.scroll_session.is_none());
    assert_eq!(app.pending_intent, None);
}

/// OV-04, OV-05: a corner handle resizes from that corner, dragging an
/// edge past the opposite one flips the rect, and dragging the body
/// moves it, clamped inside the display. The action bar follows.
#[test]
fn handles_resize_and_the_body_moves_the_selection() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let _ = a.drag(&mut app, (100.0, 100.0), (300.0, 250.0));
    let sel = |app: &App| app.overlay_selections.get("a").copied();

    // SE corner: only the right and bottom edges move.
    let _ = a.drag(&mut app, (300.0, 250.0), (350.0, 400.0));
    assert_eq!(sel(&app), Some(r(100.0, 100.0, 250.0, 300.0)));

    // W edge dragged past the E edge (x 350): the rect flips.
    let _ = a.drag(&mut app, (100.0, 250.0), (450.0, 250.0));
    assert_eq!(sel(&app), Some(r(350.0, 100.0, 100.0, 300.0)));

    // The body moves the rect; dragged toward the corner it stops at
    // the display's edges.
    let _ = a.drag(&mut app, (400.0, 250.0), (790.0, 590.0));
    assert_eq!(sel(&app), Some(r(700.0, 300.0, 100.0, 300.0)));
    let _ = a.drag(&mut app, (750.0, 400.0), (700.0, 380.0));
    assert_eq!(sel(&app), Some(r(650.0, 280.0, 100.0, 300.0)));
    assert_eq!(overlay_count(&app), 1, "refining never confirms");
}

/// OV-06: a click outside the selection drops it (and its action bar)
/// and starts a new drag from the click point.
#[test]
fn a_click_outside_drops_the_selection_and_starts_a_new_drag() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let _ = a.drag(&mut app, (100.0, 100.0), (300.0, 250.0));
    assert!(app.overlay_selections.contains_key("a"));
    let pressed = a.press(&mut app, (600.0, 50.0));
    assert!(matches!(
        pressed,
        Some(Message::Capture(CaptureMsg::OverlaySelectionChanged {
            rect: None,
            ..
        }))
    ));
    assert!(
        !app.overlay_selections.contains_key("a"),
        "the action bar goes"
    );
    let _ = a.move_to(&mut app, (700.0, 150.0));
    let _ = a.release(&mut app, (700.0, 150.0));
    assert_eq!(
        app.overlay_selections.get("a"),
        Some(&r(600.0, 50.0, 100.0, 100.0))
    );
}

/// OV-12: esc on one display closes every display's overlay and clears
/// the pending capture.
#[test]
fn esc_closes_every_overlay_and_clears_the_pending_capture() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0), display("b", 800.0, 2.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let _ = a.drag(&mut app, (100.0, 100.0), (300.0, 250.0));
    let esc = a.key(&mut app, Key::Named(Named::Escape), Modifiers::empty());
    assert!(matches!(
        esc,
        Some(Message::Capture(CaptureMsg::OverlayCancelled))
    ));
    assert_eq!(overlay_count(&app), 0);
    assert!(app.overlay_displays.is_empty());
    assert!(app.overlay_selections.is_empty());
    assert_eq!(app.pending_intent, None);
    assert!(!app.capture_in_flight);
    assert!(app.editor.is_none());
}

/// OV-13, OV-14, OV-15: with no pointer, an arrow starts a centred
/// selection; arrows nudge it (⇧ = 10 px), ⌥ + arrows resize it from the
/// bottom-right (⌥⇧ = 10 px), and Enter confirms the result.
#[test]
fn the_keyboard_alone_selects_moves_resizes_and_confirms() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let sel = |app: &App| app.overlay_selections.get("a").copied();
    let arrow = |n| Key::Named(n);
    let _ = a.key(&mut app, arrow(Named::ArrowRight), Modifiers::empty());
    assert_eq!(sel(&app), Some(r(200.0, 150.0, 400.0, 300.0)), "centred");
    let _ = a.key(&mut app, arrow(Named::ArrowRight), Modifiers::empty());
    assert_eq!(sel(&app), Some(r(201.0, 150.0, 400.0, 300.0)));
    let _ = a.key(&mut app, arrow(Named::ArrowDown), Modifiers::SHIFT);
    assert_eq!(sel(&app), Some(r(201.0, 160.0, 400.0, 300.0)));
    let _ = a.key(&mut app, arrow(Named::ArrowRight), Modifiers::ALT);
    assert_eq!(sel(&app), Some(r(201.0, 160.0, 401.0, 300.0)));
    let _ = a.key(
        &mut app,
        arrow(Named::ArrowUp),
        Modifiers::ALT | Modifiers::SHIFT,
    );
    assert_eq!(sel(&app), Some(r(201.0, 160.0, 401.0, 290.0)));
    // Nudged against the left edge, it stops there.
    for _ in 0..30 {
        let _ = a.key(&mut app, arrow(Named::ArrowLeft), Modifiers::SHIFT);
    }
    assert_eq!(sel(&app), Some(r(0.0, 160.0, 401.0, 290.0)));
    let (_, rect, intent) = selected(a.key(&mut app, arrow(Named::Enter), Modifiers::empty()));
    assert_eq!((rect, intent), (r(0.0, 160.0, 401.0, 290.0), I::Editor));
}

/// OV-16 – OV-20: each ⌘ key confirms the selection with its intent,
/// which the runtime keeps for the capture (⌘R starts a scroll session).
#[test]
fn each_intent_key_confirms_the_selection_with_its_intent() {
    let cases = [
        ("c", Modifiers::COMMAND, I::CopyToClipboard),
        (
            "c",
            Modifiers::COMMAND | Modifiers::SHIFT,
            I::CopyTextDirect,
        ),
        ("s", Modifiers::COMMAND, I::SaveDirect),
        ("p", Modifiers::COMMAND, I::Pin),
        ("r", Modifiers::COMMAND, I::ScrollCapture),
    ];
    for (letter, modifiers, want) in cases {
        let mut app = granted_app();
        open_overlays(
            &mut app,
            crate::tray::TrayAction::Capture,
            vec![display("a", 0.0, 2.0)],
        );
        let mut a = OverlayCanvas::on(&app, "a");
        let _ = a.drag(&mut app, (100.0, 100.0), (300.0, 250.0));
        let (_, rect, intent) = selected(a.key(&mut app, Key::Character(letter.into()), modifiers));
        assert_eq!(
            (rect, intent),
            (r(100.0, 100.0, 200.0, 150.0), want),
            "{letter} {modifiers:?}"
        );
        assert_eq!(overlay_count(&app), 0, "{want:?}");
        if want == I::ScrollCapture {
            let s = app.scroll_session.as_ref().expect("⌘R starts a session");
            assert_eq!(s.rect, rect);
            assert_eq!(s.scale, 2.0);
            assert!(s.region_window_id.is_some());
            assert!(app.windows.kinds().any(|k| k == WindowKind::ScrollHud));
            assert_eq!(app.pending_intent, None);
        } else {
            assert_eq!(app.pending_intent, Some(want));
            assert!(app.scroll_session.is_none());
        }
    }
}

/// OV-21: intent keys need a selection, and ⌘⌥ / Ctrl+Alt chords (AltGr)
/// are not intent keys.
#[test]
fn intent_keys_without_a_selection_or_with_alt_do_nothing() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::Capture,
        vec![display("a", 0.0, 1.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    for letter in ["c", "s", "p", "r"] {
        let sent = a.key(&mut app, Key::Character(letter.into()), Modifiers::COMMAND);
        assert!(sent.is_none(), "⌘{letter} with no selection: {sent:?}");
    }
    assert!(a.state.selection.is_none());
    let _ = a.drag(&mut app, (100.0, 100.0), (300.0, 250.0));
    for modifiers in [
        Modifiers::COMMAND | Modifiers::ALT,
        Modifiers::CTRL | Modifiers::ALT,
    ] {
        let sent = a.key(&mut app, Key::Character("c".into()), modifiers);
        assert!(sent.is_none(), "{modifiers:?}: {sent:?}");
    }
    assert_eq!(overlay_count(&app), 1);
    assert_eq!(app.pending_intent, Some(I::Editor));
}

/// OV-22, SC-01: tray "Scrolling Capture…" pre-sets the intent, so the
/// mouse-up starts the session at once: no editable selection, so no
/// action bar, and the HUD and region outline open.
#[test]
fn a_tray_scroll_capture_starts_the_session_on_mouse_up() {
    let mut app = granted_app();
    open_overlays(
        &mut app,
        crate::tray::TrayAction::ScrollCapture,
        vec![display("a", 0.0, 2.0)],
    );
    let mut a = OverlayCanvas::on(&app, "a");
    let _ = a.press(&mut app, (100.0, 100.0));
    let _ = a.move_to(&mut app, (300.0, 400.0));
    assert!(app.overlay_selections.is_empty());
    let (_, rect, intent) = selected(a.release(&mut app, (300.0, 400.0)));
    assert_eq!(
        (rect, intent),
        (r(100.0, 100.0, 200.0, 300.0), I::ScrollCapture)
    );
    assert_eq!(overlay_count(&app), 0);
    let s = app.scroll_session.as_ref().expect("the session starts");
    assert_eq!(s.display_id, "a");
    assert_eq!(s.display_size, Some((0.0, 0.0, 800.0, 600.0)));
    // The HUD's id is recorded when its window reports ready.
    let region = s.region_window_id.expect("outline opened");
    assert_eq!(app.windows.kind(region), Some(WindowKind::ScrollRegion));
    assert!(app.windows.kinds().any(|k| k == WindowKind::ScrollHud));
    // A second tray scroll capture while it runs opens nothing.
    let _ = update(
        &mut app,
        Message::Tick(TickMsg::TrayAction(crate::tray::TrayAction::ScrollCapture)),
    );
    assert!(!app.overlay_opening);
    assert_eq!(overlay_count(&app), 0);
}

/// OV-23: an Add Capture overlay confirms on mouse-up into its editor,
/// with no action bar, and the capture joins that editor.
#[test]
fn an_add_capture_overlay_confirms_on_mouse_up_into_its_editor() {
    let mut app = granted_app();
    start_add_capture(&mut app);
    let session = app.editor.as_ref().unwrap().session_id;
    // The request hides the editor, then asks for the overlay.
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    assert!(app.overlay_opening);
    assert_eq!(
        overlay_auto_confirm_intent(&app),
        Some(I::CombineIntoEditor)
    );
    list_displays(&mut app, vec![display("a", 0.0, 1.0)]);
    let mut a = OverlayCanvas::on(&app, "a");
    let (_, _, intent) = selected(a.drag(&mut app, (10.0, 10.0), (74.0, 26.0)));
    assert_eq!(intent, I::CombineIntoEditor);
    assert!(app.overlay_selections.is_empty(), "no action bar");
    assert!(app.combine_capture.is_some_and(|cc| cc.selected));
    assert_the_combine_capture_lands(app, session);
}

// ---- Capture Window… (the picker overlay) ----

fn stacked(id: &str, display: &str, x: f32) -> readshot_capture::StackedWindow {
    readshot_capture::StackedWindow {
        info: readshot_capture::WindowInfo {
            id: readshot_capture::WindowId(id.into()),
            title: "Docs".into(),
            app_name: "Safari".into(),
            display_id: display.into(),
            bounds: r(x, 40.0, 300.0, 200.0),
        },
        pickable: true,
    }
}

/// The picker canvas for `display_id`'s overlay, built as `overlay_view`
/// builds it, with its bounds.
fn picker(app: &App, display_id: &str) -> (crate::overlay::WindowPickProgram, Rectangle) {
    let d = app
        .overlay_displays
        .values()
        .find(|d| d.display_id == display_id)
        .expect("a picker overlay on that display");
    let program = crate::overlay::WindowPickProgram {
        windows: crate::overlay::pickable_windows(
            app.window_pick.as_ref().expect("picking"),
            (d.origin_x, d.origin_y, d.width, d.height),
        ),
    };
    let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(d.width, d.height));
    (program, bounds)
}

fn open_window_picker() -> App {
    let mut app = granted_app();
    let _ = update(
        &mut app,
        Message::Tick(TickMsg::TrayAction(crate::tray::TrayAction::CaptureWindow)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowPickerListed(Ok((
            vec![display("a", 0.0, 1.0), display("b", 800.0, 2.0)],
            // Front to back; w0 sits right of w2 on display b.
            vec![
                stacked("w0", "b", 1300.0),
                stacked("w1", "a", 10.0),
                stacked("w2", "b", 900.0),
            ],
        )))),
    );
    assert_eq!(overlay_count(&app), 2);
    app
}

/// CAP-07: in the window picker a click on a window (in that display's
/// local coordinates) captures it on its display; esc cancels.
#[test]
fn clicking_a_window_in_the_picker_captures_it_on_its_display() {
    let mut app = open_window_picker();
    let (program, bounds) = picker(&app, "b");
    let mut state = Default::default();
    // w2 is at global x 900–1200: local x 100–400 on display b.
    let at = Some(Point::new(150.0, 100.0));
    let _ = deliver(
        &mut app,
        &program,
        &mut state,
        bounds,
        Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(150.0, 100.0),
        }),
        at,
    );
    let picked = deliver(
        &mut app,
        &program,
        &mut state,
        bounds,
        Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        at,
    );
    assert!(
        matches!(
            &picked,
            Some(Message::Capture(CaptureMsg::WindowPicked(id))) if id.0 == "w2"
        ),
        "{picked:?}"
    );
    assert_eq!(overlay_count(&app), 0);
    assert!(app.window_pick.is_none());
    assert!(app.capture_in_flight);
    assert_eq!(app.pending_display_id.as_deref(), Some("b"));
    assert_eq!(app.pending_display_scale, Some(2.0));

    // From the keyboard: Tab lights the first window, Enter picks it.
    let mut app = open_window_picker();
    let (program, bounds) = picker(&app, "a");
    let mut state = Default::default();
    let tab = deliver(
        &mut app,
        &program,
        &mut state,
        bounds,
        key_event(Key::Named(Named::Tab), Modifiers::empty()),
        None,
    );
    assert!(tab.is_none(), "Tab only lights a window");
    let picked = deliver(
        &mut app,
        &program,
        &mut state,
        bounds,
        key_event(Key::Named(Named::Enter), Modifiers::empty()),
        None,
    );
    assert!(matches!(
        &picked,
        Some(Message::Capture(CaptureMsg::WindowPicked(id))) if id.0 == "w1"
    ));
    assert_eq!(app.pending_display_id.as_deref(), Some("a"));

    // Esc ends the pick with nothing captured.
    let mut app = open_window_picker();
    let (program, bounds) = picker(&app, "a");
    let mut state = Default::default();
    let _ = deliver(
        &mut app,
        &program,
        &mut state,
        bounds,
        key_event(Key::Named(Named::Escape), Modifiers::empty()),
        None,
    );
    assert_eq!(overlay_count(&app), 0);
    assert!(app.window_pick.is_none());
    assert!(!app.capture_in_flight);
    assert_eq!(app.pending_intent, None);
}
