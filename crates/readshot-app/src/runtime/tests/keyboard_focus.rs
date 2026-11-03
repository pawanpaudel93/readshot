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
        Message::Editor(EditorMsg::KeyPressed {
            window: id,
            key,
            modifiers: m,
            status_ignored: true,
        }),
    );
}

/// A key a focused text field took (status captured).
fn press_captured(app: &mut App, id: iced::window::Id, key: Key, m: Modifiers) {
    let _ = update(
        app,
        Message::Editor(EditorMsg::KeyPressed {
            window: id,
            key,
            modifiers: m,
            status_ignored: false,
        }),
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
            "Editor(CopyImageRequested)",
        ),
        (
            ch("c"),
            mods(true, true, false, false),
            "Editor(CopyTextRequested)",
        ),
        (
            ch("p"),
            mods(true, false, false, false),
            "Editor(PinRequested)",
        ),
        (
            ch("p"),
            editor_alt_modifiers(),
            "Editor(TextPanel(PanelToggled))",
        ),
        (
            ch("s"),
            mods(true, false, false, false),
            "Editor(SaveRequested)",
        ),
        (
            ch("w"),
            mods(true, false, false, false),
            "Editor(DiscardRequested)",
        ),
        (
            ch(RESIZE_LETTER),
            editor_alt_modifiers(),
            "Editor(Image(ResizeOpen))",
        ),
        (ch("["), mods(true, false, false, false), "clockwise: false"),
        (ch("]"), mods(true, false, false, false), "clockwise: true"),
        (ch("="), mods(true, false, false, false), "Editor(ZoomIn)"),
        (ch("-"), mods(true, false, false, false), "Editor(ZoomOut)"),
        (
            ch("0"),
            mods(true, false, false, false),
            "Editor(ZoomActual)",
        ),
        (ch("0"), editor_alt_modifiers(), "Editor(ZoomFit)"),
        (ch("i"), plain(), "Editor(EyedropperToggled)"),
        (ch("c"), plain(), "SelectTool(Crop)"),
        (ch("["), plain(), "Editor(WidthBump(-1.0))"),
        (ch(","), plain(), "Editor(ColorCycle(-1))"),
        (
            named(Named::ArrowLeft),
            plain(),
            "Editor(NudgeSelected(-1.0, 0.0))",
        ),
        (named(Named::Enter), plain(), "Editor(EditSelectedText)"),
        (named(Named::Backspace), plain(), "Editor(DeleteSelected)"),
        // New chords.
        (
            ch("i"),
            mods(true, true, false, false),
            "Editor(Focus(ImageMenuKey))",
        ),
        (
            ch("h"),
            mods(true, true, false, false),
            "Editor(Image(Flip(Horizontal)))",
        ),
        (
            ch("v"),
            mods(true, true, false, false),
            "Editor(Image(Flip(Vertical)))",
        ),
        (
            ch("t"),
            editor_alt_modifiers(),
            "Editor(Focus(PanelTabKey(Text)))",
        ),
        (
            ch("b"),
            editor_alt_modifiers(),
            "Editor(Focus(PanelTabKey(Beautify)))",
        ),
        (named(Named::Tab), plain(), "Editor(CycleSelection(1))"),
        (
            named(Named::Tab),
            mods(false, true, false, false),
            "Editor(CycleSelection(-1))",
        ),
        (named(Named::F6), plain(), "Editor(Focus(ZoneCycle(true)))"),
        (
            named(Named::F6),
            mods(false, true, false, false),
            "Editor(Focus(ZoneCycle(false)))",
        ),
        (
            named(Named::ArrowRight),
            mods(false, false, true, false),
            "Editor(ResizeByKeys(1.0, 0.0))",
        ),
        (
            named(Named::ArrowDown),
            mods(false, true, true, false),
            "Editor(ResizeByKeys(0.0, 10.0))",
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
        Some(Message::Editor(EditorMsg::Focus(FocusMsg::ZoneCycle(true))))
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
        Message::Editor(EditorMsg::PopoverToggled(EditorPopover::Image)),
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
    press(&mut app, id, ch(RESIZE_LETTER), editor_alt_modifiers());
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
    press(&mut app, id, ch("b"), editor_alt_modifiers());
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
    press(&mut app, id, ch("t"), editor_alt_modifiers());
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
    press(&mut app, id, ch("b"), editor_alt_modifiers());
    assert_eq!(focus(&app), at(FocusZone::Panel, FocusItem::PanelTabs));
    press(&mut app, id, ch("p"), editor_alt_modifiers());
    // The panel is gone: the arrow nudges the selection again.
    press(&mut app, id, named(Named::ArrowRight), plain());
    assert_eq!(focus(&app), None);
    match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Rectangle { rect, .. } => assert_eq!(rect.x, 1.0),
        other => panic!("{other:?}"),
    }
}

