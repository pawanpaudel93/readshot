//! Overlay, scroll capture, window placement, permission and pins.

use super::*;

#[test]
fn no_animation_tick_runs_for_static_overlays() {
    let src = include_str!("../../runtime.rs");
    assert!(!src.contains(concat!("Message::", "OverlayTick")));
}

#[test]
fn scroll_motion_accepts_real_vertical_motion() {
    let a = scrolling_texture(80, 120, 0);
    let b = scrolling_texture(80, 120, 14);

    assert!(frame_has_scroll_motion(&a, &b));
    let dy = scroll_motion_offset(&a, &b).unwrap();
    assert!((12..=16).contains(&dy), "expected dy near 14, got {dy}");
}

#[test]
fn scroll_motion_rejects_non_scroll_pixel_change() {
    let a = scrolling_texture(80, 120, 0);
    let mut b = a.clone();
    for y in 45..55 {
        for x in 20..30 {
            b.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
        }
    }

    assert!(!frame_has_scroll_motion(&a, &b));
    assert_eq!(scroll_motion_offset(&a, &b), Some(0));
}

#[test]
fn scroll_capture_drains_out_of_order_frames_in_sequence() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap();
    let mut session = crate::app::ScrollSession::new("display-a".into(), rect, 1.0);
    session.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
    app.scroll_session = Some(session);

    // Out-of-order arrival: seq 2 buffers until seq 1 lands.
    let _ = update(
        &mut app,
        Message::ScrollCaptureFrame {
            seq: 2,
            result: Ok(scrolling_texture(80, 120, 28)),
        },
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 1);
    assert_eq!(session.pending_frames.len(), 1);
    assert_eq!(session.judge_in_flight, None);

    // Seq 1 arrives: draining dispatches a motion judge for it and
    // pauses (the verdict task isn't executed in tests).
    let _ = update(
        &mut app,
        Message::ScrollCaptureFrame {
            seq: 1,
            result: Ok(scrolling_texture(80, 120, 14)),
        },
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 1);
    assert_eq!(session.judge_in_flight, Some(1));
    assert_eq!(session.pending_frames.len(), 1);

    // Verdict for seq 1 accepts the frame and dispatches the judge
    // for the buffered seq 2.
    let _ = update(
        &mut app,
        Message::ScrollFrameJudged {
            seq: 1,
            image: Some(scrolling_texture(80, 120, 14)),
            moved: true,
            offset: None,
            rows: None,
        },
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 2);
    assert_eq!(session.judge_in_flight, Some(2));
    assert!(session.pending_frames.is_empty());

    let _ = update(
        &mut app,
        Message::ScrollFrameJudged {
            seq: 2,
            image: Some(scrolling_texture(80, 120, 28)),
            moved: true,
            offset: None,
            rows: None,
        },
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 3);
    assert_eq!(session.judge_in_flight, None);
    assert_eq!(session.next_frame_seq_to_process, 3);
}

#[test]
fn scroll_judge_rejection_counts_no_motion_and_stale_verdicts_drop() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap();
    let mut session = crate::app::ScrollSession::new("display-a".into(), rect, 1.0);
    session.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
    session.judge_in_flight = Some(7);
    app.scroll_session = Some(session);

    // Stale verdict (wrong seq) is dropped entirely.
    let _ = update(
        &mut app,
        Message::ScrollFrameJudged {
            seq: 3,
            image: Some(scrolling_texture(80, 120, 14)),
            moved: true,
            offset: None,
            rows: None,
        },
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 1);
    assert_eq!(session.judge_in_flight, Some(7));

    // A "no motion" verdict bumps the counter without appending.
    let _ = update(
        &mut app,
        Message::ScrollFrameJudged {
            seq: 7,
            image: Some(scrolling_texture(80, 120, 0)),
            moved: false,
            offset: None,
            rows: None,
        },
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 1);
    assert_eq!(session.no_motion_count, 1);
    assert_eq!(session.judge_in_flight, None);
}

#[test]
fn scroll_hud_state_covers_every_phase() {
    let mut s = test_session();
    assert_eq!(scroll_hud_state(&s), ScrollHudState::Waiting);
    s.preview_handle = Some(iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]));
    assert_eq!(scroll_hud_state(&s), ScrollHudState::Recording);
    s.cancel_armed_at = Some(std::time::Instant::now());
    assert_eq!(scroll_hud_state(&s), ScrollHudState::Armed);
    s.stopping = true;
    s.stitch_frame_count = SCROLL_MAX_FRAMES;
    assert_eq!(scroll_hud_state(&s), ScrollHudState::Limit);
    s.stitch_frame_count = 5;
    assert_eq!(scroll_hud_state(&s), ScrollHudState::Stitching);
}

