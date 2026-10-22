//! Where a capture goes: the tray and hotkey entry points, each overlay
//! intent landing through `RegionCaptured` (copy, copy text, save, pin),
//! pin windows, and the scroll session's limits.
//!
//! The tasks `update` returns are never run here, so nothing touches the
//! clipboard, a save dialog or the real History: each test feeds back the
//! message the task would have sent.

use super::*;
use crate::app::CaptureIntent as I;
use crate::tray::TrayAction;

fn tray(app: &mut App, action: TrayAction) -> Task<Message> {
    update(app, Message::Tick(TickMsg::TrayAction(action)))
}

fn welcome_open(app: &App) -> bool {
    app.windows.kinds().any(|k| k == WindowKind::Welcome)
}

/// CAP-02: tray "Capture…" opens the overlay for the editor; without
/// permission it opens Welcome instead; during a capture it does nothing.
#[test]
fn tray_capture_opens_the_overlay_or_welcome() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = tray(&mut app, TrayAction::Capture);
    assert!(app.overlay_opening);
    assert_eq!(app.pending_intent, Some(I::Editor));
    assert!(!welcome_open(&app));

    let mut app = build_app(Arc::new(FakePermissions::denied()));
    let _ = tray(&mut app, TrayAction::Capture);
    assert!(!app.overlay_opening);
    assert_eq!(app.pending_intent, None);
    assert!(welcome_open(&app), "the permission gate shows Welcome");

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.capture_in_flight = true;
    let task = tray(&mut app, TrayAction::Capture);
    assert_eq!(task.units(), 0);
    assert!(!app.overlay_opening);
    assert_eq!(app.pending_intent, None);
}

/// OV-22 (entry): tray "Scrolling Capture…" pre-sets the scroll intent;
/// it is gated like Capture and refused while a session runs.
#[test]
fn tray_scroll_capture_presets_its_intent_and_waits_for_a_running_session() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = tray(&mut app, TrayAction::ScrollCapture);
    assert!(app.overlay_opening);
    assert_eq!(app.pending_intent, Some(I::ScrollCapture));
    assert_eq!(overlay_auto_confirm_intent(&app), Some(I::ScrollCapture));

    let mut app = build_app(Arc::new(FakePermissions::denied()));
    let _ = tray(&mut app, TrayAction::ScrollCapture);
    assert!(!app.overlay_opening);
    assert!(welcome_open(&app));

    let (mut app, _, _) = app_with_scroll_windows(false);
    let _ = tray(&mut app, TrayAction::ScrollCapture);
    assert!(!app.overlay_opening);
    assert_eq!(app.pending_intent, None);
}

/// CAP-08 (tray entry): tray "Retake Last Region" recaptures the last
/// region on its display at that display's scale.
#[test]
fn tray_retake_recaptures_the_last_region_on_its_display() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: "b".into(),
            rect,
            intent: I::Editor,
        }),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(120, 80)))),
    );
    app.editor = None;
    app.last_regions.get_mut("b").unwrap().display_scale = 2.0;

    let _ = tray(&mut app, TrayAction::RetakeLastRegion);
    assert!(app.capture_in_flight);
    assert_eq!(app.pending_intent, Some(I::Editor));
    assert_eq!(app.pending_display_id.as_deref(), Some("b"));
    assert_eq!(app.pending_display_scale, Some(2.0));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(240, 160)))),
    );
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (240, 160));
}

