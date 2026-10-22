//! Editor tools driven through the update loop: the tool keys, the
//! options bar and real mouse events on the annotation canvas, whose
//! program is rebuilt from the app's state on every event the way
//! `stage_canvas_layer` builds it. Each message the canvas publishes is
//! fed through `update` as the running app does.

use super::*;
use iced::keyboard::{Key, Modifiers};
use iced::widget::canvas::{Event, Program};
use iced::{mouse, Point, Rectangle};
use readshot_core::{ArrowHeads, FillMode, LinePath};
use readshot_ui::editor::canvas::{DrawState, EditorCanvas};
use readshot_ui::editor::toolbar::{PALETTE, TOOL_GROUPS, TOOL_ORDER};
use readshot_ui::editor::{LinePathKind, StyleMessage, ToolState};
use readshot_ui::{CanvasMessage, ToolbarMessage};

/// An editor window over `img`, focused, at display scale 1.
fn tool_app(img: image::RgbaImage) -> (App, iced::window::Id) {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    let mut ed = crate::editor::EditorSession::new(img);
    ed.window_id = Some(id);
    app.windows.register(id, WindowKind::Editor);
    app.editor = Some(ed);
    (app, id)
}

/// A plain key on the canvas (no modifiers, no field focused).
fn key(app: &mut App, id: iced::window::Id, k: &str) {
    let _ = update(
        app,
        Message::Editor(EditorMsg::KeyPressed {
            window: id,
            key: Key::Character(k.into()),
            modifiers: Modifiers::default(),
            status_ignored: true,
        }),
    );
}

fn style(app: &mut App, m: StyleMessage) {
    let _ = update(app, Message::Editor(EditorMsg::Style(m)));
}

fn ed(app: &App) -> &crate::editor::EditorSession {
    app.editor.as_ref().unwrap()
}

fn last(app: &App) -> Annotation {
    ed(app)
        .model
        .annotations()
        .last()
        .cloned()
        .expect("a shape")
}

/// The stage's canvas: its widget state lives here, its program is
/// rebuilt from the app on every event as `view()` rebuilds it.
#[derive(Default)]
struct Stage {
    state: DrawState,
}

impl Stage {
    fn program(app: &App) -> EditorCanvas {
        let ed = ed(app);
        let selected = ed.model.selected_annotation().is_some();
        EditorCanvas {
            active_tool: ed.model.active_tool(),
            color: ed.model.current_color(),
            line_width: ed.model.current_line_width(),
            next_pin_number: ed.next_pin_number,
            image_size: ed.effective_image_size(),
            image_offset: ed.crop_offset(),
            display_scale: Some(1.0),
            selected_bounds: selected.then(|| ed.model.selected_bounds()).flatten(),
            selected_annotation: ed
                .model
                .selected_annotation()
                .and_then(|i| ed.model.annotations().get(i).cloned()),
            selected_handles: if selected {
                ed.model.selected_handles()
            } else {
                Vec::new()
            },
            selected_preview: None,
            selected_preview_rendered: false,
            pending: ed.pending_overlay(),
            bubble_covers: ed.magnifier_bubble_covers(),
            shift_held: app.editor_shift_held,
            cancel_seq: ed.drag_cancel_seq,
            style: ed.model.tool_style(),
            eyedropper: ed.eyedropper.is_some(),
            loupe: None,
            text_hover: None,
            text_selected: Vec::new(),
            crop_draft: ed.crop_draft,
            viewport: None,
            parts: Vec::new(),
            cache: ed.canvas_cache.clone(),
        }
    }

    fn event(&mut self, app: &mut App, event: Event, at: (f32, f32)) -> Option<CanvasMessage> {
        built_a_view(app);
        let program = Self::program(app);
        let (w, h) = program.image_size;
        let bounds = Rectangle::new(Point::ORIGIN, iced::Size::new(w as f32, h as f32));
        let cursor = mouse::Cursor::Available(Point::new(at.0, at.1));
        let message = program
            .update(&mut self.state, &event, bounds, cursor)?
            .into_inner()
            .0?;
        let _ = update(app, Message::Editor(EditorMsg::Canvas(message.clone())));
        Some(message)
    }

    fn press(&mut self, app: &mut App, at: (f32, f32)) -> Option<CanvasMessage> {
        self.event(
            app,
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            at,
        )
    }

    fn move_to(&mut self, app: &mut App, at: (f32, f32)) -> Option<CanvasMessage> {
        self.event(
            app,
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::new(at.0, at.1),
            }),
            at,
        )
    }

    fn release(&mut self, app: &mut App, at: (f32, f32)) -> Option<CanvasMessage> {
        self.event(
            app,
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            at,
        )
    }

    /// Press, move through `path`, release at its end; what the release
    /// published.
    fn drag(&mut self, app: &mut App, from: (f32, f32), path: &[(f32, f32)]) -> CanvasMessage {
        assert_eq!(self.press(app, from), Some(CanvasMessage::DragStarted));
        for p in path {
            self.move_to(app, *p);
        }
        self.release(app, *path.last().unwrap())
            .expect("the release publishes")
    }
}