#[test]
fn accepted_frames_record_join_heights_capped_at_64() {
    let mut s = test_session();
    let first = scrolling_texture(80, 120, 0);
    let rows = preview_rows(&first, None);
    accept_scroll_frame(&mut s, first, Some(rows), None);
    // The first frame has nothing above it to join to.
    assert!(s.recent_join_heights.is_empty());
    accept_scroll_frame(
        &mut s,
        scrolling_texture(80, 120, 14),
        Some(image::RgbaImage::new(PREVIEW_STRIP_W, 60)),
        Some(14),
    );
    assert_eq!(
        s.recent_join_heights.iter().copied().collect::<Vec<_>>(),
        [60]
    );
    for _ in 0..80 {
        accept_scroll_frame(
            &mut s,
            scrolling_texture(80, 120, 14),
            Some(image::RgbaImage::new(PREVIEW_STRIP_W, 7)),
            Some(14),
        );
    }
    assert_eq!(s.recent_join_heights.len(), 64);
    assert_eq!(s.recent_join_heights.back(), Some(&7));
}

#[test]
fn join_ticks_sit_above_each_newer_slice_scaled_to_the_stage() {
    // Slices appended oldest→newest: 60 then 40 strip px. From the
    // bottom, the newest slice (40) ends at the first join, the
    // older one (60) at the second. Stage twice the strip width.
    let joins: std::collections::VecDeque<u32> = [60, 40].into_iter().collect();
    let ticks = join_tick_offsets(&joins, PREVIEW_STRIP_W as f32 * 2.0);
    assert_eq!(ticks, vec![80.0, 200.0]);
}

#[test]
fn scroll_window_footer_goes_compact_below_300px() {
    assert!(scroll_hud_compact(240.0));
    assert!(scroll_hud_compact(299.0));
    assert!(!scroll_hud_compact(SCROLL_WINDOW_W));
}

#[test]
fn discard_label_drops_the_count_below_340px() {
    assert_eq!(scroll_discard_label(299.0, 120), "Discard");
    assert_eq!(scroll_discard_label(330.0, 120), "Discard");
    assert_eq!(scroll_discard_label(340.0, 120), "Discard 120 Frames");
    assert_eq!(scroll_discard_label(SCROLL_WINDOW_W, 1), "Discard 1 Frame");
}

#[test]
fn stop_button_shows_esc_chip_only_while_recording_and_roomy() {
    assert_eq!(
        scroll_stop_key_chip(ScrollHudState::Waiting, false, true),
        None
    );
    assert_eq!(
        scroll_stop_key_chip(ScrollHudState::Recording, true, true),
        None
    );
    assert_eq!(
        scroll_stop_key_chip(ScrollHudState::Recording, false, true),
        Some(crate::runtime::ESC_KEY)
    );
    // esc not global: no chip promising a key the other app swallows.
    assert_eq!(
        scroll_stop_key_chip(ScrollHudState::Recording, false, false),
        None
    );
}

/// esc / ⇧esc are global hotkeys exactly while a session runs (not
/// while it stitches), and are released however it ends.
#[test]
fn scroll_capture_registers_esc_globally_only_while_it_runs() {
    use std::cell::RefCell;
    let registered: RefCell<Vec<global_hotkey::hotkey::HotKey>> = RefCell::new(Vec::new());
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let sync = |app: &mut App| {
        sync_scroll_escape_hotkeys_with(
            app,
            |h| {
                registered.borrow_mut().push(h);
                Ok(())
            },
            |h| registered.borrow_mut().retain(|x| *x != h),
        )
    };
    sync(&mut app);
    assert!(registered.borrow().is_empty());
    app.scroll_session = Some(test_session());
    sync(&mut app);
    assert_eq!(registered.borrow().len(), 2);
    let actions: Vec<_> = app.hotkey_actions.values().copied().collect();
    assert!(actions.contains(&crate::app::GlobalHotkeyAction::ScrollStop));
    assert!(actions.contains(&crate::app::GlobalHotkeyAction::ScrollDiscard));
    sync(&mut app);
    assert_eq!(registered.borrow().len(), 2, "idempotent");
    app.scroll_session.as_mut().unwrap().stopping = true;
    sync(&mut app);
    assert!(registered.borrow().is_empty());
    assert!(app.hotkey_actions.is_empty());
    assert!(app.scroll_escape_hotkeys.is_empty());
}