/// CAP-01: the capture hotkey opens the overlay once however often it
/// fired in a tick, shows Welcome without permission, and is ignored
/// while a capture or an overlay is already under way.
#[test]
fn the_capture_hotkey_opens_one_overlay_or_welcome() {
    use crate::app::GlobalHotkeyAction as H;
    let run = |app: &mut App, pressed: &[H]| {
        let _ = crate::runtime::tick_ops::run_hotkey_actions(app, pressed);
    };
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    run(&mut app, &[H::Capture, H::Capture, H::Capture]);
    assert!(app.overlay_opening);
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayDisplaysListed(Ok(vec![DisplayInfo {
            id: "a".into(),
            bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 800.0, 600.0).unwrap(),
            scale: 1.0,
            name: "a".into(),
            is_primary: true,
        }]))),
    );
    // A press while the overlay is up doesn't stack a second one.
    run(&mut app, &[H::Capture]);
    assert!(!app.overlay_opening);
    assert_eq!(
        app.windows
            .kinds()
            .filter(|k| *k == WindowKind::Overlay)
            .count(),
        1
    );

    let mut app = build_app(Arc::new(FakePermissions::denied()));
    run(&mut app, &[H::Capture]);
    assert!(!app.overlay_opening);
    assert!(welcome_open(&app));

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.capture_in_flight = true;
    run(&mut app, &[H::Capture]);
    assert!(!app.overlay_opening);
}

// ---- The overlay's direct intents, through `RegionCaptured` ----

/// A region selected with `intent` on display "a" (bounds 100, 0, 800 ×
/// 600) whose capture just landed. History on or off. Returns the units
/// of the task the landing returned (the History save plus the intent's
/// own work).
fn land(intent: I, history: bool) -> (App, usize) {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.history_retention = if history {
        readshot_core::HistoryRetention::Last50
    } else {
        readshot_core::HistoryRetention::Off
    };
    let overlay = window::Id::unique();
    app.windows.register(overlay, WindowKind::Overlay);
    app.overlay_displays.insert(
        overlay,
        crate::app::OverlayDisplay {
            display_id: "a".into(),
            scale: 2.0,
            origin_x: 100.0,
            origin_y: 0.0,
            width: 800.0,
            height: 600.0,
        },
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: "a".into(),
            rect: readshot_core::geom::Rect::from_xywh(10.0, 10.0, 40.0, 30.0).unwrap(),
            intent,
        }),
    );
    assert_eq!(app.pending_intent, Some(intent));
    app.capture_in_flight = true;
    let task = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(80, 60)))),
    );
    assert!(!app.capture_in_flight);
    assert_eq!(app.pending_intent, None, "the intent is used once");
    assert_eq!(app.pending_display_id, None);
    (app, task.units())
}

/// Nothing but the intent's own work: no editor, no window.
fn assert_no_editor_or_window(app: &App) {
    assert!(app.editor.is_none());
    assert!(app.pins.is_empty());
    assert_eq!(app.windows.kinds().count(), 0, "every overlay closed");
}

/// CAP-11, OV-16: Copy Image lands without the editor, is kept in
/// History (when on), and reports the clipboard result; a failure is a
/// problem raised where the user will see it.
#[test]
fn copy_image_lands_without_the_editor_and_reports_the_clipboard_result() {
    let (mut app, units) = land(I::CopyToClipboard, true);
    assert_no_editor_or_window(&app);
    assert_eq!(units, 2, "the clipboard write and the History save");
    let (_, units_off) = land(I::CopyToClipboard, false);
    assert_eq!(units_off, 1, "History off: the clipboard write alone");

    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayCopyDone(Ok(()))),
    );
    let status = app.last_capture_status.clone().unwrap();
    assert_eq!(
        (status.tone, status.text.as_str()),
        (StatusTone::Success, "Copied image to clipboard.")
    );
    CAPTURE_NOTICES.with(|n| n.borrow_mut().clear());
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayCopyDone(Err("clipboard busy".into()))),
    );
    let status = app.last_capture_status.clone().unwrap();
    assert_eq!(status.tone, StatusTone::Error);
    assert!(
        status.text.starts_with("Couldn’t copy to the clipboard"),
        "{}",
        status.text
    );
    if CAPTURE_PROBLEM_ROUTE == CaptureProblemRoute::Notification {
        assert_eq!(
            CAPTURE_NOTICES.with(|n| n.borrow().clone()),
            vec![status.text]
        );
    } else {
        assert!(welcome_open(&app));
    }
}

