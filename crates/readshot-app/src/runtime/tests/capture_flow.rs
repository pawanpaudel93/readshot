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
        Message::Scroll(ScrollMsg::Frame {
            seq: 2,
            result: Ok(scrolling_texture(80, 120, 28)),
        }),
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 1);
    assert_eq!(session.pending_frames.len(), 1);
    assert_eq!(session.judge_in_flight, None);

    // Seq 1 arrives: draining dispatches a motion judge for it and
    // pauses (the verdict task isn't executed in tests).
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::Frame {
            seq: 1,
            result: Ok(scrolling_texture(80, 120, 14)),
        }),
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 1);
    assert_eq!(session.judge_in_flight, Some(1));
    assert_eq!(session.pending_frames.len(), 1);

    // Verdict for seq 1 accepts the frame and dispatches the judge
    // for the buffered seq 2.
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::FrameJudged {
            seq: 1,
            image: Some(scrolling_texture(80, 120, 14)),
            moved: true,
            offset: None,
            rows: None,
            unplaced: false,
        }),
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 2);
    assert_eq!(session.judge_in_flight, Some(2));
    assert!(session.pending_frames.is_empty());

    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::FrameJudged {
            seq: 2,
            image: Some(scrolling_texture(80, 120, 28)),
            moved: true,
            offset: None,
            rows: None,
            unplaced: false,
        }),
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 3);
    assert_eq!(session.judge_in_flight, None);
    assert_eq!(session.next_frame_seq_to_process, 3);
}

/// A verdict for `seq` on the running session.
fn judged_frame(app: &mut App, seq: u64, moved: bool, unplaced: bool) {
    app.scroll_session.as_mut().unwrap().judge_in_flight = Some(seq);
    let _ = update(
        app,
        Message::Scroll(ScrollMsg::FrameJudged {
            seq,
            image: Some(scrolling_texture(80, 120, 14)),
            moved,
            offset: moved.then_some(14),
            rows: None,
            unplaced,
        }),
    );
}

/// SC-16 / I-1: frames that keep failing to place (the page jumped past
/// any overlap) show "Scrolled too fast — scroll back up a little" from
/// the second in a row. Any other verdict (a still page, a scroll back
/// up, a frame that places) ends the run and clears it, and Stop hides
/// it.
#[test]
fn frames_that_keep_failing_to_place_say_the_scroll_was_too_fast() {
    use crate::runtime::scroll::{scroll_too_fast, SCROLL_TOO_FAST_AFTER, SCROLL_TOO_FAST_HINT};
    assert_eq!(SCROLL_TOO_FAST_AFTER, 2);
    assert_eq!(
        SCROLL_TOO_FAST_HINT,
        "Scrolled too fast — scroll back up a little."
    );
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap();
    let mut session = crate::app::ScrollSession::new("display-a".into(), rect, 1.0);
    session.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
    app.scroll_session = Some(session);
    let too_fast = |app: &App| scroll_too_fast(app.scroll_session.as_ref().unwrap());

    judged_frame(&mut app, 1, false, true);
    assert!(!too_fast(&app), "one unplaced frame is not enough");
    judged_frame(&mut app, 2, false, false);
    judged_frame(&mut app, 3, false, true);
    assert!(
        !too_fast(&app),
        "a still frame or a scroll back up ends the run"
    );
    judged_frame(&mut app, 4, false, true);
    assert!(too_fast(&app), "two unplaced frames in a row show the hint");
    assert!(
        app.scroll_session.as_ref().unwrap().last_refused.is_some(),
        "the refused frame is kept for the next verdict"
    );
    judged_frame(&mut app, 5, false, false);
    assert!(
        !too_fast(&app),
        "a still frame or a scroll back up clears it"
    );
    judged_frame(&mut app, 6, false, true);
    judged_frame(&mut app, 7, false, true);
    assert!(too_fast(&app));
    judged_frame(&mut app, 8, true, false);
    assert!(!too_fast(&app), "a frame that places clears it");
    let s = app.scroll_session.as_ref().unwrap();
    assert_eq!(s.frames.len(), 2);
    assert!(
        s.last_refused.is_none(),
        "an accepted frame is the new anchor"
    );

    judged_frame(&mut app, 9, false, true);
    judged_frame(&mut app, 10, false, true);
    assert!(too_fast(&app));
    app.scroll_session.as_mut().unwrap().stopping = true;
    assert!(!too_fast(&app), "stitching hides it");
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
        Message::Scroll(ScrollMsg::FrameJudged {
            seq: 3,
            image: Some(scrolling_texture(80, 120, 14)),
            moved: true,
            offset: None,
            rows: None,
            unplaced: false,
        }),
    );
    let session = app.scroll_session.as_ref().unwrap();
    assert_eq!(session.frames.len(), 1);
    assert_eq!(session.judge_in_flight, Some(7));

    // A "no motion" verdict bumps the counter without appending.
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::FrameJudged {
            seq: 7,
            image: Some(scrolling_texture(80, 120, 0)),
            moved: false,
            offset: None,
            rows: None,
            unplaced: false,
        }),
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

