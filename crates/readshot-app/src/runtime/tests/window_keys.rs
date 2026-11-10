//! Keyboard paths of the History, Settings and Welcome windows (WCAG
//! 2.1.1): the actions that were pointer-only, the Settings focus ring,
//! Esc stepping back one level, and what a Settings shortcut changed.

use super::*;
use crate::app::SettingsFocus;
use iced::keyboard::{key::Named, Key, Modifiers};

fn hist(m: Option<Message>) -> Option<HistoryKeyboardAction> {
    match m {
        Some(
            Message::History(HistoryMsg::KeyboardShortcut(_, a))
            | Message::History(HistoryMsg::CapturedShortcut(_, a)),
        ) => Some(a),
        _ => None,
    }
}

#[test]
fn history_reveal_pin_copy_visible_and_clear_all_have_keys() {
    let w = iced::window::Id::unique();
    let cmd = command_modifiers();
    let ch = |c: &str| Key::Character(c.into());
    // ⌥⌘ on macOS, Ctrl+Shift elsewhere (Ctrl+Alt is AltGr).
    let second = if cfg!(target_os = "macos") {
        cmd | Modifiers::ALT
    } else {
        cmd | Modifiers::SHIFT
    };
    assert_eq!(
        hist(history_key_message(w, &ch("r"), second, true)),
        Some(HistoryKeyboardAction::Reveal)
    );
    assert_eq!(
        hist(history_key_message(w, &ch("p"), cmd, true)),
        Some(HistoryKeyboardAction::Pin)
    );
    let (visible_key, visible_mods) = if cfg!(target_os = "macos") {
        (ch("c"), cmd | Modifiers::ALT | Modifiers::SHIFT)
    } else {
        (ch("a"), cmd | Modifiers::SHIFT)
    };
    assert_eq!(
        hist(history_key_message(w, &visible_key, visible_mods, true)),
        Some(HistoryKeyboardAction::CopyVisibleText)
    );
    assert_eq!(
        hist(history_key_message(
            w,
            &Key::Named(Named::Backspace),
            cmd | Modifiers::SHIFT,
            true
        )),
        Some(HistoryKeyboardAction::ClearAll)
    );
    assert_eq!(
        hist(history_key_message(
            w,
            &Key::Named(Named::Escape),
            Modifiers::empty(),
            true
        )),
        Some(HistoryKeyboardAction::Escape)
    );
    // The existing chords still reach their own actions.
    assert_eq!(
        hist(history_key_message(w, &ch("c"), cmd, true)),
        Some(HistoryKeyboardAction::CopyImage)
    );
    assert_eq!(
        hist(history_key_message(
            w,
            &ch("c"),
            cmd | Modifiers::SHIFT,
            true
        )),
        Some(HistoryKeyboardAction::CopyText)
    );
    assert_eq!(
        hist(history_key_message(
            w,
            &Key::Named(Named::Backspace),
            cmd,
            true
        )),
        Some(HistoryKeyboardAction::Delete)
    );
    assert_eq!(
        hist(history_key_message(w, &ch("f"), cmd, true)),
        Some(HistoryKeyboardAction::FocusSearch)
    );
}

#[test]
fn clear_all_arms_then_confirms_and_esc_cancels_it() {
    let (_dir, _store, mut app, _newer, _older) = app_with_two_saved_records();
    let w = iced::window::Id::unique();
    app.history_window_id = Some(w);
    let clear = Message::History(HistoryMsg::KeyboardShortcut(
        w,
        HistoryKeyboardAction::ClearAll,
    ));
    let esc = Message::History(HistoryMsg::KeyboardShortcut(
        w,
        HistoryKeyboardAction::Escape,
    ));
    let _ = update(&mut app, clear.clone());
    assert!(app.history_prompt.is_clear_all());
    assert_eq!(app.history_records.len(), 2, "one press only arms");
    let _ = update(&mut app, esc);
    assert!(!app.history_prompt.is_clear_all(), "esc cancels the prompt");
    let _ = update(&mut app, clear.clone());
    let _ = update(&mut app, clear);
    assert!(app.history_records.is_empty());
}

#[test]
fn moving_past_the_page_shows_more() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let page = crate::app::HISTORY_PAGE_SIZE;
    app.history_records = (0..page + 3)
        .map(|_| readshot_core::CaptureRecord::new(chrono::Utc::now(), 10, 10, "d".to_string()))
        .collect();
    app.history_selected_id = Some(app.history_records[page - 1].id);
    let limit = app.history_page_limit;
    assert_eq!(limit, page);
    let _ = update(&mut app, Message::History(HistoryMsg::SelectNext));
    assert_eq!(app.history_selected_id, Some(app.history_records[page].id));
    assert!(
        app.history_page_limit > page,
        "the selection stays rendered"
    );
}

fn settings_key(app: &mut App, id: iced::window::Id, key: Key, m: Modifiers, captured: bool) {
    let _ = update(
        app,
        Message::Settings(SettingsMsg::KeyPressed {
            window: id,
            key,
            modifiers: m,
            captured,
        }),
    );
}

