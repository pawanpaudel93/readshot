//! History scrub after a redaction (B10).

use super::*;

#[test]
fn redact_all_scrubs_the_history_image_and_text() {
    let (_dir, store, mut app) = scrub_app(true);
    let _ = update(&mut app, Message::Editor(EditorMsg::RedactAll));
    app.coordinator.flush_history_updates();
    let (record, png) = stored(&store);
    let text = record.ocr_text.clone().unwrap();
    assert!(record.redacted);
    assert!(!text.contains(EMAIL) && !text.contains(PHONE), "{text}");
    assert!(text.contains("Name Priya"), "{text}");
    assert!(text.contains(&"█".repeat(EMAIL.chars().count())), "{text}");
    assert!(
        record.annotation_model.is_empty(),
        "the PNG already shows them: reopening must not apply them twice"
    );
    let ed = app.editor.as_mut().unwrap();
    assert_eq!(png, ed.model.flatten(), "the PNG is the exported image");
    assert!(ed.history_scrubbed);
}

#[test]
fn undoing_every_redaction_restores_the_original_record() {
    let (_dir, store, mut app) = scrub_app(true);
    let original = sample_doc().layout_text;
    let _ = update(&mut app, Message::Editor(EditorMsg::RedactAll));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    app.coordinator.flush_history_updates();
    let (record, png) = stored(&store);
    assert!(!record.redacted);
    assert_eq!(record.ocr_text.as_deref(), Some(original.as_str()));
    assert!(record.annotation_model.is_empty());
    assert_eq!(png, solid(64, 64));
    assert!(!app.editor.as_ref().unwrap().history_scrubbed);
}

#[test]
fn a_non_redaction_edit_keeps_the_plain_sidecar_sync() {
    let (_dir, store, mut app) = scrub_app(true);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(highlight_stroke()),
        )),
    );
    app.coordinator.flush_history_updates();
    let (record, png) = stored(&store);
    assert!(!record.redacted);
    assert_eq!(record.annotation_model.len(), 1);
    assert_eq!(png, solid(64, 64), "the original image is untouched");
}

#[test]
fn without_positions_a_redaction_withholds_the_stored_text() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
    let mut app = build_app_with_history(
        Arc::new(FakePermissions::granted()),
        store.clone() as Arc<dyn HistoryStore>,
    );
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "d");
    record.ocr_text = Some(format!("Contact\nmail {EMAIL} today"));
    store
        .save(&record, &readshot_core::encode_png(&solid(64, 64)).unwrap())
        .unwrap();
    let mut ed = crate::editor::EditorSession::from_history(solid(64, 64), record);
    super::super::text_panel::seed_text_from_history(&mut ed, false);
    app.editor = Some(ed);
    // A solid block drawn by hand somewhere: positions are unknown, so
    // nothing can tell which words it hides — no text is stored (masking
    // only the detected email would keep a hand-covered word readable).
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(
                readshot_core::redact::redaction_annotation(
                    RectLike::new(0.0, 0.0, 10.0, 10.0),
                    readshot_core::redact::RedactStyle::Solid,
                ),
            ),
        )),
    );
    app.coordinator.flush_history_updates();
    let (record, _) = stored(&store);
    assert!(record.redacted);
    assert_eq!(record.ocr_text, None);
    // Text recognized later without positions isn't stored either.
    store
        .record_recognized_text(
            &record,
            &readshot_core::text_lines::TextDocument::from_plain("Contact"),
            true,
        )
        .unwrap();
    assert_eq!(stored(&store).0.ocr_text, None);
}

#[tokio::test]
async fn a_capture_saved_after_the_editor_closed_is_still_scrubbed() {
    let (_dir, store, mut app) = scrub_app(false);
    let _ = update(&mut app, Message::Editor(EditorMsg::RedactAll));
    let expected = app.editor.as_mut().unwrap().model.flatten();
    let record = app.editor.as_ref().unwrap().source_record.clone().unwrap();
    // The editor goes away before the capture's save lands.
    app.editor = None;
    app.coordinator.flush_history_updates();
    assert!(store.list().unwrap().is_empty(), "nothing on disk yet");
    let raw = readshot_core::encode_png(&solid(64, 64)).unwrap();
    app.coordinator
        .record_history(
            record.clone(),
            raw,
            readshot_core::HistoryRetention::Unlimited,
            chrono::Utc::now(),
        )
        .await
        .unwrap();
    // The late OCR pass can't put the raw text back either.
    app.coordinator
        .record_recognized_text(&record, &sample_doc(), true)
        .unwrap();
    let (stored, png) = stored(&store);
    let text = stored.ocr_text.clone().unwrap();
    assert!(stored.redacted);
    assert!(!text.contains(EMAIL) && !text.contains(PHONE), "{text}");
    assert_eq!(png, expected);
}

#[test]
fn with_positions_but_no_word_boxes_yet_the_scrub_stores_no_text() {
    // A name no detector matches, hidden by hand.
    let (_dir, store, mut app) = positional_scrub_app("Name Priya");
    draw_solid_block(&mut app);
    app.coordinator.flush_history_updates();
    let (record, _) = stored(&store);
    assert!(record.redacted);
    assert_eq!(record.ocr_text, None, "fail closed until positions land");
    // …and recognition starts so the precise text can land.
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
}

