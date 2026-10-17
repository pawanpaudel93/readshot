//! Editor keyboard paths (WCAG 2.1.1 / 2.4.3 / 2.5.7): the focus zones
//! (Image menu, options bar, side panel, Resize), Tab through the
//! annotations, the keyboard crop and ⌥ + arrow resizing. The editor's
//! whole key map is checked for collisions here too.

use super::*;
use crate::editor::{EditorPopover, FocusItem, FocusZone, KeyFocus, PanelTab};
use iced::keyboard::{key::Named, Key, Modifiers};

fn mods(cmd: bool, shift: bool, alt: bool, ctrl: bool) -> Modifiers {
    let mut m = if cmd {
        command_modifiers()
    } else {
        Modifiers::default()
    };
    if shift {
        m.insert(Modifiers::SHIFT);
    }
    if alt {
        m.insert(Modifiers::ALT);
    }
    if ctrl {
        m.insert(Modifiers::CTRL);
    }
    m
}

fn ch(c: &str) -> Key {
    Key::Character(c.into())
}

fn named(n: Named) -> Key {
    Key::Named(n)
}

/// A focused editor window in `app`.
fn editor_app(ed: crate::editor::EditorSession) -> (App, iced::window::Id) {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    let mut ed = ed;
    ed.window_id = Some(id);
    app.editor = Some(ed);
    (app, id)
}

fn press(app: &mut App, id: iced::window::Id, key: Key, m: Modifiers) {
    let _ = update(
        app,
        Message::EditorKeyPressed {
            window: id,
            key,
            modifiers: m,
            status_ignored: true,
        },
    );
}

/// A key a focused text field took (status captured).
fn press_captured(app: &mut App, id: iced::window::Id, key: Key, m: Modifiers) {
    let _ = update(
        app,
        Message::EditorKeyPressed {
            window: id,
            key,
            modifiers: m,
            status_ignored: false,
        },
    );
}

fn plain() -> Modifiers {
    Modifiers::default()
}

fn focus(app: &App) -> Option<KeyFocus> {
    app.editor.as_ref().unwrap().key_focus
}

fn at(zone: FocusZone, item: FocusItem) -> Option<KeyFocus> {
    Some(KeyFocus { zone, item })
}

fn debug(m: Option<Message>) -> String {
    format!("{m:?}")
}

