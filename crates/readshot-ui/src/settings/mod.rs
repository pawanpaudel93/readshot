//! Settings window state + message router.
//!
//! The window itself is opened by Task 16's composition root via
//! iced's `daemon` runtime. This module owns:
//!
//! * The typed [`SettingsMessage`] enum mirroring every editable
//!   field in [`Preferences`].
//! * [`apply`] — a pure function that mutates a `Preferences` value
//!   in response to a message.
//! * The four [`SettingsTab`] identifiers that the iced view picks
//!   between.
//!
//! Splitting the state-machine from the iced view keeps every
//! preference mutation unit-testable.

use std::path::PathBuf;

use readshot_core::{ExportFormat, HistoryRetention, OcrEngineChoice, Preferences, UpdateChannel};

/// Visual tab the user is currently viewing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsTab {
    #[default]
    General,
    Capture,
    Ocr,
    Advanced,
}

/// All possible mutations the settings window can ask for. Each maps
/// to one [`Preferences`] field.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsMessage {
    SwitchTab(SettingsTab),
    SetCaptureHotkey(String),
    SetSaveFolder(PathBuf),
    SetFilenameTemplate(String),
    SetDefaultFormat(ExportFormat),
    SetHistoryRetention(HistoryRetention),
    SetOcrLanguages(Vec<String>),
    SetOcrEngine(OcrEngineChoice),
    SetLaunchAtLogin(bool),
    SetUpdateChannel(UpdateChannel),
    SetDebugLogging(bool),
    /// "Clear History" button — emits a message because the actual
    /// clear must touch the history store, not just preferences.
    ClearHistoryRequested,
    /// "About" button.
    OpenAbout,
}

/// Apply a [`SettingsMessage`] to a [`Preferences`] in place.
///
/// Returns `true` when the message changed `Preferences`, `false`
/// otherwise. The caller uses the boolean to decide whether to
/// persist via `Preferences::save`.
///
/// `SwitchTab`, `ClearHistoryRequested`, and `OpenAbout` don't
/// mutate `Preferences` — they're side-effect messages; this
/// function returns `false` for them.
pub fn apply(prefs: &mut Preferences, msg: SettingsMessage) -> bool {
    match msg {
        SettingsMessage::SwitchTab(_)
        | SettingsMessage::ClearHistoryRequested
        | SettingsMessage::OpenAbout => false,
        SettingsMessage::SetCaptureHotkey(s) => {
            if prefs.capture_hotkey == s {
                false
            } else {
                prefs.capture_hotkey = s;
                true
            }
        }
        SettingsMessage::SetSaveFolder(p) => {
            if prefs.save_folder == p {
                false
            } else {
                prefs.save_folder = p;
                true
            }
        }
        SettingsMessage::SetFilenameTemplate(s) => {
            if prefs.filename_template == s {
                false
            } else {
                prefs.filename_template = s;
                true
            }
        }
        SettingsMessage::SetDefaultFormat(f) => {
            if prefs.default_format == f {
                false
            } else {
                prefs.default_format = f;
                true
            }
        }
        SettingsMessage::SetHistoryRetention(r) => {
            if prefs.history_retention == r {
                false
            } else {
                prefs.history_retention = r;
                true
            }
        }
        SettingsMessage::SetOcrLanguages(langs) => {
            if prefs.ocr_languages == langs {
                false
            } else {
                prefs.ocr_languages = langs;
                true
            }
        }
        SettingsMessage::SetOcrEngine(c) => {
            if prefs.ocr_engine_choice == c {
                false
            } else {
                prefs.ocr_engine_choice = c;
                true
            }
        }
        SettingsMessage::SetLaunchAtLogin(v) => {
            if prefs.launch_at_login == v {
                false
            } else {
                prefs.launch_at_login = v;
                true
            }
        }
        SettingsMessage::SetUpdateChannel(c) => {
            if prefs.update_channel == c {
                false
            } else {
                prefs.update_channel = c;
                true
            }
        }
        SettingsMessage::SetDebugLogging(v) => {
            if prefs.debug_logging == v {
                false
            } else {
                prefs.debug_logging = v;
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Preferences {
        Preferences::default()
    }

    #[test]
    fn switch_tab_is_a_no_op_on_preferences() {
        let mut p = defaults();
        let changed = apply(&mut p, SettingsMessage::SwitchTab(SettingsTab::Capture));
        assert!(!changed);
    }

    #[test]
    fn setting_a_changed_value_returns_true() {
        let mut p = defaults();
        let changed = apply(&mut p, SettingsMessage::SetDebugLogging(true));
        assert!(changed);
        assert!(p.debug_logging);
    }

    #[test]
    fn setting_a_value_to_its_current_returns_false() {
        let mut p = defaults();
        // Default is Stable; setting Stable again should be a no-op.
        let changed = apply(
            &mut p,
            SettingsMessage::SetUpdateChannel(UpdateChannel::Stable),
        );
        assert!(!changed);
    }

    #[test]
    fn changing_history_retention_persists_the_new_value() {
        let mut p = defaults();
        assert_eq!(p.history_retention, HistoryRetention::Off);
        let changed = apply(
            &mut p,
            SettingsMessage::SetHistoryRetention(HistoryRetention::Last30Days),
        );
        assert!(changed);
        assert_eq!(p.history_retention, HistoryRetention::Last30Days);
    }

    #[test]
    fn launch_at_login_can_be_enabled_and_disabled() {
        let mut p = defaults();
        assert!(!p.launch_at_login);

        let changed = apply(&mut p, SettingsMessage::SetLaunchAtLogin(true));
        assert!(changed);
        assert!(p.launch_at_login);

        let changed = apply(&mut p, SettingsMessage::SetLaunchAtLogin(false));
        assert!(changed);
        assert!(!p.launch_at_login);
    }

    #[test]
    fn ocr_language_list_persists_and_compares_ordered() {
        let mut p = defaults();
        let new_list = vec!["fr".to_string(), "en".to_string()];
        let changed = apply(&mut p, SettingsMessage::SetOcrLanguages(new_list.clone()));
        assert!(changed);
        assert_eq!(p.ocr_languages, new_list);

        // Re-applying the same list is a no-op.
        let changed_again = apply(&mut p, SettingsMessage::SetOcrLanguages(new_list));
        assert!(!changed_again);
    }

    #[test]
    fn clear_history_request_is_side_effect_only() {
        let mut p = defaults();
        // Mutate something so we can confirm clearing-history doesn't
        // touch unrelated fields.
        p.debug_logging = true;
        let changed = apply(&mut p, SettingsMessage::ClearHistoryRequested);
        assert!(!changed);
        assert!(p.debug_logging);
    }
}