/// CAP-12, OV-17: Copy Text lands without the editor; an empty result
/// says nothing was recognized, a result says how much was copied.
#[test]
fn copy_text_lands_without_the_editor_and_counts_what_it_copied() {
    let (mut app, units) = land(I::CopyTextDirect, true);
    assert_no_editor_or_window(&app);
    assert_eq!(units, 2, "the OCR + copy and the History save");
    let status = |app: &App| app.last_capture_status.clone().unwrap();
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayCopyTextDone(Ok(String::new()))),
    );
    assert_eq!(
        (status(&app).tone, status(&app).text),
        (StatusTone::Warning, "No text recognized.".to_string())
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayCopyTextDone(Ok("héllo".into()))),
    );
    assert_eq!(
        (status(&app).tone, status(&app).text),
        (
            StatusTone::Success,
            "Copied 5 characters of text.".to_string()
        )
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayCopyTextDone(Ok("a".into()))),
    );
    assert_eq!(status(&app).text, "Copied 1 character of text.");
}

/// CAP-12: the OCR-and-copy step with nothing recognized returns the
/// empty text without touching the clipboard (an empty engine never
/// reaches the clipboard write).
#[tokio::test]
async fn copy_text_with_nothing_recognized_leaves_the_clipboard_alone() {
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_text("")),
        perms,
        None,
    );
    let text = ocr_then_copy(
        coord,
        solid(8, 8),
        Preferences::default(),
        false,
        editor_ops::CopyAllMask::None,
    )
    .await
    .unwrap();
    assert_eq!(text, "");
}

/// CAP-13, OV-18: Save lands without the editor; a saved file reports
/// its path and seeds the next save's folder, a cancel says so.
#[test]
fn save_lands_without_the_editor_and_remembers_the_folder() {
    let (mut app, units) = land(I::SaveDirect, true);
    assert_no_editor_or_window(&app);
    assert_eq!(units, 2, "the save dialog and the History save");
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("Screenshot.png");
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySaveDone(Ok(Some(path.clone())))),
    );
    let status = app.last_capture_status.clone().unwrap();
    assert_eq!(status.tone, StatusTone::Success);
    assert_eq!(status.text, format!("Saved to {}.", path.display()));
    assert_eq!(app.last_save_dir.as_deref(), Some(dir.path()));
    assert_eq!(
        preferred_save_seed_dir(&app.preferences, &app.last_save_dir).as_deref(),
        Some(dir.path()),
        "the next save starts in the same folder"
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySaveDone(Ok(None))),
    );
    assert_eq!(
        app.last_capture_status.as_ref().unwrap().text,
        "Save cancelled."
    );
    assert_eq!(app.last_save_dir.as_deref(), Some(dir.path()));
}

/// CAP-14, OV-19, PIN-01: Pin lands as one unlocked, fully opaque pin
/// window and no editor, and is kept in History.
#[test]
fn pin_lands_as_one_pin_window_and_no_editor() {
    let (app, units) = land(I::Pin, true);
    assert!(app.editor.is_none());
    assert_eq!(app.pins.len(), 1);
    let (id, pin) = app.pins.iter().next().unwrap();
    assert_eq!(app.windows.kind(*id), Some(WindowKind::Pin));
    assert_eq!((pin.opacity, pin.locked), (1.0, false));
    assert_eq!(app.windows.kinds().count(), 1, "the overlay closed");
    assert_eq!(units, 2, "the pin window and the History save");
    let (_, units_off) = land(I::Pin, false);
    assert_eq!(units_off, 1);
}

