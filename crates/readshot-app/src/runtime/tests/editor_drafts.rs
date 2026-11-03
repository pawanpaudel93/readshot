//! Editor text drafts, discard, output and editor keys.

use super::*;

#[test]
fn editor_select_drag_cancel_restores_baseline_without_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(5.0, 5.0)),
        )),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(12.0, 14.0)),
        )),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(readshot_ui::CanvasMessage::Cancelled)),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.move_drag.is_none());
    assert_eq!(ed.model.undo_depth(), undo_depth);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { rect, .. } => {
            assert_eq!(rect.x, 0.0);
            assert_eq!(rect.y, 0.0);
        }
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn editor_select_subpixel_drag_release_does_not_mutate_annotation() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(5.0, 5.0)),
        )),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(5.25, 5.25)),
        )),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectReleased,
        )),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.move_drag.is_none());
    assert_eq!(ed.model.undo_depth(), undo_depth);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { rect, .. } => {
            assert_eq!(rect.x, 0.0);
            assert_eq!(rect.y, 0.0);
        }
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn editor_tool_change_commits_pending_text_when_leaving_text_tool() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.set_tool(readshot_ui::editor::ToolState::Text);
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "draft".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Rectangle,
        ))),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert_eq!(ed.model.annotations().len(), 1);
    match &ed.model.annotations()[0] {
        Annotation::Text {
            content, origin, ..
        } => {
            assert_eq!(content, "draft");
            assert_eq!(*origin, PointLike::new(8.0, 9.0));
        }
        other => panic!("expected text annotation, got {other:?}"),
    }
    assert_eq!(
        ed.model.active_tool(),
        readshot_ui::editor::ToolState::Rectangle
    );
}

#[test]
fn editor_tool_change_dismisses_empty_pending_text_without_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let undo_depth = ed.model.undo_depth();
    ed.model.set_tool(readshot_ui::editor::ToolState::Text);
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "   ".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Rectangle,
        ))),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert!(ed.model.annotations().is_empty());
    assert_eq!(ed.model.undo_depth(), undo_depth);
    assert_eq!(
        ed.model.active_tool(),
        readshot_ui::editor::ToolState::Rectangle
    );
}

#[test]
fn editor_requesting_new_text_commits_existing_non_empty_draft() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.set_tool(readshot_ui::editor::ToolState::Text);
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "first note".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(readshot_ui::CanvasMessage::RequestText(
            PointLike::new(24.0, 25.0),
        ))),
    );

    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 1);
    match &ed.model.annotations()[0] {
        Annotation::Text {
            content, origin, ..
        } => {
            assert_eq!(content, "first note");
            assert_eq!(*origin, PointLike::new(8.0, 9.0));
        }
        other => panic!("expected text annotation, got {other:?}"),
    }
    let pending = ed.pending_text.as_ref().unwrap();
    assert_eq!(pending.origin, PointLike::new(24.0, 25.0));
    assert!(pending.content.is_empty());
}

#[test]
fn editor_requesting_new_text_dismisses_empty_draft_without_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let undo_depth = ed.model.undo_depth();
    ed.model.set_tool(readshot_ui::editor::ToolState::Text);
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "   ".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(readshot_ui::CanvasMessage::RequestText(
            PointLike::new(24.0, 25.0),
        ))),
    );

    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), undo_depth);
    assert!(ed.model.annotations().is_empty());
    let pending = ed.pending_text.as_ref().unwrap();
    assert_eq!(pending.origin, PointLike::new(24.0, 25.0));
    assert!(pending.content.is_empty());
}

#[test]
fn editor_text_commit_updates_original_edit_index_even_if_selection_changes() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.model.commit_annotation(Annotation::Text {
        content: "old".into(),
        origin: PointLike::new(10.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 16.0,
        style: Default::default(),
    });
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(40.0, 40.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(10.0, 20.0),
        content: "new".into(),
        edit_index: Some(0),
    });
    assert_eq!(ed.model.select_at(PointLike::new(45.0, 45.0)), Some(1));
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::TextCommit));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    match &ed.model.annotations()[0] {
        Annotation::Text { content, .. } => assert_eq!(content, "new"),
        other => panic!("expected text annotation, got {other:?}"),
    }
    assert!(matches!(
        &ed.model.annotations()[1],
        Annotation::Rectangle { .. }
    ));
}