/// Draw one shape with the active tool and check it landed as one step.
fn draw(app: &mut App, from: (f32, f32), to: (f32, f32)) -> Annotation {
    let depth = ed(app).model.undo_depth();
    let n = ed(app).model.annotations().len();
    let msg = Stage::default().drag(app, from, &[to]);
    assert!(matches!(msg, CanvasMessage::CommitAnnotation(_)), "{msg:?}");
    let e = ed(app);
    assert_eq!(e.model.annotations().len(), n + 1, "committed");
    assert_eq!(e.model.undo_depth(), depth + 1, "one undo step");
    assert!(!e.canvas_drag_active, "the drag ended");
    last(app)
}

/// Esc on the canvas: drops the selection, so the next press draws
/// instead of grabbing the last shape's handle.
fn deselect(app: &mut App) {
    let id = ed(app).window_id.unwrap();
    press_escape(app, id);
    assert_eq!(ed(app).model.selected_annotation(), None);
}

fn red() -> Rgba {
    Rgba::new(1.0, 0.0, 0.0, 1.0)
}

fn is_reddish(p: image::Rgba<u8>) -> bool {
    p[0] > 180 && p[1] < 90 && p[2] < 90
}

// ---- ED-02: the tool bar ----

/// ED-02: 15 tools in 6 groups; each tool's tooltip key, pressed on the
/// canvas, and its button both pick it.
#[test]
fn every_tool_is_picked_by_its_tooltip_key_and_its_button() {
    assert_eq!(TOOL_GROUPS.len(), 6);
    assert_eq!(TOOL_GROUPS.iter().map(|g| g.len()).sum::<usize>(), 15);
    let (mut app, id) = tool_app(solid(40, 30));
    for tool in TOOL_ORDER {
        let (name, k) = tool_label_and_key(tool);
        assert!(!name.is_empty() && k.len() == 1, "{tool:?}");
        // Start from another tool so each pick is a change.
        let other = if tool == ToolState::Rectangle {
            ToolState::Ellipse
        } else {
            ToolState::Rectangle
        };
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Toolbar(ToolbarMessage::SelectTool(other))),
        );
        app.editor.as_mut().unwrap().crop_draft = None;
        key(&mut app, id, &k.to_lowercase());
        assert_eq!(ed(&app).model.active_tool(), tool, "key {k} picks {name}");
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Toolbar(ToolbarMessage::SelectTool(other))),
        );
        app.editor.as_mut().unwrap().crop_draft = None;
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Toolbar(ToolbarMessage::SelectTool(tool))),
        );
        assert_eq!(ed(&app).model.active_tool(), tool, "the {name} button");
    }
}

// ---- ED-07 … ED-18: each tool committed through the canvas ----

/// ED-07: O, then Fill, Dash and Color: the drag commits an ellipse with
/// them, and never a corner radius.
#[test]
fn the_ellipse_tool_draws_with_its_options_and_no_corner_radius() {
    let (mut app, id) = tool_app(solid(60, 40));
    key(&mut app, id, "o");
    assert_eq!(ed(&app).model.active_tool(), ToolState::Ellipse);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(ToolbarMessage::SelectColor(red()))),
    );
    style(&mut app, StyleMessage::Fill(FillMode::Filled));
    style(&mut app, StyleMessage::Dashed(true));
    style(&mut app, StyleMessage::CornerRadius(12.0));
    match draw(&mut app, (10.0, 5.0), (50.0, 35.0)) {
        Annotation::Ellipse {
            rect, color, style, ..
        } => {
            assert_eq!(
                (rect.x, rect.y, rect.width, rect.height),
                (10.0, 5.0, 40.0, 30.0)
            );
            assert_eq!(color, red());
            assert_eq!(style.fill, FillMode::Filled);
            assert!(style.dashed);
            assert_eq!(style.corner_radius, 0.0, "ellipses take no radius");
        }
        other => panic!("{other:?}"),
    }
    let mut e = app.editor.take().unwrap();
    let flat = e.model.flatten();
    assert!(is_reddish(*flat.get_pixel(30, 20)), "filled centre");
    assert_eq!(
        *flat.get_pixel(11, 6),
        image::Rgba([255; 4]),
        "outside the curve"
    );
}

