//! Text panel: recognition, copy, redact lines.

use super::*;

#[test]
fn snap_to_text_turns_a_stroke_into_the_line_box() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Highlighter,
        )),
    );
    let _ = update(
        &mut app,
        Message::EditorStyle(readshot_ui::editor::StyleMessage::SnapToText(true)),
    );
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
            highlight_stroke(),
        )),
    );
    match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Highlighter {
            points, line_width, ..
        } => {
            assert_eq!(points.len(), 2);
            assert!((points[0].x - 22.4).abs() < 0.01, "{points:?}");
            assert!((points[1].x - 57.6).abs() < 0.01, "{points:?}");
            assert!((points[0].y - 22.4).abs() < 0.01 && (points[1].y - 22.4).abs() < 0.01);
            assert!((line_width - 6.4).abs() < 0.01);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn snap_off_keeps_the_freehand_stroke() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
            highlight_stroke(),
        )),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.annotations()[0],
        highlight_stroke()
    );
}

#[test]
fn choosing_the_highlighter_starts_recognition() {
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = false;
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    select_highlighter(&mut app);
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
}

#[test]
fn choosing_the_highlighter_without_line_positions_does_not_recognize() {
    // Snap to text needs line boxes; an engine without them would
    // copy and OCR the whole image for nothing.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = false;
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    select_highlighter(&mut app);
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Idle
    ));
}

#[test]
fn text_recognition_result_for_the_current_session_is_kept() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(&mut app, Message::EditorTextRecognize);
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
    ready_editor(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.text_panel.document().unwrap().lines.len(), 3);
    assert_eq!(ed.text_panel.matches.len(), 2);
    assert!(!ed.busy, "recognition never blocks editing");
}

#[test]
fn stale_text_recognition_result_is_dropped() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(&mut app, Message::EditorTextRecognize);
    let session = app.editor.as_ref().unwrap().session_id + 1000;
    let _ = update(
        &mut app,
        Message::EditorTextRecognized {
            session,
            part: 0,
            result: Ok(sample_doc()),
        },
    );
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
}

#[test]
fn failed_recognition_toasts_retry_and_retry_restarts() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(&mut app, Message::EditorTextRecognize);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::EditorTextRecognized {
            session,
            part: 0,
            result: Err("Vision OCR timed out".into()),
        },
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(matches!(
        ed.text_panel.recognition,
        crate::editor::TextRecognition::Failed(_)
    ));
    assert!(ed.status_is_error(), "{:?}", ed.status);
    assert_eq!(
        ed.toast_action(),
        Some(crate::editor::EditorToastAction::RetryRecognition)
    );

    let _ = update(&mut app, Message::EditorTextRecognize);
    let ed = app.editor.as_ref().unwrap();
    assert!(matches!(
        ed.text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
    assert!(ed.status.is_none() && ed.toast_action().is_none());
}

#[test]
fn recognition_is_not_restarted_once_ready() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorTextRecognize);
    assert!(app.editor.as_ref().unwrap().text_panel.document().is_some());
}

#[test]
fn opening_an_editor_starts_recognition_only_with_the_panel_open() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = open_editor_window_replacing(
        &mut app,
        crate::editor::EditorSession::new(solid(64, 64)),
        None,
    );
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
    app.preferences.editor_panel_open = false;
    let _ = open_editor_window_replacing(
        &mut app,
        crate::editor::EditorSession::new(solid(64, 64)),
        None,
    );
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Idle
    ));
}

#[test]
fn a_fresh_capture_with_history_shares_its_ocr_with_the_text_panel() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    // Panel closed, so only the history OCR could mark it Recognizing.
    app.preferences.editor_panel_open = false;
    app.preferences.history_retention = readshot_core::HistoryRetention::Last50;
    let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 64))));
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
    app.preferences.history_retention = readshot_core::HistoryRetention::Off;
    let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 64))));
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Idle
    ));
}

#[test]
fn reopening_from_history_without_positions_reuses_the_stored_text() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "fake-0");
    record.ocr_text = Some("one\ntwo".into());
    let _ =
        super::super::history_ops::open_reopened(&mut app, (solid(64, 64), record.clone()), None);
    let doc = app
        .editor
        .as_ref()
        .unwrap()
        .text_panel
        .document()
        .expect("seeded, no OCR");
    assert_eq!(doc.lines.len(), 2);
    assert!(!doc.positioned);

    // A positioned engine (Apple Vision) recognizes again for boxes.
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_lines(Vec::new())),
        perms.clone(),
        None,
    );
    let mut app = App::new(coord, perms, Preferences::default());
    let _ = super::super::history_ops::open_reopened(&mut app, (solid(64, 64), record), None);
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
}

