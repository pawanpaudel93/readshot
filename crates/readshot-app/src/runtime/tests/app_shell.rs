//! Theme, popovers, Beautify sliders, Esc, zoom and status toasts.

use super::*;
use crate::editor::StatusTone as Tone;

#[test]
fn overlay_like_windows_are_always_dark() {
    for kind in [
        WindowKind::Overlay,
        WindowKind::ScrollRegion,
        WindowKind::ScrollHud,
        WindowKind::Pin,
    ] {
        assert_eq!(
            theme_mode_for(Some(kind), ThemeMode::Light),
            ThemeMode::Dark
        );
    }
    assert_eq!(
        theme_mode_for(Some(WindowKind::Editor), ThemeMode::Light),
        ThemeMode::Light
    );
    assert_eq!(theme_mode_for(None, ThemeMode::Dark), ThemeMode::Dark);
}

#[test]
fn inline_text_editor_styles_follow_the_palette() {
    for (mode, p) in [(ThemeMode::Light, &LIGHT), (ThemeMode::Dark, &DARK)] {
        let theme = readshot_theme_for(mode);
        let chrome = editor_chrome_style(&theme);
        assert_eq!(chrome.background, Some(p.float.into()), "{mode:?}");
        assert_eq!(chrome.border.color, p.border, "{mode:?}");
        let boxed = inline_text_box_style(&theme);
        assert_eq!(boxed.background, Some(p.float.into()), "{mode:?}");
        assert_eq!(boxed.border.color, p.acc, "{mode:?}");
        let ed = inline_text_editor_style(&theme, iced::widget::text_editor::Status::Active);
        assert_eq!(ed.value, p.t1, "{mode:?}");
        assert_eq!(ed.placeholder, p.t3, "{mode:?}");
        assert_eq!(ed.selection, p.acc_tint, "{mode:?}");
        let disabled = disabled_text_input_style(&theme);
        assert_eq!(disabled.background, Some(p.hover.into()), "{mode:?}");
        assert_eq!(disabled.border.color, p.border, "{mode:?}");
        let track = line_width_track_style(&theme);
        assert_eq!(track.background, Some(p.hover.into()), "{mode:?}");
    }
}

#[test]
fn palette_for_reads_the_theme_name() {
    assert_eq!(
        palette_for(&readshot_theme_for(ThemeMode::Light)).bg,
        LIGHT.bg
    );
    assert_eq!(
        palette_for(&readshot_theme_for(ThemeMode::Dark)).bg,
        DARK.bg
    );
    assert_eq!(palette_for(&Theme::Dark).bg, DARK.bg);
}

#[test]
fn system_theme_message_switches_mode() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::SystemThemeChanged(iced::theme::Mode::Light),
    );
    assert_eq!(app.theme_mode, ThemeMode::Light);
    let _ = update(
        &mut app,
        Message::SystemThemeChanged(iced::theme::Mode::None),
    );
    assert_eq!(app.theme_mode, ThemeMode::Dark);
}

#[test]
fn theme_poll_tick_reads_os_mode_on_update_thread() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.theme_mode = ThemeMode::Light;
    let _ = update(&mut app, Message::ThemePollTick);
    // Off the main thread (tests) `os_theme_mode` reports Dark.
    assert_eq!(app.theme_mode, os_theme_mode());
}

#[test]
fn theme_poll_runs_only_while_a_system_themed_window_is_open() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    assert!(!theme_poll_needed(&app));
    app.windows.register(window::Id::unique(), WindowKind::Pin);
    app.windows
        .register(window::Id::unique(), WindowKind::ScrollHud);
    assert!(!theme_poll_needed(&app));
    for kind in [
        WindowKind::Welcome,
        WindowKind::Editor,
        WindowKind::History,
        WindowKind::Settings,
        WindowKind::CliTools,
    ] {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.windows.register(window::Id::unique(), kind);
        assert!(theme_poll_needed(&app), "{kind:?}");
    }
}

#[test]
fn opening_a_system_themed_window_reads_the_os_mode() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.theme_mode = ThemeMode::Light;
    let _ = update(&mut app, Message::OpenSettingsRequested);
    // Off the main thread (tests) `os_theme_mode` reports Dark.
    assert_eq!(app.theme_mode, os_theme_mode());
    assert_eq!(app.theme_mode, ThemeMode::Dark);
}

#[cfg(debug_assertions)]
#[test]
fn debug_editor_qa_session_is_clean_and_pin_ready() {
    let ed = debug_editor_qa_session();

    assert_eq!(ed.image_size(), (1040, 680));
    assert_eq!(ed.next_pin_number, 2);
    assert!(ed.model.undo_depth() > 0);
    assert!(!editor_has_unsaved_work(&ed));
    assert_eq!(ed.status.as_deref(), Some("Editor QA fixture loaded."));
}

#[test]
fn popover_toggles_and_esc_closes_it_before_cancelling_text() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(debug_editor_qa_session());
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().popover,
        Some(crate::editor::EditorPopover::Color)
    );
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Width),
    );
    let _ = update(&mut app, Message::EditorTextCancel);
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
}