#[test]
fn a_failed_recognition_is_retried_on_the_next_scrub() {
    let (_dir, _store, mut app) = positional_scrub_app("Name Priya");
    app.editor.as_mut().unwrap().text_panel.recognition =
        crate::editor::TextRecognition::Failed("busy".into());
    draw_solid_block(&mut app);
    assert!(matches!(
        app.editor.as_ref().unwrap().text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
}

#[tokio::test]
async fn recognized_text_reaches_history_even_after_the_editor_closed() {
    let (_dir, store, mut app) = positional_scrub_app("Name Priya");
    // Redact the email line by hand.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(
                readshot_core::redact::redaction_annotation(
                    RectLike::new(20.0, 18.0, 40.0, 9.0),
                    readshot_core::redact::RedactStyle::Solid,
                ),
            ),
        )),
    );
    let record = app.editor.as_ref().unwrap().source_record.clone().unwrap();
    app.editor = None;
    app.coordinator.flush_history_updates();
    // The recognition task's History write, run with the editor gone.
    let _ = super::super::text_panel::store_recognized_text(
        &app.coordinator,
        &record,
        None,
        Ok(sample_doc()),
    )
    .await;
    let (stored, _) = stored(&store);
    let text = stored.ocr_text.unwrap();
    assert!(!text.contains(EMAIL), "{text}");
    assert!(text.contains("Name Priya"), "{text}");
}

#[test]
fn a_failed_history_write_shows_an_error_and_forces_a_full_rewrite_next() {
    let (_dir, _store, mut app) = scrub_app(true);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::HistoryWritten {
            session,
            result: Some(Err(crate::coordinator::HistoryRewriteError::Failed(
                "No space left on device".into(),
            ))),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.status_is_error(), "{:?}", ed.status);
    assert!(
        ed.status_text()
            .is_some_and(|s| s.ends_with("Readshot will keep trying.")),
        "{:?}",
        ed.status
    );
    assert!(
        ed.history_scrubbed,
        "disk state unknown: next sync rewrites"
    );
    // A superseded write reports nothing.
    let _ = update(&mut app, Message::Editor(EditorMsg::StatusDismissed));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::HistoryWritten {
            session,
            result: None,
        }),
    );
    assert_eq!(app.editor.as_ref().unwrap().status, None);
}

#[test]
fn a_successful_history_write_reloads_an_open_history_window() {
    let (_dir, _store, mut app) = scrub_app(true);
    let session = app.editor.as_ref().unwrap().session_id;
    let msg = || {
        Message::Editor(EditorMsg::HistoryWritten {
            session,
            result: Some(Ok(())),
        })
    };
    assert_eq!(update(&mut app, msg()).units(), 0);
    app.history_window_id = Some(iced::window::Id::unique());
    assert_eq!(update(&mut app, msg()).units(), 1);
}

#[test]
fn copy_lines_masks_redacted_words() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        super::super::text_panel::copy_lines_text(ed, &[1], false).as_deref(),
        Some(format!("Email {EMAIL}").as_str())
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::RedactAll));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        super::super::text_panel::copy_lines_text(ed, &[0, 1], false).as_deref(),
        Some(format!("Name Priya\nEmail {}", "█".repeat(EMAIL.len())).as_str())
    );
}

#[test]
fn text_boxes_follow_a_rotation() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(30, 20));
    let session = ed.session_id;
    ed.text_panel.recognition = crate::editor::TextRecognition::Recognizing {
        since: std::time::Instant::now(),
    };
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextRecognized {
            session,
            part: readshot_core::document::ORIGINAL_PART,
            result: Ok(positioned_doc(
                "Example",
                RectLike::new(2.0, 4.0, 10.0, 3.0),
            )),
        }),
    );
    let ed = app.editor.as_mut().unwrap();
    ed.model.rotate(true).unwrap();
    sync_editor_history(ed, &app.coordinator);
    let doc = ed.text_panel.positioned_document().expect("still ready");
    // (x, y, w, h) → (H − y − h, x, h, w) for a clockwise turn of a 30×20 image.
    assert_eq!(doc.lines[0].rect, Some(RectLike::new(13.0, 2.0, 3.0, 10.0)));
    // Undo back to the original frame moves the boxes back.
    ed.model.undo();
    sync_editor_history(ed, &app.coordinator);
    let doc = ed.text_panel.positioned_document().expect("still ready");
    assert_eq!(doc.lines[0].rect, Some(RectLike::new(2.0, 4.0, 10.0, 3.0)));
}