#[test]
fn background_recognition_failure_is_quiet_with_the_panel_closed() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = false;
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let session = app.editor.as_ref().unwrap().session_id;
    // A history-shared OCR nobody asked for fails: no toast.
    let _ = update(
        &mut app,
        Message::EditorTextRecognized {
            session,
            part: 0,
            result: Err("no models".into()),
        },
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(matches!(
        ed.text_panel.recognition,
        crate::editor::TextRecognition::Failed(_)
    ));
    assert!(ed.status.is_none() && ed.toast_action().is_none());

    // Asked for explicitly: the failure toasts with Retry.
    let _ = update(&mut app, Message::EditorTextRecognize);
    let _ = update(
        &mut app,
        Message::EditorTextRecognized {
            session,
            part: 0,
            result: Err("no models".into()),
        },
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.toast_action(),
        Some(crate::editor::EditorToastAction::RetryRecognition)
    );
}

#[tokio::test]
async fn history_ocr_feeds_the_editor_first_and_stores_the_layout_text() {
    let (msgs, coord, record, _dir) = run_persist_history(
        FakeOcrEngine::with_text("one\n\ntwo"),
        solid(16, 16),
        Some(7),
    )
    .await;
    // OCR resolves before the PNG encode / save, so the panel isn't
    // held back by them.
    match &msgs[0] {
        Message::EditorTextRecognized {
            session: 7,
            part: 0,
            result: Ok(doc),
        } => {
            assert_eq!(doc.layout_text, "one\n\ntwo");
        }
        other => panic!("expected EditorTextRecognized first, got {other:?}"),
    }
    assert!(msgs
        .iter()
        .any(|m| matches!(m, Message::HistoryRecordSaved)));
    assert!(matches!(
        msgs.last(),
        Some(Message::HistoryRecordPersisted(Ok(true)))
    ));
    let stored = coord.history_list().unwrap();
    let stored = stored.iter().find(|r| r.id == record.id).unwrap();
    assert_eq!(stored.ocr_text.as_deref(), Some("one\n\ntwo"));

    // No editor session: nothing is sent to an editor.
    let (msgs, _, _, _dir) =
        run_persist_history(FakeOcrEngine::with_text("hi"), solid(16, 16), None).await;
    assert!(!msgs
        .iter()
        .any(|m| matches!(m, Message::EditorTextRecognized { .. })));
}

#[tokio::test]
async fn history_ocr_errors_reach_the_editor_even_when_the_save_fails() {
    let (msgs, _, _, _dir) =
        run_persist_history(FakeOcrEngine::failing("boom"), solid(16, 16), Some(3)).await;
    assert!(matches!(
        &msgs[0],
        Message::EditorTextRecognized {
            session: 3,
            part: 0,
            result: Err(_)
        }
    ));
    assert!(matches!(
        msgs.last(),
        Some(Message::HistoryRecordPersisted(Ok(false)))
    ));

    // A 0 × 0 image fails the PNG encode: no record is saved, but the
    // editor still gets its OCR result (it never stays Recognizing).
    let (msgs, _, _, _dir) = run_persist_history(
        FakeOcrEngine::with_text("hi"),
        image::RgbaImage::new(0, 0),
        Some(4),
    )
    .await;
    assert!(
        matches!(
            &msgs[..],
            [Message::EditorTextRecognized {
                session: 4,
                part: 0,
                result: Ok(_)
            }]
        ),
        "{msgs:?}"
    );
}

#[test]
fn option_command_p_toggles_the_panel_and_command_p_still_pins() {
    use iced::keyboard::Key;
    // ⌥⌘P on macOS, Ctrl+Shift+P elsewhere.
    let alt_cmd = editor_alt_modifiers();
    for ignored in [true, false] {
        assert!(matches!(
            editor_key_message(Key::Character("p".into()), alt_cmd, ignored),
            Some(Message::EditorPanelToggled)
        ));
    }
    assert!(matches!(
        editor_key_message(Key::Character("p".into()), command_modifiers(), true),
        Some(Message::EditorPinRequested)
    ));
}

