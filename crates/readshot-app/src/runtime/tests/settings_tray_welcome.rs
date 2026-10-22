//! Settings, the tray's History and Settings items, reopen requests and
//! the Permissions pane, through the update loop (W5c slice D).
//!
//! Nothing here runs a returned task: no folder picker, Finder window,
//! OS permission prompt or uninstall step runs. Preferences go to a
//! tempdir file or stay in memory. The one Open at Login test is
//! macOS-only and only ever asks to turn the entry on, which fails
//! before any file is written because the test binary is not inside a
//! Readshot.app bundle. It never asks to remove the entry.

use super::*;
use crate::app::SettingsFocus;
use crate::tray::TrayAction;
use iced::keyboard::{key::Named, Key, Modifiers};

fn tray(app: &mut App, action: TrayAction) -> Task<Message> {
    update(app, Message::Tick(TickMsg::TrayAction(action)))
}

fn count(app: &App, kind: WindowKind) -> usize {
    app.windows.kinds().filter(|k| *k == kind).count()
}

fn key(app: &mut App, id: iced::window::Id, key: Key) {
    let _ = update(
        app,
        Message::Settings(SettingsMsg::KeyPressed {
            window: id,
            key,
            modifiers: Modifiers::empty(),
            captured: false,
        }),
    );
}

fn tab(app: &mut App, id: iced::window::Id) {
    key(app, id, Key::Named(Named::Tab));
}

fn space(app: &mut App, id: iced::window::Id) {
    key(app, id, Key::Named(Named::Space));
}

/// A Settings app whose preferences save to a tempdir file.
fn settings_app_on_disk() -> (tempfile::TempDir, App, iced::window::Id) {
    let dir = tempfile::TempDir::new().unwrap();
    let (mut app, id) = settings_app();
    app.preferences_path = Some(dir.path().join("preferences.toml"));
    (dir, app, id)
}

fn saved(dir: &tempfile::TempDir) -> Preferences {
    Preferences::load(&dir.path().join("preferences.toml")).unwrap()
}

/// ST-01, TR-08: every entry opens the one Settings window (720×640, at
/// least 520×480), a second request focuses it, and Settings opens
/// without the Screen Recording permission.
#[test]
fn settings_opens_one_window_from_every_entry_even_without_permission() {
    let mut app = build_app(Arc::new(FakePermissions::denied()));
    assert!(app.welcome.should_show());
    let _ = tray(&mut app, TrayAction::Settings);
    assert_eq!(count(&app, WindowKind::Settings), 1);
    assert_eq!(count(&app, WindowKind::Welcome), 0, "Settings isn't gated");
    let id = app.settings_window_id.expect("Settings is open");
    assert_eq!(app.windows.kind(id), Some(WindowKind::Settings));

    // The tray again, then the app shortcut's and the Welcome button's
    // message: each focuses the open window.
    let _ = tray(&mut app, TrayAction::Settings);
    let _ = update(&mut app, Message::Settings(SettingsMsg::OpenRequested));
    assert_eq!(count(&app, WindowKind::Settings), 1);
    assert_eq!(app.settings_window_id, Some(id));

    let s = settings_window_settings();
    assert_eq!(s.size, iced::Size::new(720.0, 640.0));
    assert_eq!(s.min_size, Some(iced::Size::new(520.0, 480.0)));
}

/// TR-07: the tray's History opens History once permission is granted
/// and the welcome window while it isn't.
#[test]
fn tray_history_opens_history_or_the_welcome() {
    let mut app = build_app(Arc::new(FakePermissions::denied()));
    let _ = tray(&mut app, TrayAction::History);
    assert!(app.history_window_id.is_none());
    assert_eq!(count(&app, WindowKind::Welcome), 1);

    let mut app = build_app(Arc::new(FakePermissions::granted()));
    let _ = tray(&mut app, TrayAction::History);
    assert!(app.history_window_id.is_some());
    assert_eq!(count(&app, WindowKind::Welcome), 0);
    let _ = tray(&mut app, TrayAction::History);
    assert_eq!(count(&app, WindowKind::History), 1, "focused, not reopened");
}