#[test]
fn editor_delete_selected_dismisses_pending_text_edit() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.model.commit_annotation(Annotation::Text {
        content: "old".into(),
        origin: PointLike::new(10.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 16.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(10.0, 20.0),
        content: "draft replacement".into(),
        edit_index: Some(0),
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DeleteSelected));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert!(ed.model.annotations().is_empty());
    assert_eq!(ed.model.selected_annotation(), None);
    assert_eq!(
        ed.status_text(),
        Some(format!("Deleted annotation. {UNDO_HINT}").as_str())
    );
}

#[test]
fn editor_nudge_is_suppressed_while_editing_text() {
    // iced's single-line text input ignores Up/Down, so those arrows
    // reach the global key tap while a text draft is open. The nudge
    // handler must drop them rather than move the annotation being
    // typed (which would also desync the inline editor box).
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.model.commit_annotation(Annotation::Text {
        content: "hi".into(),
        origin: PointLike::new(10.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 16.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
    let undo_before = ed.model.undo_depth();
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(10.0, 20.0),
        content: "hi".into(),
        edit_index: Some(0),
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::NudgeSelected(0.0, 5.0)),
    );

    let ed = app.editor.as_ref().unwrap();
    match &ed.model.annotations()[0] {
        Annotation::Text { origin, .. } => {
            assert_eq!(origin.x, 10.0);
            assert_eq!(origin.y, 20.0);
        }
        other => panic!("expected text, got {other:?}"),
    }
    assert_eq!(ed.model.undo_depth(), undo_before);
}

#[test]
fn editor_delete_without_selection_keeps_new_pending_text() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(10.0, 20.0),
        content: "draft".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DeleteSelected));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_some());
    assert!(ed.model.annotations().is_empty());
    assert_eq!(ed.status, None);
}

#[test]
fn editor_cancel_existing_text_edit_keeps_annotation_selected() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.model.commit_annotation(Annotation::Text {
        content: "old".into(),
        origin: PointLike::new(10.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 16.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(10.0, 20.0),
        content: "draft replacement".into(),
        edit_index: Some(0),
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert_eq!(ed.model.selected_annotation(), Some(0));
    match &ed.model.annotations()[0] {
        Annotation::Text { content, .. } => assert_eq!(content, "old"),
        other => panic!("expected text annotation, got {other:?}"),
    }
}

#[test]
fn editor_cancel_new_text_draft_clears_selection() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(10.0, 20.0),
        content: "new label".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::TextCancel));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert_eq!(ed.model.selected_annotation(), None);
    assert_eq!(ed.model.annotations().len(), 1);
}

#[test]
fn editor_undo_commits_then_undoes_new_pending_text() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(30.0, 30.0),
        content: "draft label".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert_eq!(ed.model.annotations().len(), 1);
    assert!(matches!(
        ed.model.annotations()[0],
        Annotation::Rectangle { .. }
    ));
    assert!(ed.model.can_redo());
    assert_eq!(
        ed.status_text(),
        Some(format!("Undid edit. {REDO_HINT}").as_str())
    );
}

#[test]
fn cmd_z_with_a_text_draft_open_keeps_the_draft() {
    // The draft's text_input has no undo and doesn't capture ⌘Z; the
    // shortcut must not commit-then-undo the typed text.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(30.0, 30.0),
        content: "hello".into(),
        edit_index: None,
    });
    app.editor = Some(ed);
    let undo_before = app.editor.as_ref().unwrap().model.undo_depth();

    for modifiers in [
        iced::keyboard::Modifiers::COMMAND,
        iced::keyboard::Modifiers::COMMAND | iced::keyboard::Modifiers::SHIFT,
    ] {
        for status_ignored in [true, false] {
            let _ = update(
                &mut app,
                Message::Editor(EditorMsg::KeyPressed {
                    window: id,
                    key: iced::keyboard::Key::Character("z".into()),
                    modifiers,
                    status_ignored,
                }),
            );
        }
    }

    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.pending_text.as_ref().map(|p| p.content.as_str()),
        Some("hello")
    );
    assert_eq!(ed.model.annotations().len(), 1);
    assert_eq!(ed.model.undo_depth(), undo_before);
    assert!(!ed.model.can_redo());
}

