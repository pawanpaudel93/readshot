//! Off-thread composes and renders (D1) and pending steps.

use super::*;

#[test]
fn a_large_resize_composes_off_thread_and_lands_as_one_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    ed.model.commit_annotations(rect_at(5.0));
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let shown = handle_id(app.editor.as_ref().unwrap());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeOpen)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeWidthChanged("80".into()))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeApply)),
    );
    let ed = app.editor.as_ref().unwrap();
    // Nothing changes until the result lands: the previous image stays.
    assert!(ed.compose_job.is_some());
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth);
    assert_eq!(handle_id(ed), shown);
    assert_eq!(ed.popover, None);
    let landed = finish_compose(&app);
    let _ = update(&mut app, landed);
    let ed = app.editor.as_mut().unwrap();
    assert!(ed.compose_job.is_none());
    assert_eq!(ed.image_size(), (80, 40));
    assert_eq!(ed.model.undo_depth(), depth + 1, "one undo step");
    assert_ne!(handle_id(ed), shown);
    assert!(ed
        .status_text()
        .unwrap()
        .starts_with("Resized to 80\u{a0}×\u{a0}40"));
    // The same document and pixels as the synchronous path.
    let mut sync = crate::editor::EditorSession::new(solid(40, 20));
    sync.model.commit_annotations(rect_at(5.0));
    assert!(sync.model.scale_by(2.0, 2.0).unwrap());
    assert_eq!(ed.model.annotations(), sync.model.annotations());
    assert_eq!(
        ed.model.geometry().transform,
        sync.model.geometry().transform
    );
    assert!(ed.model.flatten() == sync.model.flatten(), "flattened");
    assert!(*ed.model.base() == *sync.model.base(), "base");
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    // A large undo lands composed too.
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth);
}

#[test]
fn a_pending_resize_is_applied_before_the_next_edit_never_dropped() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    app.editor
        .as_mut()
        .unwrap()
        .model
        .commit_annotations(rect_at(5.0));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    // Another edit before the result lands: the resize is applied first.
    let ed = app.editor.as_mut().unwrap();
    ed.model.select_at(PointLike::new(6.0, 3.0));
    assert!(ed.model.selected_annotation().is_some());
    let _ = update(&mut app, Message::Editor(EditorMsg::DeleteSelected));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40), "resized");
    assert!(ed.model.annotations().is_empty(), "then deleted");
    assert_eq!(ed.model.undo_depth(), depth + 2);
    assert!(
        ed.compose_job.as_ref().is_none_or(|j| !j.kind.is_step()),
        "only the delete’s re-render may run"
    );
    // Its late result changes nothing.
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert_eq!(ed.model.undo_depth(), depth + 2);
}

#[test]
fn a_rotate_after_a_pending_resize_queues_behind_it() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    // ⌘] before the result lands: queued behind the resize, nothing
    // composed on the UI thread.
    let before = super::super::compose_jobs::composes_on_this_thread();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    assert_eq!(
        super::super::compose_jobs::composes_on_this_thread(),
        before
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20), "nothing applied yet");
    assert_eq!(ed.model.undo_depth(), depth);
    assert_eq!(ed.compose_job.as_ref().map(|j| j.steps.len()), Some(2));
    // The resize lands; the rotate composes next.
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40), "resized");
    assert_eq!(ed.model.undo_depth(), depth + 1);
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 80), "resized, then rotated");
    assert_eq!(ed.model.undo_depth(), depth + 2);
}

#[test]
fn a_canvas_press_that_lands_a_pending_resize_is_mapped_into_the_new_frame() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    // A redaction at x 5–11; after a 2× resize it sits at x 10–22.
    ed.model.commit_annotation(Annotation::Pixelate {
        rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
        block_size: 4.0,
    });
    ed.model.clear_selection();
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    // A Select press on the redaction in the image shown (x 8): it
    // lands the resize and selects what is under it in the new frame
    // (x 16). Before, the press was consumed and selected nothing.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(8.0, 4.0)),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40), "the resize landed");
    assert_eq!(ed.model.undo_depth(), depth + 1, "resize only");
    assert_eq!(ed.model.selected_annotation(), Some(0), "selected");
    assert!(ed.move_drag.is_some(), "a move begins");
    // The drag the canvas sent before the next view is in the old
    // frame too: +2 there is +4 here, the redaction moves in one frame.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(10.0, 4.0)),
        )),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectReleased,
        )),
    );
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 2, "resize, then move");
    assert_eq!(
        ed.model.annotations(),
        [Annotation::Pixelate {
            rect: RectLike::new(14.0, 4.0, 12.0, 8.0),
            block_size: 8.0,
        }]
    );
}

#[test]
fn a_text_click_that_lands_a_pending_resize_opens_where_it_was_aimed() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    apply_resize_width(&mut app, "80");
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(readshot_ui::CanvasMessage::RequestText(
            PointLike::new(8.0, 4.0),
        ))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert_eq!(
        ed.pending_text.as_ref().map(|t| t.origin),
        Some(PointLike::new(16.0, 8.0))
    );
}

#[test]
fn a_press_that_lands_a_pending_combine_is_still_dropped() {
    // An added part is not an affine of the frame shown: the press
    // can't be mapped and is dropped, as before.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    ed.model.commit_annotation(Annotation::Pixelate {
        rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
        block_size: 4.0,
    });
    ed.model.clear_selection();
    app.editor = Some(ed);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::PartLoaded {
            session,
            result: Ok(Some(Arc::new(ramp(20, 10)))),
        })),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 1);
    for msg in [
        readshot_ui::CanvasMessage::SelectPressed(PointLike::new(8.0, 4.0)),
        readshot_ui::CanvasMessage::SelectDragged(PointLike::new(20.0, 4.0)),
        readshot_ui::CanvasMessage::SelectReleased,
    ] {
        let _ = update(&mut app, Message::Editor(EditorMsg::Canvas(msg)));
    }
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 2, "the combine landed");
    assert_eq!(ed.model.selected_annotation(), None, "the press dropped");
    assert!(ed.move_drag.is_none());
    assert!(ed.press_remap.is_none());
    assert!(matches!(
        ed.model.annotations(),
        [Annotation::Pixelate { rect, .. }] if *rect == RectLike::new(5.0, 2.0, 6.0, 4.0)
    ));
}

#[test]
fn a_drawing_press_that_lands_a_pending_resize_starts_no_drag() {
    use readshot_ui::CanvasMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let seq = app.editor.as_ref().unwrap().drag_cancel_seq;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::DragStarted)),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert!(!ed.canvas_drag_active, "no drag started");
    assert_ne!(ed.drag_cancel_seq, seq, "the canvas drops its drag");
    // …which it reports on its next event.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::Cancelled)),
    );
    // A click tool's commit (a pin) carries its point: it is mapped
    // into the new frame (the old image's (10, 10) is the new (5, 5)).
    apply_resize_width(&mut app, "40");
    let pin = |x, y| Annotation::NumberedPin {
        origin: PointLike::new(x, y),
        number: 1,
        color: Rgba::OPAQUE_BLACK,
    };
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(pin(
            10.0, 10.0,
        )))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.annotations(), [pin(5.0, 5.0)]);
    assert_eq!(ed.model.undo_depth(), depth + 3, "two resizes, the pin");
    // One off the shown image can't be mapped: dropped.
    apply_resize_width(&mut app, "80");
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(pin(
            60.0, 10.0,
        )))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert_eq!(ed.model.annotations().len(), 1, "dropped");
    // Pressed again with nothing pending: drawn as it is.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(pin(
            60.0, 10.0,
        )))),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 2);
}

#[test]
fn a_redaction_drag_whose_press_landed_a_pending_resize_commits_nothing() {
    use readshot_ui::CanvasMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    ed.model.set_tool(readshot_ui::editor::ToolState::Pixelate);
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    // A pixelate drag over x 5–11 of the old image: its press lands
    // the resize; its commit still arrives (old-frame points).
    let old_frame = Annotation::Pixelate {
        rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
        block_size: 4.0,
    };
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::DragStarted)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(
            old_frame.clone(),
        ))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert!(ed.model.annotations().is_empty(), "no old-frame redaction");
    assert_eq!(ed.model.undo_depth(), depth + 1, "the resize only");
    assert!(!ed.canvas_drag_active);
    // The next drag, begun on the new frame, commits.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::DragStarted)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(
            old_frame.clone(),
        ))),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.annotations(),
        [old_frame]
    );
}

#[test]
fn a_dropped_drag_stays_dropped_when_its_window_loses_focus() {
    use readshot_ui::CanvasMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    ed.model.set_tool(readshot_ui::editor::ToolState::Pixelate);
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let a = iced::window::Id::unique();
    let redaction = Annotation::Pixelate {
        rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
        block_size: 4.0,
    };
    // The press lands the pending resize; its window then loses focus
    // before the release, and the old-frame commit arrives late.
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::EditorPointerButton(true, Some(a))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::DragStarted)),
    );
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::EditorPointerWindowUnfocused(a)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(
            redaction.clone(),
        ))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert!(
        ed.model.annotations().is_empty(),
        "the late commit is refused"
    );
    assert_eq!(ed.model.undo_depth(), depth + 1, "the resize only");
    // The next real drag commits.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::DragStarted)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(
            redaction.clone(),
        ))),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.annotations(),
        [redaction]
    );
}

#[test]
fn a_drag_whose_release_never_came_refuses_no_later_commit() {
    use readshot_ui::CanvasMessage;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
    let a = iced::window::Id::unique();
    let pin = Annotation::NumberedPin {
        origin: PointLike::new(5.0, 5.0),
        number: 1,
        color: Rgba::OPAQUE_BLACK,
    };
    let commit = |app: &mut App, a: Annotation| {
        let _ = update(
            app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::CommitAnnotation(a))),
        );
    };
    // A drag begins; its window closes before the release reaches us.
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::EditorPointerButton(true, Some(a))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::DragStarted)),
    );
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(a)));
    let ed = app.editor.as_mut().unwrap();
    assert!(ed.drag_started_in.is_none(), "its frame is forgotten");
    // The frame changes later (a resize).
    ed.canvas_drag_active = false;
    assert!(ed.model.scale_by(2.0, 2.0).unwrap());
    ed.refresh_image();
    commit(&mut app, pin.clone());
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
    // A pin never answers for a drag's frame, even one left behind.
    let ed = app.editor.as_mut().unwrap();
    ed.drag_started_in = Some(crate::editor::CanvasDragFrame::Live(Arc::new(
        readshot_core::document::DocGeometry::original(Arc::new(solid(1, 1))),
    )));
    ed.canvas_drag_active = true;
    commit(&mut app, pin);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.annotations().len(), 2);
    assert!(ed.drag_started_in.is_none());
}

