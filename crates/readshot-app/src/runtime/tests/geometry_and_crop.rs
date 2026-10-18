//! Rotate, flip, resize and crop.

use super::*;

#[test]
fn rotate_and_resize_keys_map_only_when_not_typing() {
    use iced::keyboard::{Key, Modifiers};
    let cmd = command_modifiers();
    assert!(matches!(
        editor_key_message(Key::Character("[".into()), cmd, true),
        Some(Message::EditorRotate { clockwise: false })
    ));
    assert!(matches!(
        editor_key_message(Key::Character("]".into()), cmd, true),
        Some(Message::EditorRotate { clockwise: true })
    ));
    assert!(editor_key_message(Key::Character("]".into()), cmd, false).is_none());
    assert!(matches!(
        editor_key_message(
            Key::Character(RESIZE_LETTER.into()),
            editor_alt_modifiers(),
            true
        ),
        Some(Message::EditorResizeOpen)
    ));
    // Plain [ / ] still bump the width; plain I is still the eyedropper.
    assert!(matches!(
        editor_key_message(Key::Character("[".into()), Modifiers::empty(), true),
        Some(Message::EditorWidthBump(_))
    ));
    assert!(matches!(
        editor_key_message(Key::Character("i".into()), Modifiers::empty(), true),
        Some(Message::EditorEyedropperToggled)
    ));
}

#[test]
fn rotate_moves_annotations_with_the_image_and_undoes() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(30, 20));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(2.0, 4.0, 10.0, 3.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorRotate { clockwise: true });
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (20, 30));
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { rect, .. } => {
            assert_eq!(*rect, RectLike::new(13.0, 2.0, 3.0, 10.0))
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        ed.status.as_deref(),
        Some(format!("Rotated right. {UNDO_HINT}").as_str())
    );
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (30, 20));
}

#[test]
fn resize_draft_locks_the_ratio_in_px_and_percent() {
    let mut d = crate::editor::ResizeDraft::new((400, 200));
    assert_eq!((d.width.as_str(), d.height.as_str()), ("400", "200"));
    d.set_width("200");
    assert_eq!(d.height, "100");
    assert_eq!(d.target(), Some((200, 100)));
    d.set_unit(crate::editor::ResizeUnit::Percent);
    assert_eq!((d.width.as_str(), d.height.as_str()), ("50", "50"));
    d.lock = false;
    d.set_height("25");
    assert_eq!(d.target(), Some((200, 50)));
    d.set_width("abc");
    assert_eq!(d.target(), None);
    d.set_width("0");
    assert_eq!(d.target(), None, "at least 1\u{a0}px");
}

#[test]
fn a_locked_resize_of_an_extreme_aspect_keeps_the_derived_side_at_one_px() {
    let mut d = crate::editor::ResizeDraft::new((32_767, 1));
    d.set_width("100");
    assert_eq!(d.height, "1");
    assert_eq!(d.target(), Some((100, 1)), "the typed width is honoured");
    let mut d = crate::editor::ResizeDraft::new((1, 32_767));
    d.set_height("100");
    assert_eq!(d.width, "1");
    assert_eq!(d.target(), Some((1, 100)));
    // In percent too: 1 % of 32767 × 1 keeps a 1 px side.
    let mut d = crate::editor::ResizeDraft::new((32_767, 1));
    d.set_unit(crate::editor::ResizeUnit::Percent);
    d.set_width("1");
    assert_eq!(d.target(), Some((328, 1)));
}

#[test]
fn the_resize_warning_names_the_real_problem() {
    let mut d = crate::editor::ResizeDraft::new((400, 200));
    d.lock = false;
    d.set_width("abc");
    assert_eq!(d.problem(), Some("Enter the width and height as numbers."));
    d.set_width("0.4");
    assert_eq!(
        d.problem(),
        Some("The width comes to less than 1\u{a0}px. Enter a larger size.")
    );
    d.set_width("100");
    d.set_unit(crate::editor::ResizeUnit::Percent);
    d.set_height("0.1");
    assert_eq!(
        d.problem(),
        Some("The height comes to less than 1\u{a0}px. Enter a larger size.")
    );
    d.set_height("50");
    assert_eq!(d.problem(), None);
}

