//! Key maps of the scroll HUD, welcome, History, pins and Settings.

use super::*;

#[test]
fn scroll_keys_map_stop_and_discard() {
    use iced::keyboard::{key::Named, Key, Modifiers};
    let esc = Key::Named(Named::Escape);
    // Esc alone stops & stitches (same as the Stop button).
    assert!(matches!(
        scroll_key_message(&esc, Modifiers::empty()),
        Some(Message::Scroll(ScrollMsg::StopRequested))
    ));
    // Shift+Esc and ⌘. discard (same as the Cancel button, which
    // arms then confirms on repeat).
    assert!(matches!(
        scroll_key_message(&esc, Modifiers::SHIFT),
        Some(Message::Scroll(ScrollMsg::CancelRequested))
    ));
    let dot = Key::Character(".".into());
    assert!(matches!(
        scroll_key_message(&dot, command_modifiers()),
        Some(Message::Scroll(ScrollMsg::CancelRequested))
    ));
    assert!(scroll_key_message(&dot, Modifiers::empty()).is_none());
}

#[test]
fn welcome_enter_triggers_card_primary_action() {
    use crate::welcome::WelcomeState;
    use iced::keyboard::{key::Named, Key, Modifiers};
    let enter = Key::Named(Named::Enter);
    assert!(matches!(
        welcome_key_message(WelcomeState::Pending, false, &enter, Modifiers::empty()),
        Some(Message::App(AppMsg::GrantPermissionRequested))
    ));
    assert!(matches!(
        welcome_key_message(WelcomeState::Denied, false, &enter, Modifiers::empty()),
        Some(Message::App(AppMsg::OpenPermissionSettingsRequested))
    ));
    assert!(matches!(
        welcome_key_message(WelcomeState::Granted, false, &enter, Modifiers::empty()),
        Some(Message::Capture(CaptureMsg::OpenOverlayRequested))
    ));
    // Capture is disabled (so Enter is a no-op) while one is running.
    assert!(welcome_key_message(WelcomeState::Granted, true, &enter, Modifiers::empty()).is_none());
    // A chord is not the bare-Enter primary activation.
    assert!(
        welcome_key_message(WelcomeState::Pending, false, &enter, command_modifiers()).is_none()
    );
}

#[test]
fn editor_backspace_and_forward_delete_both_remove_selection() {
    use iced::keyboard::{key::Named, Key, Modifiers};
    for named in [Named::Backspace, Named::Delete] {
        let m = editor_key_message(Key::Named(named), Modifiers::empty(), true);
        assert!(
            matches!(m, Some(Message::EditorDeleteSelected)),
            "{named:?}"
        );
        // While a text field is editing (event captured) neither fires.
        assert!(editor_key_message(Key::Named(named), Modifiers::empty(), false).is_none());
    }
}

#[test]
fn history_copy_keys_match_overlay_and_editor() {
    use iced::keyboard::{Key, Modifiers};
    let w = iced::window::Id::unique();
    let c = Key::Character("c".into());
    let cmd_shift = command_modifiers() | Modifiers::SHIFT;
    assert!(matches!(
        history_key_message(w, &c, command_modifiers(), true),
        Some(Message::History(HistoryMsg::KeyboardShortcut(
            _,
            HistoryKeyboardAction::CopyImage
        )))
    ));
    assert!(matches!(
        history_key_message(w, &c, cmd_shift, true),
        Some(Message::History(HistoryMsg::KeyboardShortcut(
            _,
            HistoryKeyboardAction::CopyText
        )))
    ));
    // A focused search field captured it: deferred to the handler,
    // which only applies it while the query is empty.
    assert!(matches!(
        history_key_message(w, &c, command_modifiers(), false),
        Some(Message::History(HistoryMsg::CapturedShortcut(
            _,
            HistoryKeyboardAction::CopyImage
        )))
    ));
    assert!(matches!(
        history_key_message(w, &c, cmd_shift, false),
        Some(Message::History(HistoryMsg::CapturedShortcut(
            _,
            HistoryKeyboardAction::CopyText
        )))
    ));
}