#[test]
fn toggling_the_panel_is_remembered_and_starts_recognition() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = false;
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(&mut app, Message::EditorPanelToggled);
    assert!(app.preferences.editor_panel_open);
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
    let _ = update(&mut app, Message::EditorPanelToggled);
    assert!(!app.preferences.editor_panel_open);
}

#[test]
fn narrow_window_opens_a_temporary_overlay_that_closes_three_ways() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_width = 800.0;
    app.editor = Some(ed);
    let overlay = |app: &App| app.editor.as_ref().unwrap().panel_overlay_open;
    assert_eq!(
        editor_panel_mode(&app, app.editor.as_ref().unwrap()),
        PanelMode::Collapsed
    );
    let _ = update(&mut app, Message::EditorPanelToggled);
    assert!(overlay(&app));
    assert!(
        app.preferences.editor_panel_open,
        "the remembered preference is untouched"
    );
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
    // esc (popover level of the single Esc chain)
    let _ = update(&mut app, Message::EditorTextCancel);
    assert!(!overlay(&app));
    // click outside
    let _ = update(&mut app, Message::EditorPanelToggled);
    let _ = update(&mut app, Message::EditorPanelOverlayDismissed);
    assert!(!overlay(&app));
    // ⌥⌘P again
    let _ = update(&mut app, Message::EditorPanelToggled);
    let _ = update(&mut app, Message::EditorPanelToggled);
    assert!(!overlay(&app));
    assert!(app.preferences.editor_panel_open);
}

#[test]
fn editor_window_resize_updates_only_the_editor_width() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    app.editor = Some(ed);
    assert_eq!(
        app.editor.as_ref().unwrap().window_width,
        crate::editor::DEFAULT_WINDOW_WIDTH
    );
    let _ = update(
        &mut app,
        Message::EditorWindowResized(iced::window::Id::unique(), 800.0),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().window_width,
        crate::editor::DEFAULT_WINDOW_WIDTH
    );
    let _ = update(&mut app, Message::EditorWindowResized(id, 800.0));
    assert_eq!(app.editor.as_ref().unwrap().window_width, 800.0);
}

#[test]
fn panel_tab_is_remembered_and_no_text_links_to_text_recognition_settings() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    app.preferences.editor_panel_tab = "beautify".into();
    let _ = update(
        &mut app,
        Message::EditorPanelTabSelected(crate::editor::PanelTab::Text),
    );
    assert_eq!(app.preferences.editor_panel_tab, "text");
    let _ = update(&mut app, Message::EditorOpenTextSettings);
    assert_eq!(app.settings_pane, crate::app::SettingsPane::TextRecognition);
    assert!(app.settings_window_id.is_some());
}

#[test]
fn esc_closes_a_popover_then_the_overlay_panel_and_never_arms_discard() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.window_width = 800.0;
    ed.panel_overlay_open = true;
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    // Captured esc: the popover goes first, the overlay stays.
    let _ = update(&mut app, Message::EditorTextCancel);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(ed.panel_overlay_open);
    // An esc no widget captured closes the overlay instead of
    // falling through to Discard.
    let _ = update(
        &mut app,
        Message::EditorKeyPressed {
            window: id,
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: true,
        },
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.panel_overlay_open);
    assert!(!ed.discard_armed());
}

#[test]
fn an_overlay_widened_into_the_dock_hides_with_one_toggle() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = false;
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_width = 800.0;
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorPanelToggled);
    app.editor.as_mut().unwrap().window_width = 1200.0;
    assert_eq!(
        editor_panel_mode(&app, app.editor.as_ref().unwrap()),
        PanelMode::Docked
    );
    // "Hide Panel" on what is shown hides it, not re-opens it.
    let _ = update(&mut app, Message::EditorPanelToggled);
    assert_eq!(
        editor_panel_mode(&app, app.editor.as_ref().unwrap()),
        PanelMode::Hidden
    );
    assert!(!app.preferences.editor_panel_open);
}