#[test]
fn keep_going_disarms_cancel() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut s = test_session();
    s.cancel_armed_at = Some(std::time::Instant::now());
    app.scroll_session = Some(s);
    let _ = update(&mut app, Message::ScrollCaptureKeepGoing);
    assert!(app
        .scroll_session
        .as_ref()
        .unwrap()
        .cancel_armed_at
        .is_none());
}

/// WCAG 2.2.1: the armed Discard prompt has no time limit. It stays
/// until Discard, Keep Going or a new frame answers it.
#[test]
fn cancel_arm_waits_for_an_answer() {
    let mut s = test_session();
    s.cancel_armed_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
    assert!(cancel_armed(&s), "still armed a minute later");
    assert_eq!(scroll_hud_state(&s), ScrollHudState::Armed);
    let prompt = scroll_discard_prompt(3);
    assert!(!prompt.contains(" s ") && !prompt.contains("within"));
    assert!(prompt.contains("3 frames"));
    s.cancel_armed_at = None;
    assert!(!cancel_armed(&s));
}

#[test]
fn a_new_frame_answers_an_armed_cancel() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut session = test_session();
    session.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
    session
        .frames
        .push(Arc::new(scrolling_texture(80, 120, 10)));
    app.scroll_session = Some(session);
    let _ = update(&mut app, Message::ScrollCaptureCancelRequested);
    assert!(cancel_armed(app.scroll_session.as_ref().unwrap()));
    accept_scroll_frame(
        app.scroll_session.as_mut().unwrap(),
        scrolling_texture(80, 120, 20),
        None,
        Some(10),
    );
    assert!(!cancel_armed(app.scroll_session.as_ref().unwrap()));
}

#[test]
fn frames_arriving_after_stop_do_not_touch_the_session() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut s = test_session();
    let first = scrolling_texture(80, 120, 0);
    let rows = preview_rows(&first, None);
    accept_scroll_frame(&mut s, first, Some(rows), None);
    // A capture and a judge verdict are outstanding at Stop.
    s.judge_in_flight = Some(1);
    s.pending_frames
        .insert(2, Ok(scrolling_texture(80, 120, 28)));
    s.capture_in_flight = 1;
    app.scroll_session = Some(s);
    let _ = update(&mut app, Message::ScrollCaptureStopRequested);
    let before = app.scroll_session.as_ref().unwrap();
    let strip_h = before.preview_strip.as_ref().unwrap().height();
    let stitched = before.stitched_rows;
    assert!(before.pending_frames.is_empty());
    assert_eq!(before.judge_in_flight, None);

    let _ = update(
        &mut app,
        Message::ScrollFrameJudged {
            seq: 1,
            image: Some(scrolling_texture(80, 120, 14)),
            moved: true,
            offset: Some(14),
            rows: Some(image::RgbaImage::new(PREVIEW_STRIP_W, 60)),
        },
    );
    let _ = update(
        &mut app,
        Message::ScrollCaptureFrame {
            seq: 3,
            result: Ok(scrolling_texture(80, 120, 42)),
        },
    );
    let after = app.scroll_session.as_ref().unwrap();
    assert!(after.frames.is_empty());
    assert!(after.pending_frames.is_empty());
    assert_eq!(after.judge_in_flight, None);
    assert_eq!(after.preview_strip.as_ref().unwrap().height(), strip_h);
    assert_eq!(after.stitched_rows, stitched);
}

#[test]
fn stitch_result_without_a_session_is_dropped() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    assert!(app.scroll_session.is_none());
    let _ = update(
        &mut app,
        Message::ScrollCaptureStitched(Ok(image::RgbaImage::new(10, 10))),
    );
    assert!(app.editor.is_none());
}

#[test]
fn cancel_is_ignored_while_stitching() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut s = test_session();
    s.stopping = true;
    app.scroll_session = Some(s);
    let _ = update(&mut app, Message::ScrollCaptureCancelRequested);
    assert!(app.scroll_session.is_some());
}