/// PIN-04: a pin is borderless, always on top and resizable, scaled to
/// fit 800 × 600 (never up), at least 120 × 80, centred on its display.
#[test]
fn a_pin_window_fits_800_by_600_and_is_never_upscaled() {
    let size = |w, h| pin_window_settings((w, h), None).size;
    assert_eq!(size(1600, 1200), iced::Size::new(800.0, 600.0));
    assert_eq!(size(2400, 600), iced::Size::new(800.0, 200.0));
    assert_eq!(size(400, 300), iced::Size::new(400.0, 300.0));
    assert_eq!(size(60, 40), iced::Size::new(120.0, 80.0));
    let s = pin_window_settings((400, 300), Some((100.0, 0.0, 800.0, 600.0)));
    assert!(!s.decorations);
    assert!(s.resizable);
    assert_eq!(s.level, window::Level::AlwaysOnTop);
    assert_eq!(s.min_size, Some(iced::Size::new(120.0, 80.0)));
    assert!(
        matches!(s.position, window::Position::Specific(p) if p == iced::Point::new(300.0, 150.0)),
        "{:?}",
        s.position
    );
}

// ---- Pins ----

/// Open one more pin, as a History record's loaded image does.
fn add_pin(app: &mut App) -> window::Id {
    let before: Vec<window::Id> = app.pins.keys().copied().collect();
    let _ = update(
        app,
        Message::History(HistoryMsg::PinReady(Ok(solid(40, 30)))),
    );
    *app.pins
        .keys()
        .find(|id| !before.contains(id))
        .expect("a pin opened")
}

fn pin_key(app: &mut App, id: window::Id, key: iced::keyboard::Key, shift: bool) -> Task<Message> {
    let modifiers = if shift {
        iced::keyboard::Modifiers::SHIFT
    } else {
        iced::keyboard::Modifiers::empty()
    };
    update(
        app,
        Message::Editor(EditorMsg::Pin(PinMsg::KeyPressed {
            window: id,
            key,
            modifiers,
        })),
    )
}

fn ch(c: &str) -> iced::keyboard::Key {
    iced::keyboard::Key::Character(c.into())
}

/// PIN-03: a History record's image opens as a pin; a failed load says
/// so in History; an unknown record is "not found".
#[test]
fn a_history_pin_opens_a_pin_or_says_why_not() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = add_pin(&mut app);
    assert_eq!(app.windows.kind(id), Some(WindowKind::Pin));
    assert_eq!(app.pins.len(), 1);

    let _ = update(
        &mut app,
        Message::History(HistoryMsg::PinReady(Err("unreadable png".into()))),
    );
    let status = app.history_status.clone().unwrap();
    assert_eq!(status.tone, StatusTone::Error);
    assert!(
        status.text.starts_with("Couldn’t pin the capture"),
        "{}",
        status.text
    );
    assert_eq!(app.pins.len(), 1);

    let task = update(
        &mut app,
        Message::History(HistoryMsg::Pin(readshot_core::Uuid::new_v4())),
    );
    assert_eq!(task.units(), 0);
    assert_eq!(
        app.history_status.as_ref().unwrap().text,
        "Capture not found."
    );
}

/// PIN-07: `+`/`=` and `-`/`_` step opacity by 5 %, clamped to 20–100 %.
#[test]
fn pin_opacity_keys_step_by_five_percent_within_20_to_100() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = add_pin(&mut app);
    let opacity = |app: &App| (app.pins[&id].opacity * 100.0).round() as i32;
    let _ = pin_key(&mut app, id, ch("-"), false);
    assert_eq!(opacity(&app), 95);
    let _ = pin_key(&mut app, id, ch("_"), true);
    assert_eq!(opacity(&app), 90);
    let _ = pin_key(&mut app, id, ch("="), false);
    assert_eq!(opacity(&app), 95);
    let _ = pin_key(&mut app, id, ch("+"), true);
    let _ = pin_key(&mut app, id, ch("+"), true);
    assert_eq!(opacity(&app), 100, "clamped at 100 %");
    for _ in 0..30 {
        let _ = pin_key(&mut app, id, ch("-"), false);
    }
    assert_eq!(opacity(&app), 20, "clamped at 20 %");
}

