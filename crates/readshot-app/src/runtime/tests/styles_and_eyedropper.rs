//! Style sliders and the eyedropper.

use super::*;

#[test]
fn style_message_restyles_the_selected_rectangle_as_one_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    let depth = ed.model.undo_depth();
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Style(readshot_ui::editor::StyleMessage::Fill(
            readshot_core::FillMode::Filled,
        ))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(matches!(
        &ed.model.annotations()[0],
        Annotation::Rectangle { style, .. } if style.fill == readshot_core::FillMode::Filled
    ));
    assert_eq!(ed.model.tool_style().fill, readshot_core::FillMode::Filled);
}

#[test]
fn output_commits_the_text_draft_after_dropping_a_lingering_width_preview() {
    for request in [
        Message::Editor(EditorMsg::CopyImageRequested),
        Message::Editor(EditorMsg::SaveRequested),
        Message::Editor(EditorMsg::CopyTextRequested),
    ] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(editor_with_selected_rect());
        let _ = update(&mut app, Message::Editor(EditorMsg::LineWidthPreview(10.0)));
        app.editor.as_mut().unwrap().pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(50.0, 50.0),
            content: "typed".into(),
            edit_index: None,
        });
        let _ = update(&mut app, request.clone());
        let ed = app.editor.as_ref().unwrap();
        assert!(
            ed.model
                .annotations()
                .iter()
                .any(|a| matches!(a, Annotation::Text { content, .. } if content == "typed")),
            "{request:?} dropped the committed text"
        );
        assert!(ed.width_drag_baseline.is_none());
    }
}

#[test]
fn output_keeps_a_lingering_style_preview_as_one_undo_step() {
    use readshot_ui::editor::StyleMessage;
    for request in [
        Message::Editor(EditorMsg::CopyImageRequested),
        Message::Editor(EditorMsg::SaveRequested),
    ] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(editor_with_selected_rect());
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        // Arrow keys on the slider: previews, never a release.
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(12.0))),
        );
        let _ = update(&mut app, request.clone());
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.style_drag_baseline.is_none());
        assert_eq!(ed.model.undo_depth(), depth + 1, "{request:?}");
        assert!(
            matches!(
                &ed.model.annotations()[0],
                Annotation::Rectangle { style, .. } if style.corner_radius == 12.0
            ),
            "{request:?}: the output must match the preview"
        );
    }
}

#[test]
fn style_slider_preview_commits_once_and_esc_cancels_it() {
    use readshot_ui::editor::StyleMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    let depth = ed.model.undo_depth();
    app.editor = Some(ed);
    for r in [4.0, 8.0, 12.0] {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(r))),
        );
    }
    let _ = update(&mut app, Message::Editor(EditorMsg::StyleCommit));
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(20.0))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.style_drag_baseline.is_none());
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(matches!(
        &ed.model.annotations()[0],
        Annotation::Rectangle { style, .. } if style.corner_radius == 12.0
    ));
}

#[test]
fn undo_during_a_held_style_drag_cancels_only_the_drag() {
    use readshot_ui::editor::StyleMessage;
    let mut app = two_rects_first_selected();
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let tick = |app: &mut App, r: f32| {
        let _ = update(
            app,
            Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(r))),
        );
    };
    tick(&mut app, 6.0);
    // The mouse is down: this is a held slider drag.
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::EditorPointerButton(true, None)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    tick(&mut app, 8.0);
    let _ = update(&mut app, Message::Editor(EditorMsg::StyleCommit));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.model.annotations().len(),
        2,
        "the second rect is not undone"
    );
    assert_eq!(ed.model.undo_depth(), depth, "no history step");
    assert!(matches!(
        &ed.model.annotations()[0],
        Annotation::Rectangle { style, .. } if style.corner_radius == 0.0
    ));
    assert!(!ed.style_drag_cancelled);
    // The next drag works again.
    tick(&mut app, 10.0);
    let _ = update(&mut app, Message::Editor(EditorMsg::StyleCommit));
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
}

#[test]
fn esc_during_a_held_style_drag_ignores_the_remaining_ticks() {
    use readshot_ui::editor::StyleMessage;
    let mut app = two_rects_first_selected();
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(6.0))),
    );
    // The mouse is down: this is a held slider drag.
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::EditorPointerButton(true, None)),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(9.0))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::StyleCommit));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth);
    assert!(matches!(
        &ed.model.annotations()[0],
        Annotation::Rectangle { style, .. } if style.corner_radius == 0.0
    ));
}

