//! Combine: adding captures and files, bakes.

use super::*;

// ---- Add Image and Add Capture (combine) ----

#[test]
fn add_capture_below_hides_the_editor_and_appends_the_capture() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    assert!(
        matches!(app.combine_capture, Some(crate::app::CombineCapture { session: s, join: Join::Below, selected: false }) if s == session)
    );
    assert_eq!(
        app.pending_intent,
        Some(crate::app::CaptureIntent::CombineIntoEditor)
    );
    assert_eq!(
        overlay_auto_confirm_intent(&app),
        Some(crate::app::CaptureIntent::CombineIntoEditor)
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(32, 16)))),
    );
    let ed = app.editor.as_ref().expect("same editor");
    assert_eq!(ed.session_id, session, "no new editor opened");
    assert_eq!(ed.model.part_ids().len(), 2);
    assert_eq!(ed.image_size(), (64, 48));
    assert_eq!(
        ed.status_text(),
        Some(format!("Added capture below. {UNDO_HINT}").as_str())
    );
    assert!(app.combine_capture.is_none());
    assert!(app.pending_intent.is_none());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 1);
}

#[test]
fn a_failed_or_cancelled_combine_capture_restores_the_editor_unchanged() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Beside)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Err("boom".into()))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 1);
    assert!(ed
        .status_text()
        .unwrap()
        .starts_with("Couldn’t add the capture"));
    assert!(app.combine_capture.is_none());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Beside)),
    );
    let _ = update(&mut app, Message::Capture(CaptureMsg::OverlayCancelled));
    assert!(app.combine_capture.is_none());
    assert!(app.pending_intent.is_none());
}

#[test]
fn a_file_image_is_added_below_and_a_stale_session_is_ignored() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(10, 10)));
    let session = app.editor.as_ref().unwrap().session_id;
    let img = Arc::new(solid(10, 5));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PartLoaded {
            session: session + 1000,
            result: Ok(Some(Arc::clone(&img))),
        }),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 1);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PartLoaded {
            session,
            result: Ok(Some(img)),
        }),
    );
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (10, 15));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PartLoaded {
            session,
            result: Ok(None),
        }),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.part_ids().len(),
        2,
        "a cancelled pick changes nothing"
    );
}

#[test]
fn gap_slider_is_one_undo_step_and_alignment_moves_parts() {
    use readshot_core::document::{Align, Join};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(40, 10));
    ed.model.add_part(solid(20, 10), Join::Below).unwrap();
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    for g in [4, 8, 12] {
        let _ = update(&mut app, Message::Editor(EditorMsg::LayoutGapPreview(g)));
    }
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 32));
    assert_eq!(ed.model.undo_depth(), depth + 1);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::LayoutAlign(Align::End)),
    );
    let g = app.editor.as_ref().unwrap().model.geometry().clone();
    assert_eq!(g.layout().placements[1].x, 20);
}

#[test]
fn an_oversized_addition_is_refused() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(image::RgbaImage::new(
        10, 30_000,
    )));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(image::RgbaImage::new(
            10, 5_000,
        )))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 1);
    assert!(ed.status_text().unwrap().contains("32,767"));
}

#[test]
fn combining_a_rotated_document_maps_its_text_through_the_bake() {
    use readshot_core::document::{Join, ORIGINAL_PART};
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(30, 20));
    ed.text_panel.part_docs.insert(
        ORIGINAL_PART,
        Arc::new(positioned_doc("first", RectLike::new(2.0, 4.0, 10.0, 3.0))),
    );
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(20, 10)))),
    );
    let ed = app.editor.as_ref().unwrap();
    let ids = ed.model.part_ids();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ORIGINAL_PART, "baked into a fresh part");
    let baked = ed.text_panel.part_docs.get(&ids[0]).expect("carried over");
    // The original's box, turned with the pixels (30×20 → 20×30).
    assert_eq!(
        baked.lines[0].rect,
        Some(RectLike::new(13.0, 2.0, 3.0, 10.0))
    );
    assert_eq!(
        ed.text_panel.recognizing_part,
        Some(ids[1]),
        "only the new part is recognized; never the turned pixels"
    );
}

