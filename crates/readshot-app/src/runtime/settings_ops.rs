//! Settings- and CLI-tools-window message handling, extracted from
//! `runtime.rs`. `use super::*` pulls in the runtime's full scope so
//! the moved arms compile unchanged.

use super::*;
use crate::app::{SettingsFocus, SettingsPane};
use crate::permissions::PermissionStatus;

/// Every settings message arm, extracted from `runtime::update` so the
/// top-level dispatcher stays navigable. Settings
/// mutations are mostly synchronous (the apply fn is pure on
/// `Preferences`) but a hotkey change has a side-effect — the OS-level
/// chord registration must be re-issued — handled inside the
/// `Message::Settings` arm so the side-effect lives next to the state
/// update. Routed from `update`'s
/// grouped arm; the trailing `unreachable!` only fires if a
/// non-settings message is mis-routed here.
pub(crate) fn handle_settings_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::Settings(submsg) => {
            let needs_rehotkey = matches!(submsg, SettingsMessage::SetCaptureHotkey(_));
            let launch_at_login = match &submsg {
                SettingsMessage::SetLaunchAtLogin(v) => Some(*v),
                _ => None,
            };
            if !matches!(submsg, SettingsMessage::SetCaptureHotkey(_)) {
                state.settings_hotkey_status = None;
            }
            state.settings_reset_all_pending = false;
            state.settings_uninstall_pending = false;
            state.update_sync(Message::Settings(submsg));
            if needs_rehotkey {
                refresh_hotkey_registration(state);
                set_hotkey_registration_notice(state);
                sync_tray_capture_hotkey_label(state);
            }
            if let Some(enabled) = launch_at_login {
                if let Err(e) = crate::startup::set_launch_at_login(enabled) {
                    tracing::warn!(
                        target: readshot_core::log::cat::APP,
                        "launch-at-login update failed: {e}",
                    );
                    state.update_sync(Message::Settings(SettingsMessage::SetLaunchAtLogin(
                        !enabled,
                    )));
                }
            }
            Task::none()
        }

        Message::OpenSettingsRequested => {
            // Single-instance — focus the existing window if one is
            // already up.
            if let Some(id) = state.settings_window_id {
                return window::gain_focus(id);
            }
            let (id, open_task) = window::open(settings_window_settings());
            register_window(state, id, WindowKind::Settings);
            state.settings_window_id = Some(id);
            open_task.map(Message::SettingsWindowReady)
        }
        Message::SettingsChooseSaveFolderRequested => {
            Task::perform(pick_settings_save_folder(), |r| {
                Message::SettingsSaveFolderPicked(r.map_err(|e| e.to_string()))
            })
        }
        Message::SettingsSaveFolderPicked(result) => {
            if let Ok(Some(path)) = result {
                return update(
                    state,
                    Message::Settings(SettingsMessage::SetSaveFolder(path)),
                );
            }
            Task::none()
        }
        Message::SettingsOpenSaveFolderRequested => {
            let folder = settings_save_folder_to_open(&state.preferences);
            Task::perform(
                async move { open_folder_path(&folder).map_err(|e| e.to_string()) },
                Message::SettingsOpenSaveFolderDone,
            )
        }
        Message::SettingsOpenSaveFolderDone(result) => {
            state.settings_status = Some(match result {
                Ok(()) => "Opened save folder.".to_string(),
                Err(e) => {
                    tracing::warn!(target: readshot_core::log::cat::PREFERENCES, "open save folder failed: {e}");
                    super::history_ops::friendly_string_error("open the save folder", &e)
                }
            });
            Task::none()
        }
        Message::SettingsStartHotkeyRecording => {
            state.settings_recording_hotkey = true;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            state.settings_status = None;
            state.settings_reset_all_pending = false;
            Task::none()
        }
        Message::SettingsHotkeyRecorded(shortcut) => {
            state.settings_recording_hotkey = false;
            // Reject a chord that collides with a reserved fixed hotkey
            // (History / Settings) instead of silently disabling one of
            // them. Keep the previous capture hotkey and tell the user.
            if let Some(reserved) = capture_hotkey_conflict(&shortcut) {
                let pretty = pretty_hotkey(&shortcut);
                state.settings_hotkey_status = None;
                state.settings_hotkey_error = Some(format!(
                    "{pretty} is reserved for the {reserved} shortcut. Choose a different shortcut."
                ));
                return Task::none();
            }
            state.settings_hotkey_error = None;
            update(
                state,
                Message::Settings(SettingsMessage::SetCaptureHotkey(shortcut)),
            )
        }
        Message::SettingsHotkeyRecordingInvalid => {
            state.settings_recording_hotkey = true;
            state.settings_hotkey_status = None;
            state.settings_hotkey_error = Some(
                "That key can't be used. Combine a modifier (Command, Control, Option, or Shift) \
                 with a letter, number, arrow, or function key."
                    .to_string(),
            );
            Task::none()
        }
        Message::SettingsHotkeyRecordingCancelled => {
            state.settings_recording_hotkey = false;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            Task::none()
        }
        Message::SettingsResetAllRequested => {
            state.settings_reset_all_pending = true;
            state.settings_uninstall_pending = false;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            state.settings_status = None;
            Task::none()
        }
        Message::SettingsResetAllCancelled => {
            state.settings_reset_all_pending = false;
            Task::none()
        }
        Message::SettingsResetAllConfirmed => {
            state.settings_reset_all_pending = false;
            state.settings_recording_hotkey = false;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            state.settings_status = Some("Settings reset to defaults.".to_string());
            let old_launch_at_login = state.preferences.launch_at_login;
            reset_settings_to_defaults(state);
            refresh_hotkey_registration(state);
            set_hotkey_registration_notice(state);
            sync_tray_capture_hotkey_label(state);
            if old_launch_at_login {
                if let Err(e) = crate::startup::set_launch_at_login(false) {
                    tracing::warn!(
                        target: readshot_core::log::cat::APP,
                        "launch-at-login reset failed: {e}",
                    );
                    state.update_sync(Message::Settings(SettingsMessage::SetLaunchAtLogin(true)));
                }
            }
            Task::none()
        }
        Message::SettingsUninstallRequested
        | Message::SettingsUninstallCancelled
        | Message::SettingsUninstallPurgeToggled(_) => {
            // Pure arming / cancel / checkbox transitions — handled in
            // `App::update_sync` so the state machine is unit-testable.
            state.update_sync(message);
            Task::none()
        }
        Message::SettingsUninstallConfirmed => {
            state.settings_uninstall_pending = false;
            // Build the plan on the UI thread (read-only, fast), then run
            // the I/O-heavy executor on a blocking worker so moving the
            // bundle to Trash and deleting data dirs can't stall the UI.
            let purge = state.settings_uninstall_purge;
            let paths = crate::uninstall::default_uninstall_paths();
            let plan = crate::uninstall::plan_uninstall(&paths, purge);
            Task::perform(
                async move {
                    let _ = tokio::task::spawn_blocking(move || {
                        let reports =
                            crate::uninstall::execute(&plan, &crate::uninstall::RealSystem);
                        for report in &reports {
                            match &report.outcome {
                                Ok(status) => tracing::info!(
                                    target: readshot_core::log::cat::APP,
                                    "{}: {status:?}",
                                    report.action.describe(),
                                ),
                                Err(e) => tracing::warn!(
                                    target: readshot_core::log::cat::APP,
                                    "{} failed: {e}",
                                    report.action.describe(),
                                ),
                            }
                        }
                    })
                    .await;
                },
                |()| Message::SettingsUninstallFinished,
            )
        }
        Message::SettingsUninstallFinished => {
            // Use the existing quit path so the relaunch-on-system-quit
            // safety net is disarmed before the process exits.
            quit_readshot(&state.coordinator)
        }
        Message::SettingsWindowReady(id) => {
            // Belt-and-braces: window-open already records the id, but
            // iced may hand back a different one in some platforms.
            state.settings_window_id = Some(id);
            window::gain_focus(id)
        }
        Message::SettingsPaneSelected(pane) => {
            // A recording armed on Capture must not keep listening once
            // its prompt is off-screen, or the next chord would silently
            // become the capture hotkey.
            if pane != SettingsPane::Capture && state.settings_recording_hotkey {
                let _ = update(state, Message::SettingsHotkeyRecordingCancelled);
            }
            if state.settings_pane != pane {
                state.settings_focus = None;
                state.settings_key_feedback = None;
            }
            state.settings_pane = pane;
            Task::none()
        }
        Message::SettingsKeyPressed {
            window,
            key,
            modifiers,
            captured,
        } => {
            // The keyboard subscription is global to the daemon; only
            // act when the focused window is the settings window.
            if state.settings_window_id != Some(window) {
                return Task::none();
            }
            // Esc steps back one level: out of the filename field (which
            // has already let go of the caret), then off the focus ring,
            // and only then closes the window.
            let bare = !modifiers.command() && !modifiers.alt() && !modifiers.control();
            if bare
                && matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                )
            {
                if captured {
                    state.settings_focus = None;
                    return Task::none();
                }
                if state.settings_focus.take().is_some() {
                    return Task::none();
                }
            }
            if let Some(task) = settings_focus_key(state, &key, modifiers, captured) {
                return task;
            }
            let Some(action) = settings_key_action(&key, modifiers) else {
                return Task::none();
            };
            // Show the pane that owns the setting before changing it.
            // Remember whether the App pane (with any armed destructive
            // prompt) was already on screen.
            let prompt_visible = state.settings_pane == SettingsPane::App;
            if let Some(pane) = action.owning_pane() {
                if state.settings_pane != pane {
                    let _ = update(state, Message::SettingsPaneSelected(pane));
                }
            }
            match action {
                SettingsKeyAction::Close => {
                    state.windows.forget(window);
                    window::close(window)
                }
                SettingsKeyAction::ToggleLaunchAtLogin
                | SettingsKeyAction::ToggleDebugLogging
                | SettingsKeyAction::CycleRetention
                | SettingsKeyAction::CycleEngine
                | SettingsKeyAction::CycleFormat => {
                    let item = match action {
                        SettingsKeyAction::ToggleLaunchAtLogin => SettingsFocus::LaunchAtLogin,
                        SettingsKeyAction::ToggleDebugLogging => SettingsFocus::DebugLogging,
                        SettingsKeyAction::CycleRetention => SettingsFocus::Retention,
                        SettingsKeyAction::CycleEngine => SettingsFocus::OcrEngine,
                        _ => SettingsFocus::Format,
                    };
                    adjust_settings_item(state, item, 1)
                }
                SettingsKeyAction::RecordShortcut => {
                    if state.settings_recording_hotkey {
                        Task::none()
                    } else {
                        update(state, Message::SettingsStartHotkeyRecording)
                    }
                }
                // Destructive: the key arms the two-stage prompt, and a
                // second press confirms. A single keypress can never wipe.
                // A confirm only proceeds when its prompt was already
                // visible; from another pane the press just shows it.
                SettingsKeyAction::ResetAll => {
                    if state.settings_reset_all_pending {
                        if !prompt_visible {
                            return Task::none();
                        }
                        update(state, Message::SettingsResetAllConfirmed)
                    } else {
                        update(state, Message::SettingsResetAllRequested)
                    }
                }
                SettingsKeyAction::SelectPane(pane) => {
                    update(state, Message::SettingsPaneSelected(pane))
                }
                SettingsKeyAction::Uninstall => {
                    if state.settings_uninstall_pending {
                        if !prompt_visible {
                            return Task::none();
                        }
                        update(state, Message::SettingsUninstallConfirmed)
                    } else {
                        update(state, Message::SettingsUninstallRequested)
                    }
                }
            }
        }
        Message::OpenCliToolsRequested => {
            if let Some(id) = state.cli_tools_window_id {
                return window::gain_focus(id);
            }
            state.cli_tools_status = None;
            let (id, open_task) = window::open(cli_tools_window_settings());
            register_window(state, id, WindowKind::CliTools);
            state.cli_tools_window_id = Some(id);
            open_task.map(Message::CliToolsWindowReady)
        }
        Message::CliToolsWindowReady(id) => {
            state.cli_tools_window_id = Some(id);
            Task::none()
        }
        Message::CliToolsCopyRequested(shell) => Task::perform(
            copy_text_to_clipboard(cli_tools_setup_commands(shell)),
            move |result| Message::CliToolsCopyDone(shell, result.map_err(|e| e.to_string())),
        ),
        Message::CliToolsCopyDone(shell, result) => {
            state.cli_tools_status = Some(match result {
                Ok(()) => format!(
                    "{} commands copied. Paste them into Terminal.",
                    shell.label()
                ),
                Err(e) => {
                    tracing::warn!(target: readshot_core::log::cat::PREFERENCES, "copy cli commands failed: {e}");
                    super::history_ops::friendly_string_error_with(
                        "copy the commands",
                        &e,
                        Some(super::history_ops::RETRY_HINT),
                    )
                }
            });
            Task::none()
        }
        other => unreachable!("non-settings message routed to the settings handler: {other:?}"),
    }
}