#[test]
fn tab_walks_the_settings_pane_and_space_presses() {
    let (mut app, id) = settings_app();
    app.settings_pane = SettingsPane::App;
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Tab),
        Modifiers::empty(),
        false,
    );
    assert_eq!(app.settings_focus, Some(SettingsFocus::LaunchAtLogin));
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Tab),
        Modifiers::empty(),
        false,
    );
    assert_eq!(app.settings_focus, Some(SettingsFocus::ResetAll));
    // Space arms the reset; the focus follows to the Confirm step.
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Space),
        Modifiers::empty(),
        false,
    );
    assert!(app.settings_mode.is_confirming_reset_all());
    assert_eq!(app.settings_focus, Some(SettingsFocus::ResetConfirm));
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Tab),
        Modifiers::empty(),
        false,
    );
    assert_eq!(app.settings_focus, Some(SettingsFocus::ResetCancel));
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Enter),
        Modifiers::empty(),
        false,
    );
    assert!(!app.settings_mode.is_confirming_reset_all());
    // ⇧Tab wraps backwards.
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Tab),
        Modifiers::SHIFT,
        false,
    );
    assert!(app.settings_focus.is_some());
}

#[test]
fn settings_items_cover_every_pointer_only_control() {
    let (mut app, _id) = settings_app();
    app.settings_pane = SettingsPane::Files;
    assert_eq!(
        settings_ops::settings_focus_items(&app),
        vec![
            SettingsFocus::FolderChoose,
            SettingsFocus::FolderOpen,
            SettingsFocus::FolderReset,
            SettingsFocus::FilenameField
        ]
    );
    app.settings_pane = SettingsPane::Capture;
    assert!(settings_ops::settings_focus_items(&app).contains(&SettingsFocus::HotkeyRecord));
    app.settings_pane = SettingsPane::Permissions;
    app.permission_status = PermissionStatus::Denied;
    assert_eq!(settings_ops::settings_focus_items(&app).len(), 3);
    app.settings_pane = SettingsPane::App;
    app.settings_mode = SettingsMode::ConfirmUninstall { purge: false };
    assert!(settings_ops::settings_focus_items(&app).contains(&SettingsFocus::UninstallPurge));
}

#[test]
fn arrows_change_a_settings_picker_and_say_what_changed() {
    let (mut app, id) = settings_app();
    app.settings_pane = SettingsPane::Export;
    let before = app.preferences.default_format;
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Tab),
        Modifiers::empty(),
        false,
    );
    assert_eq!(app.settings_focus, Some(SettingsFocus::Format));
    settings_key(
        &mut app,
        id,
        Key::Named(Named::ArrowRight),
        Modifiers::empty(),
        false,
    );
    assert_ne!(app.preferences.default_format, before);
    let feedback = app.settings_key_feedback.clone().unwrap();
    assert!(feedback.starts_with("Default format: "), "{feedback}");
    settings_key(
        &mut app,
        id,
        Key::Named(Named::ArrowLeft),
        Modifiers::empty(),
        false,
    );
    assert_eq!(app.preferences.default_format, before);
}

/// A ⌘ cycle (⌘F, ⌘R) is never silent: it names the new value.
#[test]
fn settings_cycle_shortcuts_report_the_new_value() {
    let (mut app, id) = settings_app();
    press_settings_key(&mut app, id, "r", false);
    let feedback = app.settings_key_feedback.clone().unwrap();
    assert!(
        feedback.starts_with(&format!(
            "History retention: {}",
            app.preferences.history_retention
        )),
        "{feedback}"
    );
    // A choice that deletes older captures says so.
    use readshot_core::HistoryRetention as R;
    // Every retention line is whole sentences.
    for v in [R::Off, R::Last50, R::Last30Days, R::Unlimited] {
        assert!(crate::runtime::settings_ops::retention_feedback(v).ends_with('.'));
    }
    for v in [R::Last50, R::Last30Days] {
        assert!(crate::runtime::settings_ops::retention_feedback(v)
            .ends_with("Older captures are deleted at the next capture."));
    }
    assert!(crate::runtime::view::RETENTION_HELP.contains("permanently delete older captures"));
    press_settings_key(&mut app, id, "f", false);
    assert!(app
        .settings_key_feedback
        .as_deref()
        .unwrap()
        .starts_with("Default format: "));
    // Another pane clears it.
    press_settings_key(&mut app, id, "1", false);
    assert!(app.settings_key_feedback.is_none());
}

#[test]
fn command_k_records_a_shortcut() {
    assert_eq!(
        settings_key_action(&Key::Character("k".into()), command_modifiers()),
        Some(SettingsKeyAction::RecordShortcut)
    );
    let (mut app, id) = settings_app();
    press_settings_key(&mut app, id, "k", false);
    assert!(app.settings_mode.is_recording_hotkey());
    assert_eq!(app.settings_pane, SettingsPane::Capture);
}