#[test]
fn each_resize_applies_in_order_and_foreign_results_are_ignored() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let first = finish_compose(&app);
    // A second Apply settles the first: both are kept, in order.
    apply_resize_width(&mut app, "60");
    let second = finish_compose(&app);
    let _ = update(&mut app, first);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40), "the first, settled");
    let foreign = compose_result(ed.session_id + 1, ed.compose_job.as_ref().unwrap());
    let _ = update(&mut app, foreign);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
    let _ = update(&mut app, second);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (60, 30));
    assert_eq!(ed.model.undo_depth(), depth + 2);
}

#[test]
fn cmd_z_right_after_apply_undoes_the_resize_itself() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    ed.model.commit_annotations(rect_at(5.0));
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    let _ = update(&mut app, undo_msg());
    let _ = update(&mut app, late);
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth, "the earlier step is kept");
    assert_eq!(ed.model.annotations().len(), 1);
    assert!(ed.model.can_redo());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Redo)),
    );
    finish_all(&mut app);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
}

#[test]
fn a_busy_action_after_apply_keeps_the_resize() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(two_part_editor()));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    // Add Image from File sets `busy` while its picker is open.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::AddImageFromFile)),
    );
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert_eq!(ed.model.undo_depth(), depth + 1);
    // Copy Text too.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyTextRequested));
    let _ = update(&mut app, late);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
}

#[test]
fn a_text_draft_after_apply_is_in_the_new_frame_and_save_keeps_both() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    let request = Message::Editor(EditorMsg::Canvas(readshot_ui::CanvasMessage::RequestText(
        PointLike::new(50.0, 30.0),
    )));
    // The click that lands the resize was aimed at the old image: it
    // opens no draft.
    let _ = update(&mut app, request.clone());
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
    assert!(app.editor.as_ref().unwrap().pending_text.is_none());
    // Clicked again: the draft opens on the resized document.
    let _ = update(&mut app, request);
    assert!(app.editor.as_ref().unwrap().pending_text.is_some());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TextChanged("Hi".into())),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::SaveRequested));
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40), "saved with the resize");
    assert_eq!(ed.model.undo_depth(), depth + 2);
    assert!(matches!(
        ed.model.annotations().last(),
        Some(Annotation::Text { origin, .. }) if *origin == PointLike::new(50.0, 30.0)
    ));
}

#[test]
fn a_crop_draft_after_apply_is_in_the_new_frame() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    ed.model.set_tool(readshot_ui::editor::ToolState::Crop);
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    let rect = RectLike::new(10.0, 10.0, 50.0, 20.0);
    let drag = |app: &mut App| {
        for m in [
            readshot_ui::CanvasMessage::DragStarted,
            readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Crop { rect }),
        ] {
            let _ = update(app, Message::Editor(EditorMsg::Canvas(m)));
        }
    };
    // The press lands the resize: that drag was aimed at the old
    // image, so its commit is refused.
    drag(&mut app);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
    assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
    // Dragged again, on the resized document.
    drag(&mut app);
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.crop_draft, Some(rect));
    assert_eq!(ed.image_size(), (80, 40));
    assert_eq!(ed.model.undo_depth(), depth + 1);
    let _ = update(&mut app, Message::Editor(EditorMsg::CropApply));
    assert_eq!(
        app.editor.as_ref().unwrap().effective_image_size(),
        (50, 20)
    );
}

#[test]
fn a_select_drag_around_a_landing_resize_keeps_redactions_in_the_new_frame() {
    let (_dir, store, mut app) = redacted_history_editor();
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let landed = finish_compose(&app);
    // Press on the blur in the frame shown (10, 10): it lands the
    // resize and is mapped into the new frame (20, 20), still on the
    // blur. The late result changes nothing; a view is built, and the
    // drag (new-frame points) moves the blur within the new frame.
    let canvas = |m| Message::Editor(EditorMsg::Canvas(m));
    let _ = update(
        &mut app,
        canvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
            10.0, 10.0,
        ))),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(0)
    );
    let _ = update(&mut app, landed);
    built_a_view(&app);
    let _ = update(
        &mut app,
        canvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
            24.0, 20.0,
        ))),
    );
    let _ = update(&mut app, canvas(readshot_ui::CanvasMessage::SelectReleased));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert_eq!(ed.model.undo_depth(), depth + 2, "resize, then move");
    // The blur (4, 4, 10, 10) is (8, 8, 20, 20) resized, then moved.
    let new_frame = RectLike::new(12.0, 8.0, 20.0, 20.0);
    assert!(matches!(
        ed.model.annotations(),
        [Annotation::Blur { rect, .. }] if *rect == new_frame
    ));
    // History holds the resized image with the new-frame redaction, as
    // drawn: widened over the resize's fringe.
    let drawn = ed.model.geometry().redaction_cover(new_frame);
    assert!(drawn.x < new_frame.x && drawn.width > new_frame.width);
    let stored = compose_stored_record(&store, &app);
    assert_eq!((stored.width_px, stored.height_px), (80, 40));
    assert!(stored.redacted);
    assert!(matches!(
        stored.redactions.as_slice(),
        [Annotation::Blur { rect, .. }] if *rect == drawn
    ));
}

#[test]
fn a_released_gap_drag_is_synced_to_history_when_the_window_closes() {
    let (_dir, store, mut app) = redacted_history_editor();
    let coord = app.coordinator.clone();
    let id = iced::window::Id::unique();
    {
        let ed = app.editor.as_mut().unwrap();
        ed.window_id = Some(id);
        ed.model
            .add_part(solid(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        ed.refresh_image();
        write_editor_history(ed, &coord);
    }
    let before = compose_stored_record(&store, &app).geometry_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(10))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    assert!(app.editor.as_ref().unwrap().gap_commit_on_land);
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert!(app.editor.is_none());
    // The released value composes off the UI thread; its worker lands.
    let retiring = &app.retiring_editors[0];
    let landed = compose_result(
        retiring.session_id,
        retiring
            .compose_job
            .as_ref()
            .expect("the gap step is composing"),
    );
    let _ = update(&mut app, landed);
    assert!(app.retiring_editors.is_empty());
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().into_iter().next().unwrap();
    assert_ne!(stored.geometry_id, before, "the gap step reached History");
    assert_eq!((stored.width_px, stored.height_px), (40, 40));
}

#[test]
fn a_released_gap_drag_is_synced_to_history_before_a_pin() {
    let (_dir, store, mut app) = redacted_history_editor();
    let coord = app.coordinator.clone();
    {
        let ed = app.editor.as_mut().unwrap();
        ed.model
            .add_part(solid(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        ed.refresh_image();
        write_editor_history(ed, &coord);
    }
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(10))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    let _ = update(&mut app, Message::Editor(EditorMsg::PinRequested));
    assert!(app.editor.is_none());
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!((stored.width_px, stored.height_px), (40, 40));
}

#[test]
fn a_select_drag_after_a_gap_release_waits_for_a_press_on_the_settled_frame() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(two_part_editor());
    // In the lower part: it moves down with the gap.
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(2.0, 12.0, 6.0, 6.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.model.clear_selection();
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(10))),
    );
    let late = finish_compose(&app);
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    // The first press lands the gap. It was aimed at the frame shown
    // before (the rectangle at y 14), so it and the drag the canvas
    // sent with it (no view built since) are mapped with the lower
    // part, 10 px down: the rectangle moves 2 px right, in the settled
    // frame.
    select_drag(&mut app, (4.0, 14.0), (6.0, 14.0));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.geometry().gap, 10);
    assert_eq!(ed.model.undo_depth(), depth + 2, "gap, then move");
    assert!(matches!(
        ed.model.annotations(),
        [Annotation::Rectangle { rect, .. }] if *rect == RectLike::new(4.0, 22.0, 6.0, 6.0)
    ));
    // Once a view is built, the canvas's points are in the settled
    // frame and pass as they are.
    built_a_view(&app);
    // (Unselected, so the press moves it rather than grab a handle.)
    app.editor.as_mut().unwrap().model.clear_selection();
    select_drag(&mut app, (4.0, 24.0), (6.0, 24.0));
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 3, "and moved again");
    assert!(matches!(
        ed.model.annotations(),
        [Annotation::Rectangle { rect, .. }] if *rect == RectLike::new(6.0, 22.0, 6.0, 6.0)
    ));
}

#[test]
fn a_same_batch_drag_over_another_part_moves_with_the_press_map() {
    // A gap step pending on two parts; the press (lower part) lands it
    // and is mapped 10 px down. The drag point the canvas sent with it
    // is over the upper part, which did not move: it must still go
    // through the press's map, so the move is the 10 px up the user
    // dragged, not 20.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(two_part_editor());
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(2.0, 12.0, 6.0, 6.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    ed.model.clear_selection();
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(10))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    select_drag(&mut app, (4.0, 14.0), (4.0, 4.0));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.geometry().gap, 10);
    assert!(
        matches!(
            ed.model.annotations(),
            [Annotation::Rectangle { rect, .. }] if *rect == RectLike::new(2.0, 12.0, 6.0, 6.0)
        ),
        "{:?}",
        ed.model.annotations()
    );
}

#[test]
fn a_press_after_a_same_batch_frame_change_is_mapped_or_dropped() {
    use readshot_ui::CanvasMessage;
    let outline = Annotation::Rectangle {
        rect: RectLike::new(2.0, 2.0, 10.0, 6.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    };
    let fresh = || {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(40, 20));
        ed.model.commit_annotation(outline.clone());
        ed.model.clear_selection();
        ed.model.set_tool(readshot_ui::editor::ToolState::Select);
        app.editor = Some(ed);
        built_a_view(&app);
        app
    };
    let press = |app: &mut App| {
        // On the rectangle's left edge in the frame the view showed.
        let _ = update(
            app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectPressed(
                PointLike::new(2.0, 5.0),
            ))),
        );
    };
    // A rotate handled earlier in the same batch (small document: no
    // pending step) turned the frame: the press is mapped with it and
    // selects the rectangle. Before, it passed unmapped and missed.
    let mut app = fresh();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    press(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (20, 40));
    assert_eq!(ed.model.selected_annotation(), Some(0));
    // An image added in the same batch is not an affine of the frame
    // shown: the press is dropped (before, it passed unmapped).
    let mut app = fresh();
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::PartLoaded {
            session,
            result: Ok(Some(Arc::new(solid(20, 10)))),
        })),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 2);
    press(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.selected_annotation(), None);
    assert!(ed.move_drag.is_none());
    // A drawing drag begun there is refused too, and its commit with it.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(CanvasMessage::DragStarted)),
    );
    assert!(matches!(
        app.editor.as_ref().unwrap().drag_started_in,
        Some(crate::editor::CanvasDragFrame::Dropped)
    ));
    // Once a view shows the new frame, a press works as it is.
    built_a_view(&app);
    press(&mut app);
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(0)
    );
}

