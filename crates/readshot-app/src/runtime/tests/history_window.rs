//! The History window and store driven through the real paths (W5c
//! slice C: HI-03 … HI-26).
//!
//! Every History is a tempdir `FsHistoryStore`; `history_root` points at
//! it. No returned Task is run (an `iced::window::open` or a Finder
//! reveal never happens): a Pin's PNG load is awaited by hand with the
//! same `load_png_async` the task runs.

use super::*;
use iced::keyboard::key::Named;
use iced::keyboard::Key;
use readshot_core::HistoryRetention as R;

fn saved_store() -> (tempfile::TempDir, Arc<FsHistoryStore>) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    (dir, store)
}

fn png(img: &image::RgbaImage) -> Vec<u8> {
    readshot_core::encode_png(img).unwrap()
}

/// A History window over `store`, listed.
fn history_app(store: &Arc<FsHistoryStore>) -> (App, iced::window::Id) {
    let mut app = build_app_with_history(
        Arc::new(FakePermissions::granted()),
        store.clone() as Arc<dyn HistoryStore>,
    );
    app.history_root = Some(store.root().to_path_buf());
    let id = iced::window::Id::unique();
    app.history_window_id = Some(id);
    let records = app.coordinator.history_list().map_err(|e| e.to_string());
    let _ = update(&mut app, Message::History(HistoryMsg::ListLoaded(records)));
    (app, id)
}

/// A key press in the History window, as its subscription maps it.
fn history_key(
    app: &mut App,
    id: iced::window::Id,
    key: Key,
    modifiers: iced::keyboard::Modifiers,
) {
    let msg = history_key_message(id, &key, modifiers, true).expect("a History key");
    let _ = update(app, msg);
}

fn clear_all_chord() -> (Key, iced::keyboard::Modifiers) {
    let mut m = command_modifiers();
    m.insert(iced::keyboard::Modifiers::SHIFT);
    (Key::Named(Named::Backspace), m)
}

/// `persist_history` (a capture's History save) to completion.
async fn capture_into_history(app: &App, at: chrono::DateTime<chrono::Utc>) {
    use iced::futures::StreamExt;
    let record = readshot_core::CaptureRecord::new(at, 2, 2, "d");
    let (tx, rx) = iced::futures::channel::mpsc::channel(4);
    persist_history(
        app.coordinator.clone(),
        solid(2, 2),
        Some(record),
        app.preferences.history_retention,
        app.preferences.clone(),
        None,
        tx,
    )
    .await;
    let _: Vec<Message> = rx.collect().await;
}

fn cycle_retention_to(app: &mut App, settings: iced::window::Id, want: R) {
    for _ in 0..4 {
        if app.preferences.history_retention == want {
            return;
        }
        press_settings_key(app, settings, "r", false);
    }
    assert_eq!(app.preferences.history_retention, want);
}

/// HI-03: a retention change prunes nothing by itself ("Older captures
/// are deleted at the next capture."); the next saved capture applies
/// it. Off saves no new capture and deletes none; Unlimited keeps all.
/// Reset sets Last 50, as first launch does.
#[tokio::test]
async fn retention_applies_at_the_next_saved_capture() {
    use chrono::TimeZone;
    // 52 saves: durability isn't under test (nextest runs each test in
    // its own process).
    readshot_core::skip_fsync_in_this_process();
    let (_dir, store) = saved_store();
    for i in 0..52 {
        let at = chrono::Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, i).unwrap();
        store
            .save(
                &readshot_core::CaptureRecord::new(at, 2, 2, "d"),
                &png(&solid(2, 2)),
            )
            .unwrap();
    }
    let mut app = build_app_with_history(
        Arc::new(FakePermissions::granted()),
        store.clone() as Arc<dyn HistoryStore>,
    );
    app.preferences.history_retention = R::Unlimited;
    let settings = iced::window::Id::unique();
    app.settings_window_id = Some(settings);
    let count = || store.list().unwrap().len();

    cycle_retention_to(&mut app, settings, R::Last50);
    assert_eq!(
        app.settings_key_feedback.as_deref(),
        Some(
            "History retention: Last 50 captures. Older captures are deleted at the next capture."
        )
    );
    assert_eq!(count(), 52, "the setting alone deletes nothing");

    let now = chrono::Utc::now();
    capture_into_history(&app, now).await;
    let kept = store.list().unwrap();
    assert_eq!(kept.len(), 50);
    assert_eq!(kept[0].captured_at, now, "the new capture is kept");
    assert!(
        kept.iter()
            .all(|r| r.captured_at >= chrono::Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 3).unwrap()),
        "the three oldest went"
    );

    cycle_retention_to(&mut app, settings, R::Off);
    assert_eq!(
        app.settings_key_feedback.as_deref(),
        Some("History retention: Off. New captures aren’t saved.")
    );
    capture_into_history(&app, chrono::Utc::now()).await;
    assert_eq!(count(), 50, "Off saves nothing and deletes nothing");

    cycle_retention_to(&mut app, settings, R::Unlimited);
    capture_into_history(&app, chrono::Utc::now()).await;
    assert_eq!(count(), 51);

    let _ = update(&mut app, Message::Settings(SettingsMsg::ResetAllRequested));
    let _ = update(&mut app, Message::Settings(SettingsMsg::ResetAllConfirmed));
    assert_eq!(app.preferences.history_retention, R::Last50);
    assert_eq!(count(), 51, "a reset prunes nothing either");
}