/// Every way a scrolling capture ends releases the global esc / ⇧esc:
/// Stop (released as it starts stitching), the stitch landing or
/// failing, Cancel (at once or confirmed), and the HUD or region window
/// closing. `update` syncs the registration after every message; the
/// sync here stands in for its OS calls.
#[test]
fn every_scroll_capture_exit_releases_the_global_esc() {
    use std::cell::RefCell;
    let registered: RefCell<Vec<global_hotkey::hotkey::HotKey>> = RefCell::new(Vec::new());
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
    let two_frames = |app: &mut App| {
        let s = app.scroll_session.as_mut().unwrap();
        s.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
        s.frames.push(Arc::new(scrolling_texture(80, 120, 10)));
    };
    type Exit = fn(&mut App, window::Id, window::Id, &dyn Fn(&mut App));
    let exits: [(&str, Exit); 7] = [
        ("stop, then the stitch lands", |app, _, _, frames| {
            frames(app);
            let _ = update(app, Message::Scroll(ScrollMsg::StopRequested));
            assert!(app.scroll_session.as_ref().unwrap().stopping);
            let _ = update(
                app,
                Message::Scroll(ScrollMsg::Stitched(Ok(scrolling_texture(80, 130, 0)))),
            );
        }),
        ("stop, then the stitch fails", |app, _, _, frames| {
            frames(app);
            let _ = update(app, Message::Scroll(ScrollMsg::StopRequested));
            let _ = update(
                app,
                Message::Scroll(ScrollMsg::Stitched(Err("boom".into()))),
            );
        }),
        ("cancel with nothing captured", |app, _, _, _| {
            let _ = update(app, Message::Scroll(ScrollMsg::CancelRequested));
        }),
        ("cancel, confirmed", |app, _, _, frames| {
            frames(app);
            let _ = update(app, Message::Scroll(ScrollMsg::CancelRequested));
            assert!(app.scroll_session.is_some(), "armed");
            let _ = update(app, Message::Scroll(ScrollMsg::CancelRequested));
        }),
        ("stop with nothing captured", |app, _, _, _| {
            let _ = update(app, Message::Scroll(ScrollMsg::StopRequested));
        }),
        ("the HUD window closes", |app, hud, _, _| {
            let _ = update(app, Message::Window(WindowMsg::Closed(hud)));
        }),
        ("the region window closes", |app, _, region, _| {
            let _ = update(app, Message::Window(WindowMsg::Closed(region)));
        }),
    ];
    for (label, exit) in exits {
        let (mut app, hud, region) = app_with_scroll_windows(false);
        sync(&mut app);
        assert_eq!(
            registered.borrow().len(),
            2,
            "{label}: registered while it runs"
        );
        exit(&mut app, hud, region, &two_frames);
        sync(&mut app);
        assert!(registered.borrow().is_empty(), "{label}: still registered");
        assert!(app.scroll_escape_hotkeys.is_empty(), "{label}");
        assert!(
            !app.hotkey_actions.values().any(|a| matches!(
                a,
                crate::app::GlobalHotkeyAction::ScrollStop
                    | crate::app::GlobalHotkeyAction::ScrollDiscard
            )),
            "{label}: an esc action left behind"
        );
        // A later session registers them again.
        app.scroll_session = Some(test_session());
        sync(&mut app);
        assert_eq!(registered.borrow().len(), 2, "{label}: next session");
        app.scroll_session = None;
        sync(&mut app);
        assert!(registered.borrow().is_empty());
    }
}