// ===== Fix round 1 =====

/// A key a text field took never runs an editor chord: the field's own
/// ⇧⌘V pastes and must not flip the image too.
#[test]
fn new_chords_stay_out_of_text_fields() {
    let cases = [
        (ch("v"), mods(true, true, false, false)),
        (ch("h"), mods(true, true, false, false)),
        (ch("i"), mods(true, true, false, false)),
        (ch("t"), editor_alt_modifiers()),
        (ch("b"), editor_alt_modifiers()),
        (named(Named::Tab), plain()),
        (named(Named::ArrowRight), mods(false, false, true, false)),
    ];
    for (k, m) in cases {
        assert!(
            editor_key_message(k.clone(), m, true).is_some(),
            "{k:?} {m:?} works on the canvas"
        );
        assert!(
            editor_key_message(k.clone(), m, false).is_none(),
            "{k:?} {m:?} leaked out of a text field"
        );
    }
    // ⇧⌘V pasted into the Color popover's hex field leaves the image alone.
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PopoverToggled(EditorPopover::Color)),
    );
    press_captured(&mut app, id, ch("v"), mods(true, true, false, false));
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), 0);
}

/// Off macOS the second-level chords are Ctrl+Shift, never Ctrl+Alt
/// (AltGr, and the desktop's Ctrl+Alt+T).
#[test]
fn second_level_chords_avoid_ctrl_alt_off_macos() {
    let ctrl_alt = mods(true, false, true, false);
    let ctrl_shift = mods(true, true, false, false);
    for letter in ["p", "t", "b", RESIZE_LETTER] {
        let k = ch(letter);
        if cfg!(target_os = "macos") {
            assert!(editor_alt_chord(&k, ctrl_alt, letter));
            assert!(!editor_alt_chord(&k, ctrl_shift, letter));
        } else {
            assert!(editor_alt_chord(&k, ctrl_shift, letter));
            assert!(!editor_alt_chord(&k, ctrl_alt, letter));
        }
    }
    for k in [KEY_TEXT_TAB, KEY_BEAUTIFY_TAB, KEY_PANEL, KEY_RESIZE] {
        assert!(!k.1.contains("Alt"), "{k:?}");
    }
}

/// Tab in the Color popover leaves it, back to the bar control, and never
/// moves the annotation selection under the hex being typed.
#[test]
fn tab_leaves_the_color_popover_without_touching_the_selection() {
    let mut app = two_rects_first_selected();
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    press(&mut app, id, named(Named::F6), plain());
    let color = focus(&app);
    assert!(color.is_some());
    press(&mut app, id, named(Named::Enter), plain());
    assert_eq!(
        app.editor.as_ref().unwrap().popover,
        Some(EditorPopover::Color)
    );
    press(&mut app, id, named(Named::Tab), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert_eq!(ed.key_focus, color);
    assert_eq!(ed.model.selected_annotation(), Some(0));
}

#[test]
fn tab_does_not_cycle_while_a_popover_or_hex_draft_is_open() {
    let mut app = two_rects_first_selected();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::PopoverToggled(EditorPopover::Image)),
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::CycleSelection(1)));
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(0)
    );
    let _ = update(&mut app, Message::Editor(EditorMsg::PopoverClosed));
    app.editor.as_mut().unwrap().beautify_hex_draft = Some("#12".into());
    let _ = update(&mut app, Message::Editor(EditorMsg::CycleSelection(1)));
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(0)
    );
}

/// Picking a swatch closes the popover through the one close path, so
/// the keyboard focus goes back to the Color control.
#[test]
fn a_swatch_returns_the_focus_to_the_color_control() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    press(&mut app, id, ch("r"), plain());
    press(&mut app, id, named(Named::F6), plain());
    let color = focus(&app);
    press(&mut app, id, named(Named::Enter), plain());
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Toolbar(
            readshot_ui::ToolbarMessage::SelectColor(Rgba::OPAQUE_BLACK),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert_eq!(ed.key_focus, color);
    assert!(ed.key_focus_return.is_none());
}

#[test]
fn resize_fields_read_a_decimal_comma() {
    assert_eq!(image_menu::resize_field_text("12,5"), "12.5");
    assert_eq!(image_menu::resize_field_text("1,2.3"), "1.23");
}