#[test]
fn a_remapped_press_hit_tests_handles_at_the_scale_it_was_made_at() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(2.0, 2.0, 10.0, 6.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(2.0, 5.0)), Some(0));
    ed.model.set_tool(readshot_ui::editor::ToolState::Select);
    // Shown at 1 screen px per document px.
    ed.render_scale.set(1.0);
    app.editor = Some(ed);
    apply_resize_width(&mut app, "80");
    // 9 screen px below the bottom-right handle (12, 8): inside the
    // 14 px grab radius on the image shown. In the resized frame that
    // is 18 document px, which is still 9 screen px as pressed.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(12.0, 17.0)),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40));
    assert!(
        matches!(
            ed.move_drag.as_ref().map(|d| d.kind),
            Some(crate::editor::MoveDragKind::Resize(
                readshot_ui::editor::ResizeHandle::SouthEast
            ))
        ),
        "the handle is grabbed (before: missed at the old scale)"
    );
}

#[test]
fn a_gap_tick_after_apply_settles_the_resize_first() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(two_part_editor()));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    apply_resize_width(&mut app, "80");
    let late = finish_compose(&app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(10))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (80, 40), "resized before the drag");
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(matches!(
        ed.compose_job.as_ref().map(|j| j.kind),
        Some(crate::editor::ComposeKind::GapPreview(10))
    ));
    let _ = update(&mut app, late);
    let gap = finish_compose(&app);
    let _ = update(&mut app, gap);
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 2);
    assert_eq!(ed.model.geometry().gap, 10);
}

#[test]
fn a_large_alignment_lands_as_one_undo_step() {
    use readshot_core::document::Align;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(two_part_editor()));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::Align(Align::End))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.geometry().align, Align::Start);
    assert_eq!(ed.model.undo_depth(), depth);
    let landed = finish_compose(&app);
    let _ = update(&mut app, landed);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.geometry().layout().placements[1].x, 20);
    assert_eq!(ed.model.undo_depth(), depth + 1);
}

#[test]
fn large_gap_previews_coalesce_to_the_newest_value_and_commit_once() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, session) = gap_drag_composing(&mut app);
    let first = finish_compose(&app);
    // Ticks while it composes: throttled, then queued (one compose at
    // a time); the newest wins.
    for g in [14, 18, 22] {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(g))),
        );
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Combine(CombineMsg::GapFlush { session })),
        );
    }
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.gap_queued, Some(22));
    assert_eq!(ed.image_size(), (40, 20), "still the previous image");
    let _ = update(&mut app, first);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 30), "gap 10 shown");
    assert_eq!(ed.model.undo_depth(), depth, "a preview, no step");
    assert!(matches!(
        ed.compose_job.as_ref().map(|j| j.kind),
        Some(crate::editor::ComposeKind::GapPreview(22))
    ));
    assert_eq!(ed.gap_queued, None);
    // Released while 22 composes: the commit waits for it.
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.gap_commit_on_land);
    assert_eq!(ed.model.undo_depth(), depth);
    let last = finish_compose(&app);
    let _ = update(&mut app, last.clone());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 42));
    assert_eq!(ed.model.undo_depth(), depth + 1, "one step for the drag");
    assert!(ed.snapshot_drag_baseline.is_none());
    // A duplicate delivery changes nothing.
    let _ = update(&mut app, last);
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    finish_all(&mut app);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 20));
}

#[test]
fn a_cancelled_gap_drag_drops_its_running_compose() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, _) = gap_drag_composing(&mut app);
    let landed = finish_compose(&app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    let _ = update(&mut app, landed);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth);
    assert!(ed.compose_job.is_none());
}

#[test]
fn a_released_gap_drag_is_kept_when_another_edit_comes_first() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, _) = gap_drag_composing(&mut app);
    let late = finish_compose(&app);
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    assert!(app.editor.as_ref().unwrap().gap_commit_on_land);
    // Rotate before the compose lands: the released gap is kept as its
    // own step first (queued, its running compose reused), never
    // cancelled; the rotate queues behind it.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let _ = update(&mut app, late);
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 2);
    assert_eq!(ed.model.geometry().gap, 10);
    assert_eq!(ed.image_size(), (30, 40));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    finish_all(&mut app);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 30));
    // Undo right after a release (before landing) undoes the gap step.
    let (depth, _) = gap_drag_composing(&mut app);
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth);
    assert!(ed.model.can_redo(), "the gap step is on the redo stack");
    assert_eq!(ed.image_size(), (40, 20));
}

#[test]
fn an_output_mid_resize_applies_the_resize_first() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
        40, 20,
    ))));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeOpen)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeWidthChanged("80".into()))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeApply)),
    );
    let late = finish_compose(&app);
    let coord = app.coordinator.clone();
    let ed = app.editor.as_mut().unwrap();
    settle_editor_for_output(ed, &coord);
    assert_eq!(ed.image_size(), (80, 40), "the output includes the resize");
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(ed.compose_job.is_none());
    // Its late result is ignored: still one step.
    let _ = update(&mut app, late);
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
}

#[test]
fn an_output_mid_compose_keeps_the_newest_gap_now() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, session) = gap_drag_composing(&mut app);
    let late = finish_compose(&app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(16))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapFlush { session })),
    );
    let coord = app.coordinator.clone();
    let ed = app.editor.as_mut().unwrap();
    settle_editor_for_output(ed, &coord);
    assert_eq!(ed.image_size(), (40, 36), "the newest value, composed now");
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(ed.compose_job.is_none());
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 36));
    assert_eq!(ed.model.undo_depth(), depth + 1);
}

#[test]
fn gap_ticks_inside_the_throttle_are_coalesced_not_recomposed() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(two_part_editor());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(4))),
    );
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 24));
    // Within the throttle interval: kept, not composed.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(8))),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 24));
    assert_eq!(ed.layout_gap_pending, Some(8));
    // The trailing flush shows it.
    let session = ed.session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapFlush { session })),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 28));
    assert_eq!(ed.layout_gap_pending, None);
}

#[test]
fn undo_mid_gap_drag_cancels_only_the_drag_and_held_ticks_are_ignored() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(two_part_editor());
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(4))),
    );
    // The mouse is down: this is a held slider drag.
    let _ = update(
        &mut app,
        Message::Window(WindowMsg::EditorPointerButton(true, None)),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(9))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20), "back to the pre-drag gap");
    assert_eq!(ed.model.part_ids().len(), 2, "the part stays");
    assert_eq!(ed.model.undo_depth(), depth);
    assert_eq!(ed.layout_gap_pending, None);
    // The slider is still held: its ticks and the trailing flush do
    // nothing until release.
    let session = ed.session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapFlush { session })),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(12))),
    );
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 20));
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth);
    assert!(!ed.snapshot_drag_cancelled);
}

#[test]
fn copy_keeps_a_coalesced_gap_value_as_one_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(two_part_editor());
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(4))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(12))),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyImageRequested));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 32), "the slider’s value is output");
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(ed.snapshot_drag_baseline.is_none());
}

#[test]
fn a_redaction_drag_renders_at_most_once_per_tick_and_never_above_the_live_limit() {
    use crate::editor::ui_renders_on_this_thread as renders;
    use readshot_ui::CanvasMessage;
    for live in [true, false] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
        ed.model.commit_annotation(Annotation::Pixelate {
            rect: RectLike::new(2.0, 2.0, 10.0, 8.0),
            block_size: 4.0,
        });
        ed.model.set_tool(readshot_ui::editor::ToolState::Select);
        ed.refresh_image();
        if !live {
            // As if the 800 px document were over the 4 MP limit.
            ed.live_redaction_max_pixels = 800;
        }
        app.editor = Some(ed);
        let before = renders();
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectPressed(
                PointLike::new(5.0, 5.0),
            ))),
        );
        for x in [8.0, 11.0, 14.0, 17.0, 20.0] {
            let _ = update(
                &mut app,
                Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectDragged(
                    PointLike::new(x, 5.0),
                ))),
            );
        }
        let during = renders() - before;
        let ed = app.editor.as_ref().unwrap();
        if live {
            assert!(during <= 5, "one render per tick at most: {during}");
            assert!(ed.shows_present_render());
        } else {
            // One render of the document without it, then nothing per
            // tick: the canvas's opaque cover stands in.
            assert_eq!(during, 1, "{during}");
            assert!(!ed.shows_present_render());
        }
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectReleased)),
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.shows_present_render(), "the release renders it");
        assert!(renders() - before <= during + 1);
    }
}

#[test]
fn a_dragged_redaction_shows_its_real_effect_when_the_document_renders_here() {
    use readshot_ui::CanvasMessage;
    let pixelate = Annotation::Pixelate {
        rect: RectLike::new(2.0, 2.0, 10.0, 8.0),
        block_size: 4.0,
    };
    for large in [false, true] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
        ed.model.commit_annotation(pixelate.clone());
        ed.model.set_tool(readshot_ui::editor::ToolState::Select);
        ed.refresh_image();
        app.editor = Some(if large { async_compose(ed) } else { ed });
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectPressed(
                PointLike::new(5.0, 5.0),
            ))),
        );
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectDragged(
                PointLike::new(20.0, 9.0),
            ))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_ne!(ed.model.annotations()[0], pixelate, "it moved");
        if large {
            // Rendered off the UI thread: the canvas's opaque cover
            // stands in until then.
            assert!(!ed.shows_present_render());
            continue;
        }
        // Before, the image was the document without the redaction
        // and the canvas covered it with an opaque grey box.
        assert!(ed.shows_present_render());
        let expected = readshot_core::render(
            &ed.model.shared_base(),
            &ed.model.geometry().cover_redactions(ed.model.annotations()),
        );
        match &ed.image_handle {
            iced::widget::image::Handle::Rgba { pixels, .. } => {
                assert_eq!(&pixels[..], expected.as_raw().as_slice())
            }
            other => panic!("expected pixels, got {other:?}"),
        }
        // Released, it commits; Esc on another drag puts it back.
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectReleased)),
        );
        assert!(app.editor.as_ref().unwrap().shows_present_render());
    }
}