/// ST-06: the Permissions pane offers Recheck, Reset Permission and
/// Open System Settings while access is denied, only Open System
/// Settings once granted, and nothing where there is nothing to grant.
/// Each control reaches the permissions provider (a fake here).
#[test]
fn the_permissions_pane_offers_what_the_status_allows() {
    use crate::runtime::settings_ops::settings_focus_items;
    let perms = Arc::new(FakePermissions::denied());
    let mut app = build_app(perms.clone());
    let id = iced::window::Id::unique();
    app.settings_window_id = Some(id);
    app.settings_pane = SettingsPane::Permissions;
    assert_eq!(app.permission_status, PermissionStatus::Denied);
    use SettingsFocus as F;
    assert_eq!(
        settings_focus_items(&app),
        vec![F::PermissionRecheck, F::PermissionReset, F::PermissionOpen]
    );

    // Recheck while still denied changes nothing.
    tab(&mut app, id);
    assert_eq!(app.settings_focus, Some(F::PermissionRecheck));
    space(&mut app, id);
    assert_eq!(app.permission_status, PermissionStatus::Denied);

    // Reset Permission clears the grant and asks again; its task (not
    // run) re-probes and then opens System Settings.
    tab(&mut app, id);
    space(&mut app, id);
    assert_eq!(
        perms.reset_calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        perms
            .request_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(app.welcome, crate::welcome::WelcomeState::AwaitingGrant);

    tab(&mut app, id);
    assert_eq!(app.settings_focus, Some(F::PermissionOpen));
    space(&mut app, id);
    assert_eq!(
        perms
            .open_settings_calls
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    // Granted in System Settings: pressing Recheck picks it up and the
    // pane keeps only Open System Settings.
    perms.flip_to_granted();
    assert_eq!(app.permission_status, PermissionStatus::Denied);
    app.settings_focus = Some(F::PermissionRecheck);
    space(&mut app, id);
    assert_eq!(app.permission_status, PermissionStatus::Granted);
    assert_eq!(settings_focus_items(&app), vec![F::PermissionOpen]);

    // Windows and Linux: nothing to grant, nothing to press.
    app.permission_status = PermissionStatus::NotApplicable;
    app.settings_focus = None;
    assert!(settings_focus_items(&app).is_empty());
    tab(&mut app, id);
    assert_eq!(app.settings_focus, None);
}

/// ST-08: recording the History or Settings app shortcut as the capture
/// shortcut is refused with the reserved message, and the previous
/// shortcut is kept in memory and on disk.
#[test]
fn recording_a_reserved_chord_keeps_the_previous_shortcut() {
    let (dir, mut app, _) = settings_app_on_disk();
    app.preferences.capture_hotkey = "ctrl+alt+k".into();
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetDebugLogging(true))),
    );
    assert_eq!(saved(&dir).capture_hotkey, "ctrl+alt+k");
    for (reserved, owner) in [
        (history_hotkey(), "History"),
        (settings_hotkey(), "Settings"),
    ] {
        let _ = update(
            &mut app,
            Message::Settings(SettingsMsg::StartHotkeyRecording),
        );
        let _ = update(
            &mut app,
            Message::Settings(SettingsMsg::HotkeyRecorded(reserved.into())),
        );
        assert!(!app.settings_recording_hotkey, "{reserved}");
        assert_eq!(app.preferences.capture_hotkey, "ctrl+alt+k", "{reserved}");
        let error = app.settings_hotkey_error.clone().unwrap_or_default();
        assert_eq!(
            error,
            format!(
                "{} is reserved for the {owner} shortcut. Choose a different shortcut.",
                pretty_hotkey(reserved)
            )
        );
        assert!(app.settings_hotkey_status.is_none());
        assert_eq!(saved(&dir).capture_hotkey, "ctrl+alt+k", "{reserved}");
    }
}

