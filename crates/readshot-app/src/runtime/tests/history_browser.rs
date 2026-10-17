//! History browser, statuses, friendly errors and platform commands.

use super::*;

#[test]
fn editor_image_filter_is_crisp_at_actual_size_and_smooth_otherwise() {
    assert_eq!(
        editor_image_filter(1.0, 1.0),
        iced::widget::image::FilterMethod::Nearest
    );
    assert_eq!(
        editor_image_filter(0.5, 2.0),
        iced::widget::image::FilterMethod::Nearest
    );
    assert_eq!(
        editor_image_filter(0.75, 1.0),
        iced::widget::image::FilterMethod::Linear
    );
    assert_eq!(
        editor_image_filter(1.25, 1.0),
        iced::widget::image::FilterMethod::Linear
    );
}

#[test]
fn editor_fit_scale_fills_available_viewport() {
    assert_eq!(
        editor_fit_scale(iced::Size::new(800.0, 600.0), 200, 100),
        4.0
    );
    assert_eq!(
        editor_fit_scale(iced::Size::new(500.0, 400.0), 1000, 400),
        0.5
    );
}

#[cfg(target_os = "macos")]
#[test]
fn relaunch_on_exit_only_for_system_quit_while_awaiting_permission() {
    // macOS "Quit & Reopen" after a grant: relaunch ourselves.
    assert!(should_relaunch_on_exit(true, false));
    // Our own Quit / Restart: never.
    assert!(!should_relaunch_on_exit(true, true));
    // Started with permission already granted: never.
    assert!(!should_relaunch_on_exit(false, false));
    let reuse = relaunch_command_for_bundle(
        std::path::Path::new("/Applications/Readshot.app"),
        RelaunchMode::ReuseRunning,
    );
    // Plain `open` so a copy macOS already reopened is reused, not doubled.
    assert!(reuse.args[1].contains("exec /usr/bin/open \"$1\""));
    assert!(!reuse.args[1].contains("-n"));
}

#[test]
fn history_timestamp_label_is_relative_to_today() {
    use chrono::TimeZone;
    let now = chrono::Local
        .with_ymd_and_hms(2026, 9, 27, 18, 0, 0)
        .unwrap();
    let at = |y, m, d, h, min| chrono::Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap();
    assert_eq!(
        view::history_timestamp_label(at(2026, 9, 27, 14, 3), now),
        "Today 14:03"
    );
    assert_eq!(
        view::history_timestamp_label(at(2026, 9, 26, 9, 10), now),
        "Yesterday 09:10"
    );
    assert_eq!(
        view::history_timestamp_label(at(2026, 3, 5, 7, 45), now),
        "Mar 5, 07:45"
    );
    assert_eq!(
        view::history_timestamp_label(at(2025, 12, 31, 23, 0), now),
        "Dec 31, 2025"
    );
}

#[test]
fn line_width_fraction_clamps_to_toolbar_range() {
    assert_eq!(line_width_fraction(-10.0), 0.0);
    assert_eq!(line_width_fraction(99.0), 1.0);
    assert!(
        (line_width_fraction(readshot_ui::editor::toolbar::MIN_LINE_WIDTH) - 0.0).abs()
            < f32::EPSILON
    );
    assert!(
        (line_width_fraction(readshot_ui::editor::toolbar::MAX_LINE_WIDTH) - 1.0).abs()
            < f32::EPSILON
    );
}

#[test]
fn disabled_action_button_does_not_keep_active_border_strength() {
    let active = action_button_style(&Theme::Dark, button::Status::Active, ActionKind::Secondary);
    let disabled = action_button_style(
        &Theme::Dark,
        button::Status::Disabled,
        ActionKind::Secondary,
    );

    assert!(disabled.text_color.a < active.text_color.a);
    assert!(disabled.border.color.a < active.border.color.a);
}

#[test]
fn select_drag_preview_refreshes_image_only_for_crop_geometry() {
    let annotations = vec![
        Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        },
        Annotation::Crop {
            rect: RectLike::new(4.0, 4.0, 18.0, 18.0),
        },
    ];

    assert!(can_preview_drag_annotation(&annotations, 0));
    assert!(!refresh_image_during_select_drag(&annotations, 0));
    assert!(!can_preview_drag_annotation(&annotations, 1));
    assert!(refresh_image_during_select_drag(&annotations, 1));
    assert!(!refresh_image_during_select_drag(&annotations, 99));
}