#[test]
fn plain_esc_closes_popover_instead_of_arming_discard() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = debug_editor_qa_session();
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let _ = update(
        &mut app,
        Message::EditorKeyPressed {
            window: id,
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: true,
        },
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(ed.discard_pending_at.is_none());
}

#[test]
fn opening_the_color_popover_seeds_the_hex_field() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.hex_draft, ed.model.effective_color().to_hex());
}

#[test]
fn hex_submit_applies_the_color_and_remembers_it() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let _ = update(&mut app, Message::EditorHexChanged("#4f46e5".into()));
    let _ = update(&mut app, Message::EditorHexSubmitted);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.current_color().to_hex(), "#4F46E5");
    assert_eq!(ed.popover, None);
    assert_eq!(
        app.preferences.editor_recent_colors,
        vec!["#4F46E5".to_string()]
    );
}

#[test]
fn invalid_hex_keeps_the_popover_open_with_an_error() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let before = app.editor.as_ref().unwrap().model.current_color();
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let _ = update(&mut app, Message::EditorHexChanged("#12".into()));
    let _ = update(&mut app, Message::EditorHexSubmitted);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, Some(crate::editor::EditorPopover::Color));
    assert_eq!(ed.model.current_color(), before);
    assert!(ed.status_is_error());
}

#[test]
fn preset_colors_are_not_added_to_recent() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(
        &mut app,
        Message::EditorHexChanged(readshot_ui::editor::toolbar::PALETTE[4].to_hex()),
    );
    let _ = update(&mut app, Message::EditorHexSubmitted);
    assert!(app.preferences.editor_recent_colors.is_empty());
}

#[test]
fn clicking_a_recent_swatch_moves_it_to_the_front() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    app.remember_custom_color(readshot_core::Rgba::from_hex("#111111").unwrap());
    app.remember_custom_color(readshot_core::Rgba::from_hex("#222222").unwrap());
    assert_eq!(
        app.preferences.editor_recent_colors,
        vec!["#222222".to_string(), "#111111".to_string()]
    );
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectColor(
            readshot_core::Rgba::from_hex("#111111").unwrap(),
        )),
    );
    assert_eq!(
        app.preferences.editor_recent_colors,
        vec!["#111111".to_string(), "#222222".to_string()]
    );
}

#[test]
fn hex_field_filters_to_hex_characters() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(&mut app, Message::EditorHexChanged("#4z f46e5xx99".into()));
    assert_eq!(app.editor.as_ref().unwrap().hex_draft, "#4f46e5");
}

#[test]
fn esc_while_typing_in_the_hex_field_closes_the_popover() {
    // The focused text input captures Esc, so it arrives with
    // status_ignored == false and maps to EditorTextCancel.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(32, 32));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let _ = update(
        &mut app,
        Message::EditorKeyPressed {
            window: id,
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            modifiers: iced::keyboard::Modifiers::empty(),
            status_ignored: false,
        },
    );
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(ed.discard_pending_at.is_none());
}

#[test]
fn tool_letters_and_brackets_are_ignored_while_typing_in_the_hex_field() {
    // The focused text input captures printable keys, so they arrive
    // with status_ignored == false and must not switch tools or widths.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(32, 32));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let tool = app.editor.as_ref().unwrap().model.active_tool();
    let width = app.editor.as_ref().unwrap().model.current_line_width();
    for ch in ["i", "s", "e", "t", "1", "[", "]"] {
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Character(ch.into()),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: false,
            },
        );
    }
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.active_tool(), tool);
    assert_eq!(ed.model.current_line_width(), width);
    assert_eq!(ed.popover, Some(crate::editor::EditorPopover::Color));
}

#[test]
fn cmd_z_is_ignored_while_the_color_popover_is_open() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(32, 32));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.model
        .commit_annotation(readshot_core::Annotation::Rectangle {
            rect: readshot_core::RectLike::new(2.0, 2.0, 10.0, 10.0),
            color: readshot_core::Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
    app.editor = Some(ed);
    let cmd = iced::keyboard::Modifiers::COMMAND;
    let press = |app: &mut App, mods| {
        let _ = update(
            app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Character("z".into()),
                modifiers: mods,
                status_ignored: false,
            },
        );
    };
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    press(&mut app, cmd);
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
    press(&mut app, cmd | iced::keyboard::Modifiers::SHIFT);
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
    let _ = update(&mut app, Message::EditorPopoverClosed);
    press(&mut app, cmd);
    assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 0);
}

#[test]
fn hex_field_allows_hash_only_first() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(&mut app, Message::EditorHexChanged("#ab#cd#".into()));
    assert_eq!(app.editor.as_ref().unwrap().hex_draft, "#abcd");
    let _ = update(&mut app, Message::EditorHexChanged("ab#cd".into()));
    assert_eq!(app.editor.as_ref().unwrap().hex_draft, "abcd");
}

#[test]
fn picking_a_color_closes_the_color_popover() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(debug_editor_qa_session());
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectColor(
            readshot_ui::editor::toolbar::PALETTE[3],
        )),
    );
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
}