// The option orders below mirror `settings_view`'s pick-list arrays so
// keyboard cycling steps through the same sequence the list shows.
const RETENTIONS: [readshot_core::HistoryRetention; 4] = [
    readshot_core::HistoryRetention::Off,
    readshot_core::HistoryRetention::Last50,
    readshot_core::HistoryRetention::Last30Days,
    readshot_core::HistoryRetention::Unlimited,
];
const ENGINES: [readshot_core::OcrEngineChoice; 2] = [
    readshot_core::OcrEngineChoice::Native,
    readshot_core::OcrEngineChoice::Tesseract,
];
const FORMATS: [readshot_core::ExportFormat; 3] = [
    readshot_core::ExportFormat::Png,
    readshot_core::ExportFormat::Jpeg,
    readshot_core::ExportFormat::Webp,
];

/// Widget id of the filename template field (Tab hands it the caret).
pub(crate) const SETTINGS_FILENAME_ID: &str = "settings-filename";

/// The current pane's keyboard-focusable controls, in the order they are
/// drawn (WCAG 2.4.3).
pub(crate) fn settings_focus_items(state: &App) -> Vec<SettingsFocus> {
    use SettingsFocus as F;
    match state.settings_pane {
        SettingsPane::Permissions => match state.permission_status {
            PermissionStatus::NotApplicable => Vec::new(),
            PermissionStatus::Granted => vec![F::PermissionOpen],
            PermissionStatus::Denied => {
                vec![F::PermissionRecheck, F::PermissionReset, F::PermissionOpen]
            }
        },
        SettingsPane::Capture => vec![F::HotkeyRecord, F::HotkeyReset, F::Retention],
        SettingsPane::Files => vec![
            F::FolderChoose,
            F::FolderOpen,
            F::FolderReset,
            F::FilenameField,
        ],
        SettingsPane::TextRecognition => {
            let n = state.coordinator.supported_languages().len();
            let mut v = vec![F::OcrEngine];
            v.extend((0..n).map(F::OcrLanguage));
            v
        }
        SettingsPane::Export => vec![F::Format],
        SettingsPane::Diagnostics => vec![F::DebugLogging],
        SettingsPane::App => {
            let mut v = vec![F::LaunchAtLogin];
            if state.settings_reset_all_pending {
                v.extend([F::ResetConfirm, F::ResetCancel]);
            } else {
                v.push(F::ResetAll);
            }
            if state.settings_uninstall_pending {
                v.extend([F::UninstallPurge, F::UninstallConfirm, F::UninstallCancel]);
            } else {
                v.push(F::Uninstall);
            }
            v
        }
    }
}