/// A key instruction stays while what it explains goes on, and goes
/// with it (WCAG 2.2.1).
#[test]
fn key_guides_stay_while_their_draft_lasts() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(100, 50)));
    press(&mut app, id, ch("c"), plain());
    let age = |app: &mut App| {
        app.editor.as_mut().unwrap().status.as_mut().unwrap().set_at =
            std::time::Instant::now() - std::time::Duration::from_secs(60);
    };
    age(&mut app);
    let _ = update(&mut app, Message::Editor(EditorMsg::StatusTick));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.status.is_some(), "the crop guide outlives the 4 s toast");
    assert!(!crate::editor::status_tick_needed(ed));
    // esc as the running editor delivers it (the canvas captures it).
    press_captured(&mut app, id, named(Named::Escape), plain());
    let _ = update(&mut app, Message::Editor(EditorMsg::StatusTick));
    assert!(
        app.editor.as_ref().unwrap().status.is_none(),
        "it goes with the draft"
    );
    // The zone hint stays while the keyboard is in the zone.
    press(&mut app, id, ch("r"), plain());
    press(&mut app, id, named(Named::F6), plain());
    age(&mut app);
    let _ = update(&mut app, Message::Editor(EditorMsg::StatusTick));
    assert!(app.editor.as_ref().unwrap().status.is_some());
}

// ===== Fix round 2 =====

fn beautify_shown(app: &mut App) {
    app.preferences.editor_panel_tab = PanelTab::Beautify.id().to_string();
    assert!(beautify_hex_on_screen(app));
}

/// With the Beautify hex field on screen, a canvas Tab first asks iced
/// whether that field has the caret; only the answer moves the selection.
#[test]
fn a_canvas_tab_checks_the_beautify_hex_field_first() {
    let mut app = two_rects_first_selected();
    beautify_shown(&mut app);
    let _ = update(&mut app, Message::Editor(EditorMsg::CycleSelection(1)));
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(0),
        "nothing moves until the field check answers"
    );
    // The field has the caret: Tab only leaves it.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TabResolved {
            step: 1,
            field_focused: true,
        }),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(0)
    );
    // It doesn't: the selection moves.
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::TabResolved {
            step: 1,
            field_focused: false,
        }),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(1)
    );
    // Without the Beautify tab on screen there is nothing to ask.
    app.preferences.editor_panel_tab = PanelTab::Text.id().to_string();
    assert!(!beautify_hex_on_screen(&app));
    let _ = update(&mut app, Message::Editor(EditorMsg::CycleSelection(1)));
    assert_eq!(
        app.editor.as_ref().unwrap().model.selected_annotation(),
        Some(0)
    );
}

/// Tab leaving the Color popover applies a typed hex as ↵ would; an
/// invalid one keeps the popover open and says why; Esc still cancels.
#[test]
fn tab_applies_a_typed_hex_and_esc_still_cancels() {
    let red = Rgba::from_hex("#FF0000").unwrap();
    let open_color = |app: &mut App, id: iced::window::Id| {
        press(app, id, ch("r"), plain());
        press(app, id, named(Named::F6), plain());
        press(app, id, named(Named::Enter), plain());
        assert_eq!(
            app.editor.as_ref().unwrap().popover,
            Some(EditorPopover::Color)
        );
    };
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    open_color(&mut app, id);
    let color_focus = app.editor.as_ref().unwrap().key_focus_return;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::HexChanged("#ff0000".into())),
    );
    press(&mut app, id, named(Named::Tab), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(swatch_eq(ed.model.effective_color(), red));
    assert_eq!(ed.key_focus, color_focus);
    // Invalid: the popover stays, with the existing message.
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    open_color(&mut app, id);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::HexChanged("#12".into())),
    );
    press(&mut app, id, named(Named::Tab), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, Some(EditorPopover::Color));
    assert!(ed
        .status_text()
        .unwrap_or("")
        .starts_with("Couldn’t read that color"));
    // Esc cancels: the typed value is dropped.
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    open_color(&mut app, id);
    let before = app.editor.as_ref().unwrap().model.effective_color();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::HexChanged("#ff0000".into())),
    );
    press_captured(&mut app, id, named(Named::Escape), plain());
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(swatch_eq(ed.model.effective_color(), before));
}

fn first_rect_y(app: &App) -> f32 {
    match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Rectangle { rect, .. } => rect.y,
        other => panic!("{other:?}"),
    }
}