/// ED-08: L draws a line; Curved gets a bend, Hand-drawn a seed from its
/// anchor (the same anchor draws the same wobble); restyling the drawn
/// line keeps its bend.
#[test]
fn the_line_tool_draws_each_path_style_and_restyling_keeps_the_bend() {
    let (mut app, id) = tool_app(solid(80, 60));
    key(&mut app, id, "l");
    assert_eq!(ed(&app).model.active_tool(), ToolState::Line);
    let straight = draw(&mut app, (5.0, 5.0), (70.0, 50.0));
    assert!(matches!(
        straight,
        Annotation::Line { style, .. } if style.path == LinePath::Straight && style.heads == ArrowHeads::End
    ));
    style(&mut app, StyleMessage::LinePath(LinePathKind::HandDrawn));
    let hand = |a: Annotation| match a {
        Annotation::Line {
            style:
                readshot_core::LineStyle {
                    path: LinePath::HandDrawn { seed },
                    ..
                },
            ..
        } => seed,
        other => panic!("{other:?}"),
    };
    deselect(&mut app);
    let s1 = hand(draw(&mut app, (10.0, 10.0), (60.0, 20.0)));
    deselect(&mut app);
    let s2 = hand(draw(&mut app, (10.0, 10.0), (40.0, 50.0)));
    deselect(&mut app);
    let s3 = hand(draw(&mut app, (30.0, 12.0), (60.0, 20.0)));
    assert_eq!(s1, s2, "seeded from the anchor");
    assert_ne!(s1, s3, "another anchor, another wobble");
    deselect(&mut app);
    style(&mut app, StyleMessage::LinePath(LinePathKind::Curved));
    let bend = match draw(&mut app, (5.0, 30.0), (75.0, 30.0)) {
        Annotation::Line {
            style:
                readshot_core::LineStyle {
                    path: LinePath::Curved { bend },
                    ..
                },
            ..
        } => bend,
        other => panic!("{other:?}"),
    };
    assert_ne!(bend, 0.0, "a curved line bends");
    // The new line is selected: dashing it keeps its bend, one step.
    let depth = ed(&app).model.undo_depth();
    style(&mut app, StyleMessage::Dashed(true));
    assert_eq!(ed(&app).model.undo_depth(), depth + 1);
    assert!(matches!(
        last(&app),
        Annotation::Line { style, .. } if style.dashed && style.path == LinePath::Curved { bend }
    ));
}

/// ED-09: A draws an arrow with one head; Heads › Both gives it two.
#[test]
fn the_arrow_tool_draws_one_head_or_two() {
    let (mut app, id) = tool_app(solid(80, 60));
    key(&mut app, id, "a");
    assert_eq!(ed(&app).model.active_tool(), ToolState::Arrow);
    match draw(&mut app, (5.0, 5.0), (70.0, 50.0)) {
        Annotation::Arrow { a, b, style, .. } => {
            assert_eq!((a.x, a.y, b.x, b.y), (5.0, 5.0, 70.0, 50.0));
            assert_eq!(style.heads, ArrowHeads::End);
        }
        other => panic!("{other:?}"),
    }
    // Deselect, so the option is for the next arrow only.
    app.editor.as_mut().unwrap().model.clear_selection();
    style(&mut app, StyleMessage::Heads(ArrowHeads::Both));
    assert!(matches!(
        draw(&mut app, (5.0, 50.0), (70.0, 10.0)),
        Annotation::Arrow { style, .. } if style.heads == ArrowHeads::Both
    ));
    assert!(matches!(
        ed(&app).model.annotations()[0],
        Annotation::Arrow { style, .. } if style.heads == ArrowHeads::End
    ));
    // The second head is drawn.
    let both = last(&app);
    let mut end = both.clone();
    if let Annotation::Arrow { style, .. } = &mut end {
        style.heads = ArrowHeads::End;
    }
    let render = |a: Annotation| {
        let mut only = crate::editor::EditorSession::new(solid(80, 60));
        only.model.commit_annotation(a);
        only.model.flatten()
    };
    assert!(render(both) != render(end), "the second head is drawn");
}

/// ED-10: P draws a freehand stroke through every point dragged.
#[test]
fn the_pen_tool_draws_a_freehand_stroke() {
    let (mut app, id) = tool_app(solid(60, 40));
    key(&mut app, id, "p");
    key(&mut app, id, "4");
    let path = [(20.0, 10.0), (30.0, 25.0), (50.0, 30.0)];
    let depth = ed(&app).model.undo_depth();
    let msg = Stage::default().drag(&mut app, (5.0, 5.0), &path);
    assert!(matches!(msg, CanvasMessage::CommitAnnotation(_)));
    assert_eq!(ed(&app).model.undo_depth(), depth + 1);
    match last(&app) {
        Annotation::Pen {
            points,
            color,
            line_width,
        } => {
            let pts: Vec<(f32, f32)> = points.iter().map(|p| (p.x, p.y)).collect();
            assert_eq!(pts, [(5.0, 5.0), (20.0, 10.0), (30.0, 25.0), (50.0, 30.0)]);
            assert_eq!(color, red());
            assert_eq!(line_width, 4.0);
        }
        other => panic!("{other:?}"),
    }
    let flat = app.editor.as_mut().unwrap().model.flatten();
    assert!(is_reddish(*flat.get_pixel(30, 25)), "ink at a stroke point");
    assert_eq!(*flat.get_pixel(55, 5), image::Rgba([255; 4]));
}

/// ED-15: B blurs under the dragged rectangle (radius = Width) and it
/// counts as a redaction; nothing outside changes.
#[test]
fn the_blur_tool_blurs_only_under_its_rectangle_and_is_a_redaction() {
    let (mut app, id) = tool_app(ramp(64, 40));
    let before = ramp(64, 40);
    key(&mut app, id, "b");
    key(&mut app, id, "6");
    let a = draw(&mut app, (16.0, 8.0), (48.0, 32.0));
    assert!(matches!(a, Annotation::Blur { radius, .. } if radius == 6.0));
    assert_eq!(
        a.redaction_rect(),
        Some(RectLike::new(16.0, 8.0, 32.0, 24.0))
    );
    let flat = app.editor.as_mut().unwrap().model.flatten();
    let changed = (8..32)
        .flat_map(|y| (16..48).map(move |x| (x, y)))
        .filter(|&(x, y)| flat.get_pixel(x, y) != before.get_pixel(x, y))
        .count();
    assert!(
        changed > 32 * 24 / 2,
        "most pixels inside change ({changed})"
    );
    for (x, y) in [(15, 20), (48, 20), (30, 7), (30, 32), (0, 0), (63, 39)] {
        assert_eq!(flat.get_pixel(x, y), before.get_pixel(x, y), "({x}, {y})");
    }
}