#[test]
fn editor_redo_with_pending_text_commits_new_branch_and_clears_redo() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(96, 96));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.model.commit_annotation(Annotation::Ellipse {
        rect: RectLike::new(40.0, 40.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert!(ed.model.undo());
    assert!(ed.model.can_redo());
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(30.0, 30.0),
        content: "new branch".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Redo)),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert_eq!(ed.model.annotations().len(), 2);
    assert!(matches!(
        ed.model.annotations()[0],
        Annotation::Rectangle { .. }
    ));
    match &ed.model.annotations()[1] {
        Annotation::Text {
            content, origin, ..
        } => {
            assert_eq!(content, "new branch");
            assert_eq!(*origin, PointLike::new(30.0, 30.0));
        }
        other => panic!("expected text annotation, got {other:?}"),
    }
    assert!(!ed.model.can_redo());
    assert_eq!(
        ed.status_text(),
        Some(format!("Added text. {UNDO_HINT}").as_str())
    );
}

#[test]
fn editor_select_press_commits_non_empty_pending_text() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "label".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(30.0, 30.0)),
        )),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert_eq!(ed.model.annotations().len(), 1);
    match &ed.model.annotations()[0] {
        Annotation::Text {
            content, origin, ..
        } => {
            assert_eq!(content, "label");
            assert_eq!(*origin, PointLike::new(8.0, 9.0));
        }
        other => panic!("expected text annotation, got {other:?}"),
    }
}

#[test]
fn editor_select_press_dismisses_empty_pending_text_without_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let undo_depth = ed.model.undo_depth();
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "   ".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(30.0, 30.0)),
        )),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert!(ed.model.annotations().is_empty());
    assert_eq!(ed.model.undo_depth(), undo_depth);
}

#[test]
fn editor_tool_change_cancels_line_width_preview_without_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::LineWidthPreview(10.0)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Arrow,
        ))),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.width_drag_baseline.is_none());
    assert_eq!(ed.model.undo_depth(), undo_depth);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn editor_select_press_cancels_line_width_preview_without_committing() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::LineWidthPreview(10.0)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(30.0, 30.0)),
        )),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.width_drag_baseline.is_none());
    assert_eq!(ed.model.undo_depth(), undo_depth);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn editor_frame_change_cancels_line_width_preview_without_committing() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::LineWidthPreview(10.0)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::BeautifyPreset(BuiltinPreset::Soft)),
    );

    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.beautify(), BuiltinPreset::Soft.settings());
    assert!(ed.width_drag_baseline.is_none());
    assert_eq!(ed.model.undo_depth(), undo_depth + 1);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn editor_save_cancels_line_width_preview_before_output() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::LineWidthPreview(10.0)));
    let _ = update(&mut app, Message::Editor(EditorMsg::SaveRequested));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.busy);
    assert!(ed.width_drag_baseline.is_none());
    assert_eq!(ed.model.undo_depth(), undo_depth);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn editor_save_commits_pending_text_before_output() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: " ship this ".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::SaveRequested));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert_eq!(ed.model.annotations().len(), 1);
    match &ed.model.annotations()[0] {
        Annotation::Text {
            content, origin, ..
        } => {
            assert_eq!(content, "ship this");
            assert_eq!(*origin, PointLike::new(8.0, 9.0));
        }
        other => panic!("expected text, got {other:?}"),
    }
}

#[test]
fn editor_save_dismisses_empty_pending_text_without_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let undo_depth = ed.model.undo_depth();
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "   ".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::SaveRequested));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert!(ed.model.annotations().is_empty());
    assert_eq!(ed.model.undo_depth(), undo_depth);
}

#[test]
fn editor_successful_save_marks_current_output_clean_but_keeps_undo() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.model.set_beautify(BuiltinPreset::Soft.settings());
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);

    let dir = tempfile::tempdir().unwrap();
    let saved_path = dir.path().join("saved.png");
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Saved {
            session,
            result: Ok(Some(saved_path)),
        }),
    );

    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), undo_depth);
    assert!(ed.model.can_undo());
    assert!(!editor_has_unsaved_work(ed));

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    assert!(app.editor.is_none());
}

#[test]
fn editor_successful_copy_image_marks_current_output_clean() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.model.set_beautify(BuiltinPreset::Dark.settings());
    app.editor = Some(ed);

    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CopyImageDone {
            session,
            result: Ok(()),
        }),
    );

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.model.can_undo());
    assert!(!editor_has_unsaved_work(ed));
}