/// ⌘Z and ↑/↓ typed in the Beautify hex field (or the Color popover's)
/// belong to the field: they never undo or nudge the document under it.
#[test]
fn undo_and_up_down_in_a_hex_field_leave_the_document_alone() {
    let down = iced::keyboard::Key::Named(iced::keyboard::key::Named::ArrowDown);
    let z = iced::keyboard::Key::Character("z".into());

    // A typed Beautify hex draft.
    let mut app = two_rects_first_selected();
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::HexChanged("#12".into()))),
    );
    let _ = update(&mut app, key_press(id, z.clone(), true));
    let _ = update(&mut app, key_press(id, down.clone(), false));
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth);
    assert_eq!(first_rect_y(&app), 0.0, "not nudged");

    // The Color popover's hex field.
    let mut app = two_rects_first_selected();
    app.editor.as_mut().unwrap().window_id = Some(id);
    app.editor.as_mut().unwrap().popover = Some(crate::editor::EditorPopover::Color);
    let _ = update(&mut app, key_press(id, down.clone(), false));
    assert_eq!(first_rect_y(&app), 0.0, "not nudged under the popover");

    // The Beautify field on screen, nothing typed: iced is asked first.
    let mut app = two_rects_first_selected();
    app.editor.as_mut().unwrap().window_id = Some(id);
    beautify_shown(&mut app);
    let _ = update(&mut app, key_press(id, z, true));
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth);
    let _ = update(&mut app, key_press(id, down, false));
    assert_eq!(first_rect_y(&app), 0.0);
    let undo = Box::new(Message::Editor(EditorMsg::Toolbar(
        readshot_ui::ToolbarMessage::Undo,
    )));
    let nudge = Box::new(Message::Editor(EditorMsg::NudgeSelected(0.0, 1.0)));
    // The field has the caret: the keys are its own.
    for message in [undo.clone(), nudge.clone()] {
        let sid = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::FieldKeyResolved {
                session: sid,
                message,
                field_focused: true,
            }),
        );
    }
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth);
    assert_eq!(first_rect_y(&app), 0.0);
    // It doesn't: they act on the document.
    let sid = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::FieldKeyResolved {
            session: sid,
            message: nudge,
            field_focused: false,
        }),
    );
    assert_eq!(first_rect_y(&app), 1.0, "nudged");
    let sid = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::FieldKeyResolved {
            session: sid,
            message: undo,
            field_focused: false,
        }),
    );
    assert_eq!(first_rect_y(&app), 0.0, "the nudge undone");
}

/// While ⌘Z waits for the Beautify hex field check, a key pressed after
/// it (⌫) waits too: the Undo runs first, then the delete, never the
/// other way round (which would undo the delete instead).
#[test]
fn keys_after_a_field_checked_undo_wait_their_turn() {
    let mut app = two_rects_first_selected();
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    beautify_shown(&mut app);
    let count = |app: &App| app.editor.as_ref().unwrap().model.annotations().len();
    let _ = update(&mut app, key_press(id, ch("z"), true));
    let _ = update(&mut app, key_press(id, named(Named::Delete), false));
    assert_eq!(count(&app), 2, "both keys wait for the field check");
    let sid = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::FieldKeyResolved {
            session: sid,
            message: Box::new(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Undo,
            ))),
            field_focused: false,
        }),
    );
    // Undo removed the second rect; then ⌫ deleted the selected first one.
    assert_eq!(count(&app), 0);
    assert!(app.editor.as_ref().unwrap().key_queue.is_empty());
}

/// Keys that waited behind a field check pass the same guards as fresh
/// keys: once a field owns the keys (here the Color popover opened
/// meanwhile), a waiting ⌘Z or ↓ is the field's, not the document's.
#[test]
fn queued_field_keys_pass_the_field_guards_when_they_run() {
    let mut app = two_rects_first_selected();
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    beautify_shown(&mut app);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(&mut app, key_press(id, ch("z"), true));
    let _ = update(&mut app, key_press(id, named(Named::ArrowDown), false));
    app.editor.as_mut().unwrap().popover = Some(crate::editor::EditorPopover::Color);
    let sid = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::FieldKeyResolved {
            session: sid,
            message: Box::new(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Undo,
            ))),
            field_focused: false,
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth, "no undo under the popover");
    assert_eq!(first_rect_y(&app), 0.0, "no nudge under the popover");
    assert!(app.editor.as_ref().unwrap().key_queue.is_empty());
}

/// A field-check answer for an editor that is gone never touches the
/// current editor's queue.
#[test]
fn a_field_check_answer_for_another_editor_is_dropped() {
    let mut app = two_rects_first_selected();
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    beautify_shown(&mut app);
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(&mut app, key_press(id, ch("z"), true));
    let sid = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::FieldKeyResolved {
            session: sid.wrapping_add(1000),
            message: Box::new(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Undo,
            ))),
            field_focused: false,
        }),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.undo_depth(), depth);
    assert_eq!(ed.key_queue.len(), 1, "its own check is still out");
}