#[test]
fn widening_the_window_folds_an_open_overlay_into_the_docked_preference() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = false;
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.window_width = 800.0;
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorPanelToggled);
    assert!(app.editor.as_ref().unwrap().panel_overlay_open);
    let _ = update(&mut app, Message::EditorWindowResized(id, 1200.0));
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.panel_overlay_open, "no stale overlay flag");
    assert!(app.preferences.editor_panel_open, "what is shown is kept");
    assert_eq!(editor_panel_mode(&app, ed), PanelMode::Docked);
    // Narrowing again collapses; nothing reopens as an overlay.
    let _ = update(&mut app, Message::EditorWindowResized(id, 800.0));
    assert_eq!(
        editor_panel_mode(&app, app.editor.as_ref().unwrap()),
        PanelMode::Collapsed
    );
}

#[test]
fn a_stale_overlay_flag_in_a_wide_window_does_not_eat_esc() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_width = 1200.0;
    ed.panel_overlay_open = true;
    ed.model.commit_annotation(Annotation::Blur {
        rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
        radius: 4.0,
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorTextCancel);
    assert!(
        app.editor
            .as_ref()
            .unwrap()
            .model
            .selected_annotation()
            .is_none(),
        "esc reached the annotation selection"
    );
}

#[test]
fn opening_the_panel_closes_an_open_popover() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = false;
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    assert!(app.editor.as_ref().unwrap().popover.is_some());
    let _ = update(&mut app, Message::EditorPanelToggled);
    assert!(app.preferences.editor_panel_open);
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
    // Narrow-window overlay too.
    app.preferences.editor_panel_open = false;
    app.editor.as_mut().unwrap().window_width = 800.0;
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let _ = update(&mut app, Message::EditorPanelToggled);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.panel_overlay_open);
    assert_eq!(ed.popover, None);
}

#[test]
fn dismissing_no_overlay_keeps_the_hovered_line() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorTextLineHovered(1));
    let _ = update(&mut app, Message::EditorPanelOverlayDismissed);
    assert_eq!(app.editor.as_ref().unwrap().text_panel.hovered, Some(1));
    // An open overlay that closes does drop the hover.
    let ed = app.editor.as_mut().unwrap();
    ed.window_width = 800.0;
    ed.panel_overlay_open = true;
    let _ = update(&mut app, Message::EditorPanelOverlayDismissed);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.panel_overlay_open);
    assert_eq!(ed.text_panel.hovered, None);
}

#[test]
fn command_changes_are_tracked_without_touching_the_editor() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorCommandChanged(true));
    assert!(app.editor_command_held);
    let _ = update(&mut app, Message::EditorCommandChanged(false));
    assert!(!app.editor_command_held);
}

#[test]
fn editor_modifier_flags_reset_when_an_editor_opens_or_closes() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let hold = |app: &mut App| {
        app.editor_command_held = true;
        app.editor_shift_held = true;
    };
    let released = |app: &App| !app.editor_command_held && !app.editor_shift_held;

    // Opening (the old window's key-up never arrives).
    hold(&mut app);
    let _ = open_editor_window_replacing(
        &mut app,
        crate::editor::EditorSession::new(solid(16, 16)),
        None,
    );
    assert!(released(&app), "open");

    // Closing via Discard on a clean editor.
    hold(&mut app);
    let _ = update(&mut app, Message::EditorDiscardRequested);
    assert!(app.editor.is_none());
    assert!(released(&app), "discard");

    // Closing via Pin.
    let _ = open_editor_window_replacing(
        &mut app,
        crate::editor::EditorSession::new(solid(16, 16)),
        None,
    );
    hold(&mut app);
    let _ = update(&mut app, Message::EditorPinRequested);
    assert!(app.editor.is_none());
    assert!(released(&app), "pin");

    // Closing the window from the OS.
    let _ = open_editor_window_replacing(
        &mut app,
        crate::editor::EditorSession::new(solid(16, 16)),
        None,
    );
    let id = app.editor.as_ref().unwrap().window_id;
    let id = id.unwrap_or_else(|| {
        let id = iced::window::Id::unique();
        app.editor.as_mut().unwrap().window_id = Some(id);
        id
    });
    hold(&mut app);
    let _ = update(&mut app, Message::WindowClosed(id));
    assert!(app.editor.is_none());
    assert!(released(&app), "window closed");
}

#[test]
fn copy_all_uses_the_recognized_text_when_annotations_are_text_neutral() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorCopyTextRequested);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.busy);
    assert_eq!(ed.status_text(), Some("Copying text…"));
}