#[test]
fn resize_draft_unit_switches_round_trip_exactly() {
    use crate::editor::{ResizeDraft, ResizeUnit};
    let mut d = ResizeDraft::new((1000, 500));
    d.lock = false;
    d.set_width("333");
    d.set_height("167");
    d.set_unit(ResizeUnit::Percent);
    assert_eq!((d.width.as_str(), d.height.as_str()), ("33", "33"));
    assert_eq!(
        d.target(),
        Some((333, 167)),
        "the shown % stands for the px"
    );
    d.set_unit(ResizeUnit::Pixels);
    // Before, this came back as 330 x 165.
    assert_eq!((d.width.as_str(), d.height.as_str()), ("333", "167"));
    // A field typed after a switch means what it says.
    d.set_unit(ResizeUnit::Percent);
    d.set_width("50");
    assert_eq!(d.target(), Some((500, 167)));
}

#[test]
fn resize_draft_lock_follows_the_original_aspect() {
    use crate::editor::{ResizeDraft, ResizeUnit};
    let mut d = ResizeDraft::new((1000, 501));
    d.set_unit(ResizeUnit::Percent);
    d.set_width("33.3");
    assert_eq!(d.height, "33");
    // Before, the height was the rounded "33" %: 165 px, off aspect.
    assert_eq!(d.target(), Some((333, 167)));
    d.set_unit(ResizeUnit::Pixels);
    assert_eq!((d.width.as_str(), d.height.as_str()), ("333", "167"));
    // Locking again re-syncs the height from the width at the
    // original aspect.
    let mut d = ResizeDraft::new((1000, 300));
    d.lock = false;
    d.set_width("333");
    d.set_height("10");
    d.set_lock(true);
    assert_eq!(d.height, "100");
    d.set_unit(ResizeUnit::Percent);
    d.set_unit(ResizeUnit::Pixels);
    assert_eq!((d.width.as_str(), d.height.as_str()), ("333", "100"));
    assert_eq!(d.target(), Some((333, 100)));
}

#[test]
fn resize_apply_scales_the_document_and_refuses_oversize() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
    let _ = update(&mut app, Message::EditorResizeOpen);
    assert_eq!(
        app.editor.as_ref().unwrap().popover,
        Some(crate::editor::EditorPopover::Resize)
    );
    let _ = update(&mut app, Message::EditorResizeWidthChanged("20".into()));
    let _ = update(&mut app, Message::EditorResizeApply);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.effective_image_size(), (20, 10));
    assert_eq!(ed.popover, None);
    let _ = update(&mut app, Message::EditorResizeOpen);
    let _ = update(&mut app, Message::EditorResizeWidthChanged("70000".into()));
    let _ = update(&mut app, Message::EditorResizeApply);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.effective_image_size(), (20, 10), "unchanged");
    assert!(ed.status.as_deref().unwrap().starts_with("Couldn’t resize"));
}

#[test]
fn flip_mirrors_and_escape_closes_the_image_menu() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Image),
    );
    let _ = update(&mut app, Message::EditorTextCancel);
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
    let _ = update(
        &mut app,
        Message::EditorFlip(readshot_core::document::FlipAxis::Horizontal),
    );
    assert!(
        app.editor
            .as_ref()
            .unwrap()
            .model
            .geometry()
            .transform
            .flip_h
    );
}

#[test]
fn escape_closes_the_resize_dialog_and_drops_its_draft() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
    let _ = update(&mut app, Message::EditorResizeOpen);
    assert!(app.editor.as_ref().unwrap().resize_draft.is_some());
    let _ = update(&mut app, Message::EditorTextCancel);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(ed.resize_draft.is_none());
}