/// Map a Settings key to a focus-ring command (the same keys as the
/// editor's zones): Tab / ⇧Tab or ↓ / ↑ move, ← / → change, Space / ↵
/// press. `captured`: the filename field has the caret and keeps every
/// key but Tab.
pub(crate) fn settings_focus_command(
    focused: bool,
    key: &iced::keyboard::Key,
    m: iced::keyboard::Modifiers,
    captured: bool,
) -> Option<FocusKey> {
    use iced::keyboard::{key::Named, Key};
    if m.command() || m.alt() || m.control() {
        return None;
    }
    if matches!(key, Key::Named(Named::Tab)) {
        return Some(if m.shift() {
            FocusKey::Prev
        } else {
            FocusKey::Next
        });
    }
    if captured || !focused {
        return None;
    }
    match key {
        Key::Named(Named::ArrowDown) => Some(FocusKey::Next),
        Key::Named(Named::ArrowUp) => Some(FocusKey::Prev),
        Key::Named(Named::ArrowRight) => Some(FocusKey::Adjust(1)),
        Key::Named(Named::ArrowLeft) => Some(FocusKey::Adjust(-1)),
        Key::Named(Named::Enter | Named::Space) => Some(FocusKey::Activate),
        _ => None,
    }
}

fn settings_caret(item: Option<SettingsFocus>) -> Task<Message> {
    if item == Some(SettingsFocus::FilenameField) {
        iced::widget::operation::focus(iced::widget::Id::new(SETTINGS_FILENAME_ID))
    } else {
        unfocus_text_fields()
    }
}