#[test]
fn history_delete_keys() {
    use iced::keyboard::{key::Named, Key, Modifiers};
    let w = iced::window::Id::unique();
    let is_delete = |m: Option<Message>| {
        matches!(
            m,
            Some(Message::History(HistoryMsg::KeyboardShortcut(
                _,
                HistoryKeyboardAction::Delete
            )))
        )
    };
    let bs = Key::Named(Named::Backspace);
    let del = Key::Named(Named::Delete);
    // ⌘⌫ and ⌦ delete.
    assert!(is_delete(history_key_message(
        w,
        &bs,
        command_modifiers(),
        true
    )));
    assert!(is_delete(history_key_message(
        w,
        &del,
        Modifiers::empty(),
        true
    )));
    // Plain ⌫ never deletes a capture.
    assert!(history_key_message(w, &bs, Modifiers::empty(), true).is_none());
    assert!(history_key_message(w, &bs, Modifiers::empty(), false).is_none());
    // Captured by the search field: deferred, never immediate.
    assert!(matches!(
        history_key_message(w, &bs, command_modifiers(), false),
        Some(Message::History(HistoryMsg::CapturedShortcut(
            _,
            HistoryKeyboardAction::Delete
        )))
    ));
    assert!(matches!(
        history_key_message(w, &del, Modifiers::empty(), false),
        Some(Message::History(HistoryMsg::CapturedShortcut(
            _,
            HistoryKeyboardAction::Delete
        )))
    ));
}

#[test]
fn history_captured_shortcut_applies_only_for_empty_search() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.history_window_id = Some(id);
    let captured = |a| Message::History(HistoryMsg::CapturedShortcut(id, a));

    // Non-empty query: the field owns the chord, nothing happens.
    app.history_search = "invoice".into();
    let _ = update(&mut app, captured(HistoryKeyboardAction::CopyImage));
    assert_eq!(app.history_status, None);

    // Empty query: it reaches History (no selection -> status).
    app.history_search.clear();
    let _ = update(&mut app, captured(HistoryKeyboardAction::CopyImage));
    assert_eq!(
        app.history_status.as_deref(),
        Some("No history capture selected.")
    );
}

#[test]
fn history_second_delete_key_confirms() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.history_window_id = Some(id);
    let rec = readshot_core::Uuid::new_v4();
    app.history_selected_id = Some(rec);
    let del = Message::History(HistoryMsg::KeyboardShortcut(
        id,
        HistoryKeyboardAction::Delete,
    ));
    let _ = update(&mut app, del.clone());
    assert_eq!(app.history_delete_pending, Some(rec));
    // Second press goes down the confirm path (pending is consumed).
    let _ = update(&mut app, del);
    assert_ne!(app.history_delete_pending, Some(rec));
}

#[test]
fn history_arrow_while_search_focused_returns_nav_message() {
    use iced::keyboard::{key::Named, Key, Modifiers};
    let w = iced::window::Id::unique();
    assert!(matches!(
        history_key_message(w, &Key::Named(Named::ArrowDown), Modifiers::empty(), false),
        Some(Message::History(HistoryMsg::KeyboardShortcut(
            _,
            HistoryKeyboardAction::Next
        )))
    ));
    // The handler pairs the move with an unfocus task for the field.
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.history_window_id = Some(w);
    let task = update(
        &mut app,
        Message::History(HistoryMsg::KeyboardShortcut(w, HistoryKeyboardAction::Next)),
    );
    assert!(task.units() >= 1);
}

#[test]
fn pin_keys_map_to_actions() {
    use iced::keyboard::{key::Named, Key, Modifiers};
    assert_eq!(
        pin_key_action(&Key::Named(Named::Escape), Modifiers::empty()),
        Some(PinKeyAction::Close)
    );
    assert_eq!(
        pin_key_action(&Key::Character("w".into()), command_modifiers()),
        Some(PinKeyAction::Close)
    );
    assert_eq!(
        pin_key_action(&Key::Character("l".into()), Modifiers::empty()),
        Some(PinKeyAction::ToggleLock)
    );
    assert_eq!(
        pin_key_action(&Key::Character("=".into()), Modifiers::empty()),
        Some(PinKeyAction::OpacityUp)
    );
    assert_eq!(
        pin_key_action(&Key::Character("-".into()), Modifiers::empty()),
        Some(PinKeyAction::OpacityDown)
    );
    // Arrows move the window: 1 px, Shift → 10 px (same as the
    // overlay and editor nudges).
    assert_eq!(
        pin_key_action(&Key::Named(Named::ArrowLeft), Modifiers::empty()),
        Some(PinKeyAction::Nudge(-1.0, 0.0))
    );
    assert_eq!(
        pin_key_action(&Key::Named(Named::ArrowDown), Modifiers::SHIFT),
        Some(PinKeyAction::Nudge(0.0, 10.0))
    );
    // An unmapped plain key is ignored.
    assert_eq!(
        pin_key_action(&Key::Character("q".into()), Modifiers::empty()),
        None
    );
}