#[test]
fn copy_all_re_recognizes_the_output_when_annotations_add_or_hide_text() {
    let typed = Annotation::Text {
        content: "typed note".into(),
        origin: PointLike::new(4.0, 4.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 16.0,
        style: Default::default(),
    };
    let scribble = Annotation::Pen {
        points: vec![PointLike::new(0.0, 20.0), PointLike::new(60.0, 22.0)],
        color: Rgba::OPAQUE_BLACK,
        line_width: 6.0,
    };
    let blur = Annotation::Blur {
        rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
        radius: 4.0,
    };
    for a in [typed, scribble, blur] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        app.editor
            .as_mut()
            .unwrap()
            .model
            .commit_annotation(a.clone());
        let _ = update(&mut app, Message::EditorCopyTextRequested);
        assert_eq!(
            app.editor.as_ref().unwrap().status_text(),
            Some("Recognizing text…"),
            "{a:?}"
        );
    }
}

#[test]
fn copy_all_commits_a_pending_text_draft_before_choosing_the_source() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    app.editor
        .as_mut()
        .unwrap()
        .begin_text_edit(crate::editor::PendingText {
            origin: PointLike::new(4.0, 4.0),
            content: "draft".into(),
            edit_index: None,
        });
    let _ = update(&mut app, Message::EditorCopyTextRequested);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 1, "draft committed");
    assert_eq!(ed.status_text(), Some("Recognizing text…"));
}

#[test]
fn copy_all_before_recognition_falls_back_to_ocr() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(&mut app, Message::EditorCopyTextRequested);
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some("Recognizing text…")
    );
}

#[test]
fn panel_line_clicks_follow_shift_and_command() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let selected = |app: &App| -> Vec<usize> {
        app.editor
            .as_ref()
            .unwrap()
            .text_panel
            .selected
            .iter()
            .copied()
            .collect()
    };
    let _ = update(&mut app, Message::EditorTextLineClicked(0));
    assert_eq!(selected(&app), [0]);
    let _ = update(&mut app, Message::EditorShiftChanged(true));
    let _ = update(&mut app, Message::EditorTextLineClicked(2));
    assert_eq!(selected(&app), [0, 1, 2]);
    let _ = update(&mut app, Message::EditorShiftChanged(false));
    let _ = update(&mut app, Message::EditorCommandChanged(true));
    let _ = update(&mut app, Message::EditorTextLineClicked(1));
    assert_eq!(selected(&app), [0, 2]);
    let _ = update(&mut app, Message::EditorTextLineClicked(9));
    assert_eq!(selected(&app), [0, 2], "out-of-range clicks are ignored");
}

#[test]
fn hover_tracks_the_line_under_the_pointer() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorTextLineHovered(1));
    let _ = update(&mut app, Message::EditorTextLineHovered(2));
    // A late exit from line 1 must not clear line 2's hover.
    let _ = update(&mut app, Message::EditorTextLineUnhovered(1));
    assert_eq!(app.editor.as_ref().unwrap().text_panel.hovered, Some(2));
    let _ = update(&mut app, Message::EditorTextLineUnhovered(2));
    assert_eq!(app.editor.as_ref().unwrap().text_panel.hovered, None);
}

#[test]
fn copy_lines_is_ignored_while_busy() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorTextLineClicked(0));
    app.editor.as_mut().unwrap().busy = true;
    let _ = update(&mut app, Message::EditorCopyLines);
    assert!(app.editor.as_ref().unwrap().status.is_none());
}

#[test]
fn copy_lines_needs_a_selection_and_reports_the_count() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorCopyLines);
    assert!(app.editor.as_ref().unwrap().status.is_none());
    let _ = update(&mut app, Message::EditorTextLineClicked(0));
    let _ = update(&mut app, Message::EditorCopyLines);
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some("Copying text…")
    );
    let _ = update(&mut app, Message::EditorCopyLinesDone(Ok(2)));
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some("Copied 2 lines.")
    );
}

#[test]
fn remove_line_breaks_toggle_is_remembered() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorRemoveLineBreaksToggled(true));
    assert!(app.preferences.editor_copy_remove_line_breaks);
}