/// ED-16: X pixelates under the dragged rectangle in Width-sized cells
/// and it counts as a redaction; nothing outside changes.
#[test]
fn the_pixelate_tool_fills_each_cell_with_one_colour_and_is_a_redaction() {
    let (mut app, id) = tool_app(ramp(64, 40));
    let before = ramp(64, 40);
    key(&mut app, id, "x");
    key(&mut app, id, "8");
    let a = draw(&mut app, (16.0, 8.0), (48.0, 24.0));
    assert!(matches!(a, Annotation::Pixelate { block_size, .. } if block_size == 8.0));
    assert!(a.redaction_rect().is_some());
    let flat = app.editor.as_mut().unwrap().model.flatten();
    for (cx, cy) in [(16, 8), (24, 8), (40, 16)] {
        let first = *flat.get_pixel(cx, cy);
        for y in cy..cy + 8 {
            for x in cx..cx + 8 {
                assert_eq!(
                    *flat.get_pixel(x, y),
                    first,
                    "cell ({cx}, {cy}) at ({x}, {y})"
                );
            }
        }
        assert_ne!(first, *before.get_pixel(cx, cy), "averaged");
    }
    for (x, y) in [(15, 12), (48, 12), (20, 7), (20, 24)] {
        assert_eq!(flat.get_pixel(x, y), before.get_pixel(x, y), "({x}, {y})");
    }
}

/// ED-18: N, then each click places the next number from 1. Undoing a
/// pin hands its number back (4dd05a4 changed this on purpose; the
/// matrix's "not decremented on undo" predates it), and redo takes it
/// again.
#[test]
fn numbered_pins_count_up_per_click_and_undo_hands_the_number_back() {
    let (mut app, id) = tool_app(solid(80, 40));
    key(&mut app, id, "n");
    let mut stage = Stage::default();
    let number = |m: Option<CanvasMessage>| match m {
        Some(CanvasMessage::CommitAnnotation(Annotation::NumberedPin { number, .. })) => number,
        other => panic!("{other:?}"),
    };
    let placed: Vec<u32> = [(10.0, 10.0), (30.0, 10.0), (50.0, 10.0)]
        .into_iter()
        .map(|p| number(stage.press(&mut app, p)))
        .collect();
    assert_eq!(placed, [1, 2, 3]);
    assert_eq!(ed(&app).model.annotations().len(), 3);
    let _ = update(&mut app, undo_msg());
    assert_eq!(ed(&app).model.annotations().len(), 2);
    assert_eq!(ed(&app).next_pin_number, 3);
    let _ = update(&mut app, redo_msg());
    assert_eq!(ed(&app).next_pin_number, 4, "redo takes 3 again");
    let _ = update(&mut app, undo_msg());
    assert_eq!(number(stage.press(&mut app, (70.0, 10.0))), 3);
    assert!(matches!(
        last(&app),
        Annotation::NumberedPin { number: 3, color, .. } if color == red()
    ));
    assert_eq!(number(stage.press(&mut app, (70.0, 30.0))), 4);
}

// ---- ED-25 / ED-26: palette and width keys ----

/// ED-25: `.` / `,` step through the 12 presets and wrap, applying each
/// to the selection as its own step.
#[test]
fn the_palette_keys_cycle_the_presets_and_recolour_the_selection() {
    let (mut app, id) = tool_app(solid(64, 64));
    app.editor.as_mut().unwrap().model.set_color(PALETTE[0]);
    app.editor
        .as_mut()
        .unwrap()
        .model
        .commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: PALETTE[0],
            line_width: 2.0,
            style: Default::default(),
        });
    let depth = ed(&app).model.undo_depth();
    let color_of = |app: &App| match ed(app).model.annotations()[0] {
        Annotation::Rectangle { color, .. } => color,
        ref other => panic!("{other:?}"),
    };
    for i in 1..=12 {
        key(&mut app, id, ".");
        let want = PALETTE[i % 12];
        assert_eq!(ed(&app).model.current_color(), want, "step {i}");
        assert_eq!(color_of(&app), want, "the selection follows");
    }
    assert_eq!(ed(&app).model.undo_depth(), depth + 12, "a step each");
    key(&mut app, id, ",");
    assert_eq!(ed(&app).model.current_color(), PALETTE[11], "wraps back");
    assert_eq!(color_of(&app), PALETTE[11]);
    // With a modifier the keys are not the palette.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::KeyPressed {
            window: id,
            key: Key::Character(".".into()),
            modifiers: command_modifiers(),
            status_ignored: true,
        }),
    );
    assert_eq!(ed(&app).model.current_color(), PALETTE[11]);
}