#[test]
fn a_large_rotate_composes_off_thread_and_lands_as_one_undo_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let shown = handle_id(app.editor.as_ref().unwrap());
    let before = composes(&app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    // Queued: nothing composed here, nothing changes until it lands.
    assert_eq!(composes(&app), before);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        job_kind(&app),
        Some(crate::editor::ComposeKind::Rotate { clockwise: true })
    );
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth);
    assert_eq!(handle_id(ed), shown);
    let landed = finish_compose(&app);
    let _ = update(&mut app, landed);
    let ed = app.editor.as_mut().unwrap();
    assert!(ed.compose_job.is_none());
    assert_eq!(ed.image_size(), (20, 40));
    assert_eq!(ed.model.undo_depth(), depth + 1, "one undo step");
    assert_ne!(handle_id(ed), shown);
    assert_eq!(
        ed.status_text(),
        Some(format!("Rotated right. {UNDO_HINT}").as_str())
    );
    let mut sync = sync_editor();
    assert!(sync.model.rotate(true).unwrap());
    assert_eq!(ed.model.annotations(), sync.model.annotations());
    assert!(*ed.model.base() == *sync.model.base(), "base");
    assert!(
        editor_output_image(ed).unwrap() == editor_output_image(&mut sync).unwrap(),
        "the output is the rotated document"
    );
}

#[test]
fn an_edit_before_a_large_rotate_lands_applies_it_first_and_drops_its_late_result() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let late = finish_compose(&app);
    // Delete is not chained: the rotate is applied first, then the
    // selected rectangle (now in the turned frame) is deleted.
    let _ = update(&mut app, Message::Editor(EditorMsg::DeleteSelected));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (20, 40), "rotated");
    assert!(ed.model.annotations().is_empty(), "then deleted");
    assert_eq!(ed.model.undo_depth(), depth + 2);
    assert!(!super::super::compose_jobs::steps_pending(ed));
    // The late result is stale: dropped.
    let _ = update(&mut app, late);
    finish_all(&mut app);
    let ed = app.editor.as_mut().unwrap();
    assert_eq!(ed.image_size(), (20, 40));
    assert_eq!(ed.model.undo_depth(), depth + 2);
    let mut sync = sync_editor();
    sync.model.rotate(true).unwrap();
    assert!(sync.model.delete_selected_annotation());
    assert!(editor_output_image(ed).unwrap() == editor_output_image(&mut sync).unwrap());
}

#[test]
fn rotate_flip_undo_and_redo_on_a_large_document_queue_and_land_in_order() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let before = composes(&app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    // ⌘[ ⌘] ⌘Z ⇧⌘Z arrive as keys while the rotate composes: each
    // queues behind it, none settles it.
    let window = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(window);
    let key = |c: &str, shift: bool| {
        let mut modifiers = command_modifiers();
        if shift {
            modifiers.insert(iced::keyboard::Modifiers::SHIFT);
        }
        Message::Editor(EditorMsg::KeyPressed {
            window,
            key: iced::keyboard::Key::Character(c.into()),
            modifiers,
            status_ignored: true,
        })
    };
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Flip(
            readshot_core::document::FlipAxis::Horizontal,
        ))),
    );
    let _ = update(&mut app, key("z", false));
    let _ = update(&mut app, key("z", true));
    let _ = update(&mut app, key("[", false));
    assert_eq!(composes(&app), before, "nothing composed on the UI thread");
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth, "nothing applied yet");
    assert_eq!(ed.compose_job.as_ref().unwrap().steps.len(), 5);
    assert_eq!(ed.compose_job.as_ref().unwrap().composed, 1);
    finish_all(&mut app);
    let ed = app.editor.as_mut().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 3, "rotate, flip, rotate");
    assert!(!ed.model.can_redo());
    let mut sync = sync_editor();
    sync.model.rotate(true).unwrap();
    sync.model
        .flip(readshot_core::document::FlipAxis::Horizontal)
        .unwrap();
    assert!(sync.model.undo());
    assert!(sync.model.redo());
    sync.model.rotate(false).unwrap();
    assert_eq!(
        ed.model.geometry().transform,
        sync.model.geometry().transform
    );
    assert_eq!(ed.model.annotations(), sync.model.annotations());
    assert!(editor_output_image(ed).unwrap() == editor_output_image(&mut sync).unwrap());
}

#[test]
fn a_large_undo_and_redo_across_a_rotate_swap_the_cached_bases_in() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let original = app.editor.as_ref().unwrap().model.shared_base();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    finish_all(&mut app);
    let rotated = app.editor.as_ref().unwrap().model.shared_base();
    let before = composes(&app);
    let _ = update(&mut app, undo_msg());
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    assert_eq!(job.kind, crate::editor::ComposeKind::Undo);
    assert!(
        job.base.as_ref().is_some_and(|b| Arc::ptr_eq(b, &original)),
        "the worker gets the cached base"
    );
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (20, 40));
    finish_all(&mut app);
    assert_eq!(composes(&app), before, "no recompose for the undo");
    let ed = app.editor.as_mut().unwrap();
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth);
    assert!(Arc::ptr_eq(&ed.model.shared_base(), &original));
    assert_eq!(
        ed.status_text(),
        Some(format!("Undid edit. {REDO_HINT}").as_str())
    );
    let mut sync = sync_editor();
    assert!(editor_output_image(ed).unwrap() == editor_output_image(&mut sync).unwrap());
    let _ = update(&mut app, redo_msg());
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    assert!(job.base.as_ref().is_some_and(|b| Arc::ptr_eq(b, &rotated)));
    finish_all(&mut app);
    assert_eq!(composes(&app), before, "no recompose for the redo");
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (20, 40));
    assert_eq!(ed.model.undo_depth(), depth + 1);
}

#[test]
fn rapid_large_undos_land_in_order_one_compose_at_a_time() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    for msg in [
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
        Message::Editor(EditorMsg::Image(ImageMsg::Flip(
            readshot_core::document::FlipAxis::Vertical,
        ))),
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    ] {
        let _ = update(&mut app, msg);
        finish_all(&mut app);
    }
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 3);
    for _ in 0..3 {
        let _ = update(&mut app, undo_msg());
    }
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    assert_eq!((job.steps.len(), job.composed), (3, 1));
    let first = finish_compose(&app);
    let _ = update(&mut app, first);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 2, "the first undo landed");
    let job = ed.compose_job.clone().unwrap();
    assert_eq!(
        (job.steps.len(), job.composed),
        (2, 2),
        "the rest compose next"
    );
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth);
    assert_eq!(ed.model.redo_depth(), 3);
    assert!(ed.model.geometry().is_original());
    assert_eq!(ed.image_size(), (40, 20));
}

#[test]
fn an_edit_before_a_large_undo_lands_applies_it_first_and_drops_its_late_result() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    finish_all(&mut app);
    let _ = update(&mut app, undo_msg());
    let late = finish_compose(&app);
    let _ = update(&mut app, Message::Editor(EditorMsg::DeleteSelected));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20), "undone");
    assert!(ed.model.annotations().is_empty(), "then deleted");
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(!ed.model.can_redo(), "the delete dropped the redo");
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20));
    assert!(ed.model.annotations().is_empty());
    assert_eq!(ed.model.undo_depth(), depth + 1);
}

#[test]
fn a_large_added_part_lands_as_one_undo_step_with_its_bake() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.preferences.editor_panel_open = true;
    let mut ed = sync_editor();
    // A turned document is baked into one part first (D4).
    ed.model.rotate(true).unwrap();
    let mut ed = async_compose(ed);
    ed.refresh_image();
    app.editor = Some(ed);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let session = app.editor.as_ref().unwrap().session_id;
    let before = composes(&app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::PartLoaded {
            session,
            result: Ok(Some(Arc::new(ramp(20, 10)))),
        })),
    );
    assert_eq!(composes(&app), before, "the bake reused the cached base");
    assert!(matches!(
        job_kind(&app),
        Some(crate::editor::ComposeKind::AddPart { .. })
    ));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 1, "not applied yet");
    assert_eq!(ed.model.undo_depth(), depth);
    let landed = finish_compose(&app);
    let _ = update(&mut app, landed);
    let ed = app.editor.as_mut().unwrap();
    assert_eq!(ed.model.part_ids().len(), 2);
    assert_eq!(ed.model.undo_depth(), depth + 1, "one undo step");
    assert_eq!(
        ed.status_text(),
        Some(format!("Added image below. {UNDO_HINT}").as_str())
    );
    let baked = ed.model.part_ids()[0];
    assert!(ed.text_panel.bake_sources.contains_key(&baked));
    assert!(
        matches!(
            ed.text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ),
        "the new part is recognized"
    );
    let mut sync = sync_editor();
    sync.model.rotate(true).unwrap();
    sync.model
        .add_part(ramp(20, 10), readshot_core::document::Join::Below)
        .unwrap();
    assert_eq!(ed.model.part_ids(), sync.model.part_ids());
    assert_eq!(ed.model.annotations(), sync.model.annotations());
    assert!(editor_output_image(ed).unwrap() == editor_output_image(&mut sync).unwrap());
    let _ = update(&mut app, undo_msg());
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 1);
    assert_eq!(ed.model.undo_depth(), depth);
}

#[test]
fn an_edit_before_a_large_added_part_lands_applies_it_first() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::PartLoaded {
            session,
            result: Ok(Some(Arc::new(ramp(20, 10)))),
        })),
    );
    let late = finish_compose(&app);
    let _ = update(&mut app, Message::Editor(EditorMsg::DeleteSelected));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 2, "added");
    assert!(ed.model.annotations().is_empty(), "then deleted");
    assert_eq!(ed.model.undo_depth(), depth + 2);
    let _ = update(&mut app, late);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.part_ids().len(), 2);
    assert_eq!(ed.model.undo_depth(), depth + 2);
}

#[test]
fn cancelling_a_large_gap_preview_puts_the_baseline_back_without_a_render() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, _) = gap_drag_composing(&mut app);
    let baseline = handle_id(app.editor.as_ref().unwrap());
    let landed = finish_compose(&app);
    let _ = update(&mut app, landed);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 30));
    let before = composes(&app);
    // ⌘Z while the slider is held cancels the drag.
    let _ = update(&mut app, undo_msg());
    assert_eq!(composes(&app), before, "the baseline’s base was kept");
    let ed = app.editor.as_mut().unwrap();
    assert_eq!(ed.status_text(), Some("Cancelled the slider change."));
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(handle_id(ed), baseline, "the baseline’s display is back");
    assert!(ed.model.has_flattened(), "and its flattened image");
    assert!(ed.compose_job.is_none(), "nothing to render");
    assert_eq!(ed.model.undo_depth(), depth);
    let mut sync = two_part_editor();
    assert!(editor_output_image(ed).unwrap() == editor_output_image(&mut sync).unwrap());
}

