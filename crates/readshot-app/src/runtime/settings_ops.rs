//! Settings- and CLI-tools-window message handling, extracted from
//! `runtime.rs`. `use super::*` pulls in the runtime's full scope so
//! the moved arms compile unchanged.

use super::*;

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
            state.update_sync(Message::Settings(submsg));
            if needs_rehotkey {
                refresh_hotkey_registration(state);
                set_hotkey_registration_notice(state);
                sync_tray_capture_hotkey_label(state);
            }
            if let Some(enabled) = launch_at_login {
                if let Err(e) = crate::startup::set_launch_at_login(enabled) {
                    tracing::warn!(
                        target: "readshot::startup",
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
            state.windows.register(id, WindowKind::Settings);
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
                    tracing::warn!(target: "readshot::settings", "open save folder failed: {e}");
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
                    "{pretty} is reserved for the {reserved} shortcut. Pick another chord."
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
                        target: "readshot::startup",
                        "launch-at-login reset failed: {e}",
                    );
                    state.update_sync(Message::Settings(SettingsMessage::SetLaunchAtLogin(true)));
                }
            }
            Task::none()
        }
        Message::SettingsWindowReady(id) => {
            // Belt-and-braces: window-open already records the id, but
            // iced may hand back a different one in some platforms.
            state.settings_window_id = Some(id);
            window::gain_focus(id)
        }
        Message::OpenCliToolsRequested => {
            if let Some(id) = state.cli_tools_window_id {
                return window::gain_focus(id);
            }
            state.cli_tools_status = None;
            let (id, open_task) = window::open(cli_tools_window_settings());
            state.windows.register(id, WindowKind::CliTools);
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
                Err(e) => format!("Copy failed: {e}"),
            });
            Task::none()
        }
        other => unreachable!("non-settings message routed to the settings handler: {other:?}"),
    }
}