#[test]
fn editor_cancelled_or_failed_output_keeps_unsaved_work_dirty() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);

    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Saved {
            session,
            result: Ok(None),
        }),
    );
    assert!(editor_has_unsaved_work(app.editor.as_ref().unwrap()));

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CopyImageDone {
            session,
            result: Err("clipboard unavailable".into()),
        }),
    );
    assert!(editor_has_unsaved_work(app.editor.as_ref().unwrap()));
}

/// A save or copy started in one editor and finishing after another
/// replaced it never marks the new editor clean or busy-free, nor
/// reports its result there.
#[test]
fn an_output_result_from_a_replaced_editor_leaves_the_new_one_alone() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let old = crate::editor::EditorSession::new(solid(64, 64));
    let old_session = old.session_id;
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    assert_ne!(ed.session_id, old_session);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.busy = true;
    ed.set_status(crate::editor::StatusTone::Progress, "Recognizing text…");
    app.editor = Some(ed);
    let dir = tempfile::tempdir().unwrap();
    for message in [
        Message::Editor(EditorMsg::Saved {
            session: old_session,
            result: Ok(Some(dir.path().join("old.png"))),
        }),
        Message::Editor(EditorMsg::CopyImageDone {
            session: old_session,
            result: Ok(()),
        }),
        Message::Editor(EditorMsg::CopyTextDone {
            session: old_session,
            result: Ok("old text".into()),
        }),
    ] {
        let _ = update(&mut app, message);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.busy, "the new editor's own job is still running");
        assert!(editor_has_unsaved_work(ed), "never saved");
        assert_eq!(ed.status_text(), Some("Recognizing text…"));
    }
}

#[test]
fn editor_discard_requires_confirmation_for_non_empty_pending_text() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "draft".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    let ed = app.editor.as_ref().expect("first discard should only arm");
    assert!(ed.discard_armed());
    assert_eq!(
        ed.status_text(),
        Some(crate::editor::discard_prompt().as_str())
    );

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    assert!(app.editor.is_none());
}

#[test]
fn editor_text_action_supports_multi_line_and_commits_newline() {
    use iced::widget::text_editor::{Action, Edit};

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.begin_text_edit(crate::editor::PendingText {
        origin: PointLike::new(8.0, 20.0),
        content: String::new(),
        edit_index: None,
    });
    assert!(ed.text_edit_content.is_some());
    app.editor = Some(ed);

    // Type "ab", press Enter (newline), type "cd".
    for action in [
        Action::Edit(Edit::Insert('a')),
        Action::Edit(Edit::Insert('b')),
        Action::Edit(Edit::Enter),
        Action::Edit(Edit::Insert('c')),
        Action::Edit(Edit::Insert('d')),
    ] {
        let _ = update(&mut app, Message::Editor(EditorMsg::TextAction(action)));
    }

    let pending = app
        .editor
        .as_ref()
        .unwrap()
        .pending_text
        .as_ref()
        .expect("draft still open");
    assert_eq!(pending.content, "ab\ncd", "Enter should insert a newline");

    // Commit builds a multi-line Annotation::Text and tears down the
    // inline editor's backing store.
    let _ = update(&mut app, Message::Editor(EditorMsg::TextCommit));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pending_text.is_none());
    assert!(ed.text_edit_content.is_none());
    match ed.model.annotations().last().expect("text committed") {
        Annotation::Text { content, .. } => assert_eq!(content, "ab\ncd"),
        other => panic!("expected multi-line text, got {other:?}"),
    }
}

#[test]
fn editor_os_close_request_confirms_unsaved_work() {
    // OS close (red X / ⌘W) on a dirty editor must route through the
    // two-stage confirmation, not silently drop the session.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let editor_id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_id = Some(editor_id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);

    // First request arms the confirmation and keeps the editor open.
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::CloseRequested(editor_id)),
    );
    let ed = app
        .editor
        .as_ref()
        .expect("first OS close should only arm, not drop the editor");
    assert!(ed.discard_armed());
    assert_eq!(
        ed.status_text(),
        Some(crate::editor::discard_prompt().as_str())
    );

    // Second request within the confirm window closes it.
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::CloseRequested(editor_id)),
    );
    assert!(app.editor.is_none());
}

#[test]
fn editor_os_close_request_closes_clean_editor_immediately() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let editor_id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_id = Some(editor_id);
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Window(WindowMsg::CloseRequested(editor_id)),
    );
    assert!(app.editor.is_none());
}