/// PIN-08, PIN-09: `L` toggles the lock (Shift+L doesn't); Esc, ⌘W and
/// the Close button each close that pin only.
#[test]
fn pin_lock_and_close_keys() {
    use iced::keyboard::{key::Named, Key};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = add_pin(&mut app);
    let _ = pin_key(&mut app, id, ch("L"), true);
    assert!(!app.pins[&id].locked, "Shift+L is not Lock");
    let _ = pin_key(&mut app, id, ch("l"), false);
    assert!(app.pins[&id].locked);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::LockToggled(id))),
    );
    assert!(
        !app.pins[&id].locked,
        "the button (or double-click) unlocks"
    );

    for close in 0..3 {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = add_pin(&mut app);
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Pin(PinMsg::Hovered(id, true))),
        );
        let task = match close {
            0 => pin_key(&mut app, id, Key::Named(Named::Escape), false),
            1 => update(
                &mut app,
                Message::Editor(EditorMsg::Pin(PinMsg::KeyPressed {
                    window: id,
                    key: ch("w"),
                    modifiers: command_modifiers(),
                })),
            ),
            _ => update(
                &mut app,
                Message::Editor(EditorMsg::Pin(PinMsg::ClosePressed(id))),
            ),
        };
        assert_eq!(task.units(), 1, "the window closes ({close})");
        assert!(app.pins.is_empty(), "{close}");
        assert_eq!(app.windows.kind(id), None, "{close}");
        assert_eq!(app.hovered_pin, None, "{close}");
    }
}

/// PIN-10: arrows ask to move an unlocked pin (the move itself is the
/// window server's); a locked pin, or a key for a window that is no pin,
/// asks nothing.
#[test]
fn pin_arrow_keys_move_only_an_unlocked_pin() {
    use iced::keyboard::{key::Named, Key};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = add_pin(&mut app);
    for shift in [false, true] {
        let task = pin_key(&mut app, id, Key::Named(Named::ArrowLeft), shift);
        assert_eq!(task.units(), 1, "shift {shift}");
    }
    let other = window::Id::unique();
    assert_eq!(
        pin_key(&mut app, other, Key::Named(Named::ArrowLeft), false).units(),
        0
    );
    app.pins.get_mut(&id).unwrap().locked = true;
    assert_eq!(
        pin_key(&mut app, id, Key::Named(Named::ArrowLeft), false).units(),
        0
    );
}

/// PIN-11: pins are independent: keys reach only the pin they were
/// pressed in, and closing one leaves the other.
#[test]
fn several_pins_keep_their_own_opacity_and_lock() {
    use iced::keyboard::{key::Named, Key};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let a = add_pin(&mut app);
    let b = add_pin(&mut app);
    let _ = pin_key(&mut app, a, ch("-"), false);
    let _ = pin_key(&mut app, b, ch("l"), false);
    assert!(app.pins[&a].opacity < 1.0);
    assert_eq!(app.pins[&b].opacity, 1.0);
    assert!(app.pins[&b].locked);
    assert!(!app.pins[&a].locked);
    let _ = pin_key(&mut app, a, Key::Named(Named::Escape), false);
    assert!(!app.pins.contains_key(&a));
    assert!(app.pins.contains_key(&b));
    assert_eq!(app.windows.kind(b), Some(WindowKind::Pin));
}

// ---- Scroll capture limits ----

/// A running session with `n` accepted frames.
fn session_with_frames(n: usize) -> App {
    let (mut app, _, _) = app_with_scroll_windows(false);
    let s = app.scroll_session.as_mut().unwrap();
    let frame = Arc::new(solid(8, 8));
    s.frames = vec![frame; n];
    s.preview_handle = Some(iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]));
    app
}