#[test]
fn hud_recording_pulse_is_constant_under_reduce_motion() {
    // Reduce-motion: constant full alpha at any elapsed time.
    for t in [0.0_f32, 0.35, 0.7, 1.4, 3.3] {
        assert_eq!(hud_recording_pulse(t, true), 1.0);
    }
    // Motion allowed: breathes inside ~0.9..=1.0, never unreadable.
    for t in [0.0_f32, 0.35, 0.7, 1.4, 3.3] {
        let a = hud_recording_pulse(t, false);
        assert!(
            (0.89..=1.01).contains(&a),
            "pulse {a} out of range at t={t}"
        );
    }
}

#[test]
fn settings_keys_map_to_actions() {
    use iced::keyboard::{key::Named, Key, Modifiers};
    let cmd = command_modifiers();
    assert_eq!(
        settings_key_action(&Key::Named(Named::Escape), Modifiers::empty()),
        Some(SettingsKeyAction::Close)
    );
    assert_eq!(
        settings_key_action(&Key::Character("w".into()), cmd),
        Some(SettingsKeyAction::Close)
    );
    assert_eq!(
        settings_key_action(&Key::Character("l".into()), cmd),
        Some(SettingsKeyAction::ToggleLaunchAtLogin)
    );
    assert_eq!(
        settings_key_action(&Key::Character("d".into()), cmd),
        Some(SettingsKeyAction::ToggleDebugLogging)
    );
    assert_eq!(
        settings_key_action(&Key::Character("r".into()), cmd),
        Some(SettingsKeyAction::CycleRetention)
    );
    assert_eq!(
        settings_key_action(&Key::Character("e".into()), cmd),
        Some(SettingsKeyAction::CycleEngine)
    );
    assert_eq!(
        settings_key_action(&Key::Character("f".into()), cmd),
        Some(SettingsKeyAction::CycleFormat)
    );
    // Destructive commands need ⇧⌘ and only arm; the second press
    // (resolved in the runtime) confirms, so a single press can't wipe.
    let mut cmd_shift = cmd;
    cmd_shift.insert(Modifiers::SHIFT);
    assert_eq!(
        settings_key_action(&Key::Character("r".into()), cmd_shift),
        Some(SettingsKeyAction::ResetAll)
    );
    assert_eq!(
        settings_key_action(&Key::Character("u".into()), cmd_shift),
        Some(SettingsKeyAction::Uninstall)
    );
    // A bare letter (no ⌘) never changes a setting — it must reach
    // the filename text_input instead.
    assert_eq!(
        settings_key_action(&Key::Character("l".into()), Modifiers::empty()),
        None
    );
}

#[test]
fn command_digits_select_settings_panes() {
    use iced::keyboard::{Key, Modifiers};
    let k = |c: &str| Key::Character(c.into());
    assert!(matches!(
        settings_key_action(&k("1"), Modifiers::COMMAND),
        Some(SettingsKeyAction::SelectPane(SettingsPane::Permissions))
    ));
    assert!(matches!(
        settings_key_action(&k("7"), Modifiers::COMMAND),
        Some(SettingsKeyAction::SelectPane(SettingsPane::App))
    ));
    assert!(settings_key_action(&k("8"), Modifiers::COMMAND).is_none());
    // A bare digit is typing (e.g. in the filename field), not a pane switch.
    assert!(settings_key_action(&k("3"), Modifiers::empty()).is_none());
}

#[test]
fn pane_labels_follow_the_spec_order() {
    let labels: Vec<_> = SettingsPane::ALL.iter().map(|p| p.label()).collect();
    assert_eq!(
        labels,
        [
            "Permissions",
            "Capture",
            "Files",
            "Text Recognition",
            "Export",
            "Diagnostics",
            "App"
        ]
    );
}

#[test]
fn selecting_a_pane_is_remembered() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::PaneSelected(SettingsPane::Export)),
    );
    assert_eq!(app.settings_pane, SettingsPane::Export);
}

#[test]
fn destructive_shortcut_from_another_pane_arms_and_shows_app() {
    for (key, armed) in [
        (
            "r",
            (|a: &App| a.settings_reset_all_pending) as fn(&App) -> bool,
        ),
        ("u", |a: &App| a.settings_uninstall_pending),
    ] {
        let (mut app, id) = settings_app();
        app.settings_pane = SettingsPane::Permissions;
        press_settings_key(&mut app, id, key, true);
        assert_eq!(app.settings_pane, SettingsPane::App, "⇧⌘{key}");
        assert!(armed(&app), "⇧⌘{key} arms");
    }
}