/// Every editor chord, old and new, with what it must do. No two rows
/// share a chord, and each still reaches its own action, so a new key
/// can't have shadowed an old one.
#[test]
fn every_editor_chord_is_unique_and_reaches_its_action() {
    let rows: Vec<(Key, Modifiers, &str)> = vec![
        // Existing chords.
        (ch("z"), mods(true, false, false, false), "Undo"),
        (ch("z"), mods(true, true, false, false), "Redo"),
        (
            ch("c"),
            mods(true, false, false, false),
            "EditorCopyImageRequested",
        ),
        (
            ch("c"),
            mods(true, true, false, false),
            "EditorCopyTextRequested",
        ),
        (
            ch("p"),
            mods(true, false, false, false),
            "EditorPinRequested",
        ),
        (
            ch("p"),
            mods(true, false, true, false),
            "EditorPanelToggled",
        ),
        (
            ch("s"),
            mods(true, false, false, false),
            "EditorSaveRequested",
        ),
        (
            ch("w"),
            mods(true, false, false, false),
            "EditorDiscardRequested",
        ),
        (ch("i"), mods(true, false, true, false), "EditorResizeOpen"),
        (ch("["), mods(true, false, false, false), "clockwise: false"),
        (ch("]"), mods(true, false, false, false), "clockwise: true"),
        (ch("="), mods(true, false, false, false), "EditorZoomIn"),
        (ch("-"), mods(true, false, false, false), "EditorZoomOut"),
        (ch("0"), mods(true, false, false, false), "EditorZoomActual"),
        (ch("i"), plain(), "EditorEyedropperToggled"),
        (ch("c"), plain(), "SelectTool(Crop)"),
        (ch("["), plain(), "EditorWidthBump(-1.0)"),
        (ch(","), plain(), "EditorColorCycle(-1)"),
        (
            named(Named::ArrowLeft),
            plain(),
            "EditorNudgeSelected(-1.0, 0.0)",
        ),
        (named(Named::Enter), plain(), "EditorEditSelectedText"),
        (named(Named::Backspace), plain(), "EditorDeleteSelected"),
        // New chords.
        (
            ch("i"),
            mods(true, true, false, false),
            "EditorImageMenuKey",
        ),
        (
            ch("h"),
            mods(true, true, false, false),
            "EditorFlip(Horizontal)",
        ),
        (
            ch("v"),
            mods(true, true, false, false),
            "EditorFlip(Vertical)",
        ),
        (
            ch("t"),
            mods(true, false, true, false),
            "EditorPanelTabKey(Text)",
        ),
        (
            ch("b"),
            mods(true, false, true, false),
            "EditorPanelTabKey(Beautify)",
        ),
        (named(Named::Tab), plain(), "EditorCycleSelection(1)"),
        (
            named(Named::Tab),
            mods(false, true, false, false),
            "EditorCycleSelection(-1)",
        ),
        (named(Named::F6), plain(), "EditorFocusZoneCycle(true)"),
        (
            named(Named::F6),
            mods(false, true, false, false),
            "EditorFocusZoneCycle(false)",
        ),
        (
            named(Named::ArrowRight),
            mods(false, false, true, false),
            "EditorResizeByKeys(1.0, 0.0)",
        ),
        (
            named(Named::ArrowDown),
            mods(false, true, true, false),
            "EditorResizeByKeys(0.0, 10.0)",
        ),
    ];
    for (i, (k, m, _)) in rows.iter().enumerate() {
        for (k2, m2, what) in &rows[i + 1..] {
            assert!(!(k == k2 && m == m2), "{k:?} {m:?} is bound twice ({what})");
        }
    }
    for (k, m, want) in rows {
        let got = debug(editor_key_message(k.clone(), m, true));
        assert!(got.contains(want), "{k:?} {m:?}: wanted {want}, got {got}");
    }
}

/// ⌃Tab (the physical Control key) walks the zones on every platform;
/// on macOS ⌃ is not ⌘, so ⌃⌘Tab is not it.
#[test]
fn control_tab_walks_the_focus_zones() {
    let mut ctrl = Modifiers::default();
    ctrl.insert(Modifiers::CTRL);
    assert_eq!(zone_cycle_key(&named(Named::Tab), ctrl), Some(true));
    let mut back = ctrl;
    back.insert(Modifiers::SHIFT);
    assert_eq!(zone_cycle_key(&named(Named::Tab), back), Some(false));
    assert_eq!(zone_cycle_key(&named(Named::Tab), plain()), None);
    let mut alt = ctrl;
    alt.insert(Modifiers::ALT);
    assert_eq!(zone_cycle_key(&named(Named::Tab), alt), None);
    // It works from a text field too (the subscription sees it either way).
    assert!(matches!(
        editor_key_message(named(Named::Tab), ctrl, false),
        Some(Message::EditorFocusZoneCycle(true))
    ));
}

#[test]
fn the_zone_walk_wraps_through_the_canvas() {
    let zones = [FocusZone::OptionsBar, FocusZone::Panel];
    assert_eq!(next_zone(&zones, None, true), Some(FocusZone::OptionsBar));
    assert_eq!(
        next_zone(&zones, Some(FocusZone::OptionsBar), true),
        Some(FocusZone::Panel)
    );
    assert_eq!(next_zone(&zones, Some(FocusZone::Panel), true), None);
    assert_eq!(next_zone(&zones, None, false), Some(FocusZone::Panel));
    assert_eq!(next_zone(&[], None, true), None);
}