#[test]
fn an_added_part_needs_its_own_recognition_and_the_scrub_fails_closed_until_then() {
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(20, 10));
    let session = ed.session_id;
    ed.text_panel.recognition = crate::editor::TextRecognition::Recognizing {
        since: std::time::Instant::now(),
    };
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextRecognized {
            session,
            part: readshot_core::document::ORIGINAL_PART,
            result: Ok(positioned_doc("first", RectLike::new(1.0, 1.0, 10.0, 4.0))),
        }),
    );
    let ed = app.editor.as_mut().unwrap();
    let id = ed
        .model
        .add_part(solid(20, 10), readshot_core::document::Join::Below)
        .unwrap();
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 1.0,
        style: readshot_core::ShapeStyle {
            fill: readshot_core::FillMode::Filled,
            ..Default::default()
        },
    });
    sync_editor_history(ed, &app.coordinator);
    assert!(
        ed.text_panel.positioned_document().is_none(),
        "the new part isn’t recognized yet"
    );
    assert_eq!(
        scrubbed_history_text(ed),
        None,
        "no text rather than unmasked text"
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextRecognized {
            session,
            part: id,
            result: Ok(positioned_doc("second", RectLike::new(1.0, 1.0, 10.0, 4.0))),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    let doc = ed
        .text_panel
        .positioned_document()
        .expect("both parts recognized");
    assert_eq!(doc.lines.len(), 2);
    assert_eq!(doc.lines[1].rect, Some(RectLike::new(1.0, 11.0, 10.0, 4.0)));
    assert_eq!(
        scrubbed_history_text(ed).as_deref(),
        Some("█████\n\n██████")
    );
}

#[test]
fn recognition_runs_once_per_part_starting_with_the_first_missing_one() {
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(20, 10));
    ed.text_panel.part_docs.insert(
        readshot_core::document::ORIGINAL_PART,
        Arc::new(positioned_doc("first", RectLike::new(1.0, 1.0, 10.0, 4.0))),
    );
    let id = ed
        .model
        .add_part(solid(20, 10), readshot_core::document::Join::Below)
        .unwrap();
    app.editor = Some(ed);
    let _ = request_text_recognition(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.text_panel.recognizing_part, Some(id));
    assert!(matches!(
        ed.text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
}

#[test]
fn a_stale_recognition_of_the_original_frame_never_masks_a_rotated_record() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(8, 4);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
    ed.model.rotate(true).unwrap();
    ed.model.commit_annotation(Annotation::Blur {
        rect: RectLike::new(0.0, 0.0, 4.0, 8.0),
        radius: 4.0,
    });
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    assert!(stored.redacted && stored.geometry_id.is_some());
    // The capture's own OCR (original frame) lands late.
    let doc = positioned_doc("secret", RectLike::new(0.0, 0.0, 8.0, 4.0));
    app.coordinator
        .record_recognized_text(&record, &doc, true)
        .unwrap();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    assert_eq!(stored.ocr_text, None, "stale text is dropped, never stored");
}

#[tokio::test]
async fn a_rotation_rewrites_history_as_the_rotated_document_with_editable_annotations() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(8, 4);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(1.0, 1.0, 2.0, 2.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 1.0,
        style: Default::default(),
    });
    ed.model.rotate(true).unwrap();
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    assert!(stored.geometry_id.is_some());
    assert_eq!((stored.width_px, stored.height_px), (4, 8));
    assert_eq!(
        stored.annotation_model.len(),
        1,
        "annotations stay editable"
    );
    assert!(!stored.redacted);
    // Undo back to the original frame restores the original image.
    ed.model.undo();
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = store
        .list()
        .unwrap()
        .into_iter()
        .find(|r| r.id == record.id)
        .unwrap();
    assert_eq!(stored.geometry_id, None);
    assert_eq!((stored.width_px, stored.height_px), (8, 4));
}

#[test]
fn a_plain_edit_after_a_rotation_keeps_the_stored_size_of_the_rotated_png() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(8, 4);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
    ed.model.rotate(true).unwrap();
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    // An ordinary edit afterwards: the plain sidecar sync.
    ed.model.commit_annotation(arrow());
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().remove(0);
    assert_eq!(stored.annotation_model.len(), 1, "the plain sync landed");
    assert_eq!(stored_png_dims(&store, &record), (4, 8));
    assert_eq!((stored.width_px, stored.height_px), (4, 8));
}

#[test]
fn a_plain_edit_after_a_combine_stores_the_combined_size() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = solid(8, 4);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
    ed.model
        .add_part(solid(8, 6), readshot_core::document::Join::Below)
        .unwrap();
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    ed.model.commit_annotation(arrow());
    write_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().remove(0);
    assert_eq!(stored.annotation_model.len(), 1, "the plain sync landed");
    let png = stored_png_dims(&store, &record);
    assert_eq!(png, (ed.model.base().width(), ed.model.base().height()));
    assert_ne!(png, (8, 4));
    assert_eq!((stored.width_px, stored.height_px), png);
}

#[tokio::test]
async fn a_cropped_scrub_stores_its_redactions_in_the_stored_png_and_masks_reopen_ocr() {
    use readshot_core::ocr_layout::RecognizedLine;
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    // OCR of the stored (cropped) image reads the weakly hidden word
    // where it is in that image: x 10–16 of 20.
    let perms = Arc::new(FakePermissions::granted());
    let coord = CaptureCoordinator::new(
        Arc::new(FakeCapturer::new()),
        Arc::new(FakeOcrEngine::with_lines(vec![RecognizedLine {
            text: hidden_word(),
            x: 0.5,
            y: 0.1,
            w: 0.3,
            h: 0.2,
            words: Vec::new(),
        }])),
        perms.clone(),
        Some(store.clone()),
    );
    let app = App::new(coord, perms, Preferences::default());
    let record = cropped_weak_pixelate_scrub(&store, &app);
    let stored = stored_record(&store, record.id);
    assert!(stored.redacted);
    assert_eq!(stored_png_dims(&store, &record), (20, 20));
    assert_eq!((stored.width_px, stored.height_px), (20, 20));
    assert_eq!(
        stored.redactions,
        vec![Annotation::Pixelate {
            rect: RectLike::new(10.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        }],
        "in the stored PNG’s pixels"
    );
    // The capture's own OCR of the uncropped image lands late: its
    // boxes are in another frame than the stored image's.
    app.coordinator
        .record_recognized_text(
            &record,
            &positioned_doc(&hidden_word(), RectLike::new(30.0, 2.0, 6.0, 4.0)),
            true,
        )
        .unwrap();
    let text = stored_record(&store, record.id)
        .ocr_text
        .unwrap_or_default();
    assert!(!text.contains(&hidden_word()), "{text}");
    // Reopen: the editor recognizes the stored image; the task's
    // History write masks it with the stored redactions.
    let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
        store.root(),
        &stored,
    ))
    .unwrap()
    .to_rgba8();
    let ed = crate::editor::EditorSession::from_history(png.clone(), stored.clone());
    let mut framed = stored.clone();
    framed.geometry_id = history_geometry_id(&ed);
    let doc = app
        .coordinator
        .recognise_document(readshot_ocr::OCRRequest {
            image: png,
            languages: Vec::new(),
            use_language_correction: true,
        })
        .await
        .map_err(|e| e.to_string());
    assert!(doc.as_ref().unwrap().layout_text.contains(&hidden_word()));
    let _ =
        super::super::text_panel::store_recognized_text(&app.coordinator, &framed, None, doc).await;
    let text = stored_record(&store, record.id)
        .ocr_text
        .unwrap_or_default();
    assert!(!text.contains(&hidden_word()), "{text}");
    assert!(text.contains('█'), "{text}");
}