#[test]
fn a_bake_before_recognition_recognizes_the_source_part_upright() {
    use readshot_core::document::{Join, ORIGINAL_PART};
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = true;
    app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(20, 10)))),
    );
    let ed = app.editor.as_ref().unwrap();
    let ids = ed.model.part_ids();
    assert_eq!(
        ed.text_panel.recognizing_part,
        Some(ORIGINAL_PART),
        "the baked part’s source, in its own upright pixels"
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextPanel(TextPanelMsg::Recognized {
            session,
            part: ORIGINAL_PART,
            result: Ok(positioned_doc("first", RectLike::new(2.0, 4.0, 10.0, 3.0))),
        })),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.text_panel.part_docs.contains_key(&ids[0]));
    assert_eq!(ed.text_panel.recognizing_part, Some(ids[1]));
    assert!(
        ed.text_panel.document().is_none(),
        "the new part is pending"
    );
}

#[tokio::test]
async fn a_combined_record_stores_no_text_until_every_part_is_recognized() {
    use readshot_core::document::Join;
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(8, 4);
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
    record.ocr_text = Some("original".into());
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let ed = crate::editor::EditorSession::from_history(img, record.clone());
    let session = ed.session_id;
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(8, 4)))),
    );
    app.coordinator.flush_history_updates();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    assert_eq!((stored.width_px, stored.height_px), (8, 8));
    assert_eq!(stored.ocr_text, None, "no text while a part has none");
    let ids = app.editor.as_ref().unwrap().model.part_ids();
    for (part, text) in [(ids[0], "first"), (ids[1], "second")] {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::TextPanel(TextPanelMsg::Recognized {
                session,
                part,
                result: Ok(readshot_core::text_lines::TextDocument::from_plain(text)),
            })),
        );
    }
    app.coordinator.flush_history_updates();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    let text = stored.ocr_text.expect("every part recognized");
    assert!(text.contains("first") && text.contains("second"), "{text}");
}

#[tokio::test]
async fn a_combined_scrubbed_record_keeps_no_partial_text() {
    use readshot_core::document::Join;
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(8, 4);
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
    record.ocr_text = Some("██████".into());
    record.redacted = true;
    record.redactions = vec![Annotation::Blur {
        rect: RectLike::new(0.0, 0.0, 4.0, 4.0),
        radius: 4.0,
    }];
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(8, 4)))),
    );
    app.coordinator.flush_history_updates();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    assert_eq!((stored.width_px, stored.height_px), (8, 8));
    assert_eq!(
        stored.ocr_text, None,
        "the opened image’s text alone isn’t the combined document’s"
    );
}

#[test]
fn a_combine_capture_for_a_replaced_editor_is_dropped() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    // Another editor took over while the overlay was up.
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    let other = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(32, 16)))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.session_id, other);
    assert_eq!(ed.model.part_ids().len(), 1, "nothing leaks into it");
    assert_eq!(ed.image_size(), (16, 16));
    assert!(app.combine_capture.is_none());
}

#[test]
fn an_overwritten_intent_still_returns_the_capture_to_the_editor() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    // A tray click meanwhile sets another intent.
    app.pending_intent = Some(crate::app::CaptureIntent::Editor);
    assert_eq!(
        overlay_auto_confirm_intent(&app),
        Some(crate::app::CaptureIntent::CombineIntoEditor)
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: Default::default(),
            rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
            intent: crate::app::CaptureIntent::Editor,
        }),
    );
    assert_eq!(
        app.pending_intent,
        Some(crate::app::CaptureIntent::CombineIntoEditor)
    );
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(64, 16)))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.session_id, session);
    assert_eq!(ed.model.part_ids().len(), 2);
}