#[test]
fn editor_selection_chrome_follows_selection_presence() {
    // Chrome shows iff something is selected — independent of the
    // active tool, so a selection stays editable after switching to a
    // drawing tool.
    assert!(editor_show_selection_chrome(true));
    assert!(!editor_show_selection_chrome(false));
}

#[test]
fn inline_text_editor_geometry_anchors_to_clicked_image_point() {
    let geom =
        inline_text_editor_geometry(PointLike::new(60.0, 40.0), (200, 100), (10.0, 20.0), 2.0);

    assert_eq!(geom.x, 100.0);
    assert_eq!(geom.y, 40.0);
}

#[test]
fn inline_text_editor_geometry_clamps_near_image_edges() {
    let geom =
        inline_text_editor_geometry(PointLike::new(198.0, 98.0), (200, 100), (0.0, 0.0), 2.0);

    assert!(geom.x + geom.width <= 400.0);
    assert!(geom.y + geom.height <= 200.0);
}

#[test]
fn m_selects_the_magnifier_only_when_not_typing() {
    use iced::keyboard::{Key, Modifiers};
    assert!(matches!(
        editor_key_message(Key::Character("m".into()), Modifiers::empty(), true),
        Some(Message::EditorToolbar(
            readshot_ui::ToolbarMessage::SelectTool(readshot_ui::editor::ToolState::Magnifier)
        ))
    ));
    assert!(editor_key_message(Key::Character("m".into()), Modifiers::empty(), false).is_none());
}

#[test]
fn editor_selected_hint_explains_the_magnifier_handles() {
    assert_eq!(
        editor_selected_hint("Magnifier", false),
        "Selected Magnifier — drag the bubble to move it · drag the centre to change what it shows · drag the ring to resize · ⌫ removes"
    );
}

#[test]
fn editor_selected_hint_special_cases_crop_and_text() {
    assert_eq!(
        editor_selected_hint("Crop", false),
        "Selected Crop — drag frame or handles · arrows nudge · Delete restores full image"
    );
    assert_eq!(
        editor_selected_hint("Text", true),
        "Selected Text — edit inline · color/size apply here · arrows nudge · ⌫ removes"
    );
    assert_eq!(
        editor_selected_hint("Rectangle", false),
        "Selected Rectangle — drag to move · arrows nudge · handles resize · drag elsewhere to draw"
    );
}

#[test]
fn record_matches_finds_substring_in_ocr_text() {
    let mut r =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
    r.ocr_text = Some("Hello, deploy script ready".into());
    // The caller (history_view) lowercases the query first; the
    // matcher's contract is "q is already lowercase".
    assert!(record_matches(&r, "deploy"));
    assert!(record_matches(&r, "hello"));
    assert!(!record_matches(&r, "ship"));
}

#[test]
fn record_matches_falls_back_to_timestamp_when_no_ocr() {
    use chrono::TimeZone;
    let when = chrono::Utc
        .with_ymd_and_hms(2026, 4, 28, 14, 32, 5)
        .unwrap();
    let r = readshot_core::CaptureRecord::new(when, 100, 100, "primary".to_string());
    // Local-formatted timestamp lets users search by date / time.
    let local = when
        .with_timezone(&chrono::Local)
        .format("%Y-%m")
        .to_string();
    assert!(record_matches(&r, &local.to_lowercase()));
}

#[test]
fn record_matches_finds_display_id_and_dimensions() {
    let r = readshot_core::CaptureRecord::new(chrono::Utc::now(), 1440, 900, "built-in-retina");

    assert!(record_matches(&r, "retina"));
    assert!(record_matches(&r, "1440x900"));
    assert!(record_matches(&r, "900"));
    assert!(!record_matches(&r, "external"));
}