/// SC-11: at 120 frames the tick captures no more and stops instead; the
/// stop shows the "limit reached" state while it stitches.
#[test]
fn at_120_frames_the_session_stops_and_shows_the_limit() {
    let mut app = session_with_frames(SCROLL_MAX_FRAMES - 1);
    let _ = update(&mut app, Message::Scroll(ScrollMsg::Tick));
    let s = app.scroll_session.as_ref().unwrap();
    assert_eq!(s.capture_in_flight, 1, "under the limit a frame is taken");

    let mut app = session_with_frames(SCROLL_MAX_FRAMES);
    let task = update(&mut app, Message::Scroll(ScrollMsg::Tick));
    let s = app.scroll_session.as_ref().unwrap();
    assert_eq!(s.capture_in_flight, 0, "no capture at the limit");
    assert_eq!(s.next_capture_seq, 0);
    assert_eq!(task.units(), 1, "the tick asks for a stop instead");
    // That stop, as the runtime delivers it.
    let _ = update(&mut app, Message::Scroll(ScrollMsg::StopRequested));
    let s = app.scroll_session.as_ref().unwrap();
    assert!(s.stopping);
    assert_eq!(s.stitch_frame_count, SCROLL_MAX_FRAMES);
    assert_eq!(scroll_hud_state(s), ScrollHudState::Limit);
}

/// SC-12: a stitch taller than the cap fails with a status that says to
/// start again, and closes the session's windows.
#[test]
fn a_stitch_over_the_height_cap_says_to_start_again() {
    let (mut app, hud, region) = app_with_scroll_windows(true);
    let error = readshot_core::scroll_stitch::StitchError::OutputTooTall {
        height: 20_000,
        cap: 16_384,
    };
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::Stitched(Err(error.to_string()))),
    );
    assert!(app.scroll_session.is_none());
    assert!(app.editor.is_none());
    assert_eq!(app.windows.kind(hud), None);
    assert_eq!(app.windows.kind(region), None);
    let status = app.last_capture_status.clone().unwrap();
    assert_eq!(status.tone, StatusTone::Error);
    assert!(
        status
            .text
            .starts_with("Couldn’t stitch the scrolling capture"),
        "{}",
        status.text
    );
    assert!(status.text.ends_with(SCROLL_RETRY_HINT), "{}", status.text);
}

/// An 80 × 120 view of a page of noise (no repeating rows), scrolled
/// down by `offset` px and right by `dx` px.
fn noise_page(offset: u32, dx: u32) -> image::RgbaImage {
    noise_rows(offset, dx, 120)
}

/// `height` rows of that page.
fn noise_rows(offset: u32, dx: u32, height: u32) -> image::RgbaImage {
    image::RgbaImage::from_fn(80, height, |x, y| {
        let mut h = u64::from((x + dx) % 80).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ u64::from(y + offset).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        h ^= h >> 29;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 32;
        image::Rgba([h as u8, (h >> 8) as u8, (h >> 16) as u8, 255])
    })
}

/// SC-16: scrolling down is motion, measured exactly, and two frames
/// stitch into the page they show.
#[tokio::test]
async fn downward_scrolling_is_motion_and_stitches_the_page() {
    let (top, down) = (noise_page(0, 0), noise_page(14, 0));
    assert!(frame_has_scroll_motion(&top, &down));
    assert_eq!(scroll_motion_offset(&top, &down), Some(14));
    assert!(!frame_has_scroll_motion(&top, &top.clone()), "a still page");
    let stitched = stitch_frames_async(vec![Arc::new(top), Arc::new(down)])
        .await
        .unwrap();
    assert_eq!(stitched, noise_rows(0, 0, 134));
}

/// SC-16: scrolling back up or sideways is not motion, so those frames
/// are dropped. The judge used to search only downward offsets and keep
/// the cheapest with no quality bar, so a page scrolled back up 14 px
/// measured as 44 px down and the stitch appended rows the page doesn't
/// have (191 px tall instead of 134).
#[tokio::test]
async fn backward_or_sideways_scrolling_is_not_motion() {
    let (top, down) = (noise_page(0, 0), noise_page(14, 0));
    let back_up = noise_page(0, 0);
    let sideways = noise_page(14, 12);
    assert!(
        !frame_has_scroll_motion(&down, &back_up),
        "scrolling back up: offset {:?}",
        scroll_motion_offset(&down, &back_up)
    );
    assert!(
        !frame_has_scroll_motion(&down, &sideways),
        "sideways: offset {:?}",
        scroll_motion_offset(&down, &sideways)
    );
    // Were it accepted anyway, the stitch should still be the page.
    let stitched = stitch_frames_async(vec![Arc::new(top), Arc::new(down), Arc::new(back_up)])
        .await
        .unwrap();
    assert_eq!(stitched.dimensions(), (80, 134));
}