#[test]
fn an_annotation_commit_on_a_large_document_renders_off_the_ui_thread() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let shown = handle_id(app.editor.as_ref().unwrap());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(rect_at(20.0).remove(0)),
        )),
    );
    let ed = app.editor.as_mut().unwrap();
    // The edit is in the model at once (one step); the previous render
    // stays on screen while the new one renders off the UI thread.
    assert_eq!(ed.model.annotations().len(), 2);
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert_eq!(handle_id(ed), shown);
    assert!(ed.display_dirty);
    assert!(!ed.model.has_flattened());
    assert_eq!(job_kind(&app), Some(crate::editor::ComposeKind::Flatten));
    // An output meanwhile is the exact present document.
    let ed = app.editor.as_mut().unwrap();
    let expected = readshot_core::render(ed.model.base(), ed.model.annotations());
    assert!(editor_output_image(ed).unwrap() == expected);
    let landed = finish_compose(&app);
    let _ = update(&mut app, landed);
    let ed = app.editor.as_ref().unwrap();
    assert_ne!(handle_id(ed), shown);
    assert!(!ed.display_dirty);
    assert!(ed.compose_job.is_none());
    assert!(ed
        .shown
        .shows(&ed.model.shared_base(), ed.model.annotations()));
}

#[test]
fn an_output_takes_the_render_of_the_present_already_made() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(rect_at(20.0).remove(0)),
        )),
    );
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    assert_eq!(job.kind, crate::editor::ComposeKind::Flatten);
    // Its worker finished, as on its thread; then ⌘C.
    job.output
        .run(|| super::super::compose_jobs::compose_with(job.next.clone(), job.base.clone()));
    let coord = app.coordinator.clone();
    let ed = app.editor.as_mut().unwrap();
    settle_editor_for_output(ed, &coord);
    assert!(ed.model.has_flattened(), "no second render for the output");
    assert!(!ed.display_dirty);
    assert!(ed.compose_job.is_none());
    let expected = readshot_core::render(ed.model.base(), ed.model.annotations());
    assert!(editor_output_image(ed).unwrap() == expected);
}

#[test]
fn a_stale_flatten_is_shown_while_the_newest_edit_renders() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let commit = |x: f32| {
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(rect_at(x).remove(0)),
        ))
    };
    let _ = update(&mut app, commit(20.0));
    let first = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    let _ = update(&mut app, commit(30.0));
    // One render in flight: the second edit waits for it.
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.compose_job.as_ref().unwrap().seq, first.seq);
    let shown = handle_id(ed);
    let landed = finish_compose(&app);
    let _ = update(&mut app, landed);
    let ed = app.editor.as_ref().unwrap();
    // Newer than what was on screen (continuous input must not freeze
    // it), but not the present: shown, still dirty, never cached.
    assert_ne!(handle_id(ed), shown, "the older render is shown");
    assert_eq!(ed.shown.annotations.as_ref().unwrap().len(), 2);
    assert!(ed.display_dirty);
    assert!(!ed.model.has_flattened(), "a display is never output");
    let second = ed.compose_job.clone().expect("the newest is rendered next");
    assert_ne!(second.seq, first.seq);
    assert_eq!(second.next.annotations, ed.model.annotations());
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.display_dirty);
    assert!(ed
        .shown
        .shows(&ed.model.shared_base(), ed.model.annotations()));
}

#[test]
fn a_crop_on_a_large_document_shows_the_previous_render_cut_to_its_frame() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    app.editor
        .as_mut()
        .unwrap()
        .model
        .set_tool(readshot_ui::editor::ToolState::Crop);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Crop {
                rect: RectLike::new(10.0, 5.0, 20.0, 10.0),
            }),
        )),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::CropApply));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.effective_image_size(), (20, 10));
    // Until the cropped render lands, the uncropped one is shown cut
    // to the crop: the same pixels in the same place.
    let part = super::super::view::shown_part_of_frame(ed, ed.crop_offset(), (20, 10));
    assert_eq!(
        part,
        Some(Some(super::super::view::ShownPart {
            region: iced::Rectangle {
                x: 10,
                y: 5,
                width: 20,
                height: 10
            },
            at: (0.0, 0.0),
            size: (20.0, 10.0),
        }))
    );
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        super::super::view::shown_part_of_frame(ed, ed.crop_offset(), (20, 10)),
        None,
        "the cropped render is the frame"
    );
}

#[test]
fn a_large_select_drag_renders_the_background_once_and_a_cancel_restores_it() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let depth = large_editor(&mut app);
    let shown = handle_id(app.editor.as_ref().unwrap());
    app.editor.as_mut().unwrap().model.clear_selection();
    for msg in [
        readshot_ui::CanvasMessage::SelectPressed(PointLike::new(5.0, 3.0)),
        readshot_ui::CanvasMessage::SelectDragged(PointLike::new(9.0, 4.0)),
    ] {
        let _ = update(&mut app, Message::Editor(EditorMsg::Canvas(msg)));
    }
    // The document without the dragged rectangle renders off the UI
    // thread; then every tick reuses it.
    assert_eq!(job_kind(&app), Some(crate::editor::ComposeKind::Flatten));
    assert!(app
        .editor
        .as_ref()
        .unwrap()
        .compose_job
        .as_ref()
        .unwrap()
        .next
        .annotations
        .is_empty());
    finish_all(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(12.0, 4.0)),
        )),
    );
    assert!(app.editor.as_ref().unwrap().compose_job.is_none());
    // Esc cancels the drag: the display before it is back, no render.
    let ed = app.editor.as_mut().unwrap();
    cancel_editor_previews(ed);
    assert_eq!(handle_id(ed), shown);
    assert!(!ed.display_dirty);
    assert_eq!(ed.model.undo_depth(), depth);
}

#[test]
fn a_released_gap_drag_settled_by_another_action_reuses_its_running_compose() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, session) = gap_drag_composing(&mut app);
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    assert!(app.editor.as_ref().unwrap().gap_commit_on_land);
    // Its worker finishes, as on its thread, before the next action.
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    job.output
        .run(|| super::super::compose_jobs::compose_with(job.next.clone(), job.base.clone()));
    let before = composes(&app);
    // Delete is not chained: the released drag is settled first.
    let _ = update(&mut app, Message::Editor(EditorMsg::DeleteSelected));
    assert_eq!(composes(&app), before, "nothing composed on the UI thread");
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 1, "one step for the drag");
    assert_eq!(ed.model.geometry().gap, 10);
    assert_eq!(ed.image_size(), (40, 30));
    assert!(ed.snapshot_drag_baseline.is_none());
    assert!(!ed.gap_commit_on_land);
    // Its message, arriving late, changes nothing.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Composed {
            session,
            seq: job.seq,
            output: job.output.clone(),
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert_eq!(ed.image_size(), (40, 30));
}

#[test]
fn undo_during_a_held_large_gap_drag_cancels_it_without_composing_its_value() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, session) = gap_drag_composing(&mut app);
    let first = finish_compose(&app);
    let _ = update(&mut app, first);
    // A newer value waits on the throttle; the slider is still held.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(30))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapFlush { session })),
    );
    let before = composes(&app);
    let _ = update(&mut app, undo_msg());
    assert_eq!(
        composes(&app),
        before,
        "the dropped value is never composed"
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.status_text(), Some("Cancelled the slider change."));
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth);
    assert!(ed.compose_job.is_none());
}

#[test]
fn a_new_gap_drag_after_a_release_keeps_the_released_value_with_its_compose() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, _) = gap_drag_composing(&mut app);
    let _ = update(&mut app, Message::Editor(EditorMsg::SnapshotSliderCommit));
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    job.output
        .run(|| super::super::compose_jobs::compose_with(job.next.clone(), job.base.clone()));
    let before = composes(&app);
    // The next drag starts before the released one landed.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(4))),
    );
    assert_eq!(composes(&app), before, "the running compose was reused");
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.model.undo_depth(),
        depth + 1,
        "the released drag is a step"
    );
    assert_eq!(ed.model.geometry().gap, 10);
    assert!(matches!(
        ed.compose_job.as_ref().map(|j| j.kind),
        Some(crate::editor::ComposeKind::GapPreview(4))
    ));
}

#[test]
fn bare_modifiers_and_zoom_keys_never_settle_pending_steps() {
    use iced::keyboard::{key::Named, Key};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let window = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(window);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let before = composes(&app);
    // ⌘ and ⇧ alone (as winit sends them before ⌘Z / ⇧⌘Z), then ⌘=.
    for key in [
        Key::Named(Named::Super),
        Key::Named(Named::Shift),
        Key::Named(Named::Alt),
        Key::Named(Named::Control),
    ] {
        let _ = update(&mut app, key_press(window, key, false));
    }
    let _ = update(
        &mut app,
        key_press(window, Key::Character("=".into()), true),
    );
    // Then ⌘Z: chained, never settled.
    let _ = update(
        &mut app,
        key_press(window, Key::Character("z".into()), true),
    );
    assert_eq!(composes(&app), before, "nothing composed on the UI thread");
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.compose_job.as_ref().unwrap().steps.len(), 2);
    assert_eq!(ed.image_size(), (40, 20), "nothing settled");
    // A key that edits still settles first.
    let _ = update(
        &mut app,
        key_press(window, Key::Named(Named::Delete), false),
    );
    assert!(!super::super::compose_jobs::steps_pending(
        app.editor.as_ref().unwrap()
    ));
}

#[test]
fn continuous_edits_keep_the_large_display_moving() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let mut shown = handle_id(app.editor.as_ref().unwrap());
    // Each landing is already stale (the next edit came first), yet it
    // replaces the older image on screen.
    for x in [10.0, 14.0, 18.0, 22.0] {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(
                readshot_ui::CanvasMessage::CommitAnnotation(rect_at(x).remove(0)),
            )),
        );
        if app.editor.as_ref().unwrap().compose_job.is_some() {
            let landed = finish_compose(&app);
            let _ = update(
                &mut app,
                Message::Editor(EditorMsg::Canvas(
                    readshot_ui::CanvasMessage::CommitAnnotation(rect_at(x + 1.0).remove(0)),
                )),
            );
            let _ = update(&mut app, landed);
            let ed = app.editor.as_ref().unwrap();
            assert_ne!(handle_id(ed), shown, "the screen moved on");
            shown = handle_id(ed);
        }
    }
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.display_dirty);
    assert!(ed
        .shown
        .shows(&ed.model.shared_base(), ed.model.annotations()));
}

#[test]
fn edits_not_yet_rendered_are_drawn_over_the_large_display() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    assert!(app.editor.as_ref().unwrap().pending_overlay().is_empty());
    let redaction = Annotation::Pixelate {
        rect: RectLike::new(20.0, 4.0, 8.0, 8.0),
        block_size: 4.0,
    };
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(redaction.clone()),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.pending_overlay(), vec![redaction], "drawn at once");
    finish_all(&mut app);
    assert!(app.editor.as_ref().unwrap().pending_overlay().is_empty());
}