#[test]
fn os_close_request_for_non_editor_window_leaves_editor_untouched() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let editor_id = iced::window::Id::unique();
    let other_id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_id = Some(editor_id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);

    // A close request for some other window must not touch the
    // editor session (those windows auto-close on their own).
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::CloseRequested(other_id)),
    );
    assert!(app.editor.is_some());
    assert!(!app.editor.as_ref().unwrap().discard_armed());
}

#[test]
fn editor_discard_treats_empty_pending_text_as_clean() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "   ".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    assert!(app.editor.is_none());
}

#[test]
fn editor_discard_treats_unchanged_text_edit_as_clean() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Text {
        content: "same text".into(),
        origin: PointLike::new(8.0, 9.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 16.0,
        style: Default::default(),
    });
    ed.mark_output_clean();
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: " same text ".into(),
        edit_index: Some(0),
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    assert!(app.editor.is_none());
}

#[test]
fn editor_discard_requires_confirmation_for_changed_text_edit() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Text {
        content: "old text".into(),
        origin: PointLike::new(8.0, 9.0),
        color: Rgba::OPAQUE_BLACK,
        font_family: "system-ui".into(),
        size: 16.0,
        style: Default::default(),
    });
    ed.mark_output_clean();
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "new text".into(),
        edit_index: Some(0),
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    let ed = app.editor.as_ref().expect("first discard should only arm");
    assert!(ed.discard_armed());
    assert_eq!(
        ed.status_text(),
        Some(crate::editor::discard_prompt().as_str())
    );
}

#[test]
fn editor_discard_requires_confirmation_for_selected_frame() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.set_beautify(BuiltinPreset::Soft.settings());
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    let ed = app.editor.as_ref().expect("first discard should only arm");
    assert!(ed.discard_armed());
    assert_eq!(
        ed.status_text(),
        Some(crate::editor::discard_prompt().as_str())
    );
}

#[test]
fn editor_discard_confirmation_resets_after_frame_change() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));
    assert!(app.editor.as_ref().unwrap().discard_armed());

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::BeautifyPreset(BuiltinPreset::Soft)),
    );
    assert!(!app.editor.as_ref().unwrap().discard_armed());

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    let ed = app.editor.as_ref().expect("discard should re-arm");
    assert!(ed.discard_armed());
}

#[test]
fn editor_discard_confirmation_resets_after_annotation_change() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));
    assert!(app.editor.as_ref().unwrap().discard_armed());

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Rectangle {
                rect: RectLike::new(30.0, 30.0, 12.0, 12.0),
                color: Rgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: Default::default(),
            }),
        )),
    );
    assert!(!app.editor.as_ref().unwrap().discard_armed());

    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));

    let ed = app.editor.as_ref().expect("discard should re-arm");
    assert!(ed.discard_armed());
    assert_eq!(ed.model.annotations().len(), 2);
}

#[test]
fn editor_text_commit_label_matches_add_or_edit_state() {
    assert_eq!(editor_text_commit_label(false), "Add");
    assert_eq!(editor_text_commit_label(true), "Update");
}

#[test]
fn editor_busy_ignores_export_reentry_without_committing_pending_text() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.busy = true;
    ed.pending_text = Some(crate::editor::PendingText {
        origin: PointLike::new(8.0, 9.0),
        content: "draft".into(),
        edit_index: None,
    });
    app.editor = Some(ed);

    let _ = update(&mut app, Message::Editor(EditorMsg::SaveRequested));
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyImageRequested));
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyTextRequested));

    let ed = app.editor.as_ref().unwrap();
    assert!(ed.busy);
    assert!(ed.pending_text.is_some());
    assert!(ed.model.annotations().is_empty());
}