#[test]
fn keep_going_disarms_cancel() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut s = test_session();
    s.cancel_armed_at = Some(std::time::Instant::now());
    app.scroll_session = Some(s);
    let _ = update(&mut app, Message::Scroll(ScrollMsg::KeepGoing));
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
    let _ = update(&mut app, Message::Scroll(ScrollMsg::CancelRequested));
    assert!(cancel_armed(app.scroll_session.as_ref().unwrap()));
    // Momentum frames right after the click keep it armed.
    accept_scroll_frame(
        app.scroll_session.as_mut().unwrap(),
        scrolling_texture(80, 120, 20),
        None,
        Some(10),
    );
    assert!(
        cancel_armed(app.scroll_session.as_ref().unwrap()),
        "an inertia frame is not an answer"
    );
    // Scrolling on later answers it.
    app.scroll_session.as_mut().unwrap().cancel_armed_at =
        Some(std::time::Instant::now() - SCROLL_CANCEL_MOMENTUM_GRACE);
    accept_scroll_frame(
        app.scroll_session.as_mut().unwrap(),
        scrolling_texture(80, 120, 30),
        None,
        Some(10),
    );
    assert!(!cancel_armed(app.scroll_session.as_ref().unwrap()));
}

/// The second Cancel click lands while trackpad momentum still yields
/// frames: it discards, it doesn't re-arm.
#[test]
fn a_second_cancel_during_momentum_discards() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut session = test_session();
    session.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
    session
        .frames
        .push(Arc::new(scrolling_texture(80, 120, 10)));
    app.scroll_session = Some(session);
    let _ = update(&mut app, Message::Scroll(ScrollMsg::CancelRequested));
    for offset in [20, 30, 40] {
        accept_scroll_frame(
            app.scroll_session.as_mut().unwrap(),
            scrolling_texture(80, 120, offset),
            None,
            Some(10),
        );
    }
    let _ = update(&mut app, Message::Scroll(ScrollMsg::CancelRequested));
    assert!(app.scroll_session.is_none(), "discarded");
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
    let _ = update(&mut app, Message::Scroll(ScrollMsg::StopRequested));
    let before = app.scroll_session.as_ref().unwrap();
    let strip_h = before.preview_strip.as_ref().unwrap().height();
    let stitched = before.stitched_rows;
    assert!(before.pending_frames.is_empty());
    assert_eq!(before.judge_in_flight, None);

    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::FrameJudged {
            seq: 1,
            image: Some(scrolling_texture(80, 120, 14)),
            moved: true,
            offset: Some(14),
            rows: Some(image::RgbaImage::new(PREVIEW_STRIP_W, 60)),
            unplaced: false,
        }),
    );
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::Frame {
            seq: 3,
            result: Ok(scrolling_texture(80, 120, 42)),
        }),
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
        Message::Scroll(ScrollMsg::Stitched(Ok(image::RgbaImage::new(10, 10)))),
    );
    assert!(app.editor.is_none());
}

#[test]
fn cancel_is_ignored_while_stitching() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut s = test_session();
    s.stopping = true;
    app.scroll_session = Some(s);
    let _ = update(&mut app, Message::Scroll(ScrollMsg::CancelRequested));
    assert!(app.scroll_session.is_some());
}