#[test]
fn open_overlay_leaves_a_running_scroll_session_alone() {
    let (mut app, hud, _) = app_with_scroll_windows(true);
    let _ = update(&mut app, Message::OpenOverlayRequested);
    assert!(!app.overlay_opening);
    let s = app.scroll_session.as_ref().unwrap();
    assert!(s.stopping);
    assert_eq!(s.hud_window_id, Some(hud));
}

#[test]
fn retake_leaves_a_running_scroll_session_alone() {
    let (mut app, hud, _) = app_with_scroll_windows(true);
    let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();
    app.last_regions.insert(
        "display-a".into(),
        crate::app::LastRegion {
            rect,
            display_scale: 2.0,
            display_bounds: None,
        },
    );
    app.last_region_display_id = Some("display-a".into());
    let _ = update(&mut app, Message::RetakeLastRegionRequested);
    assert_eq!(app.pending_display_id, None);
    let s = app.scroll_session.as_ref().unwrap();
    assert!(s.stopping);
    assert_eq!(s.hud_window_id, Some(hud));
}

#[test]
fn stitch_result_closes_both_scroll_windows() {
    let (mut app, hud, region) = app_with_scroll_windows(true);
    let _ = update(&mut app, Message::ScrollCaptureStitched(Err("boom".into())));
    assert!(app.scroll_session.is_none());
    assert_eq!(app.windows.kind(hud), None);
    assert_eq!(app.windows.kind(region), None);
}

#[test]
fn stop_with_no_frames_cancels_instead_of_failing() {
    let (mut app, hud, region) = app_with_scroll_windows(false);
    let _ = update(&mut app, Message::ScrollCaptureStopRequested);
    assert!(app.scroll_session.is_none());
    assert_eq!(app.windows.kind(hud), None);
    assert_eq!(app.windows.kind(region), None);
    let status = app.last_capture_status.as_deref().unwrap_or("");
    assert!(!status.contains("failed"), "status: {status}");
}

#[test]
fn editor_window_centers_on_capture_display() {
    let settings = editor_window_settings(Some((1440.0, 0.0, 1920.0, 1080.0)));

    match settings.position {
        window::Position::Specific(point) => {
            assert_eq!(point.x, 1850.0);
            assert_eq!(point.y, 160.0);
        }
        other => panic!("expected specific editor position, got {other:?}"),
    }
}

#[test]
fn pin_window_centers_on_capture_display() {
    let settings = pin_window_settings((400, 300), Some((-1280.0, 120.0, 1280.0, 720.0)));

    match settings.position {
        window::Position::Specific(point) => {
            assert_eq!(point.x, -840.0);
            assert_eq!(point.y, 330.0);
        }
        other => panic!("expected specific pin position, got {other:?}"),
    }
}

#[test]
fn editor_session_remembers_capture_display_bounds() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let bounds = Some((1440.0, 0.0, 1920.0, 1080.0));
    let editor = crate::editor::EditorSession::new(solid(32, 32));

    let _ = open_editor_window_replacing(&mut app, editor, bounds);

    assert_eq!(
        app.editor.as_ref().and_then(|ed| ed.source_display_bounds),
        bounds
    );
}

#[test]
fn editor_pin_window_centers_on_capture_display() {
    let mut editor = crate::editor::EditorSession::new(solid(32, 32));
    editor.source_display_bounds = Some((-1280.0, 120.0, 1280.0, 720.0));

    let settings = editor_pin_window_settings(&editor, (400, 300));

    match settings.position {
        window::Position::Specific(point) => {
            assert_eq!(point.x, -840.0);
            assert_eq!(point.y, 330.0);
        }
        other => panic!("expected specific editor pin position, got {other:?}"),
    }
}

#[test]
fn overlay_selection_stores_last_region_per_display() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let display_id = "primary".to_string();
    let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();

    let _ = update(
        &mut app,
        Message::OverlaySelected {
            display_id: display_id.clone(),
            rect,
            intent: crate::app::CaptureIntent::Editor,
        },
    );

    assert_eq!(
        app.last_regions.get(&display_id).map(|r| r.rect),
        Some(rect)
    );
}