#[test]
fn a_dragged_redaction_left_out_of_the_render_covers_the_bubble_magnifying_it() {
    use readshot_ui::CanvasMessage;
    let pixelate = Annotation::Pixelate {
        rect: RectLike::new(2.0, 2.0, 10.0, 8.0),
        block_size: 4.0,
    };
    // Over the live limit (4 MP) and under the async one (16 MP),
    // then over both.
    for large in [false, true] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
        ed.model.commit_annotation(magnifier_at_7_6());
        ed.model.commit_annotation(pixelate.clone());
        ed.model.set_tool(readshot_ui::editor::ToolState::Select);
        ed.refresh_image();
        ed.live_redaction_max_pixels = 800;
        app.editor = Some(if large { async_compose(ed) } else { ed });
        assert!(app
            .editor
            .as_ref()
            .unwrap()
            .magnifier_bubble_covers()
            .is_empty());
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectPressed(
                PointLike::new(10.0, 4.0),
            ))),
        );
        assert_eq!(
            app.editor
                .as_ref()
                .unwrap()
                .move_drag
                .as_ref()
                .unwrap()
                .selected_index,
            1,
            "the pixelate is dragged"
        );
        // One pixel: the pixelate still hides the source.
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectDragged(
                PointLike::new(11.0, 4.0),
            ))),
        );
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        // The image on screen is the document without the pixelate.
        let mut without = ed.model.annotations().to_vec();
        without.remove(1);
        assert!(ed.shown.shows(&ed.model.shared_base(), &without), "{large}");
        if !large {
            // ...so its bubble magnifies the source unredacted.
            let present =
                readshot_core::render(&ed.model.shared_base(), &ed.model.drawn_annotations());
            let iced::widget::image::Handle::Rgba { pixels, .. } = &ed.image_handle else {
                panic!("expected pixels");
            };
            let at = |x: u32, y: u32| (y * 40 + x) as usize * 4;
            let differs = (25..35)
                .flat_map(|x| (5..15).map(move |y| (x, y)))
                .any(|(x, y)| {
                    pixels[at(x, y)..at(x, y) + 4] != present.as_raw()[at(x, y)..at(x, y) + 4]
                });
            assert!(differs, "the bubble on screen isn’t the redacted one");
        }
        assert_eq!(
            ed.magnifier_bubble_covers(),
            vec![bubble_of_magnifier_at_7_6()],
            "the bubble in the render without the pixelate is covered ({large})"
        );
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(CanvasMessage::SelectReleased)),
        );
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.shows_present_render());
        assert!(
            ed.magnifier_bubble_covers().is_empty(),
            "the render with it shows its real effect ({large})"
        );
    }
}

#[test]
fn a_redaction_in_flight_covers_the_bubble_magnifying_it() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
    ed.model.commit_annotation(magnifier_at_7_6());
    ed.refresh_image();
    app.editor = Some(async_compose(ed));
    let redaction = Annotation::Pixelate {
        rect: RectLike::new(4.0, 4.0, 4.0, 4.0),
        block_size: 4.0,
    };
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(redaction.clone()),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.pending_overlay(), vec![redaction]);
    assert_eq!(
        ed.magnifier_bubble_covers(),
        vec![bubble_of_magnifier_at_7_6()],
        "covered until the render with the redaction lands"
    );
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.shows_present_render());
    assert!(ed.magnifier_bubble_covers().is_empty());
}

#[test]
fn a_redaction_away_from_every_source_covers_no_bubble() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
    ed.model.commit_annotation(magnifier_at_7_6());
    ed.refresh_image();
    app.editor = Some(async_compose(ed));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Pixelate {
                rect: RectLike::new(14.0, 14.0, 4.0, 4.0),
                block_size: 4.0,
            }),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.pending_overlay().len(), 1);
    assert!(ed.magnifier_bubble_covers().is_empty());
}

#[test]
fn a_large_select_release_draws_the_moved_annotation_until_rendered() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    app.editor.as_mut().unwrap().model.clear_selection();
    for msg in [
        readshot_ui::CanvasMessage::SelectPressed(PointLike::new(5.0, 3.0)),
        readshot_ui::CanvasMessage::SelectDragged(PointLike::new(9.0, 4.0)),
    ] {
        let _ = update(&mut app, Message::Editor(EditorMsg::Canvas(msg)));
    }
    finish_all(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::SelectReleased,
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    // The background was rendered without it: the canvas draws it in
    // its new place until the render with it lands.
    assert_eq!(ed.pending_overlay(), ed.model.annotations().to_vec());
    assert_eq!(ed.pending_overlay().len(), 1);
    finish_all(&mut app);
    assert!(app.editor.as_ref().unwrap().pending_overlay().is_empty());
}

#[test]
fn esc_during_a_held_large_gap_drag_never_composes_its_value() {
    use iced::keyboard::{key::Named, Key};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let (depth, session) = gap_drag_composing(&mut app);
    let window = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(window);
    let first = finish_compose(&app);
    let _ = update(&mut app, first);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(30))),
    );
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::GapFlush { session })),
    );
    let before = composes(&app);
    let mut esc = key_press(window, Key::Named(Named::Escape), false);
    if let Message::Editor(EditorMsg::KeyPressed { status_ignored, .. }) = &mut esc {
        *status_ignored = false; // the canvas captured it
    }
    let _ = update(&mut app, esc);
    assert_eq!(
        composes(&app),
        before,
        "the dropped value is never composed"
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.snapshot_drag_baseline.is_none());
    assert_eq!(ed.image_size(), (40, 20));
    assert_eq!(ed.model.undo_depth(), depth);
}

#[test]
fn small_edits_send_nothing_to_the_dropper_thread() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
    let before = super::super::compose_jobs::sent_to_dropper();
    for x in [2.0, 6.0, 10.0] {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Canvas(
                readshot_ui::CanvasMessage::CommitAnnotation(rect_at(x).remove(0)),
            )),
        );
    }
    let _ = update(&mut app, undo_msg());
    assert_eq!(super::super::compose_jobs::sent_to_dropper(), before);
    // A big image let go of is freed on the dropper thread.
    app.editor = Some(crate::editor::EditorSession::new(solid(1_100, 1_000)));
    let before = super::super::compose_jobs::sent_to_dropper();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(rect_at(2.0).remove(0)),
        )),
    );
    assert_eq!(super::super::compose_jobs::sent_to_dropper(), before + 1);
}

#[test]
fn the_display_shares_the_flattened_pixels_with_the_model() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    let flat = ed.model.flattened_shared().expect("cached");
    assert_eq!(ed.image_handle.id(), flat.handle().id(), "one copy");
    // A small edit shares too.
    let mut ed = crate::editor::EditorSession::new(solid(40, 20));
    ed.model.commit_annotations(rect_at(5.0));
    ed.refresh_image();
    assert_eq!(
        ed.image_handle.id(),
        ed.model.flattened_shared().unwrap().handle().id()
    );
}

#[test]
fn a_replaced_flatten_worker_is_abandoned() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(rect_at(20.0).remove(0)),
        )),
    );
    let flatten = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    assert_eq!(flatten.kind, crate::editor::ComposeKind::Flatten);
    // A rotate replaces it before its worker starts: it never runs.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let ran = std::cell::Cell::new(false);
    flatten.output.run(|| {
        ran.set(true);
        super::super::compose_jobs::compose_with(flatten.next.clone(), flatten.base.clone())
    });
    assert!(!ran.get());
    assert!(flatten.output.take().is_none());
}

#[test]
fn the_toolbar_shows_undo_and_redo_as_they_will_be_after_queued_steps() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let _ = update(&mut app, undo_msg());
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.model.can_redo(), "nothing applied yet");
    assert_eq!(
        super::super::compose_jobs::can_undo_redo(ed),
        (true, true),
        "the rotate will be redoable"
    );
    // ⇧⌘Z (the enabled Redo) queues behind.
    let _ = update(&mut app, redo_msg());
    assert_eq!(
        super::super::compose_jobs::can_undo_redo(app.editor.as_ref().unwrap()),
        (true, false)
    );
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!((ed.model.can_undo(), ed.model.can_redo()), (true, false));
}

#[test]
fn a_large_editor_opens_on_a_placeholder_and_renders_off_the_ui_thread() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    // 16 MP: at the threshold.
    let mut ed = crate::editor::EditorSession::new(ramp(4_000, 4_000));
    assert!(ed.display_dirty);
    assert!(!ed.model.has_flattened(), "nothing rendered");
    assert!(ed.shown.scale < 1.0);
    match &ed.image_handle {
        iced::widget::image::Handle::Rgba { width, height, .. } => {
            assert!(*width <= crate::editor::PLACEHOLDER_MAX_SIDE);
            assert!(*height <= crate::editor::PLACEHOLDER_MAX_SIDE);
        }
        other => panic!("unexpected handle {other:?}"),
    }
    // An output before the render is the exact document, never the
    // placeholder.
    ed.model.commit_annotations(rect_at(5.0));
    let expected = readshot_core::render(ed.model.base(), ed.model.annotations());
    assert!(editor_output_image(&mut ed).unwrap() == expected);
    app.editor = Some(crate::editor::EditorSession::new(ramp(4_000, 4_000)));
    let _ = update(&mut app, Message::Editor(EditorMsg::StatusTick));
    assert_eq!(job_kind(&app), Some(crate::editor::ComposeKind::Flatten));
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.display_dirty);
    assert_eq!(ed.shown.scale, 1.0);
    assert!(ed
        .shown
        .shows(&ed.model.shared_base(), ed.model.annotations()));
}

#[test]
fn a_placeholder_is_cut_to_a_crop_frame_at_its_own_scale() {
    let base = Arc::new(ramp(3_200, 1_600));
    let record_crop = Annotation::Crop {
        rect: RectLike::new(800.0, 400.0, 1_600.0, 800.0),
    };
    let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
    ed.model = readshot_ui::editor::EditorState::with_annotations(
        Arc::unwrap_or_clone(base),
        vec![record_crop],
    );
    ed.shown = crate::editor::ShownImage {
        base: ed.model.shared_base(),
        annotations: Some(Vec::new()),
        offset: (0.0, 0.0),
        size: (3_200, 1_600),
        scale: 0.5,
    };
    let part = super::super::view::shown_part_of_frame(&ed, ed.crop_offset(), (1_600, 800));
    assert_eq!(
        part,
        Some(Some(super::super::view::ShownPart {
            region: iced::Rectangle {
                x: 400,
                y: 200,
                width: 800,
                height: 400,
            },
            at: (0.0, 0.0),
            size: (1_600.0, 800.0),
        }))
    );
}