#[test]
fn a_geometry_error_reaches_the_user_as_a_status() {
    let s = geometry_error_status(
        "rotate",
        true,
        readshot_core::document::GeometryError::TooLarge {
            width: 40000,
            height: 10,
        },
    );
    assert!(s.starts_with("Couldn’t rotate"), "{s}");
    assert!(
        s.contains("uncropped image would be 40,000\u{a0}×\u{a0}10\u{a0}px"),
        "{s}"
    );
    assert!(s.contains("32,767\u{a0}px on a side"), "{s}");
    assert!(
        s.ends_with("Resize the image smaller, then try again."),
        "{s}"
    );
    let s = geometry_error_status(
        "flip",
        false,
        readshot_core::document::GeometryError::TooManyPixels { pixels: 9, max: 8 },
    );
    assert!(
        s.starts_with("Couldn’t flip") && s.contains("over the 256,000,000\u{a0}px limit"),
        "{s}"
    );
    assert!(!s.contains("uncropped"), "{s}");
    // With a crop, the pixel limit is the uncropped image's too.
    let s = geometry_error_status(
        "resize",
        true,
        readshot_core::document::GeometryError::TooManyPixels { pixels: 9, max: 8 },
    );
    assert!(s.starts_with("Couldn’t resize"), "{s}");
    assert!(s.contains("the uncropped image would be 9\u{a0}px"), "{s}");
    assert!(s.ends_with("Choose a smaller size."), "{s}");
    let s = geometry_error_status(
        "add the image",
        false,
        readshot_core::document::GeometryError::TooManyPixels {
            pixels: 268_435_456,
            max: 8,
        },
    );
    assert_eq!(
        s,
        "Couldn’t add the image — the image would be 268,435,456\u{a0}px, over the 256,000,000\u{a0}px limit. Resize the image smaller first, then add it again."
    );
    let s = geometry_error_status(
        "align",
        false,
        readshot_core::document::GeometryError::Empty,
    );
    assert!(s.ends_with("Undo the last change, then try again."), "{s}");
}

#[test]
fn big_numbers_are_grouped_in_threes() {
    use crate::runtime::image_menu::group_digits;
    assert_eq!(group_digits(0), "0");
    assert_eq!(group_digits(999), "999");
    assert_eq!(group_digits(1_000), "1,000");
    assert_eq!(group_digits(32_767), "32,767");
    assert_eq!(group_digits(256_000_000), "256,000,000");
}

#[test]
fn rotating_a_redacted_capture_keeps_boxes_and_history_in_one_frame_through_undo() {
    use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
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
    let hidden = hidden_word();
    let shown = "visible";
    let word = RectLike::new(2.0, 4.0, 10.0, 3.0);
    let other = RectLike::new(2.0, 12.0, 10.0, 3.0);
    let line = |t: &str, r: RectLike| TextLine {
        text: t.into(),
        rect: Some(r),
        words: vec![WordRect {
            start: 0,
            end: t.chars().count(),
            rect: r,
        }],
    };
    recognized(
        &mut app,
        readshot_core::document::ORIGINAL_PART,
        TextDocument {
            lines: vec![line(&hidden, word), line(shown, other)],
            positioned: true,
            layout_text: format!("{hidden}\n{shown}"),
        },
    );
    app.editor
        .as_mut()
        .unwrap()
        .model
        .commit_annotation(word_blur(word));
    // The rotated frame masks exactly the hidden word, not its neighbour.
    let check = |app: &App, rotated: bool| {
        app.coordinator.flush_history_updates();
        let ed = app.editor.as_ref().unwrap();
        let doc = ed.text_panel.positioned_document().expect("boxes follow");
        let (r0, r1, size) = if rotated {
            (
                RectLike::new(13.0, 2.0, 3.0, 10.0),
                RectLike::new(5.0, 2.0, 3.0, 10.0),
                (20, 30),
            )
        } else {
            (word, other, (30, 20))
        };
        assert_eq!(doc.lines[0].rect, Some(r0));
        assert_eq!(doc.lines[1].rect, Some(r1));
        assert_eq!(ed.image_size(), size);
        let stored = stored_record(&store, record.id);
        assert!(stored.redacted, "still a scrub");
        assert_eq!((stored.width_px, stored.height_px), size);
        let text = stored.ocr_text.expect("masked text");
        assert!(!text.contains(&hidden), "{text}");
        assert!(text.contains(shown), "neighbour stays readable: {text}");
        assert_eq!(stored.geometry_id.is_some(), rotated);
    };
    let _ = update(&mut app, Message::EditorRotate { clockwise: true });
    check(&app, true);
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    check(&app, false);
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
    );
    check(&app, true);
}

