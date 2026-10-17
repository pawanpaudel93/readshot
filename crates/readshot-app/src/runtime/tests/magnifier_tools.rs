//! Magnifier drags and keyboard width bumps.

use super::*;

#[test]
fn keyboard_width_bump_updates_selected_annotation() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    app.editor = Some(ed);

    let _ = update(&mut app, Message::EditorWidthBump(3.0));

    // Steps from the selection's own width (2), which the options
    // bar's Width control shows, not the tool width (3).
    let ed = app.editor.as_ref().unwrap();
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 5.0),
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn dragging_inside_an_unselected_bubble_moves_only_the_bubble() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(200, 100));
    ed.model.commit_annotation(Annotation::Magnifier {
        source: PointLike::new(20.0, 20.0),
        radius: 10.0,
        bubble: PointLike::new(80.0, 40.0),
        zoom: 4.0,
        shape: readshot_core::MagnifierShape::Circle,
        color: Rgba::OPAQUE_BLACK,
    });
    ed.model.clear_selection();
    app.editor = Some(ed);

    // 30 px from the bubble centre: inside the bubble, off its handle.
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
            110.0, 40.0,
        ))),
    );
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
            100.0, 50.0,
        ))),
    );
    let ed = app.editor.as_ref().unwrap();
    match &ed.model.annotations()[0] {
        Annotation::Magnifier { source, bubble, .. } => {
            assert_eq!(*source, PointLike::new(20.0, 20.0));
            assert_eq!(*bubble, PointLike::new(70.0, 50.0));
        }
        other => panic!("expected magnifier, got {other:?}"),
    }
}

#[test]
fn a_first_drag_on_an_unselected_magnifiers_handles_matches_the_second() {
    for selected in [false, true] {
        // Near the source centre (the Start handle): only the source.
        let mut app = magnifier_app(selected);
        select_drag(&mut app, (43.0, 42.0), (53.0, 47.0));
        assert_eq!(
            magnifier_parts(&app),
            (
                PointLike::new(50.0, 45.0),
                20.0,
                PointLike::new(200.0, 40.0)
            ),
            "selected: {selected}"
        );
        // Inside the disc near the rim (the East handle): the radius.
        let mut app = magnifier_app(selected);
        select_drag(&mut app, (55.0, 40.0), (65.0, 40.0));
        assert_eq!(
            magnifier_parts(&app),
            (
                PointLike::new(40.0, 40.0),
                30.0,
                PointLike::new(200.0, 40.0)
            ),
            "selected: {selected}"
        );
    }
}

#[test]
fn a_selected_magnifiers_leader_moves_the_whole_magnifier() {
    let mut app = magnifier_app(true);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    select_drag(&mut app, (110.0, 41.0), (120.0, 51.0));
    assert_eq!(
        magnifier_parts(&app),
        (
            PointLike::new(50.0, 50.0),
            20.0,
            PointLike::new(210.0, 50.0)
        )
    );
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
    // An unselected leader stays click-through.
    let mut app = magnifier_app(false);
    select_drag(&mut app, (110.0, 41.0), (120.0, 51.0));
    assert_eq!(
        magnifier_parts(&app),
        (
            PointLike::new(40.0, 40.0),
            20.0,
            PointLike::new(200.0, 40.0)
        )
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        None
    );
}

#[test]
fn magnifier_shape_segments_do_not_reuse_drawing_tool_glyphs() {
    for (glyph, _, _) in MAGNIFIER_SHAPE_SEGMENTS {
        assert!(
            !matches!(glyph, crate::icons::AppIcon::Tool(_)),
            "{glyph:?}"
        );
    }
}