/// ED-26: digits 1–9 set the width of the next shape and of the
/// selection (one step each); 0 and a digit typed into a field don't.
#[test]
fn digit_keys_set_the_line_width_for_the_next_shape_and_the_selection() {
    let (mut app, id) = tool_app(solid(64, 64));
    key(&mut app, id, "r");
    for d in 1..=9 {
        key(&mut app, id, &d.to_string());
        assert_eq!(ed(&app).model.current_line_width(), d as f32, "key {d}");
    }
    key(&mut app, id, "3");
    match draw(&mut app, (5.0, 5.0), (40.0, 40.0)) {
        Annotation::Rectangle { line_width, .. } => assert_eq!(line_width, 3.0),
        other => panic!("{other:?}"),
    }
    // The drawn rectangle is selected: 7 restyles it as one step.
    let depth = ed(&app).model.undo_depth();
    key(&mut app, id, "7");
    assert_eq!(ed(&app).model.undo_depth(), depth + 1);
    assert!(matches!(last(&app), Annotation::Rectangle { line_width, .. } if line_width == 7.0));
    let _ = update(&mut app, undo_msg());
    assert!(matches!(last(&app), Annotation::Rectangle { line_width, .. } if line_width == 3.0));
    // 0 is not a width; a digit a text field took never reaches here.
    key(&mut app, id, "0");
    assert_eq!(ed(&app).model.current_line_width(), 7.0);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::KeyPressed {
            window: id,
            key: Key::Character("5".into()),
            modifiers: Modifiers::default(),
            status_ignored: false,
        }),
    );
    assert_eq!(ed(&app).model.current_line_width(), 7.0);
}

// ---- ED-28: Shift constrains the drag ----

/// ED-28: with Shift held (`EditorShiftChanged`), rectangle and
/// spotlight drags commit squares, ellipse a circle, line and arrow snap
/// to 45°; let go of Shift before the release and the drag is free.
#[test]
fn holding_shift_constrains_each_shape_drag() {
    let (mut app, id) = tool_app(solid(100, 100));
    let shift = |app: &mut App, held: bool| {
        let _ = update(app, Message::Window(WindowMsg::EditorShiftChanged(held)));
        assert_eq!(app.editor_shift_held, held);
    };
    shift(&mut app, true);
    for k in ["r", "o", "s"] {
        key(&mut app, id, k);
        deselect(&mut app);
        let rect = match draw(&mut app, (10.0, 10.0), (50.0, 30.0)) {
            Annotation::Rectangle { rect, .. }
            | Annotation::Ellipse { rect, .. }
            | Annotation::Spotlight { rect, .. } => rect,
            other => panic!("{other:?}"),
        };
        assert_eq!(rect.width, rect.height, "{k}: square");
    }
    key(&mut app, id, "l");
    deselect(&mut app);
    match draw(&mut app, (10.0, 10.0), (60.0, 22.0)) {
        Annotation::Line { a, b, .. } => assert_eq!(b.y, a.y, "snapped flat"),
        other => panic!("{other:?}"),
    }
    key(&mut app, id, "a");
    deselect(&mut app);
    match draw(&mut app, (10.0, 10.0), (50.0, 46.0)) {
        Annotation::Arrow { a, b, .. } => {
            assert!(((b.x - a.x) - (b.y - a.y)).abs() < 0.01, "45°: {a:?} {b:?}")
        }
        other => panic!("{other:?}"),
    }
    // Released mid-drag: the commit follows the key state at release.
    key(&mut app, id, "r");
    deselect(&mut app);
    let mut stage = Stage::default();
    stage.press(&mut app, (10.0, 10.0));
    stage.move_to(&mut app, (50.0, 30.0));
    shift(&mut app, false);
    let msg = stage.release(&mut app, (50.0, 30.0));
    assert!(matches!(
        msg,
        Some(CanvasMessage::CommitAnnotation(Annotation::Rectangle { rect, .. }))
            if (rect.width, rect.height) == (40.0, 20.0)
    ));
}

// ---- IM-09: the Image menu while busy ----

/// IM-09: while the editor is busy (a copy in flight) every Image-menu
/// action is inert: nothing moves, nothing starts.
#[test]
fn image_menu_actions_do_nothing_while_the_editor_is_busy() {
    use readshot_core::document::{Align, FlipAxis, Join};
    let (mut app, _) = tool_app(ramp(40, 20));
    app.editor
        .as_mut()
        .unwrap()
        .model
        .add_part(ramp(20, 10), Join::Below)
        .unwrap();
    app.editor.as_mut().unwrap().refresh_image();
    let _ = update(&mut app, Message::Editor(EditorMsg::CopyImageRequested));
    assert!(ed(&app).busy, "the copy is in flight (its task not run)");
    let before = ed(&app).model.snapshot();
    let size = ed(&app).image_size();
    for msg in [
        ImageMsg::ResizeOpen,
        ImageMsg::Rotate { clockwise: true },
        ImageMsg::Rotate { clockwise: false },
        ImageMsg::Flip(FlipAxis::Horizontal),
        ImageMsg::Flip(FlipAxis::Vertical),
    ] {
        let task = update(&mut app, Message::Editor(EditorMsg::Image(msg.clone())));
        assert_eq!(task.units(), 0, "{msg:?}");
    }
    for msg in [
        CombineMsg::CaptureRequested(Join::Below),
        CombineMsg::CaptureRequested(Join::Beside),
        CombineMsg::AddImageFromFile,
        CombineMsg::Align(Align::End),
        CombineMsg::GapPreview(12),
    ] {
        let task = update(&mut app, Message::Editor(EditorMsg::Combine(msg.clone())));
        assert_eq!(task.units(), 0, "{msg:?}");
    }
    let e = ed(&app);
    assert_eq!(e.model.snapshot(), before);
    assert_eq!(e.image_size(), size);
    assert_eq!(e.popover, None, "Resize didn't open");
    assert!(e.resize_draft.is_none());
    assert!(app.combine_capture.is_none());
    assert!(app.pending_intent.is_none());
    assert!(e.compose_job.is_none());
}