#[test]
fn open_overlay_leaves_a_running_scroll_session_alone() {
    let (mut app, hud, _) = app_with_scroll_windows(true);
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
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
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );
    assert_eq!(app.pending_display_id, None);
    let s = app.scroll_session.as_ref().unwrap();
    assert!(s.stopping);
    assert_eq!(s.hud_window_id, Some(hud));
}

#[test]
fn stitch_result_closes_both_scroll_windows() {
    let (mut app, hud, region) = app_with_scroll_windows(true);
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::Stitched(Err("boom".into()))),
    );
    assert!(app.scroll_session.is_none());
    assert_eq!(app.windows.kind(hud), None);
    assert_eq!(app.windows.kind(region), None);
}

#[test]
fn a_stitched_scroll_capture_is_kept_in_history_like_any_capture() {
    let (mut app, _, _) = app_with_scroll_windows(true);
    app.preferences.history_retention = readshot_core::HistoryRetention::Last50;
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::Stitched(Ok(solid(20, 40)))),
    );
    let ed = app.editor.as_ref().expect("the stitch opens the editor");
    let record = ed
        .source_record
        .as_ref()
        .expect("the editor is backed by a new History record");
    assert_eq!((record.width_px, record.height_px), (20, 40));
    assert_eq!(record.display_id, "display-a");
    // One OCR serves the History record and the editor's Text tab.
    assert!(matches!(
        ed.text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
}

#[test]
fn a_stitched_scroll_capture_with_history_off_is_kept_nowhere() {
    let (mut app, _, _) = app_with_scroll_windows(true);
    app.preferences.history_retention = readshot_core::HistoryRetention::Off;
    let _ = update(
        &mut app,
        Message::Scroll(ScrollMsg::Stitched(Ok(solid(20, 40)))),
    );
    let ed = app.editor.as_ref().expect("the stitch opens the editor");
    assert!(ed.source_record.is_none());
}

#[test]
fn stop_with_no_frames_cancels_instead_of_failing() {
    let (mut app, hud, region) = app_with_scroll_windows(false);
    let _ = update(&mut app, Message::Scroll(ScrollMsg::StopRequested));
    assert!(app.scroll_session.is_none());
    assert_eq!(app.windows.kind(hud), None);
    assert_eq!(app.windows.kind(region), None);
    let status = app.last_capture_status_text().unwrap_or("");
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

    let settings = pin_window_settings((400, 300), editor.source_display_bounds);

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
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: display_id.clone(),
            rect,
            intent: crate::app::CaptureIntent::Editor,
        }),
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
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: "display-a".to_string(),
            rect: first,
            intent: crate::app::CaptureIntent::Editor,
        }),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: "display-b".to_string(),
            rect: second,
            intent: crate::app::CaptureIntent::Editor,
        }),
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
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: "display-a".to_string(),
            rect,
            intent: crate::app::CaptureIntent::Editor,
        }),
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

    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );

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

    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));

    assert!(app.overlay_opening);

    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayDisplaysListed(Ok(Vec::new()))),
    );

    assert!(!app.overlay_opening);
    assert_eq!(
        app.last_capture_status_text(),
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
        Message::Capture(CaptureMsg::OverlayDisplaysListed(Err(
            "backend unavailable".into(),
        ))),
    );

    assert!(!app.overlay_opening);
    assert_eq!(app.pending_intent, None);
    assert_eq!(app.pending_display_id, None);
    assert_eq!(app.pending_display_scale, None);
    assert_eq!(app.pending_display_bounds, None);
    assert!(app.pending_hide_cursor);
    // The raw backend error is logged, not leaked into the status —
    // the user sees friendly recovery text instead.
    let status = app.last_capture_status_text().unwrap();
    assert!(status.starts_with("Couldn’t capture"), "{status}");
    assert!(!status.contains("backend unavailable"), "{status}");
}