#[test]
fn rotate_drops_an_open_slider_preview_even_with_a_text_draft() {
    use readshot_core::beautify::BeautifyEdit;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.set_tool(readshot_ui::editor::ToolState::Text);
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "draft".into(),
        edit_index: None,
    });
    app.editor = Some(ed);
    let start = app.editor.as_ref().unwrap().model.beautify().padding;
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
    );
    let _ = update(&mut app, Message::EditorRotate { clockwise: true });
    let ed = app.editor.as_mut().unwrap();
    assert_eq!(ed.model.beautify().padding, start, "preview dropped");
    assert!(ed.snapshot_drag_baseline.is_none());
    while ed.model.undo() {
        assert_ne!(ed.model.beautify().padding, 150, "never baked into history");
    }
    assert_eq!(ed.model.beautify().padding, start);
}

#[test]
fn copy_commits_an_open_slider_preview_and_the_text_draft() {
    use readshot_core::beautify::BeautifyEdit;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.set_tool(readshot_ui::editor::ToolState::Text);
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "draft".into(),
        edit_index: None,
    });
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(25)),
    );
    let _ = update(&mut app, Message::EditorCopyImageRequested);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.beautify().padding, 25, "output keeps the preview");
    assert_eq!(ed.model.undo_depth(), depth + 2, "preview and text");
    assert!(ed.snapshot_drag_baseline.is_none());
    assert_eq!(ed.model.annotations().len(), 1);
}

#[test]
fn cmd_z_is_ignored_while_the_resize_dialog_is_open() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(32, 32));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(2.0, 2.0, 10.0, 10.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorResizeOpen);
    let cmd = iced::keyboard::Modifiers::COMMAND;
    for mods in [cmd, cmd | iced::keyboard::Modifiers::SHIFT] {
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Character("z".into()),
                modifiers: mods,
                status_ignored: false,
            },
        );
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
    }
}

#[test]
fn reopening_resize_while_it_is_open_keeps_the_typed_draft() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
    let _ = update(&mut app, Message::EditorResizeOpen);
    let _ = update(&mut app, Message::EditorResizeWidthChanged("12".into()));
    let _ = update(&mut app, Message::EditorResizeOpen);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.resize_draft.as_ref().unwrap().width, "12");
    assert_eq!(ed.popover, Some(crate::editor::EditorPopover::Resize));
}

#[test]
fn resizing_a_cropped_image_reports_the_exact_size() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(200, 150));
    ed.model.commit_annotation(Annotation::Crop {
        rect: RectLike::new(10.0, 10.0, 101.0, 77.0),
    });
    assert_eq!(ed.effective_image_size(), (101, 77));
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorResizeOpen);
    let _ = update(&mut app, Message::EditorResizeWidthChanged("999".into()));
    let _ = update(&mut app, Message::EditorResizeApply);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.effective_image_size(), (999, 762));
}

#[test]
fn crop_drag_makes_a_draft_enter_applies_and_esc_cancels() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(100, 80)));
    app.editor
        .as_mut()
        .unwrap()
        .model
        .set_tool(readshot_ui::editor::ToolState::Crop);
    let crop = |r| {
        Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
            Annotation::Crop { rect: r },
        ))
    };
    let _ = update(&mut app, crop(RectLike::new(10.0, 10.0, 40.0, 30.0)));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, Some(RectLike::new(10.0, 10.0, 40.0, 30.0)));
    assert_eq!(ed.model.undo_depth(), 0, "a draft is not an edit");
    assert_eq!(ed.effective_image_size(), (100, 80));
    // esc cancels only the draft.
    let _ = update(&mut app, Message::EditorTextCancel);
    assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
    let _ = update(&mut app, crop(RectLike::new(0.0, 0.0, 50.0, 40.0)));
    // ↵ arrives as EditorEditSelectedText (status_ignored Enter).
    let _ = update(&mut app, Message::EditorEditSelectedText);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, None);
    assert_eq!(ed.effective_image_size(), (50, 40));
    assert_eq!(ed.model.undo_depth(), 1);
}