#[test]
fn the_beautify_hex_field_follows_the_solid_background_unless_mid_edit() {
    use readshot_core::beautify::{Background, BeautifyEdit, BuiltinPreset};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let hex = |app: &App| app.editor.as_ref().unwrap().beautify_hex_shown();
    // Opening the tab: seeded from the current solid background.
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));
    let _ = update(
        &mut app,
        Message::EditorPanelTabSelected(crate::editor::PanelTab::Beautify),
    );
    assert_eq!(hex(&app), "#E5EAF0");
    // Changed elsewhere (a swatch, a preset): the field follows.
    let solid_bg = |rgba| BeautifyEdit::Background(Background::Solid { rgba });
    let _ = update(
        &mut app,
        Message::EditorBeautifyEdit(solid_bg([0x4f, 0x46, 0xe5, 255])),
    );
    assert_eq!(hex(&app), "#4F46E5");
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::None));
    assert_eq!(hex(&app), "", "no solid background");
    // Mid-edit: a background change doesn't clobber what is typed.
    let _ = update(&mut app, Message::EditorBeautifyHexChanged("#12".into()));
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));
    assert_eq!(hex(&app), "#12");
    // Applied: follows the background again.
    let _ = update(
        &mut app,
        Message::EditorBeautifyHexChanged("#123456".into()),
    );
    let _ = update(&mut app, Message::EditorBeautifyHexSubmitted);
    assert_eq!(hex(&app), "#123456");
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));
    assert_eq!(hex(&app), "#E5EAF0");
    // Reopening the tab drops an abandoned draft.
    let _ = update(&mut app, Message::EditorBeautifyHexChanged("#9".into()));
    let _ = update(
        &mut app,
        Message::EditorPanelTabSelected(crate::editor::PanelTab::Beautify),
    );
    assert_eq!(hex(&app), "#E5EAF0");
}

#[test]
fn beautify_slider_drag_is_one_undo_step_and_esc_cancels_it() {
    use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
    );
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    for p in [30, 60, 90] {
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(p)),
        );
    }
    let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.beautify().padding, 90);
    assert_eq!(ed.model.undo_depth(), depth + 1);

    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
    );
    let _ = update(&mut app, Message::EditorTextCancel);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.model.beautify().padding,
        90,
        "esc restores the value before the drag"
    );
    assert!(ed.snapshot_drag_baseline.is_none());
}

#[test]
fn a_preset_click_mid_drag_keeps_the_pre_drag_state_in_undo() {
    use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
    );
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
    );
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Dark));
    let ed = app.editor.as_mut().unwrap();
    assert!(ed.snapshot_drag_baseline.is_none());
    assert_eq!(ed.model.beautify(), BuiltinPreset::Dark.settings());
    assert!(ed.model.undo());
    assert_eq!(
        ed.model.beautify(),
        BuiltinPreset::Minimal.settings(),
        "no drag value leaked into history"
    );
}

#[test]
fn undo_ends_a_slider_drag() {
    use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
    );
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
    );
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.snapshot_drag_baseline.is_none());
    assert_ne!(ed.model.beautify().padding, 150);
}

#[test]
fn undo_during_a_held_drag_cancels_only_the_drag_and_loses_nothing() {
    use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Dark));
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
    );
    // The mouse is down: this is a held slider drag.
    let _ = update(&mut app, Message::EditorPointerButton(true, None));
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    // The slider is still held: more ticks arrive, then release.
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(160)),
    );
    let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.beautify(), BuiltinPreset::Dark.settings());
    assert_eq!(ed.model.undo_depth(), depth, "one undo, no history step");
    assert!(!ed.snapshot_drag_cancelled);
    // The next drag works again.
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(30)),
    );
    let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
    assert_eq!(app.editor.as_ref().unwrap().model.beautify().padding, 30);
}

#[test]
fn esc_during_a_held_drag_ignores_the_remaining_ticks() {
    use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
    );
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
    );
    // The mouse is down: this is a held slider drag.
    let _ = update(&mut app, Message::EditorPointerButton(true, None));
    let _ = update(&mut app, Message::EditorTextCancel);
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(151)),
    );
    let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.model.beautify(), BuiltinPreset::Minimal.settings());
    assert_eq!(ed.model.undo_depth(), depth);
}

#[test]
fn output_commits_a_key_driven_slider_preview() {
    use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
    let _ = update(
        &mut app,
        Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
    );
    let depth = app.editor.as_ref().unwrap().model.undo_depth();
    // Arrow keys preview but never send a release.
    let _ = update(
        &mut app,
        Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(25)),
    );
    let _ = update(&mut app, Message::EditorCopyImageRequested);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.model.beautify().padding,
        25,
        "the copied output keeps the previewed value"
    );
    assert_eq!(ed.model.undo_depth(), depth + 1);
    assert!(ed.snapshot_drag_baseline.is_none());
}