/// HI-04: the search field matches case-insensitively, ignoring outer
/// spaces, on OCR text, display id, pixel size and the full local
/// timestamp (even of a record with text); matches are highlighted.
/// MCP `search_captures` calls the same `record_matches_query`.
#[test]
fn search_matches_text_display_size_and_local_time_in_any_case() {
    use chrono::TimeZone;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut a = readshot_core::CaptureRecord::new(chrono::Utc::now(), 1440, 900, "built-in-retina");
    a.ocr_text = Some("Deploy script ready".into());
    let at = chrono::Utc
        .with_ymd_and_hms(2026, 4, 28, 14, 32, 5)
        .unwrap();
    let mut b = readshot_core::CaptureRecord::new(at, 120, 90, "external");
    b.ocr_text = Some("beta invoice".into());
    let (a_id, b_id) = (a.id, b.id);
    app.history_records = vec![a, b];
    app.history_selected_id = Some(a_id);
    let local = at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    for (query, want) in [
        ("  DEPLOY  ", a_id),
        ("InVoice", b_id),
        ("Built-In", a_id),
        ("120X90", b_id),
        (local.as_str(), b_id),
        ("", a_id),
    ] {
        let _ = update(
            &mut app,
            Message::History(HistoryMsg::SearchChanged(query.to_string())),
        );
        let visible = visible_history_ids(&app.history_records, &app.history_search);
        if query.is_empty() {
            assert_eq!(visible.len(), 2);
            continue;
        }
        assert_eq!(visible, [want], "{query:?}");
        assert_eq!(app.history_selected_id, Some(want), "{query:?}");
    }
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::SearchChanged("  DEPLOY ".into())),
    );
    assert_eq!(
        super::super::view::highlight_spans("Deploy script ready", &app.history_search),
        [
            ("Deploy".to_string(), true),
            (" script ready".to_string(), false)
        ]
    );
    assert_eq!(
        visible_history_text(&app.history_records, &app.history_search),
        "Deploy script ready"
    );
}

/// HI-13, HI-14: ⌘P pins the selected record's PNG in a pin window. An
/// unknown id says so; a failed load names the action. (A record whose
/// scrub is pending is never pinned or revealed:
/// `history_reopen::a_withheld_record_hands_out_no_image`.)
#[tokio::test]
async fn pin_from_history_shows_the_served_png() {
    let (_dir, store) = saved_store();
    let img = ramp(12, 8);
    let shown = readshot_core::CaptureRecord::new(chrono::Utc::now(), 12, 8, "d");
    store.save(&shown, &png(&img)).unwrap();
    let (mut app, id) = history_app(&store);
    assert_eq!(app.history_selected_id, Some(shown.id));

    // ⌘P on the selection: the task loads the served PNG.
    let pin = history_key_message(id, &Key::Character("p".into()), command_modifiers(), true)
        .expect("⌘P is a History key");
    assert!(matches!(
        &pin,
        Message::History(HistoryMsg::KeyboardShortcut(_, HistoryKeyboardAction::Pin))
    ));
    assert_eq!(update(&mut app, pin).units(), 1, "one load of the PNG");
    assert!(app.pins.is_empty(), "no pin before its image loads");
    let root = app.history_root.clone().unwrap();
    let path = history_png_path(&root, &app.history_records[0]).unwrap();
    let loaded = load_png_async(path).await.unwrap();
    assert_eq!(loaded, img);
    let _ = update(&mut app, Message::History(HistoryMsg::PinReady(Ok(loaded))));
    assert_eq!(app.pins.len(), 1);
    assert_eq!(
        app.windows
            .kinds()
            .filter(|k| *k == WindowKind::Pin)
            .count(),
        1
    );
    let pin = app.pins.values().next().unwrap();
    assert!(matches!(
        &pin.handle,
        iced::widget::image::Handle::Rgba {
            width: 12,
            height: 8,
            ..
        }
    ));

    let _ = update(
        &mut app,
        Message::History(HistoryMsg::Reveal(readshot_core::Uuid::new_v4())),
    );
    assert_eq!(
        app.history_status.as_ref().map(|s| s.text.as_str()),
        Some("Capture not found.")
    );
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::PinReady(Err("decode failed".into()))),
    );
    let status = app.history_status.clone().unwrap();
    assert_eq!(status.tone, StatusTone::Error);
    assert!(status.text.contains("pin the capture"), "{}", status.text);
    assert_eq!(app.pins.len(), 1);
}