#[test]
fn a_hex_background_clears_a_stale_cancelled_drag_flag() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(16, 16));
    ed.snapshot_drag_cancelled = true;
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::HexChanged(
            "#102030".into(),
        ))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::HexSubmitted)),
    );
    assert!(!app.editor.as_ref().unwrap().snapshot_drag_cancelled);
    app.editor.as_mut().unwrap().snapshot_drag_cancelled = true;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::PickBackground)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(2.0, 2.0)),
        )),
    );
    assert!(!app.editor.as_ref().unwrap().snapshot_drag_cancelled);
}

#[test]
fn cancelled_slider_restores_the_tool_style_without_a_selection() {
    use readshot_ui::editor::StyleMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(20.0))),
    );
    assert_eq!(
        app.editor
            .as_ref()
            .unwrap()
            .model
            .tool_style()
            .corner_radius,
        20.0
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.tool_style().corner_radius, 0.0);
    assert!(ed.style_drag_baseline.is_none());
    // The cancelled drag's release ends it; the next drag keeps its value.
    let _ = update(&mut app, Message::Editor(EditorMsg::StyleCommit));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(8.0))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::StyleCommit));
    assert_eq!(
        app.editor
            .as_ref()
            .unwrap()
            .model
            .tool_style()
            .corner_radius,
        8.0
    );
}

#[test]
fn bracket_keys_step_selected_text_through_sizes() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(200, 200));
    ed.model.commit_annotation(Annotation::Text {
        content: "hi".into(),
        origin: PointLike::new(20.0, 60.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 24.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let size = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Text { size, .. } => *size,
        other => panic!("{other:?}"),
    };
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(1.0)));
    assert_eq!(size(&app), 36.0);
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(-1.0)));
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(-1.0)));
    assert_eq!(size(&app), 16.0);
}

#[test]
fn new_text_uses_the_text_style() {
    use readshot_ui::editor::{StyleMessage, TextSize};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(200, 200)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Style(StyleMessage::TextSize(TextSize::L))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Style(StyleMessage::TextPill(true))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(readshot_ui::CanvasMessage::RequestText(
            PointLike::new(20.0, 60.0),
        ))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextChanged("Hi".into())),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCommit));
    let ed = app.editor.as_ref().unwrap();
    assert!(matches!(
        ed.model.annotations().last(),
        Some(Annotation::Text { size, style, .. }) if *size == 36.0 && style.pill && !style.bold
    ));
}

#[test]
fn bracket_keys_step_legacy_text_relative_to_its_size() {
    let size = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Text { size, .. } => *size,
        other => panic!("{other:?}"),
    };
    let depth = |app: &App| app.editor.as_ref().unwrap().model.undo_depth();
    for (start, delta, want) in [
        (96.0, 1.0, 96.0),
        (12.0, -1.0, 12.0),
        (20.0, 1.0, 24.0),
        (20.0, -1.0, 16.0),
        (30.0, -1.0, 24.0),
    ] {
        let mut app = app_with_text(start);
        let before = depth(&app);
        let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(delta)));
        assert_eq!(size(&app), want, "{start} {delta}");
        let pushed = if want == start { 0 } else { 1 };
        assert_eq!(depth(&app), before + pushed, "{start} {delta}");
    }
}

#[test]
fn bracket_keys_with_the_text_tool_step_the_text_size() {
    use readshot_ui::editor::{TextSize, ToolState};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.set_tool(ToolState::Text);
    let width = ed.model.current_line_width();
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(1.0)));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.tool_style().text_size, TextSize::L);
    assert_eq!(ed.model.current_line_width(), width);
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(-1.0)));
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(-1.0)));
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(-1.0)));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.tool_style().text_size, TextSize::S);
    assert_eq!(ed.model.current_line_width(), width);
}

#[test]
fn bracket_keys_bump_a_selected_shape_from_its_own_width() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.set_line_width(3.0);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 8.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(1.0)));
    assert!(matches!(
        &app.editor.as_ref().unwrap().model.annotations()[0],
        Annotation::Rectangle { line_width, .. } if *line_width == 9.0
    ));
}