#[test]
fn overlay_selection_tracks_most_recent_region_display() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let first = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();
    let second = readshot_core::geom::Rect::from_xywh(30.0, 40.0, 160.0, 90.0).unwrap();

    let _ = update(
        &mut app,
        Message::OverlaySelected {
            display_id: "display-a".to_string(),
            rect: first,
            intent: crate::app::CaptureIntent::Editor,
        },
    );
    let _ = update(
        &mut app,
        Message::OverlaySelected {
            display_id: "display-b".to_string(),
            rect: second,
            intent: crate::app::CaptureIntent::Editor,
        },
    );

    assert_eq!(app.last_region_display_id.as_deref(), Some("display-b"));
}

#[test]
fn overlay_selection_preserves_display_context_for_fast_capture() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let window = iced::window::Id::unique();
    app.overlay_displays.insert(
        window,
        crate::app::OverlayDisplay {
            display_id: "display-a".to_string(),
            scale: 2.0,
            origin_x: 1440.0,
            origin_y: 0.0,
            width: 1920.0,
            height: 1080.0,
        },
    );
    let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();

    let _ = update(
        &mut app,
        Message::OverlaySelected {
            display_id: "display-a".to_string(),
            rect,
            intent: crate::app::CaptureIntent::Editor,
        },
    );

    assert_eq!(app.pending_display_scale, Some(2.0));
    assert_eq!(
        app.pending_display_bounds,
        Some((1440.0, 0.0, 1920.0, 1080.0))
    );
    assert_eq!(
        app.last_regions
            .get("display-a")
            .and_then(|r| r.display_bounds),
        Some((1440.0, 0.0, 1920.0, 1080.0))
    );
}

#[test]
fn retake_last_region_preserves_display_context() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let display_id = "display-a".to_string();
    let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();
    app.last_regions.insert(
        display_id.clone(),
        crate::app::LastRegion {
            rect,
            display_scale: 2.0,
            display_bounds: Some((1440.0, 0.0, 1920.0, 1080.0)),
        },
    );
    app.last_region_display_id = Some(display_id.clone());

    let _ = update(&mut app, Message::RetakeLastRegionRequested);

    assert_eq!(app.pending_display_id.as_deref(), Some(display_id.as_str()));
    assert_eq!(app.pending_display_scale, Some(2.0));
    assert_eq!(
        app.pending_display_bounds,
        Some((1440.0, 0.0, 1920.0, 1080.0))
    );
}

#[test]
fn overlay_auto_confirm_is_only_for_cli_interactive() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    assert_eq!(overlay_auto_confirm_intent(&app), None);

    app.pending_intent = Some(crate::app::CaptureIntent::Editor);
    assert_eq!(overlay_auto_confirm_intent(&app), None);

    app.pending_intent = Some(crate::app::CaptureIntent::CliInteractive);
    assert_eq!(
        overlay_auto_confirm_intent(&app),
        Some(crate::app::CaptureIntent::CliInteractive)
    );

    app.pending_intent = None;
    app.cli_interactive_output = Some(std::env::temp_dir().join("capture.png"));
    assert_eq!(
        overlay_auto_confirm_intent(&app),
        Some(crate::app::CaptureIntent::CliInteractive)
    );
}

#[test]
fn open_overlay_sets_opening_guard_until_display_list_completes() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));

    let _ = update(&mut app, Message::OpenOverlayRequested);

    assert!(app.overlay_opening);

    let _ = update(&mut app, Message::OverlayDisplaysListed(Ok(Vec::new())));

    assert!(!app.overlay_opening);
    assert_eq!(
        app.last_capture_status.as_deref(),
        Some("Couldn’t capture — no display was detected.")
    );
}

#[test]
fn overlay_display_list_error_clears_pending_capture_state() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.overlay_opening = true;
    app.pending_intent = Some(crate::app::CaptureIntent::Editor);
    app.pending_display_id = Some("display-a".to_string());
    app.pending_display_scale = Some(2.0);
    app.pending_display_bounds = Some((10.0, 20.0, 800.0, 600.0));
    app.pending_hide_cursor = false;

    let _ = update(
        &mut app,
        Message::OverlayDisplaysListed(Err("backend unavailable".into())),
    );

    assert!(!app.overlay_opening);
    assert_eq!(app.pending_intent, None);
    assert_eq!(app.pending_display_id, None);
    assert_eq!(app.pending_display_scale, None);
    assert_eq!(app.pending_display_bounds, None);
    assert!(app.pending_hide_cursor);
    // The raw backend error is logged, not leaked into the status —
    // the user sees friendly recovery text instead.
    let status = app.last_capture_status.as_deref().unwrap();
    assert!(status.starts_with("Couldn’t capture"), "{status}");
    assert!(!status.contains("backend unavailable"), "{status}");
}