// ---- BF-02 / BF-06 / BF-08: Beautify through the update loop ----

/// BF-02: each built-in preset chosen in the panel is that preset's
/// settings, one step, and the output is its render; No Frame outputs
/// the source unchanged.
#[test]
fn each_builtin_preset_sets_its_settings_and_renders_them() {
    let (mut app, _) = tool_app(ramp(40, 24));
    for preset in BuiltinPreset::ALL.into_iter().rev() {
        let depth = ed(&app).model.undo_depth();
        let _ = update(&mut app, Message::Editor(EditorMsg::BeautifyPreset(preset)));
        let e = app.editor.as_mut().unwrap();
        assert_eq!(e.model.beautify(), preset.settings(), "{preset:?}");
        assert_eq!(e.model.beautify().matching_builtin(), Some(preset));
        assert_eq!(e.model.undo_depth(), depth + 1, "{preset:?}: one step");
        let flat = e.model.flatten();
        let want = readshot_core::beautify::render(&flat, &preset.settings()).unwrap();
        let got = editor_output_image(e).unwrap();
        assert!(got == want, "{preset:?}");
        if preset == BuiltinPreset::None {
            assert!(got == ramp(40, 24), "No Frame is the source");
        } else {
            assert_ne!(got.dimensions(), (40, 24), "{preset:?} frames it");
        }
    }
}

/// BF-06: the five shadow stops, each one step and each its own look.
#[test]
fn each_shadow_level_is_one_step_with_its_own_shadow() {
    use readshot_core::beautify::{BeautifyEdit, SHADOW_LEVELS};
    let (mut app, _) = tool_app(solid(40, 24));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::BeautifyPreset(BuiltinPreset::Light)),
    );
    let mut outputs = Vec::new();
    for level in [4u8, 3, 2, 1, 0] {
        let depth = ed(&app).model.undo_depth();
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Beautify(BeautifyMsg::Edit(
                BeautifyEdit::ShadowLevel(level),
            ))),
        );
        let e = app.editor.as_mut().unwrap();
        assert_eq!(e.model.beautify().shadow, SHADOW_LEVELS[usize::from(level)]);
        assert_eq!(e.model.undo_depth(), depth + 1, "level {level}");
        outputs.push(editor_output_image(e).unwrap());
    }
    for (i, a) in outputs.iter().enumerate() {
        for b in &outputs[i + 1..] {
            assert!(a != b, "each level looks different");
        }
    }
    let _ = update(&mut app, undo_msg());
    assert_eq!(ed(&app).model.beautify().shadow, SHADOW_LEVELS[1]);
}

/// BF-08: Auto Balance centres the image on a ratio canvas; off, it sits
/// at the padding with the extra space below.
#[test]
fn auto_balance_centres_the_image_on_the_canvas() {
    use readshot_core::beautify::{Background, BeautifyEdit, CanvasSize};
    let (mut app, _) = tool_app(solid(40, 20));
    let red_bg = Background::Solid {
        rgba: [255, 0, 0, 255],
    };
    for edit in [
        BeautifyEdit::Background(red_bg),
        BeautifyEdit::Padding(10),
        BeautifyEdit::Canvas(CanvasSize::Square),
    ] {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Beautify(BeautifyMsg::Edit(edit))),
        );
    }
    let mut at = |balance: bool| {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Beautify(BeautifyMsg::Edit(
                BeautifyEdit::Balance(balance),
            ))),
        );
        let e = app.editor.as_mut().unwrap();
        assert_eq!(e.model.beautify().balance, balance);
        let out = editor_output_image(e).unwrap();
        assert_eq!(out.dimensions(), (60, 60));
        // Rows where the white image starts and ends at x 30.
        let rows: Vec<u32> = (0..60).filter(|&y| out.get_pixel(30, y)[1] > 200).collect();
        (rows[0], *rows.last().unwrap())
    };
    assert_eq!(at(true), (20, 39), "centred");
    assert_eq!(at(false), (10, 29), "at the padding");
}

// ---- BF-07: the over-limit canvas, chosen in the panel ----