#[test]
fn choosing_an_aspect_refits_the_draft_and_switching_tools_drops_it() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(100, 80)));
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Crop,
        )),
    );
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
            Annotation::Crop {
                rect: RectLike::new(0.0, 0.0, 80.0, 40.0),
            },
        )),
    );
    let _ = update(
        &mut app,
        Message::EditorStyle(readshot_ui::editor::style::StyleMessage::CropAspect(
            readshot_ui::editor::style::CropAspect::Square,
        )),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().crop_draft,
        Some(RectLike::new(20.0, 0.0, 40.0, 40.0))
    );
    assert!(visible_option_controls(app.editor.as_ref().unwrap())
        .contains(&readshot_ui::editor::style::OptionControl::CropApply));
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Select,
        )),
    );
    assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
}

#[test]
fn esc_drops_the_crop_draft_before_the_selection() {
    let mut app = crop_app_with_rect_selected();
    let _ = update(&mut app, Message::EditorTextCancel);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, None);
    assert_eq!(ed.model.selected_annotation(), Some(0), "one esc, one step");
    let _ = update(&mut app, Message::EditorTextCancel);
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        None
    );
}

#[test]
fn esc_during_a_crop_re_drag_cancels_the_drag_then_the_draft() {
    let mut app = crop_app_with_rect_selected();
    let id = iced::window::Id::unique();
    let ed = app.editor.as_mut().unwrap();
    ed.window_id = Some(id);
    ed.model.set_tool(readshot_ui::editor::ToolState::Crop);
    let draft = ed.crop_draft;
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::DragStarted),
    );
    let esc = |app: &mut App, canvas: readshot_ui::CanvasMessage| {
        // The canvas handles Escape first, then the key subscription
        // sees it captured.
        let _ = update(app, Message::EditorCanvas(canvas));
        let _ = update(
            app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: false,
            },
        );
    };
    esc(&mut app, readshot_ui::CanvasMessage::DragEscaped);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.canvas_drag_active, "the re-drag is cancelled");
    assert_eq!(ed.crop_draft, draft, "the previous draft is back");
    assert_eq!(ed.model.selected_annotation(), Some(0));
    esc(&mut app, readshot_ui::CanvasMessage::Escaped);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, None, "the second Esc cancels the draft");
    assert_eq!(ed.model.selected_annotation(), Some(0), "and only that");
}

#[test]
fn esc_during_a_crop_re_drag_is_one_step_whichever_message_comes_first() {
    let mut app = crop_app_with_rect_selected();
    let id = iced::window::Id::unique();
    let ed = app.editor.as_mut().unwrap();
    ed.window_id = Some(id);
    ed.model.set_tool(readshot_ui::editor::ToolState::Crop);
    let draft = ed.crop_draft;
    let key = |app: &mut App, named| {
        let _ = update(
            app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Named(named),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: false,
            },
        );
    };
    let canvas = |app: &mut App, m| {
        let _ = update(app, Message::EditorCanvas(m));
    };
    use iced::keyboard::key::Named;
    use readshot_ui::CanvasMessage;
    // Key first, then the canvas's message for the same Escape.
    canvas(&mut app, CanvasMessage::DragStarted);
    key(&mut app, Named::Escape);
    canvas(&mut app, CanvasMessage::DragEscaped);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.canvas_drag_active, "the re-drag is cancelled");
    assert_eq!(ed.crop_draft, draft, "the previous draft is back");
    assert_eq!(ed.escape_ended_drag, None, "nothing left to swallow");
    // A stale marker (its counterpart never came) can't swallow a
    // later Escape: another key clears it.
    canvas(&mut app, CanvasMessage::DragStarted);
    canvas(&mut app, CanvasMessage::DragEscaped);
    assert!(app.editor.as_ref().unwrap().escape_ended_drag.is_some());
    key(&mut app, Named::Shift);
    assert_eq!(app.editor.as_ref().unwrap().escape_ended_drag, None);
    // …and so does a press.
    canvas(&mut app, CanvasMessage::DragStarted);
    canvas(&mut app, CanvasMessage::DragEscaped);
    let _ = update(&mut app, Message::EditorPointerButton(true, None));
    let _ = update(&mut app, Message::EditorPointerButton(false, None));
    assert_eq!(app.editor.as_ref().unwrap().escape_ended_drag, None);
    assert_eq!(app.editor.as_ref().unwrap().crop_draft, draft);
    // The next Esc (key first again) cancels the draft, and only that.
    key(&mut app, Named::Escape);
    canvas(&mut app, CanvasMessage::Escaped);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, None, "the next Esc cancels the draft");
    assert_eq!(ed.model.selected_annotation(), Some(0), "and only that");
}