#[test]
fn esc_during_a_slider_drag_only_cancels_the_drag() {
    use readshot_ui::editor::StyleMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(20.0))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.style_drag_baseline.is_none());
    assert_eq!(ed.model.selected_annotation(), Some(0));
    assert!(matches!(
        &ed.model.annotations()[0],
        Annotation::Rectangle { style, .. } if style.corner_radius == 0.0
    ));
    // The next Esc clears the selection as before.
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        None
    );
}

/// The real delivery of one Esc (canvas `Cancelled`, then the key)
/// during a slider preview is one step: the preview is reverted and the
/// selection and an open text draft stay.
#[test]
fn one_real_escape_during_a_slider_preview_is_one_step() {
    use readshot_core::beautify::BeautifyEdit;
    use readshot_ui::editor::StyleMessage;
    let previews = [
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(20.0))),
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::SliderPreview(
            BeautifyEdit::Padding(40),
        ))),
    ];
    for preview in previews {
        let label = format!("{preview:?}");
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(&mut app, preview);
        let ed = app.editor.as_ref().unwrap();
        assert!(
            ed.style_drag_baseline.is_some() || ed.snapshot_drag_baseline.is_some(),
            "{label}: a preview is live"
        );
        press_escape(&mut app, id);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.style_drag_baseline.is_none(), "{label}");
        assert!(ed.snapshot_drag_baseline.is_none(), "{label}");
        assert_eq!(ed.model.beautify().padding, 0, "{label}: reverted");
        assert!(matches!(
            &ed.model.annotations()[0],
            Annotation::Rectangle { style, .. } if style.corner_radius == 0.0
        ));
        assert_eq!(ed.model.selected_annotation(), Some(0), "{label}: kept");
        // The next Esc clears the selection, as before.
        press_escape(&mut app, id);
        assert_eq!(
            app.editor.as_ref().unwrap().model.selected_annotation(),
            None,
            "{label}"
        );
    }

    // A Beautify slider can preview with a text draft open: one Esc
    // reverts the slider and keeps the typed draft.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.begin_text_edit(crate::editor::PendingText {
        origin: PointLike::new(10.0, 10.0),
        content: "draft".into(),
        edit_index: None,
    });
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::SliderPreview(
            BeautifyEdit::Padding(40),
        ))),
    );
    press_escape(&mut app, id);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.beautify().padding, 0);
    assert_eq!(
        ed.pending_text.as_ref().map(|p| p.content.as_str()),
        Some("draft"),
        "the draft survives the Esc that reverted the slider"
    );
}

/// A canvas `Cancelled` that is not an Escape (a released click) may
/// cancel a key-driven slider preview, but leaves no "Escape spent"
/// marker: the user's next Esc still does its step.
#[test]
fn a_cancel_without_escape_never_swallows_the_next_escape() {
    use readshot_ui::editor::StyleMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::StylePreview(StyleMessage::CornerRadius(20.0))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(readshot_ui::CanvasMessage::Cancelled)),
    );
    assert_eq!(app.editor.as_ref().unwrap().escape_ended_drag, None);
    press_escape(&mut app, id);
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        None,
        "the Esc cleared the selection"
    );
}

#[test]
fn i_toggles_the_eyedropper_but_not_while_typing() {
    let toggled = editor_key_message(
        iced::keyboard::Key::Character("i".into()),
        iced::keyboard::Modifiers::empty(),
        true,
    );
    assert!(matches!(
        toggled,
        Some(Message::Editor(EditorMsg::EyedropperToggled))
    ));
    let upper = editor_key_message(
        iced::keyboard::Key::Character("I".into()),
        iced::keyboard::Modifiers::SHIFT,
        true,
    );
    assert!(matches!(
        upper,
        Some(Message::Editor(EditorMsg::EyedropperToggled))
    ));
    // A focused text field captures the key (status not ignored):
    // tool letters and I do nothing while typing.
    for key in ["i", "s", "e", "v", "r"] {
        let typed = editor_key_message(
            iced::keyboard::Key::Character(key.into()),
            iced::keyboard::Modifiers::empty(),
            false,
        );
        assert!(typed.is_none(), "{key} leaked while typing");
    }
}