/// BF-07: choosing 16:9 in the panel on a very tall capture with a wide
/// padding is kept, with the inline reason; Copy, Save and Pin refuse it;
/// choosing Social makes it again.
#[test]
fn an_over_limit_canvas_chosen_in_the_panel_is_explained_and_refused() {
    use readshot_core::beautify::{BeautifyEdit, CanvasSize};
    let (mut app, id) = tool_app(solid(100, 20_000));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::BeautifyPreset(BuiltinPreset::Minimal)),
    );
    drag_padding(&mut app, &[100, 200]);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::Edit(
            BeautifyEdit::Canvas(CanvasSize::Wide),
        ))),
    );
    let e = ed(&app);
    assert_eq!(
        e.model.beautify().canvas,
        CanvasSize::Wide,
        "the choice stands"
    );
    let reason = super::super::beautify_panel::beautify_output_refusal(e).expect("explained");
    assert!(
        reason.ends_with("Choose Auto or Social canvas, or lower the padding."),
        "{reason}"
    );
    for (msg, verb) in [
        (EditorMsg::CopyImageRequested, "copy"),
        (EditorMsg::SaveRequested, "save"),
        (EditorMsg::PinRequested, "pin"),
    ] {
        let task = update(&mut app, Message::Editor(msg));
        assert_eq!(task.units(), 0, "{verb}: nothing started");
        let e = ed(&app);
        assert!(!e.busy);
        assert!(
            e.status_text()
                .is_some_and(|s| s.starts_with(&format!("Couldn’t {verb}."))),
            "{verb}: {:?}",
            e.status_text()
        );
    }
    assert!(app.pins.is_empty());
    assert_eq!(app.windows.kind(id), Some(WindowKind::Editor));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::Edit(
            BeautifyEdit::Canvas(CanvasSize::Social),
        ))),
    );
    let e = app.editor.as_mut().unwrap();
    assert!(super::super::beautify_panel::beautify_output_refusal(e).is_none());
    assert_eq!(editor_output_image(e).unwrap().dimensions(), (1200, 675));
}

// ---- BF-11: outputs carry the Beautify, History doesn't ----

/// BF-11: with Dark Card chosen, Copy, Save and Pin each take the
/// beautified output (the one `editor_output` renders, which all three
/// use), while the redacted History record stays the bare document.
#[test]
fn copy_save_and_pin_take_the_beautified_output_and_history_the_bare_one() {
    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(FsHistoryStore::new(dir.path().join("h")));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 24, "d");
    store
        .save(&record, &readshot_core::encode_png(&ramp(40, 24)).unwrap())
        .unwrap();
    app.editor = Some(crate::editor::EditorSession::from_history(
        ramp(40, 24),
        record.clone(),
    ));
    let mut stage = Stage::default();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(ToolbarMessage::SelectTool(
            ToolState::Pixelate,
        ))),
    );
    stage.drag(&mut app, (4.0, 4.0), &[(20.0, 16.0)]);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::BeautifyPreset(BuiltinPreset::Dark)),
    );
    let e = app.editor.as_mut().unwrap();
    let flat = e.model.flatten();
    let want = readshot_core::beautify::render(&flat, &BuiltinPreset::Dark.settings()).unwrap();
    assert!(want.width() > 40 && want.height() > 24);
    assert!(editor_output_image(e).unwrap() == want);

    let task = update(&mut app, Message::Editor(EditorMsg::CopyImageRequested));
    assert_eq!(task.units(), 1, "the copy runs off the UI thread");
    assert_eq!(ed(&app).status_text(), Some("Copying beautified image…"));
    let session = ed(&app).session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::CopyImageDone {
            session,
            result: Ok(()),
        }),
    );
    let task = update(&mut app, Message::Editor(EditorMsg::SaveRequested));
    assert_eq!(task.units(), 1);
    assert_eq!(ed(&app).status_text(), Some("Choose a save location…"));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Saved {
            session,
            result: Ok(None),
        }),
    );
    assert!(editor_output_image(app.editor.as_mut().unwrap()).unwrap() == want);
    let task = update(&mut app, Message::Editor(EditorMsg::PinRequested));
    assert!(app.editor.is_none(), "the editor closed");
    assert!(app.pins.is_empty(), "the beautified pin renders first");
    assert_eq!(task.units(), 1, "the off-thread render");
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Pin(PinMsg::Rendered {
            result: Ok(want.clone()),
            display_bounds: None,
        })),
    );
    assert_eq!(app.pins.len(), 1);

    app.coordinator.flush_history_updates();
    let (stored, png) = stored(&store);
    assert_eq!(stored.id, record.id);
    assert!(stored.redacted);
    assert_eq!(png.dimensions(), (40, 24), "History keeps no frame");
    assert!(png == flat, "the scrubbed document itself");
}

// ---- CB-02 / CB-07: Add Capture Beside, and its undo ----