#[test]
fn esc_during_any_canvas_drag_ends_only_the_drag_in_either_order() {
    use iced::keyboard::key::Named;
    use readshot_ui::CanvasMessage;
    let rect0 = Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    };
    for key_first in [false, true] {
        let mut app = crop_app_with_rect_selected();
        let id = iced::window::Id::unique();
        app.editor.as_mut().unwrap().window_id = Some(id);
        let esc = |app: &mut App, canvas: CanvasMessage| {
            let key = Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Named(Named::Escape),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: false,
            };
            if key_first {
                let _ = update(app, key);
                let _ = update(app, Message::EditorCanvas(canvas));
            } else {
                let _ = update(app, Message::EditorCanvas(canvas));
                let _ = update(app, key);
            }
        };
        let draft = app.editor.as_ref().unwrap().crop_draft;

        // A shape draw, over a selection, a crop draft and a text
        // draft: the Esc ends the draw and keeps all three.
        let ed = app.editor.as_mut().unwrap();
        ed.model.set_tool(readshot_ui::editor::ToolState::Rectangle);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(50.0, 50.0),
            content: "typing".into(),
            edit_index: None,
        });
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        esc(&mut app, CanvasMessage::DragEscaped);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.canvas_drag_in_progress(), "key first: {key_first}");
        assert_eq!(ed.model.selected_annotation(), Some(0), "{key_first}");
        assert!(ed.pending_text.is_some(), "{key_first}");
        assert_eq!(ed.crop_draft, draft, "{key_first}");
        assert_eq!(ed.escape_ended_drag, None, "{key_first}");
        app.editor.as_mut().unwrap().pending_text = None;

        // A Select move: the Esc puts the annotation back and keeps
        // it selected (and the crop draft open).
        app.editor
            .as_mut()
            .unwrap()
            .model
            .set_tool(readshot_ui::editor::ToolState::Select);
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::SelectPressed(PointLike::new(5.0, 5.0))),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::SelectDragged(PointLike::new(15.0, 15.0))),
        );
        assert_ne!(app.editor.as_ref().unwrap().model.annotations()[0], rect0);
        esc(&mut app, CanvasMessage::DragEscaped);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.canvas_drag_in_progress(), "{key_first}");
        assert_eq!(ed.model.annotations()[0], rect0, "moved back: {key_first}");
        assert_eq!(ed.model.selected_annotation(), Some(0), "{key_first}");
        assert_eq!(ed.crop_draft, draft, "{key_first}");

        // The next Esc does what it always did: the crop draft, then
        // the selection.
        esc(&mut app, CanvasMessage::Escaped);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None, "{key_first}");
        assert_eq!(ed.model.selected_annotation(), Some(0), "{key_first}");
        esc(&mut app, CanvasMessage::Escaped);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.selected_annotation(), None, "{key_first}");
    }
}