#[test]
fn a_cancelled_key_or_wheel_slider_preview_never_swallows_the_next_drag() {
    use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
    // Each way a preview with no held slider can be cancelled.
    type Cancel = fn(&mut App);
    let cancels: [(&str, Cancel); 3] = [
        ("esc", |app| {
            let _ = update(app, Message::EditorTextCancel);
        }),
        ("undo", |app| {
            let _ = update(
                app,
                Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
            );
        }),
        ("preset", |app| {
            let _ = update(app, Message::EditorBeautifyPreset(BuiltinPreset::Minimal));
        }),
    ];
    for (name, cancel) in cancels {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
        );
        // Arrow keys (one tick) or a Ctrl+wheel burst (several): no
        // button press, no release.
        for v in [20, 21, 22] {
            let _ = update(
                &mut app,
                Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(v)),
            );
        }
        cancel(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.snapshot_drag_cancelled, "{name}: no held slider");
        let depth = ed.model.undo_depth();
        drag_padding(&mut app, &[40, 44]);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.beautify().padding, 44, "{name}: the drag applies");
        assert_eq!(ed.model.undo_depth(), depth + 1, "{name}: one undo step");
    }
}

#[test]
fn a_cancelled_key_style_preview_never_swallows_the_next_drag() {
    use readshot_ui::editor::StyleMessage;
    let mut app = two_rects_first_selected();
    // An arrow key over the corner slider, then Esc.
    let _ = update(
        &mut app,
        Message::EditorStylePreview(StyleMessage::CornerRadius(6.0)),
    );
    let _ = update(&mut app, Message::EditorTextCancel);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.style_drag_cancelled);
    let depth = ed.model.undo_depth();
    let radius = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
        Annotation::Rectangle { style, .. } => style.corner_radius,
        other => panic!("expected rectangle, got {other:?}"),
    };
    assert_eq!(radius(&app), 0.0);
    // Now a mouse drag on the same slider.
    let _ = update(
        &mut app,
        Message::EditorStylePreview(StyleMessage::CornerRadius(9.0)),
    );
    assert_eq!(radius(&app), 9.0, "the press tick previews");
    let _ = update(&mut app, Message::EditorPointerButton(true, None));
    let _ = update(
        &mut app,
        Message::EditorStylePreview(StyleMessage::CornerRadius(11.0)),
    );
    let _ = update(&mut app, Message::EditorStyleCommit);
    let _ = update(&mut app, Message::EditorPointerButton(false, None));
    assert_eq!(radius(&app), 11.0);
    assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
}

#[test]
fn a_press_whose_window_loses_focus_or_closes_is_no_longer_held() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    let (a, b) = (iced::window::Id::unique(), iced::window::Id::unique());
    let held = |app: &App| app.editor.as_ref().unwrap().pointer_held;
    // Focus loss of another window leaves the press held.
    let _ = update(&mut app, Message::EditorPointerButton(true, Some(a)));
    let _ = update(&mut app, Message::EditorPointerWindowUnfocused(b));
    let _ = update(&mut app, Message::WindowClosed(b));
    assert!(held(&app));
    // The press's own window losing focus: its release may never come.
    app.editor.as_mut().unwrap().snapshot_drag_cancelled = true;
    let _ = update(&mut app, Message::EditorPointerWindowUnfocused(a));
    assert!(!held(&app));
    assert!(!app.editor.as_ref().unwrap().snapshot_drag_cancelled);
    // …or closing before the release reaches the app.
    let _ = update(&mut app, Message::EditorPointerButton(true, Some(b)));
    assert!(held(&app));
    let _ = update(&mut app, Message::WindowClosed(b));
    assert!(!held(&app));
    // A new editor session starts with nothing held.
    let _ = update(&mut app, Message::EditorPointerButton(true, Some(a)));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    assert!(!held(&app));
}

#[test]
fn a_new_press_clears_a_stale_cancelled_drag_flag() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(16, 16));
    ed.snapshot_drag_cancelled = true;
    ed.style_drag_cancelled = true;
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorPointerButton(true, None));
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.pointer_held);
    assert!(!ed.snapshot_drag_cancelled && !ed.style_drag_cancelled);
}

#[test]
fn picking_a_background_closes_the_narrow_panel_overlay() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    ed.window_width = 800.0;
    ed.panel_overlay_open = true;
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorBeautifyPickBackground);
    let ed = app.editor.as_ref().unwrap();
    assert!(!ed.panel_overlay_open);
    assert!(ed.eyedropper.is_some());
}