#[test]
fn editor_busy_ignores_canvas_toolbar_and_zoom_mutations() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    ed.busy = true;
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Arrow,
        ))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::WidthBump(3.0)));
    let _ = update(&mut app, Message::Editor(EditorMsg::ZoomIn));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Rectangle {
                rect: RectLike::new(30.0, 30.0, 10.0, 10.0),
                color: Rgba::OPAQUE_BLACK,
                line_width: 4.0,
                style: Default::default(),
            }),
        )),
    );

    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.model.active_tool(),
        readshot_ui::editor::ToolState::Select
    );
    assert_eq!(ed.model.annotations().len(), 1);
    assert_eq!(ed.zoom, crate::editor::EditorZoom::Fit);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn editor_keyboard_zoom_is_display_scale_aware() {
    // ⌘+/⌘- must follow the same display-scale-aware ladder as the
    // toolbar zoom buttons; on a HiDPI (2.0) display the hardcoded
    // 1.0 path used to diverge from the buttons.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let ed = crate::editor::EditorSession::new_with_display_scale(solid(64, 64), 2.0);
    app.editor = Some(ed);

    // In Fit mode both start from the scale the stage rendered at.
    app.editor.as_ref().unwrap().render_scale.set(0.5);
    let _ = update(&mut app, Message::Editor(EditorMsg::ZoomIn));
    let after_in = app.editor.as_ref().unwrap().zoom;
    assert_eq!(
        after_in,
        crate::editor::EditorZoom::Fit.zoom_in_from_display_scale(0.5),
        "keyboard zoom-in should match the display-scale-aware button path"
    );
    assert_ne!(
        after_in,
        crate::editor::EditorZoom::Fit.zoom_in(),
        "keyboard zoom-in must not use the hardcoded 1.0 ladder on HiDPI"
    );

    let _ = update(&mut app, Message::Editor(EditorMsg::ZoomActual));
    let _ = update(&mut app, Message::Editor(EditorMsg::ZoomOut));
    let after_out = app.editor.as_ref().unwrap().zoom;
    let expected_out = crate::editor::EditorZoom::actual_size(2.0).zoom_out_from_display_scale(2.0);
    assert_eq!(
        after_out, expected_out,
        "keyboard zoom-out should match the display-scale-aware button path"
    );
}

#[test]
fn editor_key_events_ignore_non_editor_windows() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let editor_id = iced::window::Id::unique();
    let other_id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_id = Some(editor_id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    app.editor = Some(ed);

    let delete = iced::keyboard::Key::Named(iced::keyboard::key::Named::Delete);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::KeyPressed {
            window: other_id,
            key: delete.clone(),
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: true,
        }),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::KeyPressed {
            window: editor_id,
            key: delete,
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: true,
        }),
    );
    assert!(app.editor.as_ref().unwrap().model.annotations().is_empty());
}

#[test]
fn escape_cancels_a_live_canvas_drag_and_never_discards() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let editor_id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_id = Some(editor_id);
    // A committed annotation makes the editor "dirty" so the eventual
    // discard is the two-stage confirm (keeps the window open) rather
    // than an immediate clean close — lets us assert on it afterward.
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    // A shape drag is in flight (canvas published DragStarted).
    ed.canvas_drag_active = true;
    let seq_before = ed.drag_cancel_seq;
    app.editor = Some(ed);

    let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::KeyPressed {
            window: editor_id,
            key: escape.clone(),
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: true,
        }),
    );

    // Escape cancelled the drag (seq bumped, flag cleared) and did NOT
    // discard the editor.
    let ed = app.editor.as_ref().expect("editor still open after cancel");
    assert!(!ed.canvas_drag_active);
    assert_eq!(ed.drag_cancel_seq, seq_before.wrapping_add(1));

    // A second Escape with no drag in progress is still only a cancel:
    // Esc never discards the editor (⌘W does).
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::KeyPressed {
            window: editor_id,
            key: escape,
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: true,
        }),
    );
    let ed = app.editor.as_ref().expect("Esc never closes the editor");
    assert!(!ed.discard_armed(), "Esc never arms Discard");
}

#[test]
fn editor_advertised_copy_shortcuts_map_to_copy_messages() {
    let cmd = command_modifiers();
    let copy_image = editor_key_message(iced::keyboard::Key::Character("c".into()), cmd, true);

    assert!(matches!(
        copy_image,
        Some(Message::Editor(EditorMsg::CopyImageRequested))
    ));

    let mut cmd_shift = cmd;
    cmd_shift.insert(iced::keyboard::Modifiers::SHIFT);
    let copy_text = editor_key_message(iced::keyboard::Key::Character("c".into()), cmd_shift, true);

    assert!(matches!(
        copy_text,
        Some(Message::Editor(EditorMsg::CopyTextRequested))
    ));
}

#[test]
fn editor_copy_shortcuts_do_not_hijack_captured_text_input_events() {
    let cmd = command_modifiers();
    let copy_image = editor_key_message(iced::keyboard::Key::Character("c".into()), cmd, false);

    assert!(copy_image.is_none());
}