#[test]
fn esc_exits_the_eyedropper_before_the_crop_draft() {
    let mut app = crop_app_with_rect_selected();
    app.editor.as_mut().unwrap().eyedropper = Some(crate::editor::EyedropperState {
        loupe: None,
        target: Default::default(),
    });
    let _ = update(&mut app, Message::EditorTextCancel);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.eyedropper.is_none());
    assert!(ed.crop_draft.is_some());
}

#[test]
fn undo_and_redo_only_cancel_an_open_crop_draft() {
    for redo in [false, true] {
        let mut app = crop_app_with_rect_selected();
        let msg = if redo {
            readshot_ui::ToolbarMessage::Redo
        } else {
            readshot_ui::ToolbarMessage::Undo
        };
        let _ = update(&mut app, Message::EditorToolbar(msg));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None);
        assert_eq!(ed.model.undo_depth(), 1, "history must not move");
    }
}

#[test]
fn enter_does_not_apply_a_crop_while_a_text_draft_is_open() {
    let mut app = crop_app_with_rect_selected();
    app.editor.as_mut().unwrap().pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(1.0, 1.0),
        content: "typing".into(),
        edit_index: None,
    });
    let _ = update(&mut app, Message::EditorEditSelectedText);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.crop_draft.is_some());
    assert_eq!(ed.effective_image_size(), (100, 80));
}

#[test]
fn applying_a_crop_is_one_undo_step() {
    let mut app = crop_app_with_rect_selected();
    let _ = update(&mut app, Message::EditorCropApply);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), 2);
    assert_eq!(ed.effective_image_size(), (40, 30));
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.effective_image_size(), (100, 80));
    assert_eq!(ed.model.undo_depth(), 1);
}

#[test]
fn a_crop_released_after_switching_tools_leaves_no_draft() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(100, 80)));
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Rectangle,
        )),
    );
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
            Annotation::Crop {
                rect: RectLike::new(0.0, 0.0, 50.0, 40.0),
            },
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, None);
    assert_eq!(ed.model.undo_depth(), 0);
}

#[test]
fn copy_and_save_apply_an_open_crop_draft_first() {
    for save in [false, true] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(100, 80));
        ed.crop_draft = Some(RectLike::new(0.0, 0.0, 50.0, 40.0));
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            if save {
                Message::EditorSaveRequested
            } else {
                Message::EditorCopyImageRequested
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None);
        assert_eq!(ed.effective_image_size(), (50, 40), "save={save}");
        assert_eq!(ed.model.undo_depth(), 1, "save={save}");
    }
}

#[test]
fn an_image_menu_action_drops_the_crop_draft() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(100, 80));
    ed.crop_draft = Some(RectLike::new(0.0, 0.0, 50.0, 40.0));
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorRotate { clockwise: true });
    assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
}

#[test]
fn an_aspect_change_with_a_draft_leaves_a_selected_committed_crop_alone() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(100, 80));
    let committed = RectLike::new(0.0, 0.0, 80.0, 40.0);
    ed.model
        .commit_annotation(Annotation::Crop { rect: committed });
    assert!(ed.model.selected_annotation().is_some());
    ed.crop_draft = Some(RectLike::new(0.0, 0.0, 60.0, 30.0));
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::EditorStyle(readshot_ui::editor::style::StyleMessage::CropAspect(
            readshot_ui::editor::style::CropAspect::Square,
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, Some(RectLike::new(15.0, 0.0, 30.0, 30.0)));
    assert_eq!(ed.model.undo_depth(), 1);
    assert_eq!(
        ed.model.annotations().first(),
        Some(&Annotation::Crop { rect: committed })
    );
}

#[test]
fn enter_does_not_apply_a_crop_while_a_field_popover_is_open() {
    for popover in [
        crate::editor::EditorPopover::Color,
        crate::editor::EditorPopover::Resize,
    ] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(100, 80));
        ed.crop_draft = Some(RectLike::new(0.0, 0.0, 50.0, 40.0));
        ed.popover = Some(popover);
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorEditSelectedText);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.crop_draft.is_some(), "{popover:?}");
        assert_eq!(ed.effective_image_size(), (100, 80));
    }
}