fn settings_focus_key(
    state: &mut App,
    key: &iced::keyboard::Key,
    m: iced::keyboard::Modifiers,
    captured: bool,
) -> Option<Task<Message>> {
    let items = settings_focus_items(state);
    if items.is_empty() {
        return None;
    }
    let current = state
        .settings_focus
        .and_then(|f| items.iter().position(|i| *i == f));
    let cmd = settings_focus_command(current.is_some(), key, m, captured)?;
    let n = items.len();
    Some(match cmd {
        FocusKey::Next | FocusKey::Prev => {
            let next = match (current, cmd == FocusKey::Next) {
                (None, true) => 0,
                (None, false) => n - 1,
                (Some(i), true) => (i + 1) % n,
                (Some(i), false) => (i + n - 1) % n,
            };
            state.settings_focus = Some(items[next]);
            settings_caret(state.settings_focus)
        }
        FocusKey::Adjust(step) => adjust_settings_item(state, items[current?], step),
        FocusKey::Activate => activate_settings_item(state, items[current?]),
        _ => return None,
    })
}

fn pick<T: Copy + PartialEq>(all: &[T], current: T, step: i32) -> T {
    super::key_focus::cycle(all, current, step)
}

/// Change a value-like control (← / →, or its ⌘ shortcut) and say what
/// it is now, so the change is never silent (WCAG 3.2.2).
fn adjust_settings_item(state: &mut App, item: SettingsFocus, step: i32) -> Task<Message> {
    use SettingsFocus as F;
    let p = &state.preferences;
    let (msg, feedback) = match item {
        F::Retention => {
            let v = pick(&RETENTIONS, p.history_retention, step);
            (
                SettingsMessage::SetHistoryRetention(v),
                retention_feedback(v),
            )
        }
        F::OcrEngine => {
            let v = pick(&ENGINES, p.ocr_engine_choice, step);
            (SettingsMessage::SetOcrEngine(v), format!("OCR engine: {v}"))
        }
        F::Format => {
            let v = pick(&FORMATS, p.default_format, step);
            (
                SettingsMessage::SetDefaultFormat(v),
                format!("Default format: {v}"),
            )
        }
        F::LaunchAtLogin => {
            let v = !p.launch_at_login;
            (
                SettingsMessage::SetLaunchAtLogin(v),
                format!("Open at login: {}", on_off(v)),
            )
        }
        F::DebugLogging => {
            let v = !p.debug_logging;
            (
                SettingsMessage::SetDebugLogging(v),
                format!("Debug logging: {}", on_off(v)),
            )
        }
        F::OcrLanguage(_) | F::UninstallPurge => return activate_settings_item(state, item),
        _ => return Task::none(),
    };
    let task = update(state, Message::Settings(msg));
    state.settings_key_feedback = Some(feedback);
    task
}