#[test]
fn no_display_or_a_retake_never_strands_a_combine_capture() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    app.last_region_display_id = Some("primary".to_string());
    app.last_regions.insert(
        "primary".to_string(),
        crate::app::LastRegion {
            rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
            display_scale: 1.0,
            display_bounds: None,
        },
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RetakeLastRegionRequested),
    );
    assert!(!app.capture_in_flight, "no retake while choosing a region");
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayDisplaysListed(Ok(Vec::new()))),
    );
    assert!(app.combine_capture.is_none());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.status_text(),
        Some("Couldn’t add the capture — no display was detected.")
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlayDisplaysListed(Err(
            "backend unavailable".into(),
        ))),
    );
    assert!(app.combine_capture.is_none());
    assert!(app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .unwrap()
        .starts_with("Couldn’t add the capture"));
}

#[test]
fn an_overlay_that_cannot_open_gives_the_editor_back() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    // Another capture started before the chained overlay request ran.
    app.capture_in_flight = true;
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    assert!(app.combine_capture.is_none());
    assert!(
        app.pending_intent.is_none(),
        "a stale combine intent would swallow the next capture"
    );
    assert!(app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .unwrap()
        .starts_with("Couldn’t add the capture"));
}

#[test]
fn an_overlay_refused_for_the_welcome_gives_the_editor_back() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    app.welcome = crate::welcome::WelcomeState::Pending;
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    assert!(app.combine_capture.is_none());
    assert!(app.pending_intent.is_none());
}

#[test]
fn an_overlay_closed_by_the_os_gives_the_editor_back() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    app.overlay_opening = false;
    let (a, b) = (window::Id::unique(), window::Id::unique());
    app.windows.register(a, WindowKind::Overlay);
    app.windows.register(b, WindowKind::Overlay);
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(a)));
    assert!(app.combine_capture.is_some(), "another overlay is still up");
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(b)));
    assert!(app.combine_capture.is_none());
    assert!(app.pending_intent.is_none());
}

#[test]
fn closing_the_overlays_after_a_selection_keeps_the_combine_capture() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let id = window::Id::unique();
    app.windows.register(id, WindowKind::Overlay);
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: Default::default(),
            rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
            intent: crate::app::CaptureIntent::CombineIntoEditor,
        }),
    );
    // The close event lands before the capture request runs.
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert!(app.combine_capture.is_some());
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(64, 16)))),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 2);
}

#[test]
fn a_file_that_would_overflow_the_combined_document_is_refused_before_decoding() {
    // 20000 × 10000 is fine alone; below a 20000 × 10000 document it
    // is 400 MP.
    let budget = PartBudget {
        doc_width: 20_000,
        doc_height: 10_000,
        gap: 0,
    };
    let err = decode_part_image(&png_header_only(20_000, 10_000), budget).unwrap_err();
    assert!(err.contains("combined"), "{err}");
    let tall = PartBudget {
        doc_width: 10,
        doc_height: 30_000,
        gap: 40,
    };
    let err = decode_part_image(&png_header_only(10, 3_000), tall).unwrap_err();
    assert!(err.contains("32,767"), "{err}");
    assert!(
        err.ends_with("Choose a smaller image, or resize this one first."),
        "{err}"
    );
    // The budget follows the document: a rotated one counts as baked.
    let mut ed = crate::editor::EditorSession::new(solid(30, 20));
    ed.model.rotate(true).unwrap();
    assert_eq!(
        PartBudget::below(ed.model.geometry()),
        PartBudget {
            doc_width: 20,
            doc_height: 30,
            gap: 0
        }
    );
}

#[test]
fn only_regular_files_are_read() {
    let dir = tempfile::TempDir::new().unwrap();
    let fake = dir.path().join("x.png");
    std::fs::create_dir(&fake).unwrap();
    let err = load_part_file(&fake, NO_DOC).unwrap_err();
    assert_eq!(
        err,
        "that isn’t an image file. Choose a PNG, JPEG, GIF, BMP, TIFF or WebP file."
    );
    let real = dir.path().join("y.png");
    std::fs::write(&real, readshot_core::encode_png(&solid(3, 2)).unwrap()).unwrap();
    assert_eq!(load_part_file(&real, NO_DOC).unwrap().dimensions(), (3, 2));
}