#[test]
fn a_reopened_cropped_scrub_masks_its_text_with_the_inherited_redactions() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let record = cropped_weak_pixelate_scrub(&store, &app);
    let stored = stored_record(&store, record.id);
    let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
        store.root(),
        &stored,
    ))
    .unwrap()
    .to_rgba8();
    app.editor = Some(crate::editor::EditorSession::from_history(png, stored));
    // OCR of the stored image reads the word at x 10–16.
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        positioned_doc(&hidden_word(), RectLike::new(10.0, 2.0, 6.0, 4.0)),
    );
    let ed = app.editor.as_mut().unwrap();
    ed.model.rotate(true).unwrap();
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let text = stored_record(&store, record.id)
        .ocr_text
        .unwrap_or_default();
    assert!(!text.contains(&hidden_word()), "{text}");
    assert_eq!(text, "█".repeat(6));
}

#[test]
fn a_cropped_scrub_stores_only_the_text_inside_the_crop() {
    use readshot_core::history::record_matches_query;
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let record = cropped_scrub_session(&store, &mut app, Some(three_words()));
    let stored = stored_record(&store, record.id);
    assert!(stored.redacted);
    assert_eq!(stored_png_dims(&store, &record), (20, 20));
    // Cut-away "outside" is gone, "inside" kept, the redaction masked.
    assert_eq!(stored.ocr_text.as_deref(), Some("inside ██████"));
    // History search and the MCP server (same sidecar, same matcher).
    assert!(!record_matches_query(&stored, "outside"));
    assert!(!record_matches_query(&stored, &hidden_word()));
    assert!(record_matches_query(&stored, "inside"));
}

#[test]
fn a_cropped_scrub_without_word_boxes_stores_no_text() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    // An engine without positions: only the stored plain text is known.
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let record = cropped_scrub_session(&store, &mut app, None);
    let stored = stored_record(&store, record.id);
    assert!(stored.redacted);
    assert_eq!(
        stored.ocr_text, None,
        "fail closed: nothing tells what the crop keeps"
    );
}

#[tokio::test]
async fn recognition_landing_after_a_cropped_scrub_stores_only_the_text_inside_the_crop() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    // Redacted and cropped before any recognition: no text yet.
    let record = cropped_scrub_session(&store, &mut app, None);
    assert_eq!(stored_record(&store, record.id).ocr_text, None);
    // The recognition task's History write, run with the editor gone.
    let (target, cut) =
        super::super::text_panel::recognition_history_target(app.editor.as_ref().unwrap());
    app.editor = None;
    let target = target.expect("a History target");
    assert!(cut.is_some());
    let _ = super::super::text_panel::store_recognized_text(
        &app.coordinator,
        &target,
        cut.as_ref(),
        Ok(three_words()),
    )
    .await;
    let stored = stored_record(&store, record.id);
    assert_eq!(stored.ocr_text.as_deref(), Some("inside ██████"));
}

#[test]
fn a_cropped_record_of_an_older_rule_drops_its_text_on_load_and_stores_none() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_reading(store.clone(), vec![ocr_line("kept", 0.0, 0.5, 0.4, 0.2)]);
    let record = cropped_weak_pixelate_scrub(&store, &app);
    // Rewrite the sidecar as an older build left it: an unprefixed crop
    // id and text holding a word from the cut-away area.
    let path = store
        .root()
        .join(readshot_core::FsHistoryStore::json_path(&record));
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["geometry_id"] = "crop:73d1ba24128a984d".into();
    json["ocr_text"] = "cutaway ██████".into();
    std::fs::write(&path, json.to_string()).unwrap();
    // History (and so MCP) never serves the old text.
    let listed = app.coordinator.history_list().unwrap();
    assert_eq!(listed[0].ocr_text, None);
    assert_eq!(
        listed[0].geometry_id.as_deref(),
        Some("crop3:73d1ba24128a984d")
    );
    // Whether that era's stored redactions are complete is unknown: no
    // text is recovered from the stored image (fail closed).
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ListLoaded(Ok(listed))),
    );
    app.coordinator.flush_history_updates();
    assert_eq!(stored_record(&store, record.id).ocr_text, None);
}