#[test]
fn saved_presets_persist_apply_and_cap_at_twelve() {
    use readshot_core::beautify::{BuiltinPreset, MAX_SAVED_PRESETS};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Dark));
    let _ = update(&mut app, Message::EditorBeautifySavePreset);
    assert_eq!(app.preferences.editor_beautify_presets.len(), 1);
    assert_eq!(app.preferences.editor_beautify_presets[0].name, "Preset 1");
    assert_eq!(
        app.preferences.editor_beautify_presets[0].settings,
        BuiltinPreset::Dark.settings()
    );
    let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::None));
    let _ = update(&mut app, Message::EditorBeautifySavedPresetApplied(0));
    assert_eq!(
        app.editor.as_ref().unwrap().model.beautify(),
        BuiltinPreset::Dark.settings()
    );
    for _ in 0..20 {
        let _ = update(&mut app, Message::EditorBeautifySavePreset);
    }
    assert_eq!(
        app.preferences.editor_beautify_presets.len(),
        MAX_SAVED_PRESETS
    );
    let first = app.preferences.editor_beautify_presets[0].clone();
    let _ = update(&mut app, Message::EditorBeautifyDeletePreset(0));
    assert_eq!(
        app.preferences.editor_beautify_presets.len(),
        MAX_SAVED_PRESETS - 1
    );
    // Removing offers Undo, which puts the same preset back in place.
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.visible_toast_action(),
        Some(crate::editor::EditorToastAction::RestorePreset)
    );
    assert_eq!(
        ed.status.as_deref(),
        Some(format!("Removed {}.", first.name).as_str())
    );
    let _ = update(&mut app, Message::EditorBeautifyRestorePreset);
    assert_eq!(
        app.preferences.editor_beautify_presets.len(),
        MAX_SAVED_PRESETS
    );
    assert_eq!(app.preferences.editor_beautify_presets[0], first);
    assert_eq!(app.editor.as_ref().unwrap().visible_toast_action(), None);
    // A second Undo does nothing.
    let _ = update(&mut app, Message::EditorBeautifyRestorePreset);
    assert_eq!(
        app.preferences.editor_beautify_presets.len(),
        MAX_SAVED_PRESETS
    );
}

#[test]
fn undo_for_a_removed_preset_expires_with_its_toast() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let _ = update(&mut app, Message::EditorBeautifySavePreset);
    let _ = update(&mut app, Message::EditorBeautifyDeletePreset(0));
    // Any later status replaces the toast and its Undo.
    app.editor.as_mut().unwrap().set_status("Saved.");
    let _ = update(&mut app, Message::EditorBeautifyRestorePreset);
    assert!(app.preferences.editor_beautify_presets.is_empty());
}

#[test]
fn an_out_of_range_saved_preset_applies_clamped_and_never_panics() {
    use readshot_core::beautify::{SavedBeautifyPreset, MAX_PADDING};
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
    let mut bad = SavedBeautifyPreset::default();
    bad.settings.padding = u32::MAX;
    app.preferences.editor_beautify_presets.push(bad);
    let _ = update(&mut app, Message::EditorBeautifySavedPresetApplied(0));
    assert_eq!(
        app.editor.as_ref().unwrap().model.beautify().padding,
        MAX_PADDING
    );
    let _ = update(&mut app, Message::EditorBeautifySavedPresetApplied(99));
    let _ = update(&mut app, Message::EditorBeautifyDeletePreset(99));
}

#[test]
fn background_hex_and_eyedropper_set_a_solid_background() {
    use readshot_core::beautify::Background;
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
    let _ = update(
        &mut app,
        Message::EditorBeautifyHexChanged("#102030".into()),
    );
    let _ = update(&mut app, Message::EditorBeautifyHexSubmitted);
    assert_eq!(
        app.editor.as_ref().unwrap().model.beautify().background,
        Background::Solid {
            rgba: [0x10, 0x20, 0x30, 255]
        }
    );
    let _ = update(&mut app, Message::EditorBeautifyPickBackground);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(
        ed.eyedropper.as_ref().unwrap().target,
        crate::editor::EyedropperTarget::BeautifyBackground
    );
    let tool_color = ed.model.current_color();
    let _ = update(
        &mut app,
        Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(
            readshot_core::PointLike::new(2.0, 2.0),
        )),
    );
    let ed = app.editor.as_ref().unwrap();
    let px = solid(16, 16).get_pixel(2, 2).0;
    assert_eq!(
        ed.model.beautify().background,
        Background::Solid { rgba: px }
    );
    assert_eq!(
        ed.model.current_color(),
        tool_color,
        "the tool color is untouched"
    );
}

#[test]
fn title_shows_the_output_size_when_beautified() {
    use readshot_core::beautify::BuiltinPreset;
    let mut ed = crate::editor::EditorSession::new(solid(100, 50));
    assert_eq!(editor_size_label(&ed), "100 × 50 px");
    ed.model.set_beautify(BuiltinPreset::Minimal.settings());
    assert_eq!(editor_size_label(&ed), "100 × 50 px → 148 × 98 px");
}

#[test]
fn preview_metrics_fall_back_to_the_bare_image_when_output_overflows() {
    use readshot_core::beautify::BuiltinPreset;
    let w = u32::MAX - 1;
    let m = preview_metrics(
        w,
        20,
        &BuiltinPreset::Minimal.settings(),
        iced::Size::new(800.0, 600.0),
        None,
    );
    assert_eq!(m.pad, iced::Padding::ZERO);
    assert_eq!(m.output_w, w as f32 * m.scale);
}

#[cfg(target_os = "macos")]
#[test]
fn editor_window_draws_under_a_transparent_title_bar() {
    let ps = editor_window_settings(None).platform_specific;
    assert!(ps.title_hidden && ps.titlebar_transparent && ps.fullsize_content_view);
    assert_eq!(TITLEBAR_HEIGHT, 44.0);
    assert_eq!(TITLEBAR_INSET_LEFT, 78.0);
}