#[test]
fn a_bake_source_no_state_can_reach_is_released() {
    use readshot_core::document::Join;
    let undo = || Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo));
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(20, 10)))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 2, "combined");
    assert_eq!(ed.text_panel.bake_sources.len(), 1, "baked");
    let source = Arc::downgrade(ed.text_panel.bake_sources.values().next().unwrap());
    // Undone, the bake is still on the redo stack: kept.
    let _ = update(&mut app, undo());
    let _ = update(&mut app, undo());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 1);
    assert_eq!(
        ed.text_panel.bake_sources.len(),
        1,
        "redo can still reach it"
    );
    // A new edit truncates the redo stack: nothing reaches the baked
    // part, so its source (a whole earlier document) is released.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: false })),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.model.can_redo());
    assert!(ed.text_panel.bake_sources.is_empty(), "source kept");
    assert!(source.upgrade().is_none(), "source geometry still alive");
}

#[test]
fn a_bake_of_a_bake_keeps_the_inner_source() {
    use readshot_core::document::Join;
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
    for _ in 0..2 {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
        );
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
        );
        let _ = update(
            &mut app,
            Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(20, 10)))),
        );
    }
    let ed = app.editor.as_ref().unwrap();
    // The second bake's source holds the first baked part, whose own
    // source must stay to place the original's text.
    assert_eq!(ed.text_panel.bake_sources.len(), 2);
    assert!(
        super::super::text_panel::original_part_affine(&ed.text_panel, ed.model.geometry())
            .is_some()
    );
}

#[test]
fn a_baked_part_is_never_recognized_in_its_own_pixels() {
    use readshot_core::document::Join;
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = true;
    app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(20, 10)))),
    );
    let ed = app.editor.as_mut().unwrap();
    let ids = ed.model.part_ids();
    // The source can't be placed (say, its geometry lost the original):
    // the baked part then has no text rather than OCR of turned pixels.
    let src = ed.text_panel.bake_sources.get(&ids[0]).cloned().unwrap();
    let mut broken = (*src).clone();
    broken.parts.clear();
    ed.text_panel.bake_sources.insert(ids[0], Arc::new(broken));
    ed.text_panel.recognition = crate::editor::TextRecognition::Idle;
    ed.text_panel.recognizing_part = None;
    ed.text_panel.part_docs.insert(
        ids[1],
        Arc::new(positioned_doc("x", RectLike::new(0.0, 0.0, 2.0, 2.0))),
    );
    let _ = request_text_recognition(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.text_panel.recognizing_part, None);
    assert!(!ed.text_panel.part_docs.contains_key(&ids[0]));
}

#[tokio::test]
async fn a_scrubbed_record_keeps_its_redactions_through_a_bake() {
    use readshot_core::document::Join;
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(8, 4);
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
    record.ocr_text = Some("██████".into());
    record.redacted = true;
    record.redactions = vec![Annotation::Blur {
        rect: RectLike::new(0.0, 0.0, 4.0, 2.0),
        radius: 4.0,
    }];
    record.redactions_complete = true;
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(4, 4)))),
    );
    assert_ne!(
        app.editor.as_ref().unwrap().model.part_ids()[0],
        readshot_core::document::ORIGINAL_PART,
        "baked"
    );
    app.coordinator.flush_history_updates();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    assert!(stored.redacted);
    // The blur over the original's top-left 4 × 2, turned clockwise
    // with the 8 × 4 image: the right edge's top 2 × 4.
    assert_eq!(
        stored.redactions,
        vec![Annotation::Blur {
            rect: RectLike::new(2.0, 0.0, 2.0, 4.0),
            radius: 4.0,
        }]
    );
}