#[test]
fn esc_clears_the_panel_selection_last() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorTextLineClicked(1));
    // commit_annotation selects the new annotation.
    app.editor
        .as_mut()
        .unwrap()
        .model
        .commit_annotation(Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
            radius: 4.0,
        });
    let _ = update(&mut app, Message::EditorTextCancel);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.model.selected_annotation().is_none());
    assert!(
        !ed.text_panel.selected.is_empty(),
        "first Esc clears the canvas selection"
    );
    let _ = update(&mut app, Message::EditorTextCancel);
    assert!(app.editor.as_ref().unwrap().text_panel.selected.is_empty());
}

#[test]
fn text_marks_follow_hover_and_selection_only_with_positions() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorTextLineHovered(1));
    let _ = update(&mut app, Message::EditorTextLineClicked(0));
    let ed = app.editor.as_ref().unwrap();
    let doc = ed.text_panel.document().unwrap();
    let (hover, selected) = text_marks(ed, true);
    assert_eq!(hover, doc.lines[1].rect);
    assert_eq!(selected, vec![doc.lines[0].rect.unwrap()]);
    assert_eq!(text_marks(ed, false), (None, Vec::new()));

    let mut plain = crate::editor::EditorSession::new(solid(64, 64));
    plain.text_panel.recognition = crate::editor::TextRecognition::Ready(std::sync::Arc::new(
        readshot_core::text_lines::TextDocument::from_plain("one\ntwo"),
    ));
    plain.text_panel.hovered = Some(0);
    assert_eq!(text_marks(&plain, true), (None, Vec::new()));
}

#[test]
fn redact_all_adds_one_block_per_item_as_one_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_ref().unwrap();
    let anns = ed.model.annotations();
    assert_eq!(anns.len(), 2);
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert_eq!(ed.status_text(), Some("Redacted 2 items."));
    assert!(matches!(
        ed.visible_toast_action(),
        Some(crate::editor::EditorToastAction::UndoRedaction { .. })
    ));
    // Span-precise: the email block starts at the address's word box
    // (x 22.4, padded and snapped to 21), not at "Email" (x 6.4).
    match &anns[0] {
        Annotation::Rectangle {
            rect, style, color, ..
        } => {
            assert_eq!(style.fill, readshot_core::FillMode::Filled);
            assert_eq!(*color, Rgba::OPAQUE_BLACK);
            assert_eq!(rect.x, 21.0);
        }
        other => panic!("{other:?}"),
    }

    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.model.annotations().len(),
        2,
        "covered items are not redacted twice"
    );
    assert_eq!(ed.status_text(), Some("No sensitive items left to redact."));

    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    assert!(app.editor.as_ref().unwrap().model.annotations().is_empty());
}

#[test]
fn redact_line_uses_the_chosen_style() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(
        &mut app,
        Message::EditorRedactStyleChanged(readshot_core::redact::RedactStyle::Pixelate),
    );
    let _ = update(&mut app, Message::EditorRedactLine(2));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 1);
    assert!(matches!(
        ed.model.annotations()[0],
        Annotation::Pixelate { .. }
    ));
    assert_eq!(ed.status_text(), Some("Redacted 1 item."));
    let _ = update(&mut app, Message::EditorRedactLine(0));
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
}

#[test]
fn redaction_needs_line_positions() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(&mut app, Message::EditorTextRecognize);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::EditorTextRecognized {
            session,
            part: 0,
            result: Ok(readshot_core::text_lines::TextDocument::from_plain(
                "Email priya@example.com",
            )),
        },
    );
    assert!(app.editor.as_ref().unwrap().text_panel.matches.is_empty());
    let _ = update(&mut app, Message::EditorRedactAll);
    assert!(app.editor.as_ref().unwrap().model.annotations().is_empty());
}

#[test]
fn unlocatable_items_are_reported_not_dropped() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    // A match whose line no longer exists cannot be placed.
    app.editor
        .as_mut()
        .unwrap()
        .text_panel
        .matches
        .push(readshot_core::redact::RedactMatch {
            line: 99,
            span: 0..5,
            kind: readshot_core::redact::RedactKind::Email,
        });
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(super::super::text_panel::redact_state(ed).pending.len(), 2);
    assert_eq!(
        super::super::text_panel::redact_state(ed).unlocatable.len(),
        1
    );

    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 2);
    assert_eq!(
        ed.status_text(),
        Some("Redacted 2 items. 1 more couldn’t be located; cover it by hand.")
    );
    assert!(ed.visible_toast_action().is_some());

    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.status_text(),
        Some("Couldn’t locate 1 sensitive item. Cover it by hand.")
    );
    assert!(ed.status_is_error());
}