/// ST-09: the Capture pane's Reset puts the platform default shortcut
/// back (⇧⌘X on macOS, Ctrl+Shift+X elsewhere) and saves it.
#[test]
fn reset_puts_the_platform_default_shortcut_back() {
    let (dir, mut app, id) = settings_app_on_disk();
    app.preferences.capture_hotkey = "ctrl+alt+k".into();
    app.settings_pane = SettingsPane::Capture;
    tab(&mut app, id);
    tab(&mut app, id);
    assert_eq!(app.settings_focus, Some(SettingsFocus::HotkeyReset));
    space(&mut app, id);
    let expected = if cfg!(target_os = "macos") {
        "cmd+shift+x"
    } else {
        "ctrl+shift+x"
    };
    assert_eq!(app.preferences.capture_hotkey, expected);
    assert_eq!(saved(&dir).capture_hotkey, expected);
}

/// ST-12: a picked folder is saved, a cancelled or failed pick changes
/// nothing, Reset goes back to the platform default, and Open reports
/// its result. The picker and Finder tasks are never run.
#[test]
fn the_save_folder_is_picked_reset_and_opened_through_the_loop() {
    let (dir, mut app, id) = settings_app_on_disk();
    let picked = dir.path().join("Shots");
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::SaveFolderPicked(Ok(Some(picked.clone())))),
    );
    assert_eq!(app.preferences.save_folder, picked);
    assert_eq!(saved(&dir).save_folder, picked);

    for nothing in [Ok(None), Err("dialog failed".to_string())] {
        let _ = update(
            &mut app,
            Message::Settings(SettingsMsg::SaveFolderPicked(nothing)),
        );
        assert_eq!(app.preferences.save_folder, picked);
    }

    // Open opens the configured folder (the task isn't run).
    assert_eq!(settings_save_folder_to_open(&app.preferences), picked);
    let open = update(
        &mut app,
        Message::Settings(SettingsMsg::OpenSaveFolderRequested),
    );
    assert!(open.units() > 0);
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::OpenSaveFolderDone(Ok(()))),
    );
    assert_eq!(app.settings_status.as_deref(), Some("Opened save folder."));
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::OpenSaveFolderDone(Err(
            "No such file or directory (os error 2)".into(),
        ))),
    );
    let status = app.settings_status.clone().unwrap();
    assert!(
        status.starts_with("Couldn’t open the save folder"),
        "{status}"
    );

    // Reset (focus ring: Choose, Open, Reset).
    app.settings_pane = SettingsPane::Files;
    for _ in 0..3 {
        tab(&mut app, id);
    }
    assert_eq!(app.settings_focus, Some(SettingsFocus::FolderReset));
    space(&mut app, id);
    assert!(app.preferences.save_folder.as_os_str().is_empty());
    assert!(saved(&dir).save_folder.as_os_str().is_empty());
}

/// ST-25: the hidden update channel is stored and Reset All puts it
/// back to Stable.
#[test]
fn reset_all_puts_the_update_channel_back_to_stable() {
    let (dir, mut app, _) = settings_app_on_disk();
    // Reset All turns a set Open at login off for real (it would remove
    // the developer's own login item), so it must start off here.
    assert!(
        !app.preferences.launch_at_login,
        "Reset All would remove the real login item"
    );
    let _ = update(
        &mut app,
        Message::Settings(SettingsMsg::Edit(SettingsMessage::SetUpdateChannel(
            readshot_core::UpdateChannel::Beta,
        ))),
    );
    assert_eq!(
        saved(&dir).update_channel,
        readshot_core::UpdateChannel::Beta
    );
    let _ = update(&mut app, Message::Settings(SettingsMsg::ResetAllRequested));
    let _ = update(&mut app, Message::Settings(SettingsMsg::ResetAllConfirmed));
    assert_eq!(
        app.preferences.update_channel,
        readshot_core::UpdateChannel::Stable
    );
    assert_eq!(
        saved(&dir).update_channel,
        readshot_core::UpdateChannel::Stable
    );
}

/// TR-17: a reopen (Finder, Spotlight, or a second launch on Windows
/// and Linux) shows the welcome window, or focuses the one that is
/// open; it never opens a second.
#[test]
fn a_reopen_shows_or_focuses_the_one_welcome_window() {
    for perms in [FakePermissions::granted(), FakePermissions::denied()] {
        let mut app = build_app(Arc::new(perms));
        let _ = update(&mut app, Message::App(AppMsg::ReopenRequested));
        assert_eq!(count(&app, WindowKind::Welcome), 1);
        let _ = update(&mut app, Message::App(AppMsg::ReopenRequested));
        assert_eq!(count(&app, WindowKind::Welcome), 1);
    }
}