/// HI-21: with History open, a redaction rewrites the record's
/// thumbnail as the redacted image (no pixel of the original under the
/// block survives), and its image handle changes, so the open window
/// never shows the stale raster.
#[test]
fn a_scrub_replaces_the_thumbnail_and_its_handle() {
    let (_dir, store, mut app) = scrub_app(true);
    app.history_root = Some(store.root().to_path_buf());
    app.history_window_id = Some(iced::window::Id::unique());
    let record = store.list().unwrap().remove(0);
    let thumb_path = history_thumbnail_path(store.root(), &record).unwrap();
    let before = super::super::history_images::history_image_handle(&thumb_path);
    let decode = |p: &std::path::Path| image::open(p).unwrap().to_rgba8();
    assert_eq!(
        decode(&thumb_path),
        solid(64, 64),
        "a 64 px capture is its own thumbnail"
    );

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextPanel(TextPanelMsg::RedactAll)),
    );
    app.coordinator.flush_history_updates();
    let record = store.list().unwrap().remove(0);
    assert!(record.redacted);
    let thumb_path = history_thumbnail_path(store.root(), &record).unwrap();
    let after = super::super::history_images::history_image_handle(&thumb_path);
    assert_ne!(after.id(), before.id());
    let thumb = decode(&thumb_path);
    // The email's word box (x 22.4–57.6, y 19.2–25.6) is black.
    for x in 23..57 {
        for y in 20..25 {
            assert_eq!(
                thumb.get_pixel(x, y),
                &image::Rgba([0, 0, 0, 255]),
                "({x},{y})"
            );
        }
    }
    assert_eq!(thumb, app.editor.as_mut().unwrap().model.flatten());
}

/// HI-23: a History index corrupted by hand is rebuilt from the
/// sidecars when the window lists it; nothing is lost and the next
/// save works.
#[tokio::test]
async fn a_corrupt_index_is_rebuilt_when_history_lists() {
    let (_dir, store) = saved_store();
    let first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 2, 2, "d");
    store.save(&first, &png(&solid(2, 2))).unwrap();
    std::fs::write(
        store
            .root()
            .join(readshot_core::history::HISTORY_INDEX_FILENAME),
        b"{ torn",
    )
    .unwrap();
    let (mut app, _id) = history_app(&store);
    assert_eq!(app.history_records.len(), 1);
    assert_eq!(app.history_selected_id, Some(first.id));
    assert!(app.history_status.is_none());
    app.preferences.history_retention = R::Last50;
    capture_into_history(&app, chrono::Utc::now()).await;
    let records = app.coordinator.history_list().map_err(|e| e.to_string());
    let _ = update(&mut app, Message::History(HistoryMsg::ListLoaded(records)));
    assert_eq!(app.history_records.len(), 2);
}

/// HI-26: closing the History window (its close button: the window's
/// close event) forgets the window and resets the browser: records,
/// search, selection, status. History binds no ⌘W of its own.
#[test]
fn closing_history_resets_the_browser() {
    let (_dir, _store, mut app, newer, _older) = app_with_two_saved_records();
    let id = iced::window::Id::unique();
    app.windows.register(id, WindowKind::History);
    app.history_window_id = Some(id);
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::SearchChanged("primary".into())),
    );
    app.history_status = Some(Status::new(StatusTone::Notice, "x"));
    assert_eq!(app.history_selected_id, Some(newer.id));

    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert_eq!(app.history_window_id, None);
    assert_eq!(app.windows.kind(id), None);
    assert!(app.history_records.is_empty());
    assert!(app.history_search.is_empty());
    assert_eq!(app.history_selected_id, None);
    assert!(app.history_status.is_none());

    assert!(
        history_key_message(id, &Key::Character("w".into()), command_modifiers(), true).is_none()
    );
}

/// HI-26 / HI-15, bug: Clear All armed with ⇧⌘⌫ stays armed after the
/// window is closed with its close button, so after reopening History a
/// single ⇧⌘⌫ deletes every capture, without the confirm step.
#[tokio::test]
#[ignore = "bug: History's close does not disarm Clear All, so one ⇧⌘⌫ after a reopen wipes History"]
async fn reopening_history_never_keeps_clear_all_armed() {
    let (_dir, store) = saved_store();
    for _ in 0..2 {
        let r = readshot_core::CaptureRecord::new(chrono::Utc::now(), 2, 2, "d");
        store.save(&r, &png(&solid(2, 2))).unwrap();
    }
    let (mut app, id) = history_app(&store);
    let (key, m) = clear_all_chord();
    history_key(&mut app, id, key.clone(), m);
    assert!(app.history_clear_all_pending, "the first press arms");
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));

    let _ = update(&mut app, Message::History(HistoryMsg::OpenRequested));
    let id = app.history_window_id.expect("History reopened");
    let records = app.coordinator.history_list().map_err(|e| e.to_string());
    let _ = update(&mut app, Message::History(HistoryMsg::ListLoaded(records)));
    history_key(&mut app, id, key, m);
    assert_eq!(
        store.list().unwrap().len(),
        2,
        "one press after a reopen must only arm Clear All"
    );
}