#[test]
fn visible_history_text_uses_current_search_filter() {
    let mut first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
    first.ocr_text = Some("alpha receipt".into());
    let mut second = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "secondary");
    second.ocr_text = Some("beta invoice".into());

    let records = vec![first, second];

    assert_eq!(visible_history_text(&records, "invoice"), "beta invoice");
    assert_eq!(visible_history_text_count(&records, "invoice"), 1);
    assert_eq!(
        visible_history_text(&records, ""),
        "alpha receipt\n\nbeta invoice"
    );
}

#[test]
fn history_list_load_selects_first_visible_capture() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
    let second = readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "secondary");

    let _ = update(
        &mut app,
        Message::HistoryListLoaded(Ok(vec![first.clone(), second])),
    );

    assert_eq!(app.history_selected_id, Some(first.id));
}

#[test]
fn history_search_keeps_selection_inside_filtered_results() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
    first.ocr_text = Some("alpha receipt".into());
    let mut second = readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "secondary");
    second.ocr_text = Some("beta invoice".into());
    let second_id = second.id;
    app.history_records = vec![first, second];
    app.history_selected_id = app.history_records.first().map(|r| r.id);

    let _ = update(&mut app, Message::HistorySearchChanged("invoice".into()));

    assert_eq!(app.history_selected_id, Some(second_id));
}

#[test]
fn history_keyboard_selection_moves_through_visible_rows() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
    let second = readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "secondary");
    let second_id = second.id;
    app.history_records = vec![first, second];
    app.history_selected_id = app.history_records.first().map(|r| r.id);

    let _ = update(&mut app, Message::HistorySelectNext);
    assert_eq!(app.history_selected_id, Some(second_id));

    let _ = update(&mut app, Message::HistorySelectNext);
    assert_eq!(app.history_selected_id, Some(second_id));

    let _ = update(&mut app, Message::HistorySelectPrevious);
    assert_eq!(
        app.history_selected_id,
        app.history_records.first().map(|r| r.id)
    );
}

#[test]
fn history_copy_shortcut_requires_selected_capture() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.history_window_id = Some(id);

    let _ = update(
        &mut app,
        Message::HistoryKeyboardShortcut(id, HistoryKeyboardAction::CopyText),
    );

    assert_eq!(
        app.history_status.as_deref(),
        Some("No history capture selected.")
    );
}

#[test]
fn history_focus_search_shortcut_leaves_state_alone() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.history_window_id = Some(id);
    app.history_search = "invoice".into();

    let _ = update(
        &mut app,
        Message::HistoryKeyboardShortcut(id, HistoryKeyboardAction::FocusSearch),
    );

    assert_eq!(app.history_status, None);
    assert_eq!(app.history_search, "invoice");
}

#[test]
fn history_copy_completion_updates_history_status() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));

    let _ = update(&mut app, Message::HistoryCopyImageDone(Ok(())));

    assert_eq!(
        app.history_status.as_deref(),
        Some("Copied image to clipboard.")
    );

    let _ = update(&mut app, Message::HistoryCopyTextDone(Ok("hello".into())));

    assert_eq!(
        app.history_status.as_deref(),
        Some("Copied 5 characters of text.")
    );
}

#[test]
fn permission_settings_summary_explains_denied_state() {
    let (title, hint) = permission_settings_summary(PermissionStatus::Denied);

    assert!(title.contains("needs attention"));
    assert!(hint.contains("System Settings"));
    assert!(hint.contains("restart"));
}

#[test]
fn welcome_permission_guidance_explains_restart_after_denied_poll() {
    let (title, hint) = welcome_permission_guidance(WelcomeState::Denied);

    assert!(title.contains("blocked"));
    assert!(hint.contains("restart"));
    assert!(hint.contains("System Settings"));
}

#[test]
fn history_backed_editor_commit_updates_record_annotations() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let mut record =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
    record.ocr_text = Some("before".into());
    store.save(&record, b"png").unwrap();

    let history: Arc<dyn HistoryStore> = store.clone();
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
    app.editor = Some(crate::editor::EditorSession::from_history(
        solid(100, 100),
        record.clone(),
    ));

    let annotation = Annotation::Rectangle {
        rect: RectLike::new(1.0, 2.0, 30.0, 40.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    };
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(annotation)),
    );

    // The sidecar write runs on the coordinator's background
    // writer; wait for it before reading the store.
    app.coordinator.flush_history_updates();
    let from_disk = store.list().unwrap();
    assert_eq!(from_disk[0].annotation_model.len(), 1);
    assert_eq!(from_disk[0].ocr_text.as_deref(), Some("before"));
}