#[tokio::test]
async fn a_new_redaction_over_unknown_baked_in_redactions_never_recovers_text() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let (img, record) = scrubbed_without_redactions(&store);
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    let ed = app.editor.as_mut().unwrap();
    ed.model
        .commit_annotation(word_blur(RectLike::new(20.0, 10.0, 5.0, 5.0)));
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    assert_eq!(stored_record(&store, record.id).ocr_text, None);
    // The editor's recognition of the opened image reads the word under
    // the unknown blur: it has nowhere to be stored.
    let ed = app.editor.as_ref().unwrap();
    let (target, cut) = super::super::text_panel::recognition_history_target(ed);
    assert!(target.is_none(), "{target:?}");
    let doc = positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0));
    if let Some(target) = target {
        let _ = super::super::text_panel::store_recognized_text(
            &app.coordinator,
            &target,
            cut.as_ref(),
            Ok(doc),
        )
        .await;
    }
    // Nor does loading History recover anything.
    let listed = app.coordinator.history_list().unwrap();
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ListLoaded(Ok(listed))),
    );
    app.coordinator.flush_history_updates();
    let text = stored_record(&store, record.id).ocr_text;
    assert_eq!(text, None, "fail closed");
    // The new redaction is stored, but the list is never complete.
    let stored = stored_record(&store, record.id);
    assert_eq!(stored.redactions.len(), 1);
    assert!(!stored.redactions_complete);
    // Reopen that record and recognize again: its stored list must not
    // be trusted, in the original frame or another.
    let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
        store.root(),
        &stored,
    ))
    .unwrap()
    .to_rgba8();
    app.editor = Some(crate::editor::EditorSession::from_history(
        png,
        stored.clone(),
    ));
    let ed = app.editor.as_ref().unwrap();
    let (target, _) = super::super::text_panel::recognition_history_target(ed);
    assert!(target.is_none(), "{target:?}");
    // Even handed the record directly, History stores nothing.
    let doc = positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0));
    app.coordinator
        .record_recognized_text(&stored, &doc, true)
        .unwrap();
    assert_eq!(stored_record(&store, record.id).ocr_text, None);
    // The editor-side write of another frame: no text either.
    recognized(&mut app, readshot_core::document::ORIGINAL_PART, doc);
    let ed = app.editor.as_mut().unwrap();
    ed.model.rotate(true).unwrap();
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    assert!(stored.geometry_id.is_some());
    assert_eq!(stored.ocr_text, None);
    assert!(!stored.redactions_complete);
}

#[tokio::test]
async fn a_complete_chain_of_scrubs_still_masks_recognized_text() {
    // A fresh capture scrubbed, then reopened and scrubbed again: every
    // redaction in the image is known, so recognition is stored masked.
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let img = solid(30, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    let first = RectLike::new(2.0, 4.0, 10.0, 3.0);
    let ed = app.editor.as_mut().unwrap();
    ed.model.commit_annotation(word_blur(first));
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    assert!(stored.redacted && stored.redactions_complete);
    // Reopen, add a second redaction.
    let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
        store.root(),
        &stored,
    ))
    .unwrap()
    .to_rgba8();
    app.editor = Some(crate::editor::EditorSession::from_history(png, stored));
    let second = RectLike::new(16.0, 12.0, 10.0, 3.0);
    let ed = app.editor.as_mut().unwrap();
    ed.model.commit_annotation(word_blur(second));
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    assert!(stored.redactions_complete);
    assert_eq!(stored.redactions.len(), 2);
    let ed = app.editor.as_ref().unwrap();
    let (target, cut) = super::super::text_panel::recognition_history_target(ed);
    let target = target.expect("a complete list: text may be stored");
    use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
    let line = |text: &str, rect: RectLike| TextLine {
        text: text.into(),
        rect: Some(rect),
        words: vec![WordRect {
            start: 0,
            end: text.chars().count(),
            rect,
        }],
    };
    let doc = TextDocument {
        lines: vec![line(&hidden_word(), first), line("abcdef", second)],
        positioned: true,
        layout_text: format!("{}\nabcdef", hidden_word()),
    };
    let _ = super::super::text_panel::store_recognized_text(
        &app.coordinator,
        &target,
        cut.as_ref(),
        Ok(doc),
    )
    .await;
    let text = stored_record(&store, record.id).ocr_text.unwrap();
    assert_eq!(text, format!("{}\n{}", "█".repeat(6), "█".repeat(6)));
}

#[test]
fn copy_lines_masks_a_word_hidden_only_by_a_resized_redactions_fringe() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(30, 30));
    ed.model.scale_by(3.0, 3.0).unwrap();
    app.editor = Some(ed);
    // A word at x 20–21 of the capture: x 60–63 once tripled, just
    // right of the blur's exact rect (x 30–60) but under the resize's
    // fringe, which the exported image hides.
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        positioned_doc("Zx9Qw7", RectLike::new(20.0, 10.0, 1.0, 10.0)),
    );
    let ed = app.editor.as_mut().unwrap();
    ed.model
        .commit_annotation(word_blur(RectLike::new(30.0, 30.0, 30.0, 30.0)));
    let drawn = ed.model.drawn_annotations();
    assert!(matches!(
        drawn.as_slice(),
        [Annotation::Blur { rect, .. }] if rect.x + rect.width > 61.5
    ));
    assert_eq!(
        super::super::text_panel::copy_lines_text(ed, &[0], false).as_deref(),
        Some("██████")
    );
}