/// UI-thread time of rotate, undo, redo and an annotation commit at
/// ~96 MP: the message itself, then the landing of its off-thread
/// result (the worker's compose is run untimed, as on its thread).
/// A manual benchmark: it asserts nothing and prints its timings, to
/// compare a change's UI-thread cost by hand. Release build: `cargo test
/// --release -p readshot-app --lib ui_thread_time -- --ignored
/// --nocapture`.
#[test]
#[ignore = "measurement; run in release"]
fn ui_thread_time_of_rotate_and_undo_at_96_mp() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let image = solid(12_000, 8_000);
    let t = std::time::Instant::now();
    let mut ed = crate::editor::EditorSession::new(image);
    println!("96 MP UI thread: open editor: {:?}", t.elapsed());
    ed.model.commit_annotations(rect_at(50.0));
    app.editor = Some(ed);
    let step = |app: &mut App, what: &str, msg: Message| {
        let t = std::time::Instant::now();
        let _ = update(app, msg);
        let handled = t.elapsed();
        let mut landed = std::time::Duration::ZERO;
        while app.editor.as_ref().unwrap().compose_job.is_some() {
            let done = finish_compose(app);
            let t = std::time::Instant::now();
            let _ = update(app, done);
            landed += t.elapsed();
        }
        println!("96 MP UI thread: {what}: message {handled:?}, landing {landed:?}");
    };
    step(
        &mut app,
        "rotate",
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    step(&mut app, "undo", undo_msg());
    step(
        &mut app,
        "redo",
        Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Redo)),
    );
    step(&mut app, "undo again", undo_msg());
    step(
        &mut app,
        "commit annotation",
        Message::Editor(EditorMsg::Canvas(
            readshot_ui::CanvasMessage::CommitAnnotation(rect_at(90.0).remove(0)),
        )),
    );
    let session = app.editor.as_ref().unwrap().session_id;
    step(
        &mut app,
        "add part (12000 x 1000)",
        Message::Editor(EditorMsg::Combine(CombineMsg::PartLoaded {
            session,
            result: Ok(Some(Arc::new(solid(12_000, 1_000)))),
        })),
    );
    step(&mut app, "undo the part", undo_msg());
}

#[test]
fn discard_abandons_a_queued_step_and_frees_nothing_here() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    let late = finish_compose(&app);
    let before = super::super::compose_jobs::composes_on_this_thread();
    // The pending rotate is unsaved work: the first press arms.
    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));
    assert!(app.editor.as_ref().unwrap().discard_armed());
    assert!(app.editor.as_ref().unwrap().compose_job.is_some());
    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));
    assert!(app.editor.is_none());
    assert_eq!(
        super::super::compose_jobs::composes_on_this_thread(),
        before,
        "nothing composed for a discard"
    );
    assert!(app.retiring_editors.is_empty(), "a discard keeps nothing");
    // The worker that hadn't started never composes the discarded document.
    job.output
        .run(|| panic!("composed a document nobody will use"));
    // A result already on its way is dropped, never applied.
    let _ = update(&mut app, late);
    assert!(app.editor.is_none());
}

#[test]
fn closing_the_editor_mid_compose_never_composes_here_and_still_syncs_history() {
    let (_dir, store, mut app) = redacted_history_editor();
    let coord = app.coordinator.clone();
    let id = iced::window::Id::unique();
    {
        let ed = app.editor.as_mut().unwrap();
        ed.window_id = Some(id);
        write_editor_history(ed, &coord);
    }
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let session = app.editor.as_ref().unwrap().session_id;
    let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
    let before = super::super::compose_jobs::composes_on_this_thread();
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert!(app.editor.is_none());
    assert_eq!(
        super::super::compose_jobs::composes_on_this_thread(),
        before,
        "the close composed nothing on the UI thread"
    );
    assert_eq!(app.retiring_editors.len(), 1, "kept until its step lands");
    // The worker lands: the step is applied and History synced.
    let _ = update(&mut app, compose_result(session, &job));
    assert!(app.retiring_editors.is_empty());
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!(
        (stored.width_px, stored.height_px),
        (20, 40),
        "the rotate reached History"
    );
}

/// Editor A rotates a large capture and is replaced (by reopening the
/// same record from History) while the rotate composes. Editor B redacts
/// and closes: History is scrubbed. A's rotate then lands; its late
/// History sync must never put the unredacted record back.
#[test]
fn a_retiring_editors_late_sync_never_unredacts_a_newer_scrub() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let img = ramp(40, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut a = async_compose(crate::editor::EditorSession::from_history(
        img.clone(),
        record.clone(),
    ));
    let id_a = iced::window::Id::unique();
    a.window_id = Some(id_a);
    app.editor = Some(a);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let session_a = app.editor.as_ref().unwrap().session_id;
    let job_a = app.editor.as_ref().unwrap().compose_job.clone().unwrap();

    // An editor of the record opens while A's rotate composes. (Every
    // History open waits for A; this opens one without that wait, to test
    // the coordinator's own guard.)
    let stored = store.list().unwrap().into_iter().next().unwrap();
    let _ = super::super::history_ops::open_reopened(&mut app, (img.clone(), stored), None);
    assert_eq!(app.retiring_editors.len(), 1, "A still finishes");
    let id_b = iced::window::Id::unique();
    {
        let coord = app.coordinator.clone();
        let b = app.editor.as_mut().unwrap();
        assert_ne!(b.session_id, session_a);
        b.window_id = Some(id_b);
        app.windows.register(id_b, WindowKind::Editor);
        b.model.commit_annotation(Annotation::Blur {
            rect: RectLike::new(4.0, 4.0, 10.0, 10.0),
            radius: 4.0,
        });
        write_editor_history(b, &coord);
    }
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id_b)));
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().into_iter().next().unwrap();
    assert!(stored.redacted, "B's scrub landed");

    // A's rotate lands now (if A is still around to land it).
    let _ = update(&mut app, compose_result(session_a, &job_a));
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().into_iter().next().unwrap();
    assert!(stored.redacted, "a late sync un-redacted the record");
    assert_eq!((stored.width_px, stored.height_px), (40, 20));
    assert!(app.retiring_editors.is_empty());
}

/// With the Beautify tab on screen, ⌘Z goes through the hex field check
/// (`EditorFieldKeyResolved`). It still queues behind a pending large
/// step, like a plain Undo: nothing composes on the UI thread.
#[test]
fn undo_through_the_hex_field_check_queues_behind_a_large_step() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    large_editor(&mut app);
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    app.preferences.editor_panel_tab = crate::editor::PanelTab::Beautify.id().to_string();
    app.preferences.editor_panel_open = true;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    let before = composes(&app);
    let _ = update(
        &mut app,
        key_press(id, iced::keyboard::Key::Character("z".into()), true),
    );
    for field_focused in [true, false] {
        let sid = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::FieldKeyResolved {
                session: sid,
                message: Box::new(Message::Editor(EditorMsg::Toolbar(
                    readshot_ui::ToolbarMessage::Undo,
                ))),
                field_focused,
            }),
        );
    }
    assert_eq!(composes(&app), before, "nothing composed on the UI thread");
    finish_all(&mut app);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.image_size(), (40, 20), "rotated, then undone");
}

/// The re-review's probe: in editor A on a large capture X, rotate,
/// redact R, ⌘Z, ⌘Z, ⌘⇧Z ×2 (the redos chain behind a composing undo),
/// then reopen X from History. A's redone redaction must reach History:
/// the reopen waits for A to land and sync, then opens X from disk.
#[test]
fn reopening_a_record_waits_for_its_composing_editor_and_keeps_a_redone_redaction() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    app.history_root = Some(dir.path().join("h"));
    let img = ramp(40, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.history_records = store.list().unwrap();
    let mut a = async_compose(crate::editor::EditorSession::from_history(
        img.clone(),
        record.clone(),
    ));
    let id_a = iced::window::Id::unique();
    a.window_id = Some(id_a);
    app.windows.register(id_a, WindowKind::Editor);
    app.editor = Some(a);
    let coord = app.coordinator.clone();
    let undo = || Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo));
    let redo = || Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Redo));

    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    finish_all(&mut app);
    {
        let a = app.editor.as_mut().unwrap();
        a.model.commit_annotation(Annotation::Blur {
            rect: RectLike::new(4.0, 4.0, 10.0, 10.0),
            radius: 4.0,
        });
        super::super::editor_ops::sync_editor_history(a, &coord);
    }
    let _ = update(&mut app, undo()); // R undone
    finish_all(&mut app);
    let _ = update(&mut app, undo()); // the rotate: composes
    let _ = update(&mut app, redo()); // chained
    let _ = update(&mut app, redo()); // chained: R again
    let session_a = app.editor.as_ref().unwrap().session_id;
    assert!(app.editor.as_ref().unwrap().compose_job.is_some());

    let _ = update(
        &mut app,
        Message::History(HistoryMsg::OpenInEditor(record.id)),
    );
    assert!(app.editor.is_none(), "A closed; nothing opened yet");
    assert_eq!(app.pending_history_open, Some(record.id));
    assert_eq!(
        app.history_status_text(),
        Some("Finishing your previous edits to this capture…")
    );
    // A's composes land, one after another.
    for _ in 0..8 {
        let Some(job) = app
            .retiring_editors
            .iter()
            .find(|ed| ed.session_id == session_a)
            .and_then(|ed| ed.compose_job.clone())
        else {
            break;
        };
        let _ = update(&mut app, compose_result(session_a, &job));
    }
    assert!(app.retiring_editors.is_empty());
    assert_eq!(app.pending_history_open, None, "the open went ahead");
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().into_iter().next().unwrap();
    assert!(stored.redacted, "the redone redaction reached History");
    assert_eq!((stored.width_px, stored.height_px), (20, 40));
    // The open (from disk) lands: B starts from A's final state.
    let (result, unsaved) = super::super::history_ops::load_after_writes(
        &app.coordinator,
        app.history_root.clone(),
        record.id,
    );
    assert_eq!(unsaved, None);
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ReopenedAfterSync {
            record_id: record.id,
            result,
            unsaved,
        }),
    );
    let b = app.editor.as_ref().unwrap();
    assert!(b.source_record.as_ref().unwrap().redacted);
    assert_eq!(b.image_size(), (20, 40));
    assert_eq!(app.history_status, None);
}