#[test]
fn editor_advertised_pin_shortcut_maps_to_pin_message() {
    let pin = editor_key_message(
        iced::keyboard::Key::Character("p".into()),
        command_modifiers(),
        true,
    );

    assert!(matches!(
        pin,
        Some(Message::Editor(EditorMsg::PinRequested))
    ));
}

#[test]
fn editor_advertised_tool_shortcuts_map_to_their_tools() {
    for tool in readshot_ui::editor::toolbar::TOOL_ORDER {
        let (_label, key) = tool_label_and_key(tool);
        assert_eq!(tool_for_key(key), Some(tool), "shortcut drift for {tool:?}");
        let message = editor_key_message(
            iced::keyboard::Key::Character(key.to_string().into()),
            iced::keyboard::Modifiers::empty(),
            true,
        );
        assert!(matches!(
            message,
            Some(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::SelectTool(mapped)
            ))) if mapped == tool
        ));
    }
}

#[test]
fn e_selects_the_stamp_tool() {
    let msg = editor_key_message(
        iced::keyboard::Key::Character("e".into()),
        iced::keyboard::Modifiers::empty(),
        true,
    );
    assert!(matches!(
        msg,
        Some(Message::Editor(EditorMsg::Toolbar(
            readshot_ui::ToolbarMessage::SelectTool(readshot_ui::editor::ToolState::Stamp)
        )))
    ));
}

#[test]
fn selecting_a_new_tool_marks_it_seen() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Spotlight,
        ))),
    );
    assert_eq!(
        app.preferences.editor_tools_seen,
        vec!["spotlight".to_string()]
    );
}

#[test]
fn selecting_an_old_tool_records_nothing() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    for tool in [
        readshot_ui::editor::ToolState::Rectangle,
        readshot_ui::editor::ToolState::Arrow,
        readshot_ui::editor::ToolState::Text,
    ] {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
                tool,
            ))),
        );
    }
    assert!(app.preferences.editor_tools_seen.is_empty());
}

#[test]
fn spotlight_drag_commits_an_undoable_spotlight() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Spotlight {
                rect: RectLike::new(8.0, 8.0, 20.0, 20.0),
                shape: readshot_core::SpotlightShape::Rectangle,
                dim: 0.55,
            }),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.model.can_undo());
    assert_eq!(ed.model.selected_kind_label(), Some("Spotlight"));
}

#[test]
fn editor_existing_command_shortcuts_stay_mapped() {
    let cmd = command_modifiers();
    let undo = editor_key_message(iced::keyboard::Key::Character("z".into()), cmd, false);
    assert!(matches!(
        undo,
        Some(Message::Editor(EditorMsg::Toolbar(
            readshot_ui::ToolbarMessage::Undo
        )))
    ));

    let mut cmd_shift = cmd;
    cmd_shift.insert(iced::keyboard::Modifiers::SHIFT);
    let redo = editor_key_message(iced::keyboard::Key::Character("z".into()), cmd_shift, false);
    assert!(matches!(
        redo,
        Some(Message::Editor(EditorMsg::Toolbar(
            readshot_ui::ToolbarMessage::Redo
        )))
    ));

    let save = editor_key_message(iced::keyboard::Key::Character("s".into()), cmd, false);
    assert!(matches!(
        save,
        Some(Message::Editor(EditorMsg::SaveRequested))
    ));

    let mut cmd_shift = cmd;
    cmd_shift.insert(iced::keyboard::Modifiers::SHIFT);
    let zoom_in = editor_key_message(iced::keyboard::Key::Character("=".into()), cmd_shift, false);
    assert!(matches!(zoom_in, Some(Message::Editor(EditorMsg::ZoomIn))));

    let close = editor_key_message(iced::keyboard::Key::Character("w".into()), cmd, false);
    assert!(matches!(
        close,
        Some(Message::Editor(EditorMsg::DiscardRequested))
    ));
}

#[test]
fn editor_arrow_keys_nudge_selection_with_shift_coarsening() {
    use iced::keyboard::{key::Named, Key, Modifiers};

    fn nudge(key: Named, mods: Modifiers, status_ignored: bool) -> (f32, f32) {
        match editor_key_message(Key::Named(key), mods, status_ignored) {
            Some(Message::Editor(EditorMsg::NudgeSelected(dx, dy))) => (dx, dy),
            other => panic!("expected nudge, got {other:?}"),
        }
    }

    let none = Modifiers::empty();
    assert_eq!(nudge(Named::ArrowLeft, none, true), (-1.0, 0.0));
    assert_eq!(nudge(Named::ArrowRight, none, true), (1.0, 0.0));
    assert_eq!(nudge(Named::ArrowUp, none, true), (0.0, -1.0));
    assert_eq!(nudge(Named::ArrowDown, none, true), (0.0, 1.0));
    // Shift coarsens the step to 10px for fast positioning.
    assert_eq!(nudge(Named::ArrowDown, Modifiers::SHIFT, true), (0.0, 10.0));
    // While typing in a text field (status not ignored) arrows do nothing.
    assert!(editor_key_message(Key::Named(Named::ArrowLeft), none, false).is_none());
}