#[test]
fn eyedropper_pick_samples_the_source_image_and_exits() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut img = solid(16, 16);
    img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
    let mut ed = crate::editor::EditorSession::new(img);
    // A red rectangle over the pixel must not affect the pick.
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 16.0, 16.0),
        color: Rgba::new(1.0, 0.0, 0.0, 1.0),
        line_width: 2.0,
        style: readshot_core::ShapeStyle {
            fill: readshot_core::FillMode::Filled,
            ..Default::default()
        },
    });
    ed.model.clear_selection();
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(3.5, 4.5)),
        )),
    );
    let loupe = app
        .editor
        .as_ref()
        .unwrap()
        .eyedropper
        .as_ref()
        .unwrap()
        .loupe
        .clone()
        .expect("hover fills the loupe");
    let n = (readshot_ui::editor::LOUPE_RADIUS * 2 + 1) as usize;
    assert_eq!(loupe.pixels.len(), n * n);
    assert_eq!(
        loupe.pixels[n * n / 2].map(|c| c.to_hex()),
        Some("#4F46E5".to_string())
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(3.5, 4.5)),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.eyedropper.is_none());
    assert_eq!(ed.model.current_color().to_hex(), "#4F46E5");
    assert_eq!(
        app.preferences.editor_recent_colors,
        vec!["#4F46E5".to_string()]
    );
}

#[test]
fn eyedropper_pick_recolors_the_selection_as_one_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut img = solid(16, 16);
    img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
    let mut ed = crate::editor::EditorSession::new(img);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 16.0, 16.0),
        color: Rgba::new(1.0, 0.0, 0.0, 1.0),
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(3.5, 4.5)),
        )),
    );
    let color_of = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Rectangle { color, .. } => color.to_hex(),
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(color_of(&app), "#4F46E5");
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    assert_eq!(color_of(&app), "#FF0000");
}

#[test]
fn eyedropper_pick_of_a_preset_does_not_join_recent() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let preset = readshot_ui::editor::toolbar::PALETTE[0];
    let mut img = solid(16, 16);
    let to8 = |v: f32| (v * 255.0).round() as u8;
    img.put_pixel(
        3,
        4,
        image::Rgba([to8(preset.r), to8(preset.g), to8(preset.b), 255]),
    );
    app.editor = Some(crate::editor::EditorSession::new(img));
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(3.5, 4.5)),
        )),
    );
    assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
    assert!(app.preferences.editor_recent_colors.is_empty());
}

#[test]
fn esc_closes_the_popover_then_the_eyedropper() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PopoverToggled(
            crate::editor::EditorPopover::Width,
        )),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(ed.eyedropper.is_some(), "first esc only closes the popover");
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
}

#[test]
fn esc_exits_the_eyedropper_before_clearing_the_selection() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(16, 16));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 8.0, 8.0),
        color: Rgba::new(1.0, 0.0, 0.0, 1.0),
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.eyedropper.is_none());
    assert_eq!(ed.model.selected_annotation(), Some(0));
}

#[test]
fn eyedropper_button_closes_the_color_popover() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PopoverToggled(
            crate::editor::EditorPopover::Color,
        )),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(ed.eyedropper.is_some());
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
}

#[test]
fn choosing_a_tool_exits_the_eyedropper() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Rectangle,
        ))),
    );
    assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
}

#[test]
fn eyedropper_pick_matches_the_pixel_shown_after_a_fractional_crop() {
    let mut img = solid(32, 16);
    img.put_pixel(10, 3, image::Rgba([79, 70, 229, 255]));
    img.put_pixel(11, 4, image::Rgba([200, 10, 10, 255]));
    let crop = |x: f32, y: f32| Annotation::Crop {
        rect: RectLike::new(x, y, 12.0, 8.0),
    };
    // x0 = 10.6, y0 = 3.6: the displayed image starts at base (10, 3),
    // and the canvas maps with that whole-pixel origin (crop_offset), so
    // local (0.5, 0.5) → base (10.5, 3.5).
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(img.clone());
    ed.model.commit_annotation(crop(10.6, 3.6));
    ed.model.clear_selection();
    assert_eq!(ed.crop_offset(), (10.0, 3.0));
    let (ox, oy) = ed.crop_offset();
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(0.5 + ox, 0.5 + oy)),
        )),
    );
    let loupe = app
        .editor
        .as_ref()
        .unwrap()
        .eyedropper
        .as_ref()
        .unwrap()
        .loupe
        .clone()
        .unwrap();
    let n = (readshot_ui::editor::LOUPE_RADIUS * 2 + 1) as usize;
    assert_eq!(
        loupe.pixels[n * n / 2].map(|c| c.to_hex()),
        Some("#4F46E5".to_string())
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(0.5 + ox, 0.5 + oy)),
        )),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.current_color().to_hex(),
        "#4F46E5"
    );

    // Whole-pixel crop: no shift.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(img);
    ed.model.commit_annotation(crop(10.0, 3.0));
    ed.model.clear_selection();
    app.editor = Some(ed);
    pick(&mut app, 11.5, 4.5);
    assert_eq!(
        app.editor.as_ref().unwrap().model.current_color().to_hex(),
        "#C80A0A"
    );
}