#[tokio::test]
async fn known_complete_redactions_still_mask_recognized_text() {
    // A reopened scrub whose stored redactions are known: the editor's
    // recognition is stored, masked with all of them.
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let (img, record) = reopened_scrubbed(&store);
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    let ed = app.editor.as_mut().unwrap();
    ed.model
        .commit_annotation(word_blur(RectLike::new(20.0, 10.0, 5.0, 5.0)));
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let ed = app.editor.as_ref().unwrap();
    let (target, cut) = super::super::text_panel::recognition_history_target(ed);
    let target = target.expect("redactions known: text may be stored");
    let _ = super::super::text_panel::store_recognized_text(
        &app.coordinator,
        &target,
        cut.as_ref(),
        Ok(positioned_doc(
            &hidden_word(),
            RectLike::new(2.0, 4.0, 10.0, 3.0),
        )),
    )
    .await;
    let text = stored_record(&store, record.id).ocr_text.unwrap();
    assert_eq!(text, "█".repeat(6));
}

#[test]
fn a_rotated_reopened_scrub_never_stores_the_raw_text_of_its_flattened_image() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let (img, record) = reopened_scrubbed(&store);
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0)),
    );
    let ed = app.editor.as_mut().unwrap();
    ed.model.rotate(true).unwrap();
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    assert!(stored.redacted);
    assert!(stored.geometry_id.is_some());
    let text = stored.ocr_text.unwrap_or_default();
    assert!(!text.contains(&hidden_word()), "{text}");
    assert_eq!(text, "█".repeat(6));
    // The inherited redaction, in the rotated frame.
    assert_eq!(
        stored.redactions,
        vec![word_blur(RectLike::new(13.0, 2.0, 3.0, 10.0))]
    );
}

#[test]
fn redacting_then_undoing_on_a_reopened_scrub_never_stores_the_raw_text() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let (img, record) = reopened_scrubbed(&store);
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0)),
    );
    let ed = app.editor.as_mut().unwrap();
    ed.model
        .commit_annotation(word_blur(RectLike::new(20.0, 10.0, 5.0, 5.0)));
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    let text = stored.ocr_text.clone().unwrap_or_default();
    assert!(!text.contains(&hidden_word()), "{text}");
    assert_eq!(stored.redactions.len(), 2, "inherited + new");
    ed.model.undo();
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    assert!(stored.redacted);
    let text = stored.ocr_text.clone().unwrap_or_default();
    assert!(!text.contains(&hidden_word()), "{text}");
    assert_eq!(stored.redactions, record.redactions);
}

#[test]
fn a_plain_sync_keeps_the_merged_text_of_a_combined_record() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let img = solid(20, 10);
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 20, 10, "d");
    record.ocr_text = Some("first".into());
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        positioned_doc("first", RectLike::new(1.0, 1.0, 10.0, 4.0)),
    );
    let ed = app.editor.as_mut().unwrap();
    let id = ed
        .model
        .add_part(solid(20, 10), readshot_core::document::Join::Below)
        .unwrap();
    sync_editor_history(ed, &app.coordinator);
    recognized(
        &mut app,
        id,
        positioned_doc("second", RectLike::new(1.0, 1.0, 10.0, 4.0)),
    );
    app.coordinator.flush_history_updates();
    assert_eq!(
        stored_record(&store, record.id).ocr_text.as_deref(),
        Some("first\n\nsecond")
    );
    let ed = app.editor.as_mut().unwrap();
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(1.0, 1.0, 2.0, 2.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 1.0,
        style: Default::default(),
    });
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    assert_eq!(stored.annotation_model.len(), 1);
    assert_eq!(stored.ocr_text.as_deref(), Some("first\n\nsecond"));
}

#[test]
fn redact_after_a_cancelled_geometry_preview_places_rects_in_the_restored_frame() {
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    let email = format!("{}@{}", "priya", "example.com");
    let line = RectLike::new(2.0, 4.0, 20.0, 3.0);
    let mut ed = crate::editor::EditorSession::new(solid(30, 20));
    ed.text_panel.part_docs.insert(
        readshot_core::document::ORIGINAL_PART,
        Arc::new(positioned_doc(&email, line)),
    );
    // A geometry preview is open, and the panel was rebuilt for it.
    let baseline = ed.model.snapshot();
    let g = ed.model.geometry();
    let rotated = g.with_transform(g.transform.rotated_cw());
    ed.model.preview_geometry(&baseline, rotated).unwrap();
    ed.snapshot_drag_baseline = Some(baseline);
    refresh_text_for_geometry(&mut ed);
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::RedactAll));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.model.geometry().is_original(), "preview cancelled");
    let rects: Vec<RectLike> = ed
        .model
        .annotations()
        .iter()
        .filter_map(|a| match a {
            Annotation::Rectangle { rect, .. }
            | Annotation::Blur { rect, .. }
            | Annotation::Pixelate { rect, .. } => Some(*rect),
            _ => None,
        })
        .collect();
    assert_eq!(rects.len(), 1, "{rects:?}");
    let r = rects[0];
    assert!(
        r.x <= line.x + 1.0
            && r.y <= line.y
            && r.x + r.width >= line.x + line.width - 1.0
            && r.y + r.height >= line.y + line.height,
        "{r:?} misses the line {line:?}"
    );
}

#[test]
fn a_late_failure_for_another_part_leaves_the_running_recognition_alone() {
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(20, 10));
    let session = ed.session_id;
    ed.text_panel.recognition = crate::editor::TextRecognition::Recognizing {
        since: std::time::Instant::now(),
    };
    ed.text_panel.recognizing_part = Some(5);
    ed.text_panel.user_requested = true;
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextRecognized {
            session,
            part: readshot_core::document::ORIGINAL_PART,
            result: Err("late".into()),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(matches!(
        ed.text_panel.recognition,
        crate::editor::TextRecognition::Recognizing { .. }
    ));
    assert_eq!(ed.text_panel.recognizing_part, Some(5));
    assert!(ed.text_panel.user_requested);
}