#[test]
fn line_width_label_drops_needless_decimals() {
    assert_eq!(line_width_label(3.0), "3");
    assert_eq!(line_width_label(2.5), "2.5");
    assert_eq!(line_width_label(0.5), "0.5");
}

#[test]
fn text_editor_lets_escape_through_to_the_single_cancel_path() {
    // If the text editor also emitted EditorTextCancel, one press would
    // cancel twice (close the popover *and* drop the draft).
    assert!(editor_text_key_binding(escape_key_press()).is_none());
}

#[test]
fn one_esc_closes_the_popover_but_keeps_the_draft() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = editor_with_draft_and_color_popover(&mut app);
    press_escape(&mut app, id);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert_eq!(
        ed.pending_text.as_ref().map(|t| t.content.as_str()),
        Some("hello")
    );

    // With no popover open, the next Esc cancels the draft.
    press_escape(&mut app, id);
    let ed = app.editor.as_ref().expect("editor stays open");
    assert!(ed.pending_text.is_none());
    assert!(ed.discard_pending_at.is_none());
}

#[test]
fn one_esc_closes_the_popover_but_keeps_the_selection() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );

    press_escape(&mut app, id);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert_eq!(ed.model.selected_annotation(), Some(0));

    // Next Esc clears the selection; the editor stays open.
    press_escape(&mut app, id);
    let ed = app.editor.as_ref().expect("editor stays open");
    assert_eq!(ed.model.selected_annotation(), None);
    assert!(ed.discard_pending_at.is_none());
}

#[test]
fn esc_closing_the_width_popover_cancels_a_live_width_preview() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
    let undo_depth = ed.model.undo_depth();
    app.editor = Some(ed);
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Width),
    );
    let _ = update(&mut app, Message::EditorLineWidthPreview(10.0));
    assert!(app.editor.as_ref().unwrap().width_drag_baseline.is_some());
    press_escape(&mut app, id);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.popover, None);
    assert!(ed.width_drag_baseline.is_none());
    assert_eq!(ed.model.undo_depth(), undo_depth);
    match &ed.model.annotations()[0] {
        Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
        other => panic!("expected rectangle, got {other:?}"),
    }
}

#[test]
fn zoom_and_relayout_pin_the_stage_to_the_recorded_offset() {
    // No scroll event follows a zoom or a window resize: each pins the
    // stage scrollable to the offset the editor records, so where a
    // new magnifier bubble goes matches what is on screen.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = crate::editor::EditorSession::new(solid(64, 64));
    let window = iced::window::Id::unique();
    ed.window_id = Some(window);
    app.editor = Some(ed);
    for msg in [
        Message::EditorZoomIn,
        Message::EditorZoomOut,
        Message::EditorZoomActual,
        Message::EditorZoomFit,
        Message::EditorWindowResized(window, 900.0),
    ] {
        let task = update(&mut app, msg.clone());
        assert!(task.units() > 0, "{msg:?} pins the stage offset");
    }
    // Another window's resize does not touch this stage.
    let task = update(
        &mut app,
        Message::EditorWindowResized(iced::window::Id::unique(), 900.0),
    );
    assert_eq!(task.units(), 0);
}

#[test]
fn zoom_keys_keep_the_stage_centre_on_the_same_point() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let ed = crate::editor::EditorSession::new_with_display_scale(solid(1000, 500), 1.0);
    ed.stage_size.set((800.0, 600.0));
    ed.stage_offset.set((100.0, 0.0));
    app.editor = Some(ed);
    app.editor.as_mut().unwrap().zoom = crate::editor::EditorZoom::Percent(1.0);
    let _ = update(&mut app, Message::EditorZoomIn);
    let ed = app.editor.as_ref().unwrap();
    let s = crate::editor::EditorZoom::STEP;
    assert_eq!(ed.zoom, crate::editor::EditorZoom::Percent(s));
    // Document (500, 250) was at the stage centre; it still is.
    let (ox, oy) = ed.stage_offset.get();
    assert!((ox - (500.0 * s - 400.0)).abs() < 1e-3, "{ox}");
    assert!(
        (oy - (250.0 * s + (600.0 - 500.0 * s).max(0.0) / 2.0 - 300.0).max(0.0)).abs() < 1e-3,
        "{oy}"
    );
    // Before, the offset stayed (100, 0).
    assert_ne!((ox, oy), (100.0, 0.0));
}

#[test]
fn zoom_steps_from_fit_use_the_scale_rendered_at_update_time() {
    // The view can be stale after a resize (iced relayouts without a
    // rebuild), so the step resolves the Fit scale when handled.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let ed = crate::editor::EditorSession::new_with_display_scale(solid(64, 64), 2.0);
    ed.render_scale.set(0.4);
    app.editor = Some(ed);
    // Window grew after the last view build: fit is now 0.8.
    app.editor.as_ref().unwrap().render_scale.set(0.8);
    let _ = update(&mut app, Message::EditorZoomIn);
    assert_eq!(
        app.editor.as_ref().unwrap().zoom,
        crate::editor::EditorZoom::Percent(0.8 * crate::editor::EditorZoom::STEP)
    );
    let _ = update(&mut app, Message::EditorZoomFit);
    let _ = update(&mut app, Message::EditorZoomOut);
    assert_eq!(
        app.editor.as_ref().unwrap().zoom,
        crate::editor::EditorZoom::Percent(0.8 / crate::editor::EditorZoom::STEP)
    );
}