/// Inside a zone the keys are the same everywhere; tool letters and other
/// chords fall through to the canvas map.
#[test]
fn zone_keys_are_uniform_and_leave_tool_letters_alone() {
    let f = KeyFocus {
        zone: FocusZone::OptionsBar,
        item: FocusItem::Option(readshot_ui::editor::OptionControl::Fill),
    };
    let k = |key: Key, m: Modifiers| focus_key(f, &key, m, false);
    assert_eq!(k(named(Named::Tab), plain()), Some(FocusKey::Next));
    assert_eq!(
        k(named(Named::Tab), mods(false, true, false, false)),
        Some(FocusKey::Prev)
    );
    assert_eq!(k(named(Named::ArrowDown), plain()), Some(FocusKey::Next));
    assert_eq!(k(named(Named::ArrowUp), plain()), Some(FocusKey::Prev));
    assert_eq!(
        k(named(Named::ArrowRight), plain()),
        Some(FocusKey::Adjust(1))
    );
    assert_eq!(
        k(named(Named::ArrowLeft), mods(false, true, false, false)),
        Some(FocusKey::Adjust(-10))
    );
    assert_eq!(k(named(Named::Enter), plain()), Some(FocusKey::Activate));
    assert_eq!(k(named(Named::Space), plain()), Some(FocusKey::Activate));
    assert_eq!(k(named(Named::Escape), plain()), Some(FocusKey::Leave));
    for letter in ["v", "r", "c", "i", "1", "[", ","] {
        assert_eq!(
            k(ch(letter), plain()),
            None,
            "{letter} must reach the canvas"
        );
    }
    assert_eq!(k(ch("z"), mods(true, false, false, false)), None);
    // A text field keeps its caret keys; only Tab moves on.
    assert_eq!(focus_key(f, &named(Named::ArrowRight), plain(), true), None);
    assert_eq!(
        focus_key(f, &named(Named::Tab), plain(), true),
        Some(FocusKey::Next)
    );
    // The line list is a list box.
    let lines = KeyFocus {
        zone: FocusZone::Panel,
        item: FocusItem::TextLines,
    };
    let l = |key: Key, m: Modifiers| focus_key(lines, &key, m, false);
    assert_eq!(
        l(named(Named::ArrowDown), mods(false, true, false, false)),
        Some(FocusKey::LineMove {
            delta: 1,
            extend: true
        })
    );
    assert_eq!(l(named(Named::Space), plain()), Some(FocusKey::LineToggle));
    assert_eq!(l(named(Named::Enter), plain()), Some(FocusKey::LineCopy));
    assert_eq!(
        l(ch("c"), mods(true, false, false, false)),
        Some(FocusKey::LineCopy)
    );
    assert_eq!(l(ch("r"), plain()), Some(FocusKey::LineRedact));
    // Image menu: -/+ step the gap, ,/. cycle the alignment.
    let menu = KeyFocus {
        zone: FocusZone::ImageMenu,
        item: FocusItem::MenuResize,
    };
    assert_eq!(
        focus_key(menu, &ch("="), plain(), false),
        Some(FocusKey::GapStep(1))
    );
    assert_eq!(
        focus_key(menu, &ch("-"), plain(), false),
        Some(FocusKey::GapStep(-1))
    );
    assert_eq!(
        focus_key(menu, &ch("."), plain(), false),
        Some(FocusKey::AlignStep(1))
    );
    // Resize: ⌥P / ⌥L even while a field has the caret.
    let resize = KeyFocus {
        zone: FocusZone::Resize,
        item: FocusItem::ResizeWidth,
    };
    let alt = mods(false, false, true, false);
    assert_eq!(
        focus_key(resize, &ch("p"), alt, true),
        Some(FocusKey::ToggleUnit)
    );
    assert_eq!(
        focus_key(resize, &ch("l"), alt, true),
        Some(FocusKey::ToggleLock)
    );
}