#[test]
fn history_clear_all_removes_records_and_resets_browser_state() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let older =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
    let newer =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "primary".to_string());
    store.save(&older, b"png").unwrap();
    store.save(&newer, b"png").unwrap();

    let history: Arc<dyn HistoryStore> = store.clone();
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
    app.history_records = vec![newer, older];
    app.history_status = Some("stale".into());

    // First click arms the destructive prompt; records stay intact.
    let _ = update(&mut app, Message::HistoryClearAllRequested);
    assert!(app.history_clear_all_pending);
    assert_eq!(app.history_records.len(), 2);
    // Cancel exits the armed state without touching anything.
    let _ = update(&mut app, Message::HistoryClearAllCancelled);
    assert!(!app.history_clear_all_pending);
    assert_eq!(app.history_records.len(), 2);
    // Re-arm and confirm; now the records actually go.
    let _ = update(&mut app, Message::HistoryClearAllRequested);
    let _ = update(&mut app, Message::HistoryClearAllConfirmed);

    assert!(!app.history_clear_all_pending);
    assert!(store.list().unwrap().is_empty());
    assert!(app.history_records.is_empty());
    assert!(app
        .history_status
        .as_deref()
        .unwrap_or("")
        .starts_with("Cleared "));
}

#[test]
fn single_delete_arms_before_removing_anything() {
    let (_dir, store, mut app, newer, _older) = app_with_two_saved_records();

    // First press only arms the prompt; nothing is removed.
    let _ = update(&mut app, Message::HistoryDelete(newer.id));
    assert_eq!(app.history_delete_pending, Some(newer.id));
    assert_eq!(store.list().unwrap().len(), 2);

    // Explicit confirm removes exactly the armed record.
    let _ = update(&mut app, Message::HistoryDeleteConfirmed(newer.id));
    assert_eq!(app.history_delete_pending, None);
    let remaining = store.list().unwrap();
    assert_eq!(remaining.len(), 1);
    assert!(remaining.iter().all(|r| r.id != newer.id));
}

#[test]
fn single_delete_second_press_confirms() {
    // Delete-key path: a repeat press for the same id confirms.
    let (_dir, store, mut app, newer, _older) = app_with_two_saved_records();
    let _ = update(&mut app, Message::HistoryDelete(newer.id));
    let _ = update(&mut app, Message::HistoryDelete(newer.id));
    assert_eq!(app.history_delete_pending, None);
    assert_eq!(store.list().unwrap().len(), 1);
}

#[test]
fn single_delete_cancelled_by_other_actions() {
    let (_dir, store, mut app, newer, older) = app_with_two_saved_records();

    // Selecting another row cancels the armed delete.
    let _ = update(&mut app, Message::HistoryDelete(newer.id));
    let _ = update(&mut app, Message::HistorySelect(older.id));
    assert_eq!(app.history_delete_pending, None);

    // A search change (Escape maps here too) cancels it.
    let _ = update(&mut app, Message::HistoryDelete(newer.id));
    let _ = update(&mut app, Message::HistorySearchChanged("x".into()));
    assert_eq!(app.history_delete_pending, None);

    // Explicit cancel clears it as well.
    let _ = update(&mut app, Message::HistoryDelete(newer.id));
    let _ = update(&mut app, Message::HistoryDeleteCancelled);
    assert_eq!(app.history_delete_pending, None);

    // Nothing was ever removed while only arming/cancelling.
    assert_eq!(store.list().unwrap().len(), 2);
}

// ---- #7: open browser refreshes on save and on OCR result ----

#[test]
fn history_record_saved_refreshes_only_when_browser_open() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));

    // Closed browser: no reload work is scheduled.
    app.history_window_id = None;
    assert_eq!(update(&mut app, Message::HistoryRecordSaved).units(), 0);

    // Open browser: the fresh (text-less) row triggers a reload.
    app.history_window_id = Some(iced::window::Id::unique());
    assert_eq!(update(&mut app, Message::HistoryRecordSaved).units(), 1);
}