#[test]
fn periodic_poll_notices_grant_without_probing() {
    let perms = Arc::new(FakePermissions::denied());
    let mut app = build_app(perms.clone());
    assert!(app.welcome.should_show());
    perms.flip_to_granted();
    let _ = update(&mut app, Message::App(AppMsg::PermissionPollTick));
    assert_eq!(app.permission_status, PermissionStatus::Granted);
}

#[test]
fn url_capture_waits_for_permission_then_replays() {
    let perms = Arc::new(FakePermissions::denied());
    let mut app = build_app(perms.clone());

    let _ = update(
        &mut app,
        Message::Tick(TickMsg::UrlAction(UrlAction::NewCapture)),
    );

    assert_eq!(
        app.pending_url_after_permission,
        Some(UrlAction::NewCapture)
    );

    perms.flip_to_granted();
    let _ = update(&mut app, Message::App(AppMsg::PermissionTick));

    assert_eq!(app.pending_url_after_permission, None);
}

#[test]
fn closing_overlay_window_removes_display_mapping() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.windows.register(id, WindowKind::Overlay);
    app.overlay_displays
        .insert(id, overlay_display("display-a"));

    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));

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

    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );

    assert_eq!(
        app.last_capture_status_text(),
        Some("No previous region to retake. Capture a region first, then use Retake Last Region.")
    );
}

#[test]
fn pin_hover_tracks_the_window() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::Hovered(id, true))),
    );
    assert_eq!(app.hovered_pin, Some(id));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::Hovered(id, false))),
    );
    assert_eq!(app.hovered_pin, None);
}

#[test]
fn pin_hover_exit_of_another_pin_keeps_current() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (a, b) = (iced::window::Id::unique(), iced::window::Id::unique());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::Hovered(a, true))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::Hovered(b, false))),
    );
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

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::OpacityChanged(id, 0.4))),
    );

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
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );
    let status = app
        .last_capture_status_text()
        .map(str::to_owned)
        .expect("a status");
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
    let _ = surface_capture_problem(
        &mut other,
        app.last_capture_status.as_ref().unwrap(),
        CaptureProblemRoute::WelcomeWindow,
    );
    assert!(other.windows.kinds().any(|k| k == WindowKind::Welcome));
    // With the Welcome window open the status is shown there instead.
    CAPTURE_NOTICES.with(|n| n.borrow_mut().clear());
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.windows
        .register(iced::window::Id::unique(), WindowKind::Welcome);
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );
    assert!(notices().is_empty());
}

/// A failure and a refusal are problems worth a notification; a cancel
/// the user chose and a success are not. The handlers that write them
/// pick those tones.
#[test]
fn only_problems_are_notified() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let status = |app: &App| app.last_capture_status.clone().unwrap();
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayDisplaysListed(Ok(Vec::new()))),
    );
    assert_eq!(
        status(&app).text,
        "Couldn’t capture — no display was detected."
    );
    assert!(capture_status_is_problem(&status(&app)));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );
    assert_eq!(
        status(&app).text,
        "No previous region to retake. Capture a region first, then use Retake Last Region."
    );
    assert!(capture_status_is_problem(&status(&app)));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySaveDone(Ok(None))),
    );
    assert_eq!(status(&app).text, "Save cancelled.");
    assert!(!capture_status_is_problem(&status(&app)));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayCopyDone(Ok(()))),
    );
    assert_eq!(status(&app).text, "Copied image to clipboard.");
    assert!(!capture_status_is_problem(&status(&app)));
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
        Some(Message::Scroll(ScrollMsg::CancelRequested))
    ));
    // A new session tries again.
    app.scroll_session = None;
    sync(&mut app);
    app.scroll_session = Some(test_session());
    sync(&mut app);
    assert_eq!(tries.get(), 4);
}