#[test]
fn a_redaction_follows_a_rotation_and_its_undo_in_history() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let img = solid(30, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        record.clone(),
    ));
    let word = RectLike::new(2.0, 4.0, 10.0, 3.0);
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        positioned_doc(&hidden_word(), word),
    );
    let ed = app.editor.as_mut().unwrap();
    // A: redact the word.
    ed.model.commit_annotation(word_blur(word));
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let a = stored_record(&store, record.id);
    assert!(a.redacted);
    assert_eq!(a.geometry_id, None);
    assert_eq!(a.ocr_text.as_deref(), Some("██████"));
    assert_eq!(a.redactions, vec![word_blur(word)]);
    // B: rotate; the redaction and the boxes follow.
    ed.model.rotate(true).unwrap();
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let b = stored_record(&store, record.id);
    assert!(b.redacted);
    assert!(b.geometry_id.is_some());
    assert_eq!(b.ocr_text.as_deref(), Some("██████"));
    assert_eq!(
        b.redactions,
        vec![word_blur(RectLike::new(13.0, 2.0, 3.0, 10.0))]
    );
    // Back to A.
    ed.model.undo();
    sync_editor_history(ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let back = stored_record(&store, record.id);
    assert_eq!(back.geometry_id, None);
    assert_eq!(back.ocr_text.as_deref(), Some("██████"));
    assert_eq!(back.redactions, vec![word_blur(word)]);
}

#[test]
fn without_positions_a_hand_drawn_redaction_withholds_the_text() {
    // Windows / Linux: the capture's text has no word positions, so
    // nothing can tell which words a hand-drawn cover hides. The scrub
    // must store no text, never the text with only detected items masked
    // ("John Smith" isn't a detected item).
    let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 20, 10, "d");
    let email = format!("{}@{}", "priya", "example.com");
    record.ocr_text = Some(format!("Patient: John Smith\nEmail {email}"));
    let mut ed = crate::editor::EditorSession::from_history(solid(20, 10), record);
    super::super::text_panel::seed_text_from_history(&mut ed, false);
    ed.model
        .commit_annotation(word_blur(RectLike::new(0.0, 0.0, 4.0, 4.0)));
    assert_eq!(scrubbed_history_text(&ed), None);
    ed.model.rotate(true).unwrap();
    assert_eq!(scrubbed_history_text(&ed), None);
    // Copy N Lines withholds it too.
    assert_eq!(
        super::super::text_panel::copy_lines_text(&ed, &[0], false),
        None
    );
}

/// A reopened scrub (its baked-in blur over [`hidden_word`] recorded)
/// whose fresh recognition still reads the word under the weak blur.
fn reopened_scrub_reading_its_hidden_word(
    scrubbed: fn(
        &readshot_core::FsHistoryStore,
    ) -> (image::RgbaImage, readshot_core::CaptureRecord),
) -> (tempfile::TempDir, App) {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let (img, record) = scrubbed(&store);
    app.editor = Some(crate::editor::EditorSession::from_history(img, record));
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0)),
    );
    (dir, app)
}

#[test]
fn copy_lines_on_a_reopened_scrub_masks_its_baked_in_redactions() {
    let (_dir, mut app) = reopened_scrub_reading_its_hidden_word(reopened_scrubbed);
    let ed = app.editor.as_ref().unwrap();
    let text = super::super::text_panel::copy_lines_text(ed, &[0], false).unwrap();
    assert_eq!(text, "█".repeat(6));
    // Through the message too.
    let _ = update(&mut app, Message::Editor(EditorMsg::TextLineClicked(0)));
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyLines));
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some("Copying text…")
    );
}

#[test]
fn copy_all_on_a_reopened_scrub_masks_its_baked_in_redactions() {
    use super::super::editor_ops::{copy_all_mask, CopyAllMask};
    let (_dir, mut app) = reopened_scrub_reading_its_hidden_word(reopened_scrubbed);
    let ed = app.editor.as_ref().unwrap();
    // Never the panel's raw recognition: the output is recognized and
    // masked with the inherited redaction.
    let CopyAllMask::Hide(hide) = copy_all_mask(ed) else {
        panic!("a reopened scrub must be masked");
    };
    let doc = positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0));
    assert_eq!(
        readshot_core::scrub::masked_text(&doc, &hide).as_deref(),
        Some("██████")
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyTextRequested));
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some("Recognizing text…")
    );
}

#[test]
fn copies_of_a_reopened_scrub_with_unknown_redactions_are_withheld() {
    use super::super::editor_ops::{copy_all_mask, CopyAllMask};
    let (_dir, mut app) = reopened_scrub_reading_its_hidden_word(scrubbed_without_redactions);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        super::super::text_panel::copy_lines_text(ed, &[0], false),
        None
    );
    assert!(matches!(copy_all_mask(ed), CopyAllMask::Withhold));
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyTextRequested));
    let status = app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .map(str::to_owned)
        .unwrap_or_default();
    assert!(status.starts_with("Text hidden"), "{status}");
    assert!(!app.editor.as_ref().unwrap().busy);
    let _ = update(&mut app, Message::Editor(EditorMsg::TextLineClicked(0)));
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyLines));
    let status = app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .map(str::to_owned)
        .unwrap_or_default();
    assert!(status.starts_with("Text hidden"), "{status}");
}