#[test]
fn editor_escape_keeps_canvas_cancellation_when_already_handled() {
    // The annotation canvas captures every Esc, so this is the path a
    // real Esc press takes: one EditorTextCancel per press.
    let cancel = editor_key_message(
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        iced::keyboard::Modifiers::empty(),
        false,
    );

    assert!(matches!(
        cancel,
        Some(Message::Editor(EditorMsg::TextCancel))
    ));
    // Nor is an Esc the canvas didn't capture ever a Discard.
    let uncaptured = editor_key_message(
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
        iced::keyboard::Modifiers::empty(),
        true,
    );
    assert!(matches!(
        uncaptured,
        Some(Message::Editor(EditorMsg::TextCancel))
    ));
}

/// The per-tool and per-selection how-to lines the options bar displaced
/// are reachable again: a notice when a tool is chosen, a guide while a
/// clicked selection lasts. Neither covers an error.
#[test]
fn tool_and_selection_hints_reach_the_status_line() {
    use readshot_ui::editor::ToolState as T;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.model.select_index(None);
    app.editor = Some(ed);

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            T::Magnifier,
        ))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.status_text(),
        Some(editor_compact_hint(T::Magnifier).as_str())
    );
    assert_eq!(ed.status_tone(), Some(crate::editor::StatusTone::Info));

    // An error stays put.
    app.editor.as_mut().unwrap().set_status(
        crate::editor::StatusTone::Error,
        "Couldn’t save the image. Try again.",
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
            T::Crop,
        ))),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().status_text(),
        Some("Couldn’t save the image. Try again.")
    );

    // A clicked selection explains itself while it lasts.
    let ed = app.editor.as_mut().unwrap();
    ed.status = None;
    ed.model.set_tool(T::Select);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(5.0, 5.0)),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.status_text(),
        Some(editor_selected_hint("Rectangle", false).as_str())
    );
    assert_eq!(ed.key_guide_live(), Some(true));
}

#[test]
fn an_unanswered_focus_query_still_moves_the_selection() {
    assert!(!tab_field_focused(&[]));
    assert!(tab_field_focused(&[true]));
    assert!(!tab_field_focused(&[false]));
}

/// Undoing a numbered pin gives its number back: the next pin follows
/// the highest one still on the document, and Redo moves it on again.
#[test]
fn undoing_a_numbered_pin_rewinds_the_counter() {
    use readshot_ui::{CanvasMessage, ToolbarMessage};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    for n in 1..=3 {
        let number = app.editor.as_ref().unwrap().next_pin_number;
        assert_eq!(number, n);
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(
                Annotation::NumberedPin {
                    origin: PointLike::new(5.0 * n as f32, 5.0),
                    number,
                    color: Rgba::OPAQUE_BLACK,
                },
            ))),
        );
    }
    assert_eq!(app.editor.as_ref().unwrap().next_pin_number, 4);
    let undo = || Message::Editor(EditorMsg::Toolbar(ToolbarMessage::Undo));
    let _ = update(&mut app, undo());
    assert_eq!(app.editor.as_ref().unwrap().next_pin_number, 3);
    let _ = update(&mut app, undo());
    assert_eq!(app.editor.as_ref().unwrap().next_pin_number, 2);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(ToolbarMessage::Redo)),
    );
    assert_eq!(app.editor.as_ref().unwrap().next_pin_number, 3);
}

/// B#6: the Discard prompt names the key and the way back, never only a
/// button (most arms come from ⌘W).
#[test]
fn the_discard_prompt_names_the_key_and_the_way_back() {
    let want = if cfg!(target_os = "macos") {
        "Unsaved edits. Press ⌘W again to discard them, or keep editing."
    } else {
        "Unsaved edits. Press Ctrl+W again to discard them, or keep editing."
    };
    assert_eq!(crate::editor::discard_prompt(), want);
}