/// A notification refused because Readshot's notifications are off is
/// shown in the open editor's toast, with how to turn them back on.
#[test]
fn a_refused_notification_shows_in_the_editor_with_the_way_back() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    record_undelivered(
        NotShownReason::NotificationsOff,
        "Couldn’t save the capture.",
        crate::editor::StatusTone::Error,
    );
    let _ = update(&mut app, Message::Tick(TickMsg::Tray));
    let status = app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .unwrap()
        .to_owned();
    assert!(status.starts_with("Couldn’t save the capture."), "{status}");
    assert!(status.contains(NOTIFICATIONS_OFF_HINT), "{status}");
    assert!(app.notifications_off_told);
    // The queue is drained: the next tick shows nothing new.
    assert!(take_undelivered_notices().is_empty());
}

/// Without an editor, a refused capture-problem notification opens the
/// Welcome window, which shows the status; from then on capture problems
/// go there directly instead of to a notification that cannot show.
#[test]
fn with_notifications_off_capture_problems_open_the_welcome_window() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );
    let status = app
        .last_capture_status_text()
        .map(str::to_owned)
        .expect("a status");
    if CAPTURE_PROBLEM_ROUTE == CaptureProblemRoute::WelcomeWindow {
        // Off macOS capture problems open Welcome already.
        return;
    }
    assert!(!app.windows.kinds().any(|k| k == WindowKind::Welcome));
    record_undelivered(
        NotShownReason::NotificationsOff,
        &status,
        app.last_capture_status.as_ref().unwrap().tone,
    );
    let _ = update(&mut app, Message::Tick(TickMsg::Tray));
    assert!(app.windows.kinds().any(|k| k == WindowKind::Welcome));
    assert!(notifications_off());
    assert_eq!(capture_problem_route(), CaptureProblemRoute::WelcomeWindow);
}

/// A dev run outside Readshot.app cannot post: the text still reaches an
/// open editor, without the "turn notifications on" hint.
#[test]
fn an_unbundled_notification_shows_in_the_editor() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    record_undelivered(
        NotShownReason::NotBundled,
        "Checking for updates…",
        crate::editor::StatusTone::Notice,
    );
    let _ = update(&mut app, Message::Tick(TickMsg::Tray));
    let status = app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .unwrap()
        .to_owned();
    assert_eq!(status, "Checking for updates…");
    assert!(!app.notifications_off_told);
    assert!(!notifications_off());
}

/// Once a notification is accepted again (or the tray item's check finds
/// them allowed), the "notifications are off" state ends: capture
/// problems go back to notifications, and a later refusal tells the user
/// again.
#[test]
fn notifications_that_work_again_end_the_off_state() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    record_undelivered(
        NotShownReason::NotificationsOff,
        "Couldn’t capture.",
        crate::editor::StatusTone::Error,
    );
    let _ = update(&mut app, Message::Tick(TickMsg::Tray));
    assert!(app.notifications_off_told);
    assert!(notifications_off());
    record_notifications_on();
    let _ = update(&mut app, Message::Tick(TickMsg::Tray));
    assert!(!app.notifications_off_told, "the tray item goes");
    assert!(!notifications_off());
    assert_eq!(capture_problem_route(), CAPTURE_PROBLEM_ROUTE);
    // Off again: said again.
    record_undelivered(
        NotShownReason::NotificationsOff,
        "Couldn’t capture.",
        crate::editor::StatusTone::Error,
    );
    let _ = update(&mut app, Message::Tick(TickMsg::Tray));
    assert!(app.notifications_off_told);
}

/// Without a tray (it failed to install) there is no `TrayTick`: the URL
/// tick, which always runs on macOS, shows the notices instead.
#[test]
fn without_a_tray_undelivered_notices_still_reach_the_editor() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    assert!(app.tray.is_none());
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    record_undelivered(
        NotShownReason::NotificationsOff,
        "Couldn’t save the capture.",
        crate::editor::StatusTone::Error,
    );
    let _ = update(&mut app, Message::Tick(TickMsg::Url));
    let status = app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .unwrap()
        .to_owned();
    assert!(status.starts_with("Couldn’t save the capture."), "{status}");
    assert!(app.notifications_off_told);
    assert!(take_undelivered_notices().is_empty());
}