#[test]
fn image_menu_opens_from_the_keyboard_and_runs_a_row() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    press(&mut app, id, ch("i"), mods(true, true, false, false));
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, Some(EditorPopover::Image));
    assert_eq!(focus(&app), at(FocusZone::ImageMenu, FocusItem::MenuResize));
    // ↓ ↓ to Rotate Right, ↵ runs it: the menu closes, focus is back on
    // the canvas.
    press(&mut app, id, named(Named::ArrowDown), plain());
    press(&mut app, id, named(Named::ArrowDown), plain());
    assert_eq!(
        focus(&app),
        at(FocusZone::ImageMenu, FocusItem::MenuRotateRight)
    );
    press(&mut app, id, named(Named::Enter), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.effective_image_size(), (20, 40));
    assert_eq!(ed.popover, None);
    assert_eq!(ed.key_focus, None);
    // ↑ from the first row wraps to the last.
    press(&mut app, id, ch("i"), mods(true, true, false, false));
    press(&mut app, id, named(Named::ArrowUp), plain());
    assert_eq!(
        focus(&app),
        at(FocusZone::ImageMenu, FocusItem::MenuAddImage)
    );
    // esc closes it and the focus goes back to the canvas.
    press(&mut app, id, named(Named::Escape), plain());
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
    assert_eq!(focus(&app), None);
}

#[test]
fn a_pointer_opened_image_menu_takes_the_arrow_keys() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(EditorPopover::Image),
    );
    assert_eq!(focus(&app), None);
    press(&mut app, id, named(Named::ArrowDown), plain());
    assert_eq!(focus(&app), at(FocusZone::ImageMenu, FocusItem::MenuResize));
}

#[test]
fn flip_keys_flip_without_the_menu() {
    let mut img = solid(4, 2);
    img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(img));
    press(&mut app, id, ch("h"), mods(true, true, false, false));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.model.can_undo(), "⇧⌘H flips as one undo step");
    press(&mut app, id, ch("v"), mods(true, true, false, false));
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), 2);
}

#[test]
fn layout_gap_and_align_step_from_the_keyboard() {
    let (mut app, id) = editor_app(two_part_editor());
    press(&mut app, id, ch("i"), mods(true, true, false, false));
    let gap0 = app.editor.as_ref().unwrap().model.geometry().gap;
    // Down to the Gap row (after the eight action rows), then → / ⇧→.
    for _ in 0..8 {
        press(&mut app, id, named(Named::ArrowDown), plain());
    }
    assert_eq!(focus(&app), at(FocusZone::ImageMenu, FocusItem::MenuGap));
    press(&mut app, id, named(Named::ArrowRight), plain());
    assert_eq!(app.editor.as_ref().unwrap().model.geometry().gap, gap0 + 1);
    press(
        &mut app,
        id,
        named(Named::ArrowRight),
        mods(false, true, false, false),
    );
    assert_eq!(app.editor.as_ref().unwrap().model.geometry().gap, gap0 + 11);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    // + / - work anywhere in the menu.
    press(&mut app, id, ch("-"), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.geometry().gap, gap0 + 10);
    assert_eq!(ed.model.undo_depth(), depth + 1, "one undo step per key");
    let align0 = ed.model.geometry().align;
    press(&mut app, id, ch("."), plain());
    assert_ne!(app.editor.as_ref().unwrap().model.geometry().align, align0);
}

#[test]
fn resize_opens_with_width_focused_and_tabs_through_the_dialog() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    press(&mut app, id, ch("i"), mods(true, false, true, false));
    assert_eq!(
        app.editor.as_ref().unwrap().popover,
        Some(EditorPopover::Resize)
    );
    assert_eq!(focus(&app), at(FocusZone::Resize, FocusItem::ResizeWidth));
    // Tab (which the field does not take) moves to Height, then Unit.
    press_captured(&mut app, id, named(Named::Tab), plain());
    assert_eq!(focus(&app), at(FocusZone::Resize, FocusItem::ResizeHeight));
    press_captured(&mut app, id, named(Named::Tab), plain());
    assert_eq!(focus(&app), at(FocusZone::Resize, FocusItem::ResizeUnit));
    press(&mut app, id, named(Named::ArrowRight), plain());
    let d = app.editor.as_ref().unwrap().resize_draft.clone().unwrap();
    assert_eq!(d.unit, crate::editor::ResizeUnit::Percent);
    let lock0 = d.lock;
    // ⌥L toggles the lock even while a field has the caret.
    press_captured(&mut app, id, ch("l"), mods(false, false, true, false));
    assert_ne!(
        app.editor
            .as_ref()
            .unwrap()
            .resize_draft
            .as_ref()
            .unwrap()
            .lock,
        lock0
    );
    // ⇧Tab goes back; esc closes the dialog and returns to the canvas.
    press(
        &mut app,
        id,
        named(Named::Tab),
        mods(false, true, false, false),
    );
    assert_eq!(focus(&app), at(FocusZone::Resize, FocusItem::ResizeHeight));
    press_captured(&mut app, id, named(Named::Escape), plain());
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
    assert_eq!(focus(&app), None);
}