#[test]
fn redaction_undo_is_dropped_by_any_other_history_change() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorRedactAll);
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    assert_eq!(app.editor.as_ref().unwrap().toast_action(), None);
    // Redo returns to the redaction's depth; the Undo must not come
    // back (a different edit could sit at that depth too).
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 2);
    assert_eq!(ed.visible_toast_action(), None);

    // Same for history changes that set no status of their own
    // (e.g. a move): the history sync itself drops the Undo.
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    let _ = update(&mut app, Message::EditorRedactAll);
    let coord = app.coordinator.clone();
    let ed = app.editor.as_mut().unwrap();
    assert!(ed.visible_toast_action().is_some());
    assert!(ed.model.undo());
    super::super::editor_ops::sync_editor_history(ed, &coord);
    assert!(ed.model.redo());
    assert_eq!(ed.visible_toast_action(), None);
}

#[test]
fn arming_discard_drops_the_redaction_undo() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorRedactAll);
    let _ = update(&mut app, Message::EditorDiscardRequested);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.discard_armed());
    assert_eq!(ed.toast_action(), None);
}

#[test]
fn a_blur_redaction_does_not_count_as_solid() {
    use readshot_core::redact::RedactStyle;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(
        &mut app,
        Message::EditorRedactStyleChanged(RedactStyle::Blur),
    );
    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 2);
    let rs = super::super::text_panel::redact_state(ed);
    assert!(rs.pending.is_empty(), "Blur chosen: blurred items are done");
    assert_eq!(rs.weak.len(), 2);
    // Blur again: no duplicates.
    let _ = update(&mut app, Message::EditorRedactAll);
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 2);

    // Switch to Solid: the blurred items are pending again and Redact
    // All puts a solid block on top of each.
    let _ = update(
        &mut app,
        Message::EditorRedactStyleChanged(RedactStyle::Solid),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(super::super::text_panel::redact_state(ed).pending.len(), 2);
    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.status_text(), Some("Redacted 2 items."));
    let anns = ed.model.annotations();
    assert_eq!(anns.len(), 4);
    assert!(matches!(anns[2], Annotation::Rectangle { .. }));
    let rs = super::super::text_panel::redact_state(ed);
    assert!(rs.pending.is_empty() && rs.weak.is_empty());
    assert_eq!(rs.covered.len(), 2);
}

#[test]
fn a_weak_manual_blur_is_not_reported_as_redacted() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    app.editor
        .as_mut()
        .unwrap()
        .model
        .commit_annotation(Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 64.0, 64.0),
            radius: 2.0,
        });
    let ed = app.editor.as_ref().unwrap();
    let rs = super::super::text_panel::redact_state(ed);
    assert_eq!(rs.pending.len(), 2, "Solid (default) still offers both");
    assert_eq!(rs.weak.len(), 2);
    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 3);
    assert!(super::super::text_panel::redact_state(ed)
        .pending
        .is_empty());
}

#[test]
fn copy_all_after_a_redaction_recognizes_the_output() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let _ = update(&mut app, Message::EditorRedactAll);
    let _ = update(&mut app, Message::EditorCopyTextRequested);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.busy);
    assert_eq!(
        ed.status_text(),
        Some("Recognizing text…"),
        "redacted text must not come from the cached panel text"
    );
}