/// A 300-wide view of a text page: white, with dark word runs on an
/// 18 px line pitch (no two lines alike), scrolled down by `offset` px
/// and right by `dx` px, `height` rows tall.
fn text_rows(offset: u32, dx: u32, height: u32) -> image::RgbaImage {
    let hash = |a: u32, b: u32| {
        let mut h = u64::from(a).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ u64::from(b);
        h ^= h >> 29;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^ (h >> 32)
    };
    image::RgbaImage::from_fn(300, height, |x, y| {
        let (py, px) = (y + offset, x + dx);
        let (line, row) = (py / 18, py % 18);
        if !(4..14).contains(&row) {
            return image::Rgba([255, 255, 255, 255]);
        }
        // Words: runs of 12–43 px with 6 px gaps, laid out per line.
        let mut start = 4 + (hash(line, 0) % 20) as u32;
        let mut k = 1;
        while start < 600 {
            let len = 12 + (hash(line, k) % 32) as u32;
            if (start..start + len).contains(&px) {
                // Letter texture: a few light columns inside a word.
                let ink = hash(line * 1000 + px, row) % 5 != 0;
                let v = if ink { 30 } else { 230 };
                return image::Rgba([v, v, v, 255]);
            }
            start += len + 6;
            k += 1;
        }
        image::Rgba([255, 255, 255, 255])
    })
}

/// SC-16 on a realistic page (the reviewer's case): 300 × 400 frames of
/// text lines. Down 60 px is motion of exactly 60; back up 60 or 20 px
/// and sideways are not; and a stitch of [top, down 60, back to top] is
/// the 460 px page, not 616 px with a band repeated.
#[tokio::test]
async fn scrolling_back_up_a_text_page_never_repeats_a_band() {
    let top = text_rows(0, 0, 400);
    let down = text_rows(60, 0, 400);
    assert_eq!(scroll_motion_offset(&top, &down), Some(60));
    assert!(frame_has_scroll_motion(&top, &down));
    // A scroll back up is told apart from a frame that can't be placed:
    // only the latter counts towards the "scrolled too fast" hint.
    use crate::runtime::scroll::{scroll_motion, ScrollMotion};
    for (what, frame, verdict) in [
        ("back up 60", text_rows(0, 0, 400), ScrollMotion::Back),
        ("back up 20", text_rows(40, 0, 400), ScrollMotion::Back),
        (
            "sideways 12",
            text_rows(60, 12, 400),
            ScrollMotion::Unplaced,
        ),
    ] {
        assert!(
            !frame_has_scroll_motion(&down, &frame),
            "{what}: offset {:?}",
            scroll_motion_offset(&down, &frame)
        );
        assert_eq!(scroll_motion(&down, &frame), verdict, "{what}");
    }
    let stitched = stitch_frames_async(vec![
        Arc::new(top.clone()),
        Arc::new(down.clone()),
        Arc::new(top),
    ])
    .await
    .unwrap();
    assert_eq!(stitched.dimensions(), (300, 460));
    assert_eq!(stitched, text_rows(0, 0, 460));
    // Scrolling on after the detour joins at the right place.
    let further = text_rows(120, 0, 400);
    let stitched = stitch_frames_async(vec![
        Arc::new(text_rows(0, 0, 400)),
        Arc::new(down),
        Arc::new(text_rows(20, 0, 400)),
        Arc::new(further),
    ])
    .await
    .unwrap();
    assert_eq!(stitched, text_rows(0, 0, 520));
}