/// C queued behind a field-checked ⌘Z still opens the keyboard crop
/// draft when it runs, as a fresh C does.
#[test]
fn a_queued_c_still_opens_the_keyboard_crop_draft() {
    let mut app = two_rects_first_selected();
    let id = iced::window::Id::unique();
    app.editor.as_mut().unwrap().window_id = Some(id);
    beautify_shown(&mut app);
    let _ = update(&mut app, key_press(id, ch("z"), true));
    let _ = update(&mut app, key_press(id, ch("c"), false));
    assert!(app.editor.as_ref().unwrap().crop_draft.is_none(), "C waits");
    let sid = app.editor.as_ref().unwrap().session_id;
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::FieldKeyResolved {
            session: sid,
            message: Box::new(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Undo,
            ))),
            field_focused: false,
        }),
    );
    assert!(
        app.editor.as_ref().unwrap().crop_draft.is_some(),
        "the queued C opened its crop draft"
    );
}

/// B#1: removing a saved Beautify preset from the keyboard moves the ring
/// off Remove (onto the preset now in that place), so a second Space
/// never removes the next one; and removing A then B, then Undo, puts
/// both back in order.
#[test]
fn removing_presets_by_keyboard_keeps_each_one_undoable() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    for _ in 0..3 {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Beautify(BeautifyMsg::SavePreset)),
        );
    }
    let before = app.preferences.editor_beautify_presets.clone();
    press(&mut app, id, ch("b"), editor_alt_modifiers());
    while focus(&app) != at(FocusZone::Panel, FocusItem::BeautifyRemoveSaved(0)) {
        press(&mut app, id, named(Named::Tab), plain());
    }
    press(&mut app, id, named(Named::Space), plain());
    assert_eq!(
        focus(&app),
        at(FocusZone::Panel, FocusItem::BeautifySaved(0)),
        "the ring leaves Remove"
    );
    // Back onto Remove (now B's) and remove that too.
    press(&mut app, id, named(Named::Tab), plain());
    assert_eq!(
        focus(&app),
        at(FocusZone::Panel, FocusItem::BeautifyRemoveSaved(0))
    );
    press(&mut app, id, named(Named::Enter), plain());
    assert_eq!(app.preferences.editor_beautify_presets, before[2..]);
    let _ = update(
        &mut app,
        Message::Editor(EditorMsg::Beautify(BeautifyMsg::RestorePreset)),
    );
    assert_eq!(
        app.preferences.editor_beautify_presets, before,
        "Undo puts A and B back, in order"
    );
}

/// Removing the last saved preset leaves the ring on the one before it.
#[test]
fn removing_the_last_preset_by_keyboard_focuses_the_one_before() {
    let (mut app, id) = editor_app(crate::editor::EditorSession::new(solid(40, 20)));
    for _ in 0..2 {
        let _ = update(
            &mut app,
            Message::Editor(EditorMsg::Beautify(BeautifyMsg::SavePreset)),
        );
    }
    press(&mut app, id, ch("b"), editor_alt_modifiers());
    while focus(&app) != at(FocusZone::Panel, FocusItem::BeautifyRemoveSaved(1)) {
        press(&mut app, id, named(Named::Tab), plain());
    }
    press(&mut app, id, named(Named::Space), plain());
    assert_eq!(app.preferences.editor_beautify_presets.len(), 1);
    assert_eq!(
        focus(&app),
        at(FocusZone::Panel, FocusItem::BeautifySaved(0))
    );
}

/// B#4: Fit to View has a key: ⌥⌘0 (Ctrl+Shift+0 off macOS, where
/// Ctrl+Alt is AltGr and never a command); ⌘0 stays actual pixels.
#[test]
fn fit_to_view_has_a_key() {
    let fit = editor_key_message(ch("0"), editor_alt_modifiers(), true);
    assert!(matches!(fit, Some(Message::Editor(EditorMsg::ZoomFit))));
    let actual = editor_key_message(ch("0"), mods(true, false, false, false), true);
    assert!(matches!(
        actual,
        Some(Message::Editor(EditorMsg::ZoomActual))
    ));
    if !cfg!(target_os = "macos") {
        // AltGr+0 types a character; it never zooms.
        let altgr = editor_key_message(ch("0"), mods(true, false, true, true), true);
        assert!(altgr.is_none(), "{altgr:?}");
    }
    assert_eq!(
        ZOOM_FIT_KEY,
        if cfg!(target_os = "macos") {
            "⌥⌘0"
        } else {
            "Ctrl+Shift+0"
        }
    );
}