/// Nothing may drain the queue for a long time: it keeps the newest
/// notices only.
#[test]
fn the_undelivered_queue_keeps_only_the_newest_notices() {
    for i in 0..MAX_UNDELIVERED + 10 {
        record_undelivered(
            NotShownReason::NotBundled,
            &format!("notice {i}"),
            crate::editor::StatusTone::Notice,
        );
    }
    let kept = take_undelivered_notices();
    assert_eq!(kept.len(), MAX_UNDELIVERED);
    assert_eq!(kept.first().unwrap().text, "notice 10");
    assert_eq!(
        kept.last().unwrap().text,
        format!("notice {}", MAX_UNDELIVERED + 9)
    );
}

/// The notification center hands back only the body of a notification it
/// refused: the in-app notice gets the tone the body was posted with, the
/// newest one when the same body went out twice.
#[cfg(target_os = "macos")]
#[test]
fn a_refused_notification_keeps_the_tone_it_was_posted_with() {
    use crate::editor::StatusTone;
    remember_posted_tone("Couldn’t capture.", StatusTone::Error);
    remember_posted_tone("No previous region to retake.", StatusTone::Warning);
    remember_posted_tone("Couldn’t capture.", StatusTone::Warning);
    assert_eq!(posted_tone("Couldn’t capture."), StatusTone::Warning);
    assert_eq!(
        posted_tone("No previous region to retake."),
        StatusTone::Warning
    );
    assert_eq!(posted_tone("never posted"), StatusTone::Notice);
}

// ---- Capture Window… (overlay window picker) ----

fn picker_display(id: &str, x: f32) -> DisplayInfo {
    DisplayInfo {
        id: id.into(),
        bounds: readshot_core::geom::Rect::from_xywh(x, 0.0, 800.0, 600.0).unwrap(),
        scale: 2.0,
        name: id.into(),
        is_primary: x == 0.0,
    }
}

fn picker_window(id: &str, display: &str, x: f32) -> readshot_capture::StackedWindow {
    readshot_capture::StackedWindow {
        info: readshot_capture::WindowInfo {
            id: readshot_capture::WindowId(id.into()),
            title: "Docs".into(),
            app_name: "Safari".into(),
            display_id: Some(display.into()),
            bounds: readshot_core::geom::Rect::from_xywh(x, 40.0, 300.0, 200.0).unwrap(),
        },
        pickable: true,
    }
}

fn app_with_window_picker() -> App {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowCaptureRequested),
    );
    assert!(app.overlay_opening, "the request lists windows first");
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowPickerListed(Ok((
            vec![picker_display("a", 0.0), picker_display("b", 800.0)],
            vec![
                picker_window("w1", "a", 10.0),
                picker_window("w2", "b", 900.0),
            ],
        )))),
    );
    app
}

#[test]
fn capture_window_opens_one_picker_overlay_per_display() {
    let app = app_with_window_picker();
    assert!(!app.overlay_opening);
    assert_eq!(app.window_pick.as_ref().map(Vec::len), Some(2));
    assert_eq!(
        app.windows
            .kinds()
            .filter(|k| *k == WindowKind::Overlay)
            .count(),
        2
    );
}

#[test]
fn picking_a_window_captures_it_into_the_editor_on_its_display() {
    let mut app = app_with_window_picker();
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowPicked(readshot_capture::WindowId(
            "w2".into(),
        ))),
    );
    assert!(app.window_pick.is_none());
    assert_eq!(
        app.windows
            .kinds()
            .filter(|k| *k == WindowKind::Overlay)
            .count(),
        0
    );
    assert!(app.capture_in_flight);
    assert_eq!(app.pending_intent, Some(crate::app::CaptureIntent::Editor));
    assert_eq!(app.pending_display_id.as_deref(), Some("b"));
    assert_eq!(app.pending_display_scale, Some(2.0));
    // The capture lands like a region capture: an editor (History-backed
    // when History is on) on that display.
    app.preferences.history_retention = readshot_core::HistoryRetention::Last50;
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(30, 20)))),
    );
    let ed = app.editor.as_ref().expect("editor opened");
    assert_eq!(ed.source_record.as_ref().unwrap().display_id, "b");
}