#[test]
fn periodic_poll_notices_grant_without_probing() {
    let perms = Arc::new(FakePermissions::denied());
    let mut app = build_app(perms.clone());
    assert!(app.welcome.should_show());
    perms.flip_to_granted();
    let _ = update(&mut app, Message::PermissionPollTick);
    assert_eq!(app.permission_status, PermissionStatus::Granted);
}

#[test]
fn url_capture_waits_for_permission_then_replays() {
    let perms = Arc::new(FakePermissions::denied());
    let mut app = build_app(perms.clone());

    let _ = update(&mut app, Message::UrlActionReceived(UrlAction::NewCapture));

    assert_eq!(
        app.pending_url_after_permission,
        Some(UrlAction::NewCapture)
    );

    perms.flip_to_granted();
    let _ = update(&mut app, Message::PermissionTick);

    assert_eq!(app.pending_url_after_permission, None);
}

#[test]
fn closing_overlay_window_removes_display_mapping() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.windows.register(id, WindowKind::Overlay);
    app.overlay_displays
        .insert(id, overlay_display("display-a"));

    let _ = update(&mut app, Message::WindowClosed(id));

    assert!(app.overlay_displays.is_empty());
    assert!(app.windows.kind(id).is_none());
}

#[test]
fn opening_replacement_editor_forgets_previous_editor_window() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let old_id = iced::window::Id::unique();
    let mut old = crate::editor::EditorSession::new(solid(16, 16));
    old.window_id = Some(old_id);
    app.windows.register(old_id, WindowKind::Editor);
    app.editor = Some(old);

    let new = crate::editor::EditorSession::new(solid(32, 32));
    let _ = open_editor_window_replacing(&mut app, new, None);

    assert!(app.windows.kind(old_id).is_none());
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (32, 32));
}

#[test]
fn opening_replacement_editor_commits_old_pending_history_text() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let record =
        readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
    store.save(&record, b"png").unwrap();

    let history: Arc<dyn HistoryStore> = store.clone();
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
    let old_id = iced::window::Id::unique();
    let mut old = crate::editor::EditorSession::from_history(solid(100, 100), record);
    old.window_id = Some(old_id);
    old.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(12.0, 18.0),
        content: "replacement draft".into(),
        edit_index: None,
    });
    app.windows.register(old_id, WindowKind::Editor);
    app.editor = Some(old);

    let new = crate::editor::EditorSession::new(solid(32, 32));
    let _ = open_editor_window_replacing(&mut app, new, None);

    assert!(app.windows.kind(old_id).is_none());
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (32, 32));
    // The sidecar write runs on the coordinator's background
    // writer; wait for it before reading the store.
    app.coordinator.flush_history_updates();
    let from_disk = store.list().unwrap();
    assert_eq!(from_disk[0].annotation_model.len(), 1);
    match &from_disk[0].annotation_model[0] {
        Annotation::Text {
            content, origin, ..
        } => {
            assert_eq!(content, "replacement draft");
            assert_eq!(*origin, PointLike::new(12.0, 18.0));
        }
        other => panic!("expected text annotation, got {other:?}"),
    }
}

#[test]
fn retake_last_region_without_previous_region_sets_status() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));

    let _ = update(&mut app, Message::RetakeLastRegionRequested);

    assert_eq!(
        app.last_capture_status.as_deref(),
        Some("No previous region to retake. Capture a region first, then use Retake Last Region.")
    );
}

#[test]
fn pin_hover_tracks_the_window() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    let _ = update(&mut app, Message::PinHovered(id, true));
    assert_eq!(app.hovered_pin, Some(id));
    let _ = update(&mut app, Message::PinHovered(id, false));
    assert_eq!(app.hovered_pin, None);
}

#[test]
fn pin_hover_exit_of_another_pin_keeps_current() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (a, b) = (iced::window::Id::unique(), iced::window::Id::unique());
    let _ = update(&mut app, Message::PinHovered(a, true));
    let _ = update(&mut app, Message::PinHovered(b, false));
    assert_eq!(app.hovered_pin, Some(a));
}