#[test]
fn editor_chrome_layers_clear_the_stage() {
    // Hint (one caption line) ends above the image; the toast (one
    // body line + padding) ends below the image's bottom inset.
    const HINT_LINE: f32 = 18.0;
    const TOAST_HEIGHT: f32 = 34.0;
    const { assert!(EDITOR_HINT_TOP + HINT_LINE <= EDITOR_STAGE_PAD_TOP) };
    const { assert!(EDITOR_TOAST_BOTTOM + TOAST_HEIGHT <= EDITOR_STAGE_PAD_BOTTOM) };
    const { assert!(EDITOR_OPTIONS_TOP + EDITOR_OPTIONS_HEIGHT <= EDITOR_STAGE_PAD_TOP) };
    const { assert!(EDITOR_OPTIONS_TOP + EDITOR_OPTIONS_HEIGHT <= EDITOR_POPOVER_TOP) };
    assert_eq!(EDITOR_STAGE_PAD_TOP, 138.0);
}

#[test]
fn editor_toast_kind_classifies_status() {
    assert_eq!(
        editor_toast_kind(
            "Couldn't save — the folder is read-only.",
            false,
            false,
            Tone::Auto
        ),
        ToastKind::Error
    );
    assert_eq!(
        editor_toast_kind("Copied image", false, false, Tone::Auto),
        ToastKind::Ok
    );
    assert_eq!(
        editor_toast_kind("Saving…", true, false, Tone::Auto),
        ToastKind::Info
    );
    assert_eq!(
        editor_toast_kind("anything", false, true, Tone::Auto),
        ToastKind::Warn
    );
    // A status set as a warning is never the success check, but an
    // error string still reads as an error.
    assert_eq!(
        editor_toast_kind(
            "The image changed. Open Resize again.",
            false,
            false,
            Tone::Warning
        ),
        ToastKind::Warn
    );
    assert_eq!(
        editor_toast_kind("Couldn't locate 1 item.", false, false, Tone::Warning),
        ToastKind::Error
    );
}

#[test]
fn refusals_and_partial_failures_toast_as_sticky_warnings() {
    let mut ed = debug_editor_qa_session();
    ed.set_warning("Enter a width and height of at least 1 px.");
    assert!(ed.status_is_warning());
    assert!(ed.status_is_sticky());
    // Any later status (or a direct write) drops the warning.
    ed.set_status("Saved.");
    assert!(!ed.status_is_warning());
    ed.set_warning("x");
    ed.status = Some("Unsaved edits".into());
    assert!(!ed.status_is_warning());
    // Warnings survive the auto-dismiss tick.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = debug_editor_qa_session();
    ed.set_warning("The image changed. Open Resize again.");
    ed.status_set_at = Some(std::time::Instant::now() - crate::editor::STATUS_AUTO_DISMISS * 2);
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorStatusTick);
    assert!(app.editor.as_ref().unwrap().status.is_some());
}

#[test]
fn a_partial_redaction_status_is_a_warning() {
    let s = super::super::text_panel::redact_status(2, 1);
    assert!(s.contains("cover it by hand"), "{s}");
    let mut ed = debug_editor_qa_session();
    ed.set_warning_with_action(
        s.clone(),
        crate::editor::EditorToastAction::UndoRedaction { undo_depth: 0 },
    );
    assert_eq!(
        editor_toast_kind(&s, false, false, ed.status_tone()),
        ToastKind::Warn
    );
}

#[test]
fn status_toast_kind_separates_notices_from_successes() {
    for s in [
        "Capture not found.",
        "No history capture selected.",
        "No OCR text on this capture yet.",
        "No OCR text in the visible captures.",
        "Scrolling capture cancelled.",
    ] {
        assert_eq!(status_toast_kind(s), ToastKind::Info, "{s:?}");
    }
    for s in [
        "Couldn't read the history — permission denied. Close History and open it again.",
        "Couldn't stitch the scrolling capture. Start the scrolling capture again.",
        "Couldn't start the scrolling capture — the selected area was lost. Start the scrolling capture again.",
    ] {
        assert_eq!(status_toast_kind(s), ToastKind::Error, "{s:?}");
    }
    for s in ["Saved to /tmp/x.png.", "Copied text."] {
        assert_eq!(status_toast_kind(s), ToastKind::Ok, "{s:?}");
    }
    // The editor shares the notice rule for its idle statuses.
    assert_eq!(
        editor_toast_kind("No text recognized.", false, false, Tone::Auto),
        ToastKind::Info
    );
}

#[test]
fn history_status_dismissed_clears_history_status() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.history_status = Some("Couldn't read history: denied".into());
    let _ = update(&mut app, Message::HistoryStatusDismissed);
    assert!(app.history_status.is_none());
}