#[test]
fn copy_all_masks_a_magnifier_bubble_that_shows_a_redaction() {
    use super::super::editor_ops::{copy_all_mask, CopyAllMask};
    let mut ed = crate::editor::EditorSession::new(solid(200, 120));
    ed.model
        .commit_annotation(word_blur(RectLike::new(0.0, 0.0, 30.0, 10.0)));
    // A magnifier over the blur, its bubble at (100, 60), radius 20.
    ed.model.commit_annotation(Annotation::Magnifier {
        source: PointLike::new(15.0, 5.0),
        radius: 10.0,
        bubble: PointLike::new(100.0, 60.0),
        zoom: 2.0,
        shape: Default::default(),
        color: Rgba::OPAQUE_BLACK,
    });
    let CopyAllMask::Hide(hide) = copy_all_mask(&ed) else {
        panic!("a redaction must be masked");
    };
    // Recognition of the output reads the magnified word in the bubble.
    let doc = positioned_doc(&hidden_word(), RectLike::new(85.0, 55.0, 30.0, 10.0));
    assert_eq!(
        readshot_core::scrub::masked_text(&doc, &hide).as_deref(),
        Some("██████")
    );
}

#[tokio::test]
async fn a_reopened_record_with_a_baked_bubble_masks_the_bubble_word_everywhere() {
    use super::super::editor_ops::{copy_all_mask, CopyAllMask};
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_positions_and_history(store.clone());
    let img = solid(200, 120);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 200, 120, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    // Scrub: a weak blur over the word, and a magnifier whose bubble
    // (centre (100, 60), radius 20) shows the blurred word enlarged.
    let mut ed = crate::editor::EditorSession::from_history(img.clone(), record.clone());
    ed.model
        .commit_annotation(word_blur(RectLike::new(0.0, 0.0, 30.0, 10.0)));
    ed.model.commit_annotation(Annotation::Magnifier {
        source: PointLike::new(15.0, 5.0),
        radius: 10.0,
        bubble: PointLike::new(100.0, 60.0),
        zoom: 2.0,
        shape: Default::default(),
        color: Rgba::OPAQUE_BLACK,
    });
    sync_editor_history(&mut ed, &app.coordinator);
    app.coordinator.flush_history_updates();
    let stored = stored_record(&store, record.id);
    assert!(stored.redacted && stored.redactions_complete);

    // Reopen it: recognition of the baked image reads the word in the
    // bubble.
    app.editor = Some(crate::editor::EditorSession::from_history(
        img,
        stored.clone(),
    ));
    let in_bubble = positioned_doc(&hidden_word(), RectLike::new(85.0, 55.0, 30.0, 10.0));
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        in_bubble.clone(),
    );
    let ed = app.editor.as_ref().unwrap();
    let masked = "█".repeat(6);
    // 1. Copy Lines.
    assert_eq!(
        super::super::text_panel::copy_lines_text(ed, &[0], false).as_deref(),
        Some(masked.as_str())
    );
    // 2. Copy All.
    let CopyAllMask::Hide(hide) = copy_all_mask(ed) else {
        panic!("a reopened scrub must be masked");
    };
    assert_eq!(
        readshot_core::scrub::masked_text(&in_bubble, &hide).as_deref(),
        Some(masked.as_str())
    );
    // 3. History text recognized for the reopened record.
    let (target, cut) = super::super::text_panel::recognition_history_target(ed);
    let target = target.expect("redactions known: text may be stored");
    let _ = super::super::text_panel::store_recognized_text(
        &app.coordinator,
        &target,
        cut.as_ref(),
        Ok(in_bubble.clone()),
    )
    .await;
    assert_eq!(
        stored_record(&store, record.id).ocr_text,
        Some(masked.clone())
    );
    // 4. A further scrub of the reopened record.
    let ed = app.editor.as_mut().unwrap();
    ed.model
        .commit_annotation(word_blur(RectLike::new(150.0, 100.0, 10.0, 10.0)));
    assert_eq!(scrubbed_history_text(ed), Some(masked));
}

/// A restore History refused (another window redacted the record): the
/// editor says so, stops writing to the record and offers Save.
#[test]
fn a_refused_restore_tells_the_editor_and_unbinds_it() {
    let (_dir, _store, mut app) = scrub_app(true);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::HistoryWritten {
            session,
            result: Some(Err(
                crate::coordinator::HistoryRewriteError::RedactedElsewhere,
            )),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.source_record.is_none(), "no more writes to that record");
    let status = ed.status_text().map(str::to_owned).unwrap_or_default();
    assert!(status.contains("redacted in another window"), "{status}");
    assert!(status.contains(super::super::keys::SAVE_KEY), "{status}");
}

/// A restore refused because the record's sidecar couldn't be read says
/// exactly that, keeps the editor bound and rewrites again next time.
#[test]
fn an_unreadable_record_refusal_is_reported_as_such_and_not_final() {
    let (_dir, _store, mut app) = scrub_app(true);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::HistoryWritten {
            session,
            result: Some(Err(crate::coordinator::HistoryRewriteError::Unreadable)),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(
        ed.source_record.is_some(),
        "still bound: a later write retries"
    );
    assert!(
        ed.history_scrubbed,
        "the next sync rewrites the whole record"
    );
    let status = ed.status_text().map(str::to_owned).unwrap_or_default();
    assert!(
        status.starts_with("Couldn’t read this capture’s History record"),
        "{status}"
    );
    assert!(!status.contains("another window"), "{status}");
}