#[test]
fn an_overlay_request_during_the_combine_capture_never_replaces_the_editor() {
    use readshot_core::document::Join;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CombineCaptureRequested(Join::Below)),
    );
    app.overlay_opening = false;
    let id = window::Id::unique();
    app.windows.register(id, WindowKind::Overlay);
    let display_id: readshot_capture::DisplayId = Default::default();
    let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap();
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: display_id.clone(),
            rect,
            intent: crate::app::CaptureIntent::CombineIntoEditor,
        }),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionRequested { display_id, rect }),
    );
    assert!(app.capture_in_flight);
    // A readshot:// NewCapture or History's Capture button meanwhile.
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    assert!(app.combine_capture.is_some(), "the combine is in flight");
    assert_eq!(
        app.pending_intent,
        Some(crate::app::CaptureIntent::CombineIntoEditor)
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(solid(64, 16)))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.session_id, session, "never a new editor");
    assert_eq!(ed.model.part_ids().len(), 2);
    assert!(ed.source_record.is_none(), "no History record made");
    assert!(app.combine_capture.is_none());
}

#[test]
fn a_finished_scroll_capture_leaves_no_pending_display() {
    let app = app_after_a_scroll_capture();
    assert_eq!(app.pending_display_id, None);
    assert_eq!(app.pending_display_scale, None);
    assert_eq!(app.pending_intent, None);
}

#[test]
fn add_capture_after_a_scroll_capture_is_never_stranded() {
    // The overlay is refused (welcome showing): the editor comes back.
    let mut app = app_after_a_scroll_capture();
    start_add_capture(&mut app);
    app.welcome = crate::welcome::WelcomeState::Pending;
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    assert!(app.combine_capture.is_none(), "welcome refusal restores");
    assert!(app.pending_intent.is_none());

    // The overlay is closed by the OS before any selection.
    let mut app = app_after_a_scroll_capture();
    start_add_capture(&mut app);
    app.overlay_opening = false;
    let id = window::Id::unique();
    app.windows.register(id, WindowKind::Overlay);
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert!(app.combine_capture.is_none(), "OS close restores");
    assert!(app.pending_intent.is_none());

    // The user cancels the overlay.
    let mut app = app_after_a_scroll_capture();
    start_add_capture(&mut app);
    let _ = update(&mut app, Message::Capture(CaptureMsg::OverlayCancelled));
    assert!(app.combine_capture.is_none(), "cancel restores");
}

#[test]
fn a_combine_with_no_selection_restores_despite_a_stale_pending_display() {
    // Another flow left the shared pending display set; the combine
    // has made no selection, so its safety nets still fire.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    start_add_capture(&mut app);
    app.pending_display_id = Some("display-a".into());
    app.welcome = crate::welcome::WelcomeState::Pending;
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    assert!(app.combine_capture.is_none(), "welcome refusal restores");

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    start_add_capture(&mut app);
    app.pending_display_id = Some("display-a".into());
    app.overlay_opening = false;
    let id = window::Id::unique();
    app.windows.register(id, WindowKind::Overlay);
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert!(app.combine_capture.is_none(), "OS close restores");

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    start_add_capture(&mut app);
    app.pending_display_id = Some("display-a".into());
    let _ = abort_combine_capture(&mut app, None);
    assert!(app.combine_capture.is_none(), "abort restores");
}

#[test]
fn a_stray_cancel_after_a_combine_selection_keeps_the_capture() {
    let (mut app, session, _) = app_with_a_selected_combine();
    app.pending_display_scale = Some(2.0);
    app.pending_hide_cursor = false;
    // A stray cancel leaves the in-flight capture's pending state be.
    let _ = update(&mut app, Message::Capture(CaptureMsg::OverlayCancelled));
    assert!(app.combine_capture.is_some(), "cancel after selection");
    assert_eq!(app.pending_display_scale, Some(2.0));
    assert!(!app.pending_hide_cursor, "the show-cursor choice survives");
    assert_eq!(
        app.pending_intent,
        Some(crate::app::CaptureIntent::CombineIntoEditor)
    );
    assert_the_combine_capture_lands(app, session);
}