#[test]
fn history_persisted_refreshes_on_text_but_not_on_ocr_failure() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.history_window_id = Some(iced::window::Id::unique());

    // OCR landed text → reload to surface it.
    assert_eq!(
        update(&mut app, Message::HistoryRecordPersisted(Ok(true))).units(),
        1
    );
    // OCR failed/empty → row already shown via HistoryRecordSaved,
    // text won't change, so no extra reload.
    assert_eq!(
        update(&mut app, Message::HistoryRecordPersisted(Ok(false))).units(),
        0
    );
}

// ---- #10: pagination bounds the rendered list ----

#[test]
fn history_show_more_grows_page_and_search_resets_it() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    assert_eq!(app.history_page_limit, crate::app::HISTORY_PAGE_SIZE);

    let _ = update(&mut app, Message::HistoryShowMore);
    assert_eq!(app.history_page_limit, crate::app::HISTORY_PAGE_SIZE * 2);

    // A search change resets paging to the first page so the new
    // result set starts from the top.
    let _ = update(&mut app, Message::HistorySearchChanged("q".into()));
    assert_eq!(app.history_page_limit, crate::app::HISTORY_PAGE_SIZE);
}

// ---- #32: user-facing statuses are human-readable ----

#[test]
fn friendly_errors_hide_raw_internals_and_keep_hints() {
    use readshot_core::HistoryError;

    // Permission-denied io errors keep a useful, plain hint and
    // never leak the raw Display string into the status line.
    let denied = HistoryError::Io("Permission denied (os error 13)".into());
    let msg = history_ops::friendly_history_error("delete this capture", &denied);
    assert!(msg.contains("permission denied"), "{msg}");
    assert!(!msg.contains("os error 13"), "{msg}");

    // Unclassified io errors fall back to a generic message.
    let other = HistoryError::Io("broken pipe".into());
    let msg = history_ops::friendly_history_error("clear the history", &other);
    assert!(msg.starts_with("Couldn't clear the history"), "{msg}");
    assert!(!msg.contains("broken pipe"), "{msg}");

    // String-typed (clipboard / file-manager) errors: not-found
    // hint, and no raw text leaked otherwise.
    let nf = history_ops::friendly_string_error("open the save folder", "No such file");
    assert!(nf.contains("already gone"), "{nf}");
    let generic =
        history_ops::friendly_string_error("copy the image to the clipboard", "weird backend");
    assert!(!generic.contains("weird backend"), "{generic}");

    // A recovery next-step is appended as a second sentence; an
    // unclassified error drops the "See the log" tail in its favour.
    let save = history_ops::friendly_string_error_with(
        "save the image",
        "No space left on device",
        Some(history_ops::SAVE_HINT),
    );
    assert!(save.contains("the disk is full"), "{save}");
    assert!(
        save.ends_with("Pick a different folder in Settings and try again."),
        "{save}"
    );
    let retry = history_ops::friendly_string_error_with(
        "copy the text",
        "weird backend",
        Some("Try again."),
    );
    assert_eq!(retry, "Couldn't copy the text. Try again.", "{retry}");

    // Capture errors suggest the Screen Recording permission only
    // when the error looks permission-related, never otherwise.
    let perm = history_ops::friendly_capture_error("permission denied (os error 1)");
    assert!(perm.contains("permission denied"), "{perm}");
    assert!(perm.contains("Screen Recording"), "{perm}");
    let other = history_ops::friendly_capture_error("backend offline");
    assert!(!other.contains("backend offline"), "{other}");
    assert!(other.ends_with("Try again."), "{other}");
}

#[cfg(target_os = "macos")]
#[test]
fn reveal_command_uses_finder_selection_on_macos() {
    let command = reveal_command_for_path(std::path::Path::new("/tmp/readshot/capture.png"));
    assert_eq!(command.program, "open");
    assert_eq!(command.args, vec!["-R", "/tmp/readshot/capture.png"]);
}

