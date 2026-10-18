//! One geometry pipeline: a small document (steps applied on the UI
//! thread) and a large one (steps composed off it) end every history
//! step in the same editor state. Only where the pixels are composed
//! differs.

use super::*;

use readshot_core::document::{Align, FlipAxis, Join};

/// Everything a landed step decides, besides where its pixels came from
/// (the document and its pixels are compared apart: their Debug is the
/// whole image).
#[derive(Debug, PartialEq)]
struct Landed {
    annotations: Vec<Annotation>,
    parts: Vec<(readshot_core::document::PartId, u32, u32)>,
    image_size: (u32, u32),
    effective_size: (u32, u32),
    undo_depth: usize,
    can_undo_redo: (bool, bool),
    status: Option<String>,
    status_tone: Option<crate::editor::StatusTone>,
    loupe_shown: Option<bool>,
    discard_armed: bool,
    history_sync_owed: bool,
    bake_sources: Vec<readshot_core::document::PartId>,
    recognizing_part: Option<readshot_core::document::PartId>,
    popover: Option<crate::editor::EditorPopover>,
}

fn landed(app: &mut App) -> (Landed, readshot_ui::editor::Snapshot, image::RgbaImage) {
    let ed = app.editor.as_mut().unwrap();
    assert!(ed.compose_job.is_none(), "every step landed");
    let mut bake_sources: Vec<_> = ed.text_panel.bake_sources.keys().copied().collect();
    bake_sources.sort_unstable();
    let snapshot = ed.model.snapshot();
    let parts = snapshot
        .doc
        .geometry
        .parts
        .iter()
        .map(|p| (p.id, p.image.width(), p.image.height()))
        .collect();
    let landed = Landed {
        annotations: snapshot.annotations.clone(),
        parts,
        image_size: ed.image_size(),
        effective_size: ed.effective_image_size(),
        undo_depth: ed.model.undo_depth(),
        can_undo_redo: (ed.model.can_undo(), ed.model.can_redo()),
        status: ed.status_text().map(str::to_owned),
        status_tone: ed.status_tone(),
        loupe_shown: ed.eyedropper.as_ref().map(|e| e.loupe.is_some()),
        discard_armed: ed.discard_armed(),
        history_sync_owed: ed.history_sync_owed,
        bake_sources,
        recognizing_part: ed.text_panel.recognizing_part,
        popover: ed.popover,
    };
    (landed, snapshot, ed.model.flatten())
}

/// The same small document twice: one editor composes on the UI thread,
/// the other off it. The text panel is open, so an added part is
/// recognized; the eyedropper is on, so a step must clear its loupe.
fn twin_apps() -> (App, App) {
    twins_of(sync_editor)
}

/// [`twin_apps`] over the document `editor` builds.
fn twins_of(editor: fn() -> crate::editor::EditorSession) -> (App, App) {
    let make = |async_compose_min_pixels: u64| {
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = true;
        let mut ed = editor();
        ed.async_compose_min_pixels = async_compose_min_pixels;
        ed.refresh_image();
        app.editor = Some(ed);
        let _ = update(&mut app, Message::Editor(EditorMsg::EyedropperToggled));
        app
    };
    let sync = make(u64::MAX);
    let async_ = make(0);
    (sync, async_)
}

/// Run one step's messages on both editors (the async one's composes
/// finished as their workers would), after a hover that fills the loupe
/// and an armed Discard; then their states must agree.
fn step_both(sync: &mut App, async_: &mut App, what: &str, messages: &[Message]) {
    for app in [&mut *sync, &mut *async_] {
        // The Crop tool turns the eyedropper off: back on for every step.
        if app
            .editor
            .as_ref()
            .is_some_and(|ed| ed.eyedropper.is_none())
        {
            let _ = update(app, Message::Editor(EditorMsg::EyedropperToggled));
        }
        let _ = update(
            app,
            Message::Editor(EditorMsg::Canvas(
                readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(3.5, 4.5)),
            )),
        );
        if let Some(ed) = app.editor.as_mut() {
            ed.arm_discard_prompt(std::time::Instant::now());
        }
        for m in messages {
            let _ = update(app, m.clone());
        }
        finish_all(app);
    }
    let ((a, a_doc, a_pixels), (b, b_doc, b_pixels)) = (landed(sync), landed(async_));
    assert!(
        b.loupe_shown != Some(true),
        "{what}: the loupe is cleared under a new frame"
    );
    assert!(!b.discard_armed, "{what}: a step disarms Discard");
    assert_eq!(a, b, "{what}: sync and async disagree");
    assert!(
        same_document(&a_doc, &b_doc),
        "{what}: the documents differ"
    );
    assert!(a_pixels == b_pixels, "{what}: the flattened pixels differ");
}