#[test]
fn resize_fields_keep_digits_and_one_point() {
    assert_eq!(image_menu::resize_field_text("12¬π%a3.5.1"), "123.51");
    assert_eq!(image_menu::resize_field_text(""), "");
}

#[test]
fn the_options_bar_is_reached_and_changed_from_the_keyboard() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    press(&mut app, id, ch("r"), plain());
    let mut ctrl = Modifiers::default();
    ctrl.insert(Modifiers::CTRL);
    press(&mut app, id, named(Named::Tab), ctrl);
    use readshot_ui::editor::OptionControl as O;
    assert_eq!(
        focus(&app),
        at(FocusZone::OptionsBar, FocusItem::Option(O::Color))
    );
    press(&mut app, id, named(Named::Tab), plain());
    press(&mut app, id, named(Named::Tab), plain());
    assert_eq!(
        focus(&app),
        at(FocusZone::OptionsBar, FocusItem::Option(O::Fill))
    );
    press(&mut app, id, named(Named::ArrowRight), plain());
    assert_eq!(
        app.editor.as_ref().unwrap().model.effective_style().fill,
        readshot_core::FillMode::Filled
    );
    // Space presses Dash (a toggle).
    press(&mut app, id, named(Named::Tab), plain());
    let dashed0 = app.editor.as_ref().unwrap().model.effective_style().dashed;
    press(&mut app, id, named(Named::Space), plain());
    assert_ne!(
        app.editor.as_ref().unwrap().model.effective_style().dashed,
        dashed0
    );
    // The corner radius slider steps by 1, ⇧ by 10.
    press(&mut app, id, named(Named::Tab), plain());
    press(
        &mut app,
        id,
        named(Named::ArrowRight),
        mods(false, true, false, false),
    );
    assert_eq!(
        app.editor
            .as_ref()
            .unwrap()
            .model
            .effective_style()
            .corner_radius,
        10.0
    );
    // esc leaves the bar; the next esc would reach the canvas chain.
    press(&mut app, id, named(Named::Escape), plain());
    assert_eq!(focus(&app), None);
}

#[test]
fn closing_a_popover_opened_from_the_bar_returns_the_focus_to_it() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    press(&mut app, id, ch("r"), plain());
    press(&mut app, id, named(Named::F6), plain());
    use readshot_ui::editor::OptionControl as O;
    let color = at(FocusZone::OptionsBar, FocusItem::Option(O::Color));
    assert_eq!(focus(&app), color);
    press(&mut app, id, named(Named::Enter), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, Some(EditorPopover::Color));
    assert_eq!(ed.key_focus, None, "the popover has the keys");
    press_captured(&mut app, id, named(Named::Escape), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert_eq!(ed.key_focus, color);
}