/// The feedback for a retention change: a choice that prunes says so,
/// since the next capture deletes older ones for good.
pub(crate) fn retention_feedback(v: readshot_core::HistoryRetention) -> String {
    use readshot_core::HistoryRetention as R;
    match v {
        R::Last50 | R::Last30Days => {
            format!("History retention: {v}. Older captures are deleted at the next capture.")
        }
        R::Off => format!("History retention: {v}. New captures aren’t saved."),
        R::Unlimited => format!("History retention: {v}. Every capture is kept."),
    }
}

fn on_off(v: bool) -> &'static str {
    if v {
        "On"
    } else {
        "Off"
    }
}

/// Press the focused control.
fn activate_settings_item(state: &mut App, item: SettingsFocus) -> Task<Message> {
    use SettingsFocus as F;
    let msg = match item {
        F::PermissionRecheck => Message::PermissionTick,
        F::PermissionReset => Message::ResetPermissionRequested,
        F::PermissionOpen => Message::OpenPermissionSettingsRequested,
        F::HotkeyRecord if state.settings_recording_hotkey => {
            Message::SettingsHotkeyRecordingCancelled
        }
        F::HotkeyRecord => Message::SettingsStartHotkeyRecording,
        F::HotkeyReset => Message::Settings(SettingsMessage::SetCaptureHotkey(
            default_capture_hotkey().into(),
        )),
        F::FolderChoose => Message::SettingsChooseSaveFolderRequested,
        F::FolderOpen => Message::SettingsOpenSaveFolderRequested,
        F::FolderReset => Message::Settings(SettingsMessage::SetSaveFolder(PathBuf::new())),
        F::FilenameField => return settings_caret(Some(item)),
        F::OcrLanguage(i) => {
            let supported = state.coordinator.supported_languages();
            let Some(lang) = supported.get(i) else {
                return Task::none();
            };
            let mut next = state.preferences.ocr_languages.clone();
            let on = !next.iter().any(|l| l == lang);
            if on {
                next.push(lang.clone());
            } else {
                next.retain(|l| l != lang);
            }
            let name = ocr_language_display_name(lang);
            let task = update(
                state,
                Message::Settings(SettingsMessage::SetOcrLanguages(next)),
            );
            state.settings_key_feedback = Some(format!("{name}: {}", on_off(on)));
            return task;
        }
        F::ResetAll => Message::SettingsResetAllRequested,
        F::ResetConfirm => Message::SettingsResetAllConfirmed,
        F::ResetCancel => Message::SettingsResetAllCancelled,
        F::Uninstall => Message::SettingsUninstallRequested,
        F::UninstallPurge => {
            Message::SettingsUninstallPurgeToggled(!state.settings_uninstall_purge)
        }
        F::UninstallConfirm => Message::SettingsUninstallConfirmed,
        F::UninstallCancel => Message::SettingsUninstallCancelled,
        F::Retention | F::OcrEngine | F::Format | F::LaunchAtLogin | F::DebugLogging => {
            return adjust_settings_item(state, item, 1)
        }
    };
    let task = update(state, msg);
    // An armed prompt swaps its buttons: keep the focus on the step
    // that replaced the one pressed.
    let items = settings_focus_items(state);
    if !items.contains(&item) {
        state.settings_focus = match item {
            F::ResetAll => Some(F::ResetConfirm),
            F::ResetConfirm | F::ResetCancel => Some(F::ResetAll),
            F::Uninstall => Some(F::UninstallConfirm),
            F::UninstallConfirm | F::UninstallCancel => Some(F::Uninstall),
            _ => items.first().copied(),
        }
        .filter(|f| items.contains(f));
    }
    task
}