/// Esc steps back: a focused field's Esc only leaves the field, then the
/// ring, and only then the window closes.
#[test]
fn esc_in_the_filename_field_does_not_close_settings() {
    let (mut app, id) = settings_app();
    app.windows.register(id, WindowKind::Settings);
    app.settings_pane = SettingsPane::Files;
    app.settings_focus = Some(SettingsFocus::FilenameField);
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Escape),
        Modifiers::empty(),
        true,
    );
    assert_eq!(app.windows.kind(id), Some(WindowKind::Settings));
    assert_eq!(app.settings_focus, None);
    app.settings_focus = Some(SettingsFocus::FolderOpen);
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Escape),
        Modifiers::empty(),
        false,
    );
    assert_eq!(app.windows.kind(id), Some(WindowKind::Settings));
    assert_eq!(app.settings_focus, None);
    settings_key(
        &mut app,
        id,
        Key::Named(Named::Escape),
        Modifiers::empty(),
        false,
    );
    assert_eq!(app.windows.kind(id), None, "the third esc closes");
}

#[test]
fn a_focused_filename_field_keeps_its_keys_but_tab() {
    assert_eq!(
        settings_ops::settings_focus_command(
            true,
            &Key::Named(Named::ArrowRight),
            Modifiers::empty(),
            true
        ),
        None
    );
    assert_eq!(
        settings_ops::settings_focus_command(
            true,
            &Key::Named(Named::Space),
            Modifiers::empty(),
            true
        ),
        None
    );
    assert_eq!(
        settings_ops::settings_focus_command(
            true,
            &Key::Named(Named::Tab),
            Modifiers::empty(),
            true
        ),
        Some(FocusKey::Next)
    );
    // Without the ring nothing but Tab is taken (plain typing is safe).
    assert_eq!(
        settings_ops::settings_focus_command(
            false,
            &Key::Named(Named::Space),
            Modifiers::empty(),
            false
        ),
        None
    );
}

#[test]
fn welcome_restart_and_reset_permission_have_keys() {
    use crate::welcome::WelcomeState as W;
    let r = Key::Character("r".into());
    let cmd = command_modifiers();
    let shift = cmd | Modifiers::SHIFT;
    assert!(matches!(
        welcome_key_message(W::Denied, false, &r, cmd),
        Some(Message::App(AppMsg::RestartRequested))
    ));
    assert!(matches!(
        welcome_key_message(W::AwaitingGrant, false, &r, shift),
        Some(Message::App(AppMsg::ResetPermissionRequested))
    ));
    assert!(matches!(
        welcome_key_message(W::Pending, false, &r, shift),
        Some(Message::App(AppMsg::ResetPermissionRequested))
    ));
    // Only while those buttons are on the card.
    assert!(welcome_key_message(W::Pending, false, &r, cmd).is_none());
    assert!(welcome_key_message(W::Granted, false, &r, shift).is_none());
    // A bare R is never Reset Permission.
    assert!(welcome_key_message(W::Denied, false, &r, Modifiers::empty()).is_none());
}

/// Every History chord is distinct, and none is Ctrl+Alt off macOS.
#[test]
fn history_chords_are_unique_and_avoid_ctrl_alt() {
    let w = iced::window::Id::unique();
    let cmd = command_modifiers();
    let ch = |c: &str| Key::Character(c.into());
    let mac = cfg!(target_os = "macos");
    let second = if mac {
        cmd | Modifiers::ALT
    } else {
        cmd | Modifiers::SHIFT
    };
    let rows: Vec<(Key, Modifiers, HistoryKeyboardAction)> = vec![
        (ch("c"), cmd, HistoryKeyboardAction::CopyImage),
        (
            ch("c"),
            cmd | Modifiers::SHIFT,
            HistoryKeyboardAction::CopyText,
        ),
        (ch("f"), cmd, HistoryKeyboardAction::FocusSearch),
        (ch("p"), cmd, HistoryKeyboardAction::Pin),
        (ch("r"), second, HistoryKeyboardAction::Reveal),
        if mac {
            (
                ch("c"),
                cmd | Modifiers::ALT | Modifiers::SHIFT,
                HistoryKeyboardAction::CopyVisibleText,
            )
        } else {
            (
                ch("a"),
                cmd | Modifiers::SHIFT,
                HistoryKeyboardAction::CopyVisibleText,
            )
        },
        (
            Key::Named(Named::Backspace),
            cmd,
            HistoryKeyboardAction::Delete,
        ),
        (
            Key::Named(Named::Backspace),
            cmd | Modifiers::SHIFT,
            HistoryKeyboardAction::ClearAll,
        ),
    ];
    for (i, (k, m, _)) in rows.iter().enumerate() {
        for (k2, m2, a) in &rows[i + 1..] {
            assert!(!(k == k2 && m == m2), "{k:?} {m:?} bound twice ({a:?})");
        }
    }
    for (k, m, want) in rows {
        assert_eq!(
            hist(history_key_message(w, &k, m, true)),
            Some(want),
            "{k:?} {m:?}"
        );
    }
}