/// CB-02, CB-07: a successful Add Capture Beside lands to the right in
/// the same editor; Undo removes the part, Redo puts it back.
#[test]
fn add_capture_beside_appends_to_the_right_and_undo_removes_it() {
    use readshot_core::document::Join;
    let (mut app, _) = tool_app(ramp(40, 20));
    let session = ed(&app).session_id;
    let depth = ed(&app).model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Combine(CombineMsg::CaptureRequested(
            Join::Beside,
        ))),
    );
    assert!(matches!(
        app.combine_capture,
        Some(crate::app::CombineCapture {
            join: Join::Beside,
            ..
        })
    ));
    let added = solid(16, 12);
    let _ = update(
        &mut app,
        Message::Capture(CaptureMsg::RegionCaptured(Ok(added.clone()))),
    );
    let e = app.editor.as_mut().unwrap();
    assert_eq!(e.session_id, session, "no new editor");
    assert_eq!(e.model.part_ids().len(), 2);
    let g = e.model.geometry();
    let gap = g.gap;
    assert_eq!(e.image_size(), (40 + gap + 16, 20), "beside, not below");
    let right = g.part_rect(e.model.part_ids()[1]).unwrap();
    assert_eq!(
        (right.x, right.width, right.height),
        ((40 + gap) as f32, 16.0, 12.0)
    );
    assert_eq!(
        e.status_text(),
        Some(format!("Added capture beside. {UNDO_HINT}").as_str())
    );
    assert_eq!(e.model.undo_depth(), depth + 1, "one step");
    let flat = e.model.flatten();
    assert_eq!(flat.get_pixel(5, 5), ramp(40, 20).get_pixel(5, 5));
    assert_eq!(*flat.get_pixel(40 + gap + 2, 2), image::Rgba([255; 4]));
    assert!(app.combine_capture.is_none());
    let _ = update(&mut app, undo_msg());
    finish_all(&mut app);
    let e = ed(&app);
    assert_eq!(e.model.part_ids().len(), 1);
    assert_eq!(e.image_size(), (40, 20));
    let _ = update(&mut app, redo_msg());
    finish_all(&mut app);
    assert_eq!(ed(&app).image_size(), (40 + gap + 16, 20));
}

// ---- ED-45: the Ctrl+Shift chords off macOS ----

/// ED-45: off macOS the second-level chords are Ctrl+Shift+{P,R,T,B}
/// and the Shift chords Ctrl+Shift+{I,H,V}; Ctrl+Alt runs none of them,
/// and their labels name Ctrl, never a Mac glyph.
#[cfg(not(target_os = "macos"))]
#[test]
fn off_macos_the_ctrl_shift_chords_run_and_ctrl_alt_does_not() {
    use crate::editor::{EditorPopover, PanelTab};
    use crate::runtime::key_focus as kf;
    let chord = |app: &mut App, id: iced::window::Id, k: &str, alt: bool| {
        let mut m = Modifiers::CTRL;
        m.insert(if alt {
            Modifiers::ALT
        } else {
            Modifiers::SHIFT
        });
        let _ = update(
            app,
            Message::Editor(EditorMsg::KeyPressed {
                window: id,
                key: Key::Character(k.into()),
                modifiers: m,
                status_ignored: true,
            }),
        );
    };
    // Ctrl+Alt: nothing.
    let (mut app, id) = tool_app(ramp(40, 20));
    let panel = app.preferences.editor_panel_open;
    let tab = app.preferences.editor_panel_tab.clone();
    for k in ["p", "r", "t", "b", "i", "h", "v"] {
        chord(&mut app, id, k, true);
    }
    assert_eq!(app.preferences.editor_panel_open, panel);
    assert_eq!(app.preferences.editor_panel_tab, tab);
    assert_eq!(ed(&app).popover, None);
    assert!(app.editor.as_mut().unwrap().model.flatten() == ramp(40, 20));
    // Ctrl+Shift: each runs.
    chord(&mut app, id, "p", false);
    assert_eq!(app.preferences.editor_panel_open, !panel, "Ctrl+Shift+P");
    chord(&mut app, id, "t", false);
    assert_eq!(
        PanelTab::from_id(&app.preferences.editor_panel_tab),
        PanelTab::Text
    );
    chord(&mut app, id, "b", false);
    assert_eq!(
        PanelTab::from_id(&app.preferences.editor_panel_tab),
        PanelTab::Beautify
    );
    app.editor.as_mut().unwrap().key_focus = None;
    chord(&mut app, id, "r", false);
    assert_eq!(
        ed(&app).popover,
        Some(EditorPopover::Resize),
        "Ctrl+Shift+R"
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::PopoverClosed));
    app.editor.as_mut().unwrap().key_focus = None;
    chord(&mut app, id, "i", false);
    assert_eq!(ed(&app).popover, Some(EditorPopover::Image), "Ctrl+Shift+I");
    let _ = update(&mut app, Message::Editor(EditorMsg::PopoverClosed));
    app.editor.as_mut().unwrap().key_focus = None;
    chord(&mut app, id, "h", false);
    let flat = app.editor.as_mut().unwrap().model.flatten();
    assert_eq!(
        flat.get_pixel(0, 0),
        ramp(40, 20).get_pixel(39, 0),
        "flipped"
    );
    chord(&mut app, id, "v", false);
    let flat = app.editor.as_mut().unwrap().model.flatten();
    assert_eq!(
        flat.get_pixel(0, 0),
        ramp(40, 20).get_pixel(39, 19),
        "flipped"
    );
    for (label, want) in [
        (kf::KEY_PANEL, "Ctrl+Shift+P"),
        (kf::KEY_RESIZE, "Ctrl+Shift+R"),
        (kf::KEY_TEXT_TAB, "Ctrl+Shift+T"),
        (kf::KEY_BEAUTIFY_TAB, "Ctrl+Shift+B"),
        (kf::KEY_IMAGE_MENU, "Ctrl+Shift+I"),
        (kf::KEY_FLIP_H, "Ctrl+Shift+H"),
        (kf::KEY_FLIP_V, "Ctrl+Shift+V"),
    ] {
        assert_eq!(kf::key_label(label), want);
    }
}