#[test]
fn an_os_close_after_a_combine_selection_keeps_the_capture() {
    let (mut app, session, id) = app_with_a_selected_combine();
    assert!(!app.capture_in_flight, "only the selection guards it");
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert!(app.combine_capture.is_some(), "OS close after selection");
    assert_the_combine_capture_lands(app, session);
}

#[test]
fn the_welcome_after_a_combine_selection_keeps_the_capture() {
    let (mut app, session, id) = app_with_a_selected_combine();
    // The overlay is gone and the capture not started yet: only the
    // selection keeps the welcome refusal from restoring the editor.
    app.windows.forget(id);
    assert!(!app.capture_in_flight);
    app.welcome = crate::welcome::WelcomeState::Pending;
    let _ = update(&mut app, Message::Capture(CaptureMsg::OpenOverlayRequested));
    assert!(app.combine_capture.is_some(), "welcome after selection");
    assert_the_combine_capture_lands(app, session);
}

#[test]
fn an_abort_after_a_combine_selection_keeps_the_capture() {
    let (mut app, session, _) = app_with_a_selected_combine();
    let _ = abort_combine_capture(&mut app, None);
    assert!(app.combine_capture.is_some(), "abort after selection");
    assert_the_combine_capture_lands(app, session);
}

#[test]
fn a_selected_combine_capture_that_fails_gives_the_editor_back() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    start_add_capture(&mut app);
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::OverlaySelected {
            display_id: Default::default(),
            rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
            intent: crate::app::CaptureIntent::CombineIntoEditor,
        }),
    );
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Err("boom".into()))),
    );
    assert!(app.combine_capture.is_none());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 1);
    assert!(ed
        .status_text()
        .unwrap()
        .starts_with("Couldn’t add the capture"));
}

#[test]
fn a_picked_file_is_decoded_as_untrusted_input() {
    let ok = readshot_core::encode_png(&solid(3, 2)).unwrap();
    assert_eq!(decode_part_image(&ok, NO_DOC).unwrap().dimensions(), (3, 2));
    let err = decode_part_image(b"not an image at all", NO_DOC).unwrap_err();
    assert!(err.contains("isn’t an image"), "{err}");
    assert!(
        err.ends_with("Choose a PNG, JPEG, GIF, BMP, TIFF or WebP file."),
        "{err}"
    );
    let mut truncated = ok.clone();
    truncated.truncate(ok.len() / 2);
    assert!(decode_part_image(&truncated, NO_DOC).is_err());
    // Refused from the header alone, before any pixel is allocated.
    let err = decode_part_image(&png_header_only(40_000, 1), NO_DOC).unwrap_err();
    assert!(err.contains("32,767"), "{err}");
    assert!(err.ends_with("Choose a smaller image."), "{err}");
    let err = decode_part_image(&png_header_only(20_000, 20_000), NO_DOC).unwrap_err();
    assert!(err.contains("256,000,000"), "{err}");
    let err = decode_part_image(&png_header_only(0, 5), NO_DOC).unwrap_err();
    assert!(!err.is_empty());
}

#[test]
fn a_file_that_fails_to_load_leaves_the_editor_unchanged() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(10, 10));
    ed.busy = true;
    let session = ed.session_id;
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PartLoaded {
            session,
            result: Err(
                "that file isn’t an image Readshot can read. Choose a PNG, JPEG, GIF, BMP, TIFF or WebP file."
                    .into(),
            ),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.busy);
    assert_eq!(ed.model.part_ids().len(), 1);
    assert_eq!(
        ed.status_text(),
        Some("Couldn’t add the image — that file isn’t an image Readshot can read. Choose a PNG, JPEG, GIF, BMP, TIFF or WebP file.")
    );
}