#[cfg(target_os = "macos")]
#[test]
fn open_folder_command_uses_plain_open_on_macos() {
    let command = open_folder_command_for_path(std::path::Path::new("/tmp/readshot"));
    assert_eq!(command.program, "open");
    assert_eq!(command.args, vec!["/tmp/readshot"]);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_notification_script_escapes_title_and_body() {
    let script = macos_notification_script("Readshot \"updates\"", "Path C:\\tmp\\\"x\"");
    assert_eq!(
        script,
        r#"display notification "Path C:\\tmp\\\"x\"" with title "Readshot \"updates\"""#
    );
}

#[cfg(target_os = "macos")]
#[test]
fn relaunch_command_waits_then_forces_a_fresh_instance() {
    let command = relaunch_command_for_bundle(
        std::path::Path::new("/Applications/Readshot.app"),
        RelaunchMode::NewInstance,
    );

    assert_eq!(command.program, "/bin/sh");
    assert!(command.args[1].contains("sleep 0.35"));
    assert!(command.args[1].contains("/usr/bin/open -n \"$1\""));
    assert_eq!(command.args[3], "/Applications/Readshot.app");
}

#[test]
fn history_thumbnail_path_matches_core_layout() {
    use chrono::TimeZone;
    let record = readshot_core::CaptureRecord::new(
        chrono::Utc
            .with_ymd_and_hms(2026, 4, 25, 14, 30, 0)
            .unwrap(),
        100,
        100,
        "primary",
    );
    let root = std::path::Path::new("/history");
    assert_eq!(
        history_thumbnail_path(root, &record),
        root.join(readshot_core::FsHistoryStore::thumbnail_path(&record))
    );
}

#[cfg(target_os = "windows")]
#[test]
fn reveal_command_uses_explorer_selection_on_windows() {
    let command = reveal_command_for_path(std::path::Path::new("C:\\tmp\\capture.png"));
    assert_eq!(command.program, "explorer");
    // explorer.exe needs `/select,` and the path in ONE argv token,
    // otherwise it ignores the selection.
    assert_eq!(command.args, vec!["/select,C:\\tmp\\capture.png"]);
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn reveal_command_opens_parent_directory_on_unix() {
    let command = reveal_command_for_path(std::path::Path::new("/tmp/readshot/capture.png"));
    assert_eq!(command.program, "xdg-open");
    assert_eq!(command.args, vec!["/tmp/readshot"]);
}

#[test]
fn permission_tick_drives_welcome_state_machine() {
    let perms = Arc::new(FakePermissions::denied());
    let mut app = build_app(perms.clone());
    // Pre-condition: welcome shows.
    assert!(app.welcome.should_show());
    // After the OS flips to granted, a tick clears welcome.
    perms.flip_to_granted();
    let _ = update(&mut app, Message::PermissionTick);
    assert!(!app.welcome.should_show());
}

#[test]
fn welcome_ready_marks_onboarding_completed_after_grant() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.onboarding_completed = false;
    app.welcome = WelcomeState::Granted;

    let _ = update(
        &mut app,
        Message::WelcomeWindowReady(iced::window::Id::unique()),
    );

    assert!(app.preferences.onboarding_completed);
}

#[test]
fn capture_request_marks_in_flight_and_clears_on_completion() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(&mut app, Message::CaptureFullPrimaryRequested);
    assert!(app.capture_in_flight);
    let _ = update(
        &mut app,
        Message::CaptureSaved(Ok(PathBuf::from("/tmp/x.png"))),
    );
    assert!(!app.capture_in_flight);
    let toast = app.last_capture_status.as_deref().unwrap();
    assert!(toast.contains("/tmp/x.png"));
}

#[test]
fn welcome_status_dismissed_clears_capture_status() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.last_capture_status = Some("Couldn't save.".into());
    let _ = update(&mut app, Message::WelcomeStatusDismissed);
    assert!(app.last_capture_status.is_none());
}

#[test]
fn capture_failure_message_lands_in_toast() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::CaptureSaved(Err("permission denied".into())),
    );
    assert!(!app.capture_in_flight);
    let toast = app.last_capture_status.as_deref().unwrap();
    assert!(toast.contains("permission denied"));
}