/// Two snapshots of separate editors: equal values (a `Part` compares its
/// image by pointer, which two editors never share).
fn same_document(a: &readshot_ui::editor::Snapshot, b: &readshot_ui::editor::Snapshot) -> bool {
    let (ga, gb) = (&a.doc.geometry, &b.doc.geometry);
    a.annotations == b.annotations
        && a.doc.beautify == b.doc.beautify
        && ga.gap == gb.gap
        && ga.align == gb.align
        && ga.transform == gb.transform
        && ga.baked_from == gb.baked_from
        && ga.parts.len() == gb.parts.len()
        && ga
            .parts
            .iter()
            .zip(&gb.parts)
            .all(|(p, q)| p.id == q.id && p.join == q.join && *p.image == *q.image)
}

fn resize_to(width: &str) -> Vec<Message> {
    vec![
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeOpen)),
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeWidthChanged(width.into()))),
        Message::Editor(EditorMsg::Image(ImageMsg::ResizeApply)),
    ]
}

#[test]
fn sync_and_async_steps_land_in_the_same_editor_state() {
    let (mut sync, mut async_) = twin_apps();
    let steps: Vec<(&str, Vec<Message>)> = vec![
        (
            "rotate",
            vec![Message::Editor(EditorMsg::Image(ImageMsg::Rotate {
                clockwise: true,
            }))],
        ),
        (
            "flip",
            vec![Message::Editor(EditorMsg::Image(ImageMsg::Flip(
                FlipAxis::Horizontal,
            )))],
        ),
        ("resize", resize_to("40")),
        (
            "crop",
            vec![
                Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::SelectTool(
                    readshot_ui::editor::ToolState::Crop,
                ))),
                Message::Editor(EditorMsg::Canvas(
                    readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Crop {
                        rect: RectLike::new(2.0, 2.0, 30.0, 40.0),
                    }),
                )),
                Message::Editor(EditorMsg::CropApply),
            ],
        ),
        (
            "add a capture (bakes the turned document)",
            vec![
                Message::Editor(EditorMsg::Combine(CombineMsg::CaptureRequested(
                    Join::Below,
                ))),
                Message::Capture(CaptureMsg::RegionCaptured(Ok(ramp(24, 12)))),
            ],
        ),
        (
            "align",
            vec![Message::Editor(EditorMsg::Combine(CombineMsg::Align(
                Align::Center,
            )))],
        ),
        ("undo", vec![undo_msg()]),
        ("undo again", vec![undo_msg()]),
        ("undo a third time", vec![undo_msg()]),
        ("redo", vec![redo_msg()]),
        ("redo again", vec![redo_msg()]),
        (
            "rotate left",
            vec![Message::Editor(EditorMsg::Image(ImageMsg::Rotate {
                clockwise: false,
            }))],
        ),
        ("undo the rotate", vec![undo_msg()]),
    ];
    for (what, messages) in &steps {
        step_both(&mut sync, &mut async_, what, messages);
    }
}

#[test]
fn a_sync_undo_clears_the_loupe_as_a_landed_async_one_does() {
    let (mut sync, mut async_) = twin_apps();
    step_both(
        &mut sync,
        &mut async_,
        "rotate",
        &[Message::Editor(EditorMsg::Image(ImageMsg::Rotate {
            clockwise: true,
        }))],
    );
    step_both(&mut sync, &mut async_, "undo", &[undo_msg()]);
    let ed = sync.editor.as_ref().unwrap();
    assert_eq!(
        ed.status_text(),
        Some(format!("Undid edit. {REDO_HINT}").as_str())
    );
    assert!(ed.eyedropper.as_ref().is_some_and(|e| e.loupe.is_none()));
}

#[test]
fn a_sync_added_part_is_recognized_through_the_owed_flag_as_a_queued_one_is() {
    let (mut sync, mut async_) = twin_apps();
    step_both(
        &mut sync,
        &mut async_,
        "add a capture",
        &[
            Message::Editor(EditorMsg::Combine(CombineMsg::CaptureRequested(
                Join::Below,
            ))),
            Message::Capture(CaptureMsg::RegionCaptured(Ok(ramp(24, 12)))),
        ],
    );
    for app in [&sync, &async_] {
        let ed = app.editor.as_ref().unwrap();
        let ids = ed.model.part_ids();
        assert!(!ed.recognition_owed, "requested once the step landed");
        assert!(ed
            .text_panel
            .recognizing_part
            .is_some_and(|p| ids.contains(&p)));
    }
}