/// If the previous editor's History write failed, the record still
/// opens (as History has it) and the editor says what wasn't saved.
#[test]
fn a_reopen_whose_previous_write_failed_says_so() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.history_status = Some(crate::editor::Status::new(
        crate::editor::StatusTone::Progress,
        "Finishing your previous edits to this capture…",
    ));
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    app.awaiting_reopen = Some(record.id);
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ReopenedAfterSync {
            record_id: record.id,
            result: Ok((ramp(40, 20), record)),
            unsaved: Some("the disk is full".into()),
        }),
    );
    assert_eq!(app.history_status, None);
    let status = app
        .editor
        .as_ref()
        .unwrap()
        .status_text()
        .unwrap()
        .to_owned();
    assert!(status.contains("weren’t all saved to History"), "{status}");
    assert!(status.contains("the disk is full"), "{status}");
}

/// An FsHistoryStore whose image commits wait while `held` is set: a
/// stand-in for a slow encode/write of a very large document.
struct SlowCommitStore {
    inner: readshot_core::FsHistoryStore,
    held: std::sync::Mutex<bool>,
    released: std::sync::Condvar,
}

impl SlowCommitStore {
    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.released.notify_all();
    }
}

impl readshot_core::HistoryStore for SlowCommitStore {
    fn save(
        &self,
        r: &readshot_core::CaptureRecord,
        png: &[u8],
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.save(r, png)
    }
    fn list(&self) -> Result<Vec<readshot_core::CaptureRecord>, readshot_core::HistoryError> {
        self.inner.list()
    }
    fn apply_retention(
        &self,
        p: readshot_core::HistoryRetention,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.apply_retention(p, now)
    }
    fn clear_all(&self) -> Result<(), readshot_core::HistoryError> {
        self.inner.clear_all()
    }
    fn update(&self, r: &readshot_core::CaptureRecord) -> Result<(), readshot_core::HistoryError> {
        self.inner.update(r)
    }
    fn stage_image(
        &self,
        r: &readshot_core::CaptureRecord,
        png: &[u8],
        thumb: Option<&[u8]>,
    ) -> Result<readshot_core::StagedImage, readshot_core::HistoryError> {
        self.inner.stage_image(r, png, thumb)
    }
    fn commit_rewrite(
        &self,
        r: &readshot_core::CaptureRecord,
        staged: readshot_core::StagedImage,
    ) -> Result<bool, readshot_core::HistoryError> {
        let mut held = self.held.lock().unwrap();
        while *held {
            held = self.released.wait(held).unwrap();
        }
        drop(held);
        self.inner.commit_rewrite(r, staged)
    }
    fn commit_save(
        &self,
        r: &readshot_core::CaptureRecord,
        staged: readshot_core::StagedImage,
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.commit_save(r, staged)
    }
    fn sweep_staging(&self) {
        self.inner.sweep_staging()
    }
    fn finish_pending_scrub(
        &self,
        id: readshot_core::Uuid,
    ) -> Result<bool, readshot_core::HistoryError> {
        self.inner.finish_pending_scrub(id)
    }
    fn finish_pending_scrubs(&self) -> usize {
        self.inner.finish_pending_scrubs()
    }
    fn stored_redacted(
        &self,
        r: &readshot_core::CaptureRecord,
    ) -> Result<bool, readshot_core::HistoryError> {
        self.inner.stored_redacted(r)
    }
    fn begin_scrub(
        &self,
        r: &readshot_core::CaptureRecord,
    ) -> Result<bool, readshot_core::HistoryError> {
        self.inner.begin_scrub(r)
    }
    fn record_recognized_text(
        &self,
        r: &readshot_core::CaptureRecord,
        doc: &readshot_core::text_lines::TextDocument,
        fill: bool,
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.record_recognized_text(r, doc, fill)
    }
    fn contains(&self, r: &readshot_core::CaptureRecord) -> bool {
        self.inner.contains(r)
    }
    fn delete(&self, id: readshot_core::Uuid) -> Result<(), readshot_core::HistoryError> {
        self.inner.delete(id)
    }
}

/// The fix-2 probe: a frame rewrite (a rotate that landed while the
/// editor was open) is still being written when the record is reopened.
/// The reopen waits for it, however long it takes, then opens the
/// rotated record; later edits in the new editor persist.
#[test]
fn a_reopen_waits_for_every_write_of_the_record_with_no_timeout() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("h");
    let store = Arc::new(SlowCommitStore {
        inner: readshot_core::FsHistoryStore::new(root.clone()),
        held: std::sync::Mutex::new(false),
        released: std::sync::Condvar::new(),
    });
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    app.history_root = Some(root.clone());
    app.history_window_id = Some(iced::window::Id::unique());
    let img = ramp(40, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    app.history_records = store.list().unwrap();
    let mut a = crate::editor::EditorSession::from_history(img, record.clone());
    let id_a = iced::window::Id::unique();
    a.window_id = Some(id_a);
    app.windows.register(id_a, WindowKind::Editor);
    app.editor = Some(a);
    // The rotate lands at once (a small document) and its frame rewrite
    // is queued; its write is slow.
    *store.held.lock().unwrap() = true;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Image(ImageMsg::Rotate { clockwise: true })),
    );
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (20, 40));

    let _ = update(
        &mut app,
        Message::History(HistoryMsg::OpenInEditor(record.id)),
    );
    assert!(app.editor.is_none());
    assert_eq!(app.awaiting_reopen, Some(record.id));
    assert_eq!(
        app.history_status_text(),
        Some("Finishing your previous edits to this capture…")
    );
    // What the reopen task runs, off the UI thread: it must not finish
    // while the write is outstanding.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let coord = app.coordinator.clone();
        let root = Some(root.clone());
        let id = record.id;
        std::thread::spawn(move || {
            let _ = tx.send(super::super::history_ops::load_after_writes(
                &coord, root, id,
            ));
        });
    }
    // Longer than the writer flush's 5 s timeout: there is none here.
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(5_500))
            .is_err(),
        "opened before the rewrite landed"
    );
    store.release();
    let (result, unsaved) = rx
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the wait ends once the write is done");
    assert_eq!(unsaved, None);
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ReopenedAfterSync {
            record_id: record.id,
            result,
            unsaved,
        }),
    );
    let b = app.editor.as_mut().unwrap();
    assert_eq!(b.image_size(), (20, 40), "B opens on the rotated record");
    // B's later edits persist.
    let coord = app.coordinator.clone();
    let b = app.editor.as_mut().unwrap();
    b.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(2.0, 2.0, 5.0, 5.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 1.0,
        style: Default::default(),
    });
    super::super::editor_ops::sync_editor_history(b, &coord);
    app.coordinator.flush_history_updates();
    let stored = store.list().unwrap().into_iter().next().unwrap();
    assert_eq!((stored.width_px, stored.height_px), (20, 40));
    assert_eq!(stored.annotation_model.len(), 1, "B's edit is stored");
}

fn dirty_editor(app: &mut App) -> iced::window::Id {
    let mut ed = crate::editor::EditorSession::new(solid(30, 30));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    app.windows.register(id, WindowKind::Editor);
    ed.model.commit_annotation(rect_at(3.0).remove(0));
    app.editor = Some(ed);
    id
}

/// A deferred open never replaces an editor with unsaved work that
/// became current while it waited: it asks with the unsaved-work prompt,
/// and opens only when Discard confirms.
#[test]
fn a_deferred_open_asks_before_replacing_unsaved_work() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.history_window_id = Some(iced::window::Id::unique());
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    app.awaiting_reopen = Some(record.id);
    let c = dirty_editor(&mut app);
    let c_session = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ReopenedAfterSync {
            record_id: record.id,
            result: Ok((ramp(40, 20), record.clone())),
            unsaved: None,
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.session_id, c_session, "C stays");
    assert!(ed.discard_armed(), "the unsaved-work prompt is armed");
    assert!(ed.status_text().unwrap().contains("Press Discard"));
    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));
    assert!(app.editor.is_none(), "C closed");
    assert_eq!(app.windows.kind(c), None, "C's window closed");
    // The record opens again through the one open path (read fresh).
    assert_eq!(app.awaiting_reopen, Some(record.id));
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ReopenedAfterSync {
            record_id: record.id,
            result: Ok((ramp(40, 20), record.clone())),
            unsaved: None,
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_ne!(ed.session_id, c_session);
    assert_eq!(ed.source_record.as_ref().map(|r| r.id), Some(record.id));
}

/// History closed during the wait: the open is cancelled and the
/// capture status says so; no window pops up.
#[test]
fn a_deferred_open_is_cancelled_when_history_closed() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    app.awaiting_reopen = Some(record.id);
    let _ = update(&mut app, Message::History(HistoryMsg::Closed));
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ReopenedAfterSync {
            record_id: record.id,
            result: Ok((ramp(40, 20), record)),
            unsaved: Some("the disk is full".into()),
        }),
    );
    assert!(app.editor.is_none());
    let status = app.last_capture_status_text().map(str::to_owned).unwrap();
    assert!(status.contains("History was closed"), "{status}");
    assert!(status.contains("the disk is full"), "{status}");
}

/// A reopen that fails to load shows the failure and the reason edits
/// weren't saved.
#[test]
fn a_failed_reopen_shows_why_and_what_wasnt_saved() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.history_window_id = Some(iced::window::Id::unique());
    let id = readshot_core::Uuid::new_v4();
    app.awaiting_reopen = Some(id);
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::ReopenedAfterSync {
            record_id: id,
            result: Err("No such file or directory (os error 2)".into()),
            unsaved: Some("the disk is full".into()),
        }),
    );
    assert!(app.editor.is_none());
    let status = app.history_status_text().map(str::to_owned).unwrap();
    assert!(status.starts_with("Couldn’t open the capture"), "{status}");
    assert!(
        status.contains("weren’t all saved to History (the disk is full)"),
        "{status}"
    );
}

/// Reopening the record open right now asks first when the window holds
/// what History doesn't keep (Beautify here); Discard then reopens it,
/// writing its History edits first.
#[test]
fn reopening_the_open_record_asks_before_dropping_beautify() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    app.history_root = Some(dir.path().join("h"));
    app.history_window_id = Some(iced::window::Id::unique());
    let img = ramp(40, 20);
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&img).unwrap())
        .unwrap();
    let mut a = crate::editor::EditorSession::from_history(img, record.clone());
    a.window_id = Some(iced::window::Id::unique());
    a.model.set_beautify(BuiltinPreset::Soft.settings());
    app.editor = Some(a);
    let _ = update(
        &mut app,
        Message::History(HistoryMsg::OpenInEditor(record.id)),
    );
    let ed = app.editor.as_ref().expect("asked first, nothing closed");
    assert!(ed.discard_armed());
    assert!(ed.status_text().unwrap().contains("History doesn’t keep"));
    assert_eq!(app.awaiting_reopen, None);
    let _ = update(&mut app, Message::Editor(EditorMsg::DiscardRequested));
    assert!(app.editor.is_none(), "closed (retired) for the reopen");
    assert_eq!(app.awaiting_reopen, Some(record.id));
}