#[test]
fn cancelling_the_picker_leaves_no_pick_behind() {
    let mut app = app_with_window_picker();
    let _ = update(&mut app, Message::Capture(CaptureMsg::OverlayCancelled));
    assert!(app.window_pick.is_none());
    assert!(!app.capture_in_flight);
    assert_eq!(app.pending_intent, None);
}

#[test]
fn window_capture_where_the_platform_has_none_says_so() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowCaptureRequested),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowPickerListed(Err(
            crate::runtime::capture_ops::WINDOW_CAPTURE_UNSUPPORTED.into(),
        ))),
    );
    assert!(!app.overlay_opening);
    assert!(app.window_pick.is_none());
    let status = app.last_capture_status.as_ref().unwrap();
    assert_eq!(status.tone, StatusTone::Warning);
    assert!(status.text.contains("isn’t available"), "{}", status.text);
}

#[test]
fn window_capture_with_no_windows_opens_nothing() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowCaptureRequested),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowPickerListed(Ok((
            vec![picker_display("a", 0.0)],
            Vec::new(),
        )))),
    );
    assert!(app.window_pick.is_none());
    assert_eq!(app.windows.kinds().count(), 0);
    assert_eq!(
        app.last_capture_status.as_ref().unwrap().tone,
        StatusTone::Warning
    );
}

#[test]
fn window_capture_with_only_panels_on_screen_opens_nothing() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowCaptureRequested),
    );
    let mut dock = picker_window("dock", "a", 0.0);
    dock.pickable = false;
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowPickerListed(Ok((
            vec![picker_display("a", 0.0)],
            vec![dock],
        )))),
    );
    assert!(app.window_pick.is_none());
    assert_eq!(app.windows.kinds().count(), 0);
}

#[test]
fn a_panel_can_not_be_picked_even_by_id() {
    let mut app = app_with_window_picker();
    if let Some(stack) = app.window_pick.as_mut() {
        stack[0].pickable = false;
    }
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowPicked(readshot_capture::WindowId(
            "w1".into(),
        ))),
    );
    assert!(!app.capture_in_flight);
}

#[test]
fn window_capture_waits_for_the_permission_like_any_capture() {
    let mut app = build_app(Arc::new(FakePermissions::denied()));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::WindowCaptureRequested),
    );
    assert!(!app.overlay_opening);
    assert!(app.window_pick.is_none());
}

#[test]
fn the_tray_item_and_the_shortcut_both_start_a_window_pick() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::Tick(TickMsg::TrayAction(crate::tray::TrayAction::CaptureWindow)),
    );
    assert!(app.overlay_opening);
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = crate::runtime::tick_ops::run_hotkey_actions(
        &mut app,
        &[crate::app::GlobalHotkeyAction::CaptureWindow],
    );
    assert!(app.overlay_opening);
}

/// The interactive CLI child exits 77 when permission is what stopped
/// it, so the parent can tell that apart from a cancel (64).
#[test]
fn an_interactive_child_failure_sets_its_exit_code() {
    assert_eq!(
        cli_interactive_exit_code(),
        crate::cli::INTERACTIVE_CHILD_CANCELLED
    );
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.cli_interactive_output = Some(std::path::PathBuf::from("/nonexistent/out.png"));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::CliInteractiveWritten(Err(
            "Screen Recording permission is required for interactive capture".into(),
        ))),
    );
    assert_eq!(
        cli_interactive_exit_code(),
        crate::cli::INTERACTIVE_CHILD_NO_PERMISSION
    );
}