#[test]
fn reset_confirm_from_another_pane_only_shows_the_prompt() {
    let (mut app, id) = settings_app();
    app.preferences.debug_logging = true;
    app.settings_reset_all_pending = true;
    app.settings_pane = SettingsPane::Capture;
    press_settings_key(&mut app, id, "r", true);
    assert_eq!(app.settings_pane, SettingsPane::App);
    assert!(app.settings_reset_all_pending, "still armed, not confirmed");
    assert!(app.preferences.debug_logging, "preferences not wiped");
}

#[test]
fn reset_confirm_on_app_pane_still_resets() {
    let (mut app, id) = settings_app();
    app.preferences.debug_logging = true;
    app.settings_reset_all_pending = true;
    app.settings_pane = SettingsPane::App;
    press_settings_key(&mut app, id, "r", true);
    assert!(!app.settings_reset_all_pending);
    assert_eq!(
        app.preferences.debug_logging,
        Preferences::default().debug_logging
    );
}

#[test]
fn uninstall_confirm_from_another_pane_only_shows_the_prompt() {
    let (mut app, id) = settings_app();
    app.settings_uninstall_pending = true;
    app.settings_pane = SettingsPane::Files;
    press_settings_key(&mut app, id, "u", true);
    assert_eq!(app.settings_pane, SettingsPane::App);
    assert!(app.settings_uninstall_pending, "still armed, not confirmed");
}

#[test]
fn setting_shortcut_switches_to_its_pane() {
    let (mut app, id) = settings_app();
    app.settings_pane = SettingsPane::Permissions;
    let before = app.preferences.debug_logging;
    press_settings_key(&mut app, id, "d", false);
    assert_eq!(app.settings_pane, SettingsPane::Diagnostics);
    assert_ne!(app.preferences.debug_logging, before);
}

#[test]
fn leaving_capture_pane_cancels_hotkey_recording() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.settings_pane = SettingsPane::Capture;
    app.settings_recording_hotkey = true;
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::PaneSelected(SettingsPane::Capture)),
    );
    assert!(app.settings_recording_hotkey, "staying on Capture keeps it");
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::PaneSelected(SettingsPane::Files)),
    );
    assert!(!app.settings_recording_hotkey);
    assert_eq!(app.settings_pane, SettingsPane::Files);
}

#[test]
fn command_digit_in_settings_window_switches_pane() {
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let id = iced::window::Id::unique();
    app.settings_window_id = Some(id);
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::KeyPressed {
            window: id,
            key: iced::keyboard::Key::Character("3".into()),
            modifiers: command_modifiers(),
            captured: false,
        }),
    );
    assert_eq!(app.settings_pane, SettingsPane::Files);
    // Another window's key press is ignored.
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::KeyPressed {
            window: iced::window::Id::unique(),
            key: iced::keyboard::Key::Character("7".into()),
            modifiers: command_modifiers(),
            captured: false,
        }),
    );
    assert_eq!(app.settings_pane, SettingsPane::Files);
}

/// The platform's command key (⌘, or Ctrl off macOS) drives every
/// History chord: Ctrl itself must not be refused as an "extra" key off
/// macOS (it was, so Ctrl+F / Ctrl+C / Ctrl+Backspace did nothing).
#[test]
fn history_command_chords_work_with_the_platform_command_key() {
    use iced::keyboard::{key::Named, Key, Modifiers};
    let w = iced::window::Id::unique();
    let f = Key::Character("f".into());
    assert!(matches!(
        history_key_message(w, &f, command_modifiers(), true),
        Some(Message::History(HistoryMsg::KeyboardShortcut(
            _,
            HistoryKeyboardAction::FocusSearch
        )))
    ));
    assert!(matches!(
        history_key_message(w, &Key::Named(Named::Backspace), command_modifiers(), true),
        Some(Message::History(HistoryMsg::KeyboardShortcut(
            _,
            HistoryKeyboardAction::Delete
        )))
    ));
    // The other command-ish key on top (⌃ on macOS, the logo key
    // elsewhere) is not the chord.
    let mut extra = command_modifiers();
    extra.insert(if cfg!(target_os = "macos") {
        Modifiers::CTRL
    } else {
        Modifiers::LOGO
    });
    assert!(history_key_message(w, &f, extra, true).is_none());
    assert!(history_key_message(w, &Key::Character("c".into()), extra, true).is_none());
    // ⌦ alone deletes; with the command key it is not a delete chord.
    assert!(matches!(
        history_key_message(w, &Key::Named(Named::Delete), Modifiers::empty(), true),
        Some(Message::History(HistoryMsg::KeyboardShortcut(
            _,
            HistoryKeyboardAction::Delete
        )))
    ));
}