#[test]
fn the_beautify_tab_is_operable_from_the_keyboard() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    press(&mut app, id, ch("b"), mods(true, false, true, false));
    assert_eq!(
        PanelTab::from_id(&app.preferences.editor_panel_tab),
        PanelTab::Beautify
    );
    assert_eq!(focus(&app), at(FocusZone::Panel, FocusItem::PanelTabs));
    // Tab to the first preset and apply it.
    press(&mut app, id, named(Named::Tab), plain());
    let first = BuiltinPreset::ALL[0];
    assert_eq!(
        focus(&app),
        at(FocusZone::Panel, FocusItem::BeautifyPreset(first))
    );
    press(&mut app, id, named(Named::Enter), plain());
    assert_eq!(
        app.editor
            .as_ref()
            .unwrap()
            .model
            .beautify()
            .matching_builtin(),
        Some(first)
    );
    // Walk to Background and cycle it, then to Padding and step it.
    while focus(&app) != at(FocusZone::Panel, FocusItem::BeautifyBackground) {
        press(&mut app, id, named(Named::ArrowDown), plain());
    }
    let bg0 = app.editor.as_ref().unwrap().model.beautify().background;
    press(&mut app, id, named(Named::ArrowRight), plain());
    assert_ne!(
        app.editor.as_ref().unwrap().model.beautify().background,
        bg0
    );
    while focus(&app) != at(FocusZone::Panel, FocusItem::BeautifyPadding) {
        press(&mut app, id, named(Named::Tab), plain());
    }
    let pad0 = app.editor.as_ref().unwrap().model.beautify().padding;
    press(&mut app, id, named(Named::ArrowRight), plain());
    assert_eq!(
        app.editor.as_ref().unwrap().model.beautify().padding,
        pad0 + 1
    );
    // ←/→ on the tabs switch tab.
    press(
        &mut app,
        id,
        named(Named::Tab),
        mods(false, true, false, false),
    );
    while focus(&app) != at(FocusZone::Panel, FocusItem::PanelTabs) {
        press(&mut app, id, named(Named::ArrowUp), plain());
    }
    press(&mut app, id, named(Named::ArrowRight), plain());
    assert_eq!(
        PanelTab::from_id(&app.preferences.editor_panel_tab),
        PanelTab::Text
    );
}

#[test]
fn text_lines_are_selected_copied_and_redacted_from_the_keyboard() {
    let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
    ready_editor(&mut app);
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    press(&mut app, id, ch("t"), mods(true, false, true, false));
    while focus(&app) != at(FocusZone::Panel, FocusItem::TextLines) {
        press(&mut app, id, named(Named::Tab), plain());
    }
    let tp = &app.editor.as_ref().unwrap().text_panel;
    assert_eq!(tp.cursor, Some(0), "the list opens on its first line");
    // ↓ moves the cursor, ⇧↓ extends the selection, Space toggles.
    press(&mut app, id, named(Named::ArrowDown), plain());
    assert_eq!(app.editor.as_ref().unwrap().text_panel.cursor, Some(1));
    press(
        &mut app,
        id,
        named(Named::ArrowDown),
        mods(false, true, false, false),
    );
    let tp = &app.editor.as_ref().unwrap().text_panel;
    assert_eq!(tp.cursor, Some(2));
    assert_eq!(tp.selected.iter().copied().collect::<Vec<_>>(), vec![1, 2]);
    press(&mut app, id, named(Named::Space), plain());
    let tp = &app.editor.as_ref().unwrap().text_panel;
    assert_eq!(tp.selected.iter().copied().collect::<Vec<_>>(), vec![1]);
    // The cursor line reads as hovered: outlined on the image.
    let (hovered, _) = text_marks(app.editor.as_ref().unwrap(), true);
    assert!(hovered.is_some());
    // R on the e-mail line redacts it.
    press(&mut app, id, named(Named::ArrowUp), plain());
    let before = app.editor.as_ref().unwrap().model.annotations().len();
    press(&mut app, id, ch("r"), plain());
    assert!(app.editor.as_ref().unwrap().model.annotations().len() > before);
    // Leaving the list drops the cursor.
    press(&mut app, id, named(Named::Escape), plain());
    assert_eq!(app.editor.as_ref().unwrap().text_panel.cursor, None);
}

#[test]
fn tab_walks_the_annotations_on_the_canvas() {
    assert_eq!(cycle_selection_index(0, None, 1), None);
    assert_eq!(cycle_selection_index(3, None, 1), Some(0));
    assert_eq!(cycle_selection_index(3, None, -1), Some(2));
    assert_eq!(cycle_selection_index(3, Some(2), 1), Some(0));
    assert_eq!(cycle_selection_index(3, Some(0), -1), Some(2));
    let mut app = two_rects_first_selected();
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    press(&mut app, id, named(Named::Tab), plain());
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(1)
    );
    // The arrows then move what Tab selected.
    press(&mut app, id, named(Named::ArrowRight), plain());
    match &app.editor.as_ref().unwrap().model.annotations()[1] {
        Annotation::Rectangle { rect, .. } => assert_eq!(rect.x, 31.0),
        other => panic!("{other:?}"),
    }
}