#[test]
fn zoom_clears_the_eyedropper_loupe() {
    for zoom in [
        Message::Editor(EditorMsg::ZoomIn),
        Message::Editor(EditorMsg::ZoomOut),
        Message::Editor(EditorMsg::ZoomActual),
        Message::Editor(EditorMsg::ZoomFit),
    ] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(
                readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(3.5, 4.5)),
            )),
        );
        let label = format!("{zoom:?}");
        let _ = update(&mut app, zoom);
        let eye = app
            .editor
            .as_ref()
            .unwrap()
            .eyedropper
            .as_ref()
            .expect("still on");
        assert!(eye.loupe.is_none(), "{label} kept a stale loupe");
    }
}

#[test]
fn eyedropper_pick_on_a_selection_keeps_the_undo_hint() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut img = solid(16, 16);
    img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
    let mut ed = crate::editor::EditorSession::new(img);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 16.0, 16.0),
        color: Rgba::new(1.0, 0.0, 0.0, 1.0),
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    pick(&mut app, 3.5, 4.5);
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some(format!("Picked #4F46E5. {UNDO_HINT}").as_str())
    );
    // Without a selection there is nothing to undo.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut img = solid(16, 16);
    img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
    app.editor = Some(crate::editor::EditorSession::new(img));
    pick(&mut app, 3.5, 4.5);
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some("Picked #4F46E5.")
    );
}

#[test]
fn rotate_flip_and_resize_wait_for_a_held_canvas_drag() {
    for held_move in [false, true] {
        for msg in [
            Message::Editor(EditorMsg::Rotate { clockwise: true }),
            Message::Editor(EditorMsg::Flip(
                readshot_core::document::FlipAxis::Horizontal,
            )),
            Message::Editor(EditorMsg::ResizeOpen),
        ] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            let mut ed = crate::editor::EditorSession::new(solid(16, 8));
            if held_move {
                ed.move_drag = Some(crate::editor::MoveDrag {
                    baseline: Vec::new(),
                    selected_index: 0,
                    start: PointLike::new(2.0, 2.0),
                    moved: true,
                    kind: crate::editor::MoveDragKind::Move,
                });
            } else {
                ed.canvas_drag_active = true;
            }
            let depth = ed.model.undo_depth();
            app.editor = Some(ed);
            let _ = update(&mut app, msg.clone());
            let ed = app.editor.as_ref().unwrap();
            assert!(ed.model.geometry().is_original(), "{msg:?}");
            assert_eq!(ed.model.undo_depth(), depth, "{msg:?}");
            assert!(ed.popover.is_none(), "{msg:?}");
            assert!(ed.resize_draft.is_none(), "{msg:?}");
        }
    }
    // Released: the rotation applies.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 8)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Rotate { clockwise: true }),
    );
    assert!(!app.editor.as_ref().unwrap().model.geometry().is_original());
}

#[test]
fn i_is_ignored_during_a_canvas_drag() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(16, 16));
    ed.canvas_drag_active = true;
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
}

#[test]
fn i_is_ignored_during_a_select_move_drag() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(16, 16));
    ed.move_drag = Some(crate::editor::MoveDrag {
        baseline: Vec::new(),
        selected_index: 0,
        start: PointLike::new(2.0, 2.0),
        moved: true,
        kind: crate::editor::MoveDragKind::Move,
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
    assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
}