#[test]
fn other_status_after_arming_disarms_discard() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = debug_editor_qa_session();
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorDiscardRequested);
    let ed = app.editor.as_mut().unwrap();
    let armed = ed.discard_pending_at.is_some();
    assert!(armed, "dirty editor should arm");
    ed.set_status("Copying…");
    assert!(ed.discard_pending_at.is_none());
    let s = ed.status.clone().unwrap();
    assert_ne!(
        editor_toast_kind(
            &s,
            crate::editor::status_str_is_in_progress(&s),
            ed.discard_pending_at.is_some(),
            Tone::Auto
        ),
        ToastKind::Warn
    );
    let _ = update(&mut app, Message::EditorDiscardRequested);
    let ed = app.editor.as_mut().unwrap();
    ed.set_status("Couldn't save — disk full.");
    let s = ed.status.clone().unwrap();
    assert_eq!(
        editor_toast_kind(&s, false, ed.discard_pending_at.is_some(), Tone::Auto),
        ToastKind::Error
    );
}

#[test]
fn keep_editing_disarms_discard() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = debug_editor_qa_session();
    ed.discard_pending_at = Some(std::time::Instant::now());
    ed.status = Some("Unsaved edits".into());
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorDiscardDisarmed);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.discard_pending_at.is_none());
    assert!(ed.status.is_none());
}

#[test]
fn dismissing_status_clears_it_and_tick_keeps_errors() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = debug_editor_qa_session();
    ed.status = Some("Couldn't save — the folder is read-only.".into());
    ed.status_set_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorStatusTick);
    assert!(app.editor.as_ref().unwrap().status.is_some());
    let _ = update(&mut app, Message::EditorStatusDismissed);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.status.is_none());
    assert!(ed.status_set_at.is_none());
}

#[test]
fn popover_closed_message_clears_any_open_popover() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.editor = Some(debug_editor_qa_session());
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Width),
    );
    let _ = update(
        &mut app,
        Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
    );
    assert_eq!(
        app.editor.as_ref().unwrap().popover,
        Some(crate::editor::EditorPopover::Color)
    );
    let _ = update(&mut app, Message::EditorPopoverClosed);
    assert_eq!(app.editor.as_ref().unwrap().popover, None);
}

#[test]
fn instructions_toast_as_info_not_success() {
    assert_eq!(
        editor_toast_kind("Press ↵ to crop, esc to cancel.", false, false, Tone::Info),
        ToastKind::Info
    );
    // Drawing a crop draft sets its instruction as Info.
    let mut ed = debug_editor_qa_session();
    ed.set_info("Press ↵ to crop, esc to cancel.");
    assert_eq!(ed.status_tone(), Tone::Info);
    assert!(!ed.status_is_sticky());
    ed.set_status("Cropped.");
    assert_eq!(ed.status_tone(), Tone::Auto);
}

#[test]
fn warnings_can_be_dismissed_and_stop_the_status_tick() {
    assert!(toast_is_dismissable(ToastKind::Warn));
    assert!(toast_is_dismissable(ToastKind::Error));
    assert!(!toast_is_dismissable(ToastKind::Ok));
    assert!(!toast_is_dismissable(ToastKind::Info));
    let mut ed = debug_editor_qa_session();
    ed.set_status("Saved.");
    assert!(crate::editor::status_tick_needed(&ed));
    ed.set_warning("Enter a width and height of at least 1 px.");
    assert!(!crate::editor::status_tick_needed(&ed));
    ed.set_status("Couldn't save — disk full.");
    assert!(!crate::editor::status_tick_needed(&ed));
    // An armed Discard never expires, so it needs no tick (WCAG 2.2.1).
    ed.set_status("Unsaved edits. Press Discard again to close without saving.");
    ed.discard_pending_at = Some(std::time::Instant::now());
    assert!(!crate::editor::status_tick_needed(&ed));
    // Dismiss clears a warning like an error.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = debug_editor_qa_session();
    ed.set_warning("The image changed. Open Resize again.");
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorStatusDismissed);
    assert!(app.editor.as_ref().unwrap().status.is_none());
}

/// WCAG 2.2.1: the armed Discard waits for an answer, however long.
#[test]
fn an_armed_discard_does_not_expire() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let mut ed = debug_editor_qa_session();
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 2.0,
        style: Default::default(),
    });
    app.editor = Some(ed);
    let _ = update(&mut app, Message::EditorDiscardRequested);
    let ed = app.editor.as_mut().unwrap();
    let long_ago = std::time::Instant::now() - std::time::Duration::from_secs(600);
    ed.discard_pending_at = Some(long_ago);
    ed.status_set_at = Some(long_ago);
    let _ = update(&mut app, Message::EditorStatusTick);
    let ed = app.editor.as_ref().unwrap();
    assert!(ed.discard_pending_at.is_some(), "still armed");
    assert!(ed.status.is_some(), "the prompt is still shown");
    // The second press, ten minutes later, discards.
    let _ = update(&mut app, Message::EditorDiscardRequested);
    assert!(app.editor.is_none());
}