#[test]
fn c_starts_a_crop_the_arrows_move_and_size() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(100, 50)));
    press(&mut app, id, ch("c"), plain());
    let d = app.editor.as_ref().unwrap().crop_draft.expect("a draft");
    assert_eq!(d, RectLike::new(10.0, 5.0, 80.0, 40.0));
    press(
        &mut app,
        id,
        named(Named::ArrowLeft),
        mods(false, true, false, false),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().crop_draft.unwrap().x,
        0.0,
        "kept inside the image"
    );
    press(
        &mut app,
        id,
        named(Named::ArrowRight),
        mods(false, false, true, false),
    );
    let d = app.editor.as_ref().unwrap().crop_draft.unwrap();
    assert_eq!((d.width, d.height), (81.0, 40.0));
    press(
        &mut app,
        id,
        named(Named::ArrowUp),
        mods(false, true, true, false),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().crop_draft.unwrap().height,
        30.0
    );
    press(&mut app, id, named(Named::Enter), plain());
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.crop_draft.is_none());
    assert_eq!(ed.effective_image_size(), (81, 30));
}

#[test]
fn a_keyboard_crop_keeps_the_chosen_aspect() {
    let frame = RectLike::new(0.0, 0.0, 100.0, 50.0);
    let d = keyboard_crop_draft(frame, Some(1.0));
    assert!((d.width - d.height).abs() < 1e-3);
    let grown = resize_crop_draft(d, 5.0, 0.0, Some(1.0), frame);
    assert!((grown.width - grown.height).abs() < 1e-3);
    assert!(grown.width > d.width);
    // Never past the image, never below the minimum.
    let huge = resize_crop_draft(d, 500.0, 0.0, Some(1.0), frame);
    assert!(huge.y + huge.height <= 50.0 + 1e-3);
    let tiny = resize_crop_draft(d, -500.0, -500.0, None, frame);
    assert!(tiny.width >= 8.0 && tiny.height >= 8.0);
}

#[test]
fn alt_arrows_move_only_the_magnifier_bubble() {
    let mut app = magnifier_app(true);
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    let (source, radius, bubble) = magnifier_parts(&app);
    press(
        &mut app,
        id,
        named(Named::ArrowDown),
        mods(false, true, true, false),
    );
    let (s2, r2, b2) = magnifier_parts(&app);
    assert_eq!((s2, r2), (source, radius), "the source stays put");
    assert_eq!(b2, PointLike::new(bubble.x, bubble.y + 10.0));
}

#[test]
fn alt_arrows_resize_a_selected_box_from_its_corner() {
    let (mut app, id) = editor_app(editor_with_selected_rect());
    press(
        &mut app,
        id,
        named(Named::ArrowRight),
        mods(false, false, true, false),
    );
    match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Rectangle { rect, .. } => {
            assert_eq!((rect.x, rect.width, rect.height), (0.0, 41.0, 40.0))
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn hiding_the_panel_hands_the_keys_back_to_the_canvas() {
    let (mut app, id) = editor_app(two_rects_first_selected().editor.take().unwrap());
    press(&mut app, id, ch("b"), mods(true, false, true, false));
    assert_eq!(focus(&app), at(FocusZone::Panel, FocusItem::PanelTabs));
    press(&mut app, id, ch("p"), mods(true, false, true, false));
    // The panel is gone: the arrow nudges the selection again.
    press(&mut app, id, named(Named::ArrowRight), plain());
    assert_eq!(focus(&app), None);
    match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Rectangle { rect, .. } => assert_eq!(rect.x, 1.0),
        other => panic!("{other:?}"),
    }
}