#[test]
fn a_kept_gap_lands_as_a_step_whether_or_not_it_composed_off_thread() {
    let (mut sync, mut async_) = twins_of(two_part_editor);
    step_both(
        &mut sync,
        &mut async_,
        "gap drag released",
        &[
            Message::Editor(EditorMsg::Combine(CombineMsg::GapPreview(10))),
            Message::Editor(EditorMsg::SnapshotSliderCommit),
        ],
    );
    let ed = sync.editor.as_ref().unwrap();
    assert_eq!(ed.model.geometry().gap, 10);
    assert_eq!(
        ed.status_text(),
        Some(format!("Gap set to 10\u{a0}px. {UNDO_HINT}").as_str())
    );
    step_both(&mut sync, &mut async_, "undo the gap", &[undo_msg()]);
    assert_eq!(sync.editor.as_ref().unwrap().model.geometry().gap, 0);
}

/// The status a no-op or refused step must not take down unless it
/// replaces it: work in flight that the step has nothing to do with.
const IN_FLIGHT: &str = "Recognizing text…";

/// [`step_both`] for a step that lands nothing (no change, or refused):
/// both editors start with work in flight on the status line and a
/// filled loupe, then must agree, document, pixels and status included.
fn step_both_unlanded(sync: &mut App, async_: &mut App, what: &str, messages: &[Message]) {
    for app in [&mut *sync, &mut *async_] {
        if app
            .editor
            .as_ref()
            .is_some_and(|ed| ed.eyedropper.is_none())
        {
            let _ = update(app, Message::Editor(EditorMsg::EyedropperToggled));
        }
        let _ = update(
            app,
            Message::Editor(EditorMsg::Canvas(
                readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(3.5, 4.5)),
            )),
        );
        if let Some(ed) = app.editor.as_mut() {
            ed.set_status(crate::editor::StatusTone::Progress, IN_FLIGHT);
        }
        for m in messages {
            let _ = update(app, m.clone());
        }
        finish_all(app);
    }
    let ((a, a_doc, a_pixels), (b, b_doc, b_pixels)) = (landed(sync), landed(async_));
    assert_eq!(a, b, "{what}: sync and async disagree");
    assert!(
        same_document(&a_doc, &b_doc),
        "{what}: the documents differ"
    );
    assert!(a_pixels == b_pixels, "{what}: the flattened pixels differ");
}

/// A document with no edits: nothing to undo or redo.
fn fresh_editor() -> crate::editor::EditorSession {
    crate::editor::EditorSession::new(ramp(40, 20))
}

#[test]
fn steps_that_change_nothing_leave_work_in_flight_on_the_status_line() {
    let (mut sync, mut async_) = twins_of(fresh_editor);
    for (what, messages) in [
        ("undo with nothing to undo", vec![undo_msg()]),
        ("redo with nothing to redo", vec![redo_msg()]),
        ("resize to the same size", resize_to("40")),
    ] {
        step_both_unlanded(&mut sync, &mut async_, what, &messages);
        let ed = sync.editor.as_ref().unwrap();
        assert_eq!(ed.status_text(), Some(IN_FLIGHT), "{what}");
        assert_eq!(ed.model.undo_depth(), 0, "{what}: no undo step");
    }
    let (mut sync, mut async_) = twins_of(two_part_editor);
    step_both_unlanded(
        &mut sync,
        &mut async_,
        "align to the current alignment",
        &[Message::Editor(EditorMsg::Combine(CombineMsg::Align(
            Align::Start,
        )))],
    );
    let ed = sync.editor.as_ref().unwrap();
    assert_eq!(ed.status_text(), Some(IN_FLIGHT));
}

#[test]
fn a_refused_step_reports_the_same_refusal_on_both_paths() {
    let (mut sync, mut async_) = twin_apps();
    let depth = sync.editor.as_ref().unwrap().model.undo_depth();
    // Joined below, a part this wide is past the side limit.
    let wide = readshot_core::document::MAX_DOC_SIDE + 1;
    step_both_unlanded(
        &mut sync,
        &mut async_,
        "add a capture that is too wide",
        &[
            Message::Editor(EditorMsg::Combine(CombineMsg::CaptureRequested(
                Join::Below,
            ))),
            Message::Capture(CaptureMsg::RegionCaptured(Ok(ramp(wide, 1)))),
        ],
    );
    let ed = sync.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth, "nothing landed");
    assert!(ed.status_is_error(), "{:?}", ed.status);
    assert!(ed
        .status_text()
        .is_some_and(|s| s.starts_with("Couldn’t add the capture")));
}