/// TR-21: a second launch with a `readshot://` URL (Windows, Linux)
/// hands it to the running copy, whose reopen tick puts it on the URL
/// queue the URL tick drains.
#[test]
fn a_second_launch_with_a_url_reaches_the_url_queue() {
    use crate::single_instance::{acquire, Instance, WakeRequest};
    let _lock = crate::url_events::lock_for_tests();
    crate::url_events::clear_for_tests();
    let dir = tempfile::tempdir().unwrap();
    let Instance::Primary(_guard) = acquire(dir.path(), None).unwrap() else {
        panic!("first copy");
    };
    let mut app = build_app(Arc::new(FakePermissions::granted()));
    app.instance_mailbox = Some(dir.path().to_path_buf());
    let url = WakeRequest::Url("readshot://new".into());
    assert!(matches!(
        acquire(dir.path(), Some(&url)).unwrap(),
        Instance::Secondary
    ));
    let _ = update(&mut app, Message::Tick(TickMsg::AppReopen));
    assert_eq!(
        crate::url_events::drain_actions(),
        vec![UrlAction::NewCapture]
    );
}

/// ST-18 (macOS): Open at Login that can't be turned on (here the test
/// binary isn't in a Readshot.app bundle, so nothing is written) turns
/// the toggle back off, in memory and on disk.
#[cfg(target_os = "macos")]
#[test]
fn open_at_login_that_cannot_be_set_turns_back_off() {
    let exe = std::env::current_exe().unwrap();
    assert!(
        !exe.to_string_lossy().contains(".app/Contents/MacOS/"),
        "refusing to run from an app bundle: enabling would write a real LaunchAgent"
    );
    let (dir, mut app, id) = settings_app_on_disk();
    assert!(!app.preferences.launch_at_login);
    press_settings_key(&mut app, id, "l", false);
    assert_eq!(app.settings_pane, SettingsPane::App);
    assert!(!app.preferences.launch_at_login, "reverted");
    assert!(!saved(&dir).launch_at_login, "reverted on disk");
}

/// ST-18 (macOS): after the revert, ⌘L announces the real state (Off)
/// and why the change failed, and the toggle's row shows the reason in
/// the error tone until Settings closes.
#[cfg(target_os = "macos")]
#[test]
fn a_failed_open_at_login_toggle_does_not_announce_on() {
    let exe = std::env::current_exe().unwrap();
    assert!(!exe.to_string_lossy().contains(".app/Contents/MacOS/"));
    let (_dir, mut app, id) = settings_app_on_disk();
    press_settings_key(&mut app, id, "l", false);
    assert!(!app.preferences.launch_at_login);
    let feedback = app.settings_key_feedback.clone().unwrap_or_default();
    assert!(
        feedback.starts_with("Open at login: Off. Couldn’t turn on Open at login"),
        "announces the real state and the failure: {feedback}"
    );
    let why = app
        .settings_launch_at_login_error
        .clone()
        .expect("an error is shown");
    assert!(why.contains("Readshot.app"), "{why}");
    assert!(feedback.ends_with(&why), "{feedback}");
    let _ = update(&mut app, Message::Window(WindowMsg::Closed(id)));
    assert_eq!(app.settings_launch_at_login_error, None, "cleared on close");
}

/// ST-18: the failure line names the change, a reason the user can act
/// on, and the next step; it never shows a raw path or error.
#[test]
fn an_open_at_login_failure_says_what_failed_and_why() {
    use crate::startup::StartupError;
    let io = StartupError::Io {
        path: "/secret/LaunchAgents/x.plist".into(),
        source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
    };
    let off = crate::runtime::settings_ops::launch_at_login_failure(false, &io);
    assert_eq!(
        off,
        "Couldn’t turn off Open at login — permission denied. Try again."
    );
    let bundle = StartupError::UnsupportedBundleLayout("/tmp/readshot".into());
    let on = crate::runtime::settings_ops::launch_at_login_failure(true, &bundle);
    assert!(on.starts_with("Couldn’t turn on Open at login — "), "{on}");
    assert!(!on.contains("/tmp/readshot"), "{on}");
}