#[test]
fn pin_opacity_message_updates_pin_state() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.pins.insert(
        id,
        crate::app::PinState {
            handle: iced::widget::image::Handle::from_rgba(1, 1, vec![255, 255, 255, 255]),
            opacity: 1.0,
            locked: false,
        },
    );

    let _ = update(&mut app, Message::PinOpacityChanged(id, 0.4));

    assert_eq!(app.pins.get(&id).unwrap().opacity, 0.4);
}

#[test]
fn a_locked_pin_keeps_close_in_its_hover_bar() {
    assert_eq!(
        pin_hover_controls(true, 400.0),
        &[PinControl::Lock, PinControl::Close]
    );
    assert!(pin_hover_controls(false, 400.0).contains(&PinControl::Opacity));
    assert!(pin_hover_controls(false, 100.0).contains(&PinControl::Close));
}

/// A capture problem only the Welcome window would show is raised as a
/// system notification when that window is closed, once.
#[test]
fn a_capture_problem_with_no_window_to_show_it_is_notified() {
    let notices = || CAPTURE_NOTICES.with(|n| n.borrow().clone());
    CAPTURE_NOTICES.with(|n| n.borrow_mut().clear());
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(&mut app, Message::RetakeLastRegionRequested);
    let status = app.last_capture_status.clone().expect("a status");
    if CAPTURE_PROBLEM_ROUTE == CaptureProblemRoute::Notification {
        assert_eq!(notices(), vec![status.clone()]);
    } else {
        assert!(app.windows.kinds().any(|k| k == WindowKind::Welcome));
        return;
    }
    // Another update with the same status doesn't notify again.
    let _ = update(&mut app, Message::NoOp);
    assert_eq!(notices().len(), 1);
    // Off macOS there is no notification path: the Welcome window, which
    // shows the status, opens instead.
    let mut other = build_app(Arc::new(FakePermissions::granted()));
    let _ = surface_capture_problem(&mut other, &status, CaptureProblemRoute::WelcomeWindow);
    assert!(other.windows.kinds().any(|k| k == WindowKind::Welcome));
    // With the Welcome window open the status is shown there instead.
    CAPTURE_NOTICES.with(|n| n.borrow_mut().clear());
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.windows
        .register(iced::window::Id::unique(), WindowKind::Welcome);
    let _ = update(&mut app, Message::RetakeLastRegionRequested);
    assert!(notices().is_empty());
}

#[test]
fn only_problems_are_notified() {
    assert!(capture_status_is_problem(
        "Couldn’t capture — no display was detected."
    ));
    assert!(capture_status_is_problem(
        "No previous region to retake. Capture a region first, then use Retake Last Region."
    ));
    assert!(!capture_status_is_problem("Scrolling capture cancelled."));
    assert!(!capture_status_is_problem("Copied image to clipboard."));
}

/// A refused registration is not retried on every update, and only the
/// chords that did register are left out of the in-window map.
#[test]
fn a_refused_scroll_esc_is_tried_once_and_the_other_still_works() {
    use std::cell::Cell;
    let tries = Cell::new(0);
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.scroll_session = Some(test_session());
    let sync = |app: &mut App| {
        sync_scroll_escape_hotkeys_with(
            app,
            |h| {
                tries.set(tries.get() + 1);
                if h.mods.contains(global_hotkey::hotkey::Modifiers::SHIFT) {
                    Err("taken".into())
                } else {
                    Ok(())
                }
            },
            |_| {},
        )
    };
    sync(&mut app);
    sync(&mut app);
    sync(&mut app);
    assert_eq!(tries.get(), 2, "each chord tried once per session");
    assert_eq!(scroll_escape_globals(&app), (true, false));
    use iced::keyboard::{key::Named, Key, Modifiers};
    let esc = Key::Named(Named::Escape);
    // esc is the hotkey's; ⇧esc still works in the HUD.
    assert!(scroll_key_message_beside_globals(&esc, Modifiers::empty(), true, false).is_none());
    assert!(matches!(
        scroll_key_message_beside_globals(&esc, Modifiers::SHIFT, true, false),
        Some(Message::ScrollCaptureCancelRequested)
    ));
    // A new session tries again.
    app.scroll_session = None;
    sync(&mut app);
    app.scroll_session = Some(test_session());
    sync(&mut app);
    assert_eq!(tries.get(), 4);
}