/// At the 64-step history cap a push drops the oldest step, so the undo
/// depth stays the same: the redaction's toast Undo must still go away
/// once another edit (a nudge, which sets no status) is newer.
#[test]
fn the_redaction_undo_hides_after_a_newer_edit_at_the_history_cap() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    {
        let ed = app.editor.as_mut().unwrap();
        for i in 0..70 {
            ed.model.commit_annotation(Annotation::Rectangle {
                rect: RectLike::new(1.0, 1.0, 2.0 + i as f32 * 0.01, 2.0),
                color: Rgba::OPAQUE_BLACK,
                line_width: 1.0,
                style: Default::default(),
            });
        }
        assert_eq!(
            ed.model.undo_depth(),
            readshot_ui::editor::undo::MAX_HISTORY
        );
    }
    let _ = update(&mut app, Message::EditorRedactAll);
    let ed = app.editor.as_mut().unwrap();
    assert!(ed.visible_toast_action().is_some());
    let last = ed.model.annotations().len() - 1;
    let Annotation::Rectangle { rect, .. } = ed.model.annotations()[last].clone() else {
        panic!("a redaction block");
    };
    let centre = PointLike::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    assert_eq!(ed.model.select_at(centre), Some(last));
    let _ = update(&mut app, Message::EditorNudgeSelected(1.0, 0.0));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.model.undo_depth(),
        readshot_ui::editor::undo::MAX_HISTORY
    );
    assert_eq!(
        ed.visible_toast_action(),
        None,
        "Undo would undo the nudge, not the redaction"
    );
}

/// A store that refuses the record (here: its root is a file, as with a
/// read-only or broken History folder): no success signal, no text
/// step, and the user is told in the editor what happened and how to
/// keep the image.
#[tokio::test]
async fn a_history_save_that_fails_is_reported_never_as_success() {
    use iced::futures::StreamExt;
    let dir = tempfile::TempDir::new().unwrap();
    let blocker = dir.path().join("history");
    std::fs::write(&blocker, b"not a folder").unwrap();
    let store: Arc<dyn HistoryStore> = Arc::new(FsHistoryStore::new(blocker.join("inner")));
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_text("hello")),
        perms.clone(),
        Some(store.clone()),
    );
    let img = solid(16, 16);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 16, 16, "fake-0");
    assert!(coord
        .record_history(
            record.clone(),
            readshot_core::encode_png(&img).unwrap(),
            readshot_core::HistoryRetention::Last50,
            chrono::Utc::now(),
        )
        .await
        .is_err());
    let session = 41;
    let (tx, rx) = iced::futures::channel::mpsc::channel(4);
    persist_history(
        coord.clone(),
        img.clone(),
        Some(record),
        readshot_core::HistoryRetention::Last50,
        Preferences::default(),
        Some(session),
        tx,
    )
    .await;
    let msgs: Vec<Message> = rx.collect().await;
    assert!(
        !msgs.iter().any(|m| matches!(
            m,
            Message::HistoryRecordSaved | Message::HistoryRecordPersisted(_)
        )),
        "{msgs:?}"
    );
    let failed = msgs
        .into_iter()
        .find(|m| matches!(m, Message::HistoryRecordSaveFailed { .. }))
        .expect("the failure is reported");

    // In the editor opened for the capture: a warning with the way out.
    let record = match &failed {
        Message::HistoryRecordSaveFailed { record_id, .. } => {
            let mut r = readshot_core::CaptureRecord::new(chrono::Utc::now(), 16, 16, "fake-0");
            r.id = *record_id;
            r
        }
        _ => unreachable!(),
    };
    let mut app = App::new(coord, perms, Preferences::default());
    let mut ed = crate::editor::EditorSession::from_history(img, record);
    ed.session_id = session;
    app.editor = Some(ed);
    let _ = update(&mut app, failed.clone());
    let ed = app.editor.as_ref().unwrap();
    assert!(
        ed.source_record.is_none(),
        "the editor no longer writes to it"
    );
    let status = ed.status_text().unwrap().to_owned();
    assert!(
        status.starts_with("Couldn’t save the capture to History"),
        "{status}"
    );
    assert!(status.contains(super::super::keys::SAVE_KEY), "{status}");
    assert!(ed.status_is_warning() || ed.status_is_error(), "{status}");
    // Without that editor: the capture status says so.
    app.editor = None;
    let _ = update(&mut app, failed);
    let status = app.last_capture_status.clone().unwrap();
    assert!(status.contains("capture again"), "{status}");
}

#[test]
fn history_save_failures_name_the_reason_and_the_fix() {
    use super::super::history_ops::history_save_failure;
    let full = history_save_failure("history io: No space left on device (os error 28)", true);
    assert!(full.contains("the disk is full"), "{full}");
    assert!(full.contains("Free some space"), "{full}");
    let ro = history_save_failure("Read-only file system (os error 30)", false);
    assert!(
        ro.contains("read-only") && ro.contains("capture again"),
        "{ro}"
    );
}
