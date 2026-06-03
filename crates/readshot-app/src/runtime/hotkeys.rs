// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.

use std::collections::HashMap;

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use readshot_core::Preferences;
use readshot_ui::hotkey;

use crate::app::{App, GlobalHotkeyAction};

/// Per-platform suggested default for `Preferences::capture_hotkey`.
/// macOS users have ⌘ muscle memory; everyone else uses Ctrl.
#[cfg(target_os = "macos")]
pub(crate) fn default_capture_hotkey() -> &'static str {
    "cmd+shift+x"
}
#[cfg(not(target_os = "macos"))]
pub(crate) fn default_capture_hotkey() -> &'static str {
    "ctrl+shift+x"
}

/// Render a hotkey string like "cmd+shift+x" as the macOS-native
/// glyph form "⌘⇧X". Falls back to the input string if there's no
/// recognisable component (so parse failures still display
/// *something* instead of an empty label).
pub fn pretty_hotkey(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    if lower.is_empty() {
        return String::new();
    }
    let mut modifiers = String::new();
    let mut keys: Vec<String> = Vec::new();
    for raw in lower.split('+') {
        let p = raw.trim();
        if p.is_empty() {
            continue;
        }
        match p {
            "cmd" | "command" | "meta" | "super" | "win" => modifiers.push('\u{2318}'),
            "ctrl" | "control" => modifiers.push('\u{2303}'),
            "shift" => modifiers.push('\u{21E7}'),
            "alt" | "option" | "opt" => modifiers.push('\u{2325}'),
            other => keys.push(other.to_uppercase()),
        }
    }
    let pretty: String = format!("{modifiers}{}", keys.join(""));
    if pretty.is_empty() {
        s.to_string()
    } else {
        pretty
    }
}

pub(crate) fn register_default_hotkey(
    prefs: &Preferences,
) -> Option<(GlobalHotKeyManager, HashMap<u32, GlobalHotkeyAction>)> {
    let spec = match hotkey::parse(&prefs.capture_hotkey) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "readshot::hotkey",
                "could not parse capture_hotkey `{}`: {e}",
                prefs.capture_hotkey,
            );
            return None;
        }
    };
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(target: "readshot::hotkey", "GlobalHotKeyManager init failed: {e}");
            return None;
        }
    };
    let capture_hotkey = spec.to_global_hotkey();
    if let Err(e) = manager.register(capture_hotkey) {
        tracing::warn!(
            target: "readshot::hotkey",
            "failed to register `{}`: {e}",
            prefs.capture_hotkey,
        );
        return None;
    }
    let mut actions = HashMap::new();
    actions.insert(capture_hotkey.id(), GlobalHotkeyAction::Capture);
    register_fixed_global_hotkey(
        &manager,
        &mut actions,
        history_hotkey(),
        GlobalHotkeyAction::History,
    );
    register_fixed_global_hotkey(
        &manager,
        &mut actions,
        settings_hotkey(),
        GlobalHotkeyAction::Settings,
    );
    tracing::info!(
        target: "readshot::hotkey",
        "registered global hotkey: {}",
        prefs.capture_hotkey,
    );
    Some((manager, actions))
}

#[cfg(target_os = "macos")]
pub(crate) fn history_hotkey() -> &'static str {
    "cmd+y"
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn history_hotkey() -> &'static str {
    "ctrl+y"
}

#[cfg(target_os = "macos")]
pub(crate) fn settings_hotkey() -> &'static str {
    "cmd+comma"
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn settings_hotkey() -> &'static str {
    "ctrl+comma"
}

pub(crate) fn register_fixed_global_hotkey(
    manager: &GlobalHotKeyManager,
    actions: &mut HashMap<u32, GlobalHotkeyAction>,
    hotkey_string: &str,
    action: GlobalHotkeyAction,
) {
    let Ok(spec) = hotkey::parse(hotkey_string) else {
        tracing::warn!(target: "readshot::hotkey", "fixed hotkey `{hotkey_string}` did not parse");
        return;
    };
    let hotkey = spec.to_global_hotkey();
    if let Err(e) = manager.register(hotkey) {
        tracing::warn!(
            target: "readshot::hotkey",
            "failed to register fixed hotkey `{hotkey_string}`: {e}",
        );
        return;
    }
    actions.insert(hotkey.id(), action);
}

pub(crate) fn refresh_hotkey_registration(state: &mut App) {
    // Drop the old manager first — that releases the OS-level chord.
    // Only then try the new one; if the new chord fails to parse or
    // conflicts with another app, the field stays `None`.
    state.hotkey_manager = None;
    state.hotkey_actions.clear();
    if let Some((manager, actions)) = register_default_hotkey(&state.preferences) {
        state.hotkey_manager = Some(manager);
        state.hotkey_actions = actions;
    }
}

pub(crate) fn set_hotkey_registration_notice(state: &mut App) {
    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    state.settings_hotkey_error = None;
    state.settings_hotkey_status = None;
    if pretty.is_empty() {
        state.settings_hotkey_error = Some("That shortcut could not be read.".to_string());
    } else if state.hotkey_manager.is_some() {
        state.settings_hotkey_status = Some(format!("{pretty} is ready."));
    } else {
        state.settings_hotkey_error = Some(format!("{pretty} could not be registered globally."));
    }
}

pub(crate) fn sync_tray_capture_hotkey_label(state: &App) {
    let Some(tray) = state.tray.as_ref() else {
        return;
    };
    let label = pretty_hotkey(&state.preferences.capture_hotkey);
    if label.is_empty() {
        tray.set_capture_hotkey(None, None);
    } else {
        tray.set_capture_hotkey(Some(&state.preferences.capture_hotkey), Some(&label));
    }
}
pub(crate) fn global_hotkey_action(
    event: &GlobalHotKeyEvent,
    actions: &HashMap<u32, GlobalHotkeyAction>,
) -> Option<GlobalHotkeyAction> {
    if event.state != HotKeyState::Pressed {
        return None;
    }
    actions.get(&event.id).copied()
}

#[cfg(target_os = "macos")]
pub(crate) fn extra_command_modifier(modifiers: iced::keyboard::Modifiers) -> bool {
    modifiers.control()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn extra_command_modifier(modifiers: iced::keyboard::Modifiers) -> bool {
    modifiers.logo()
}
pub(crate) fn shortcut_string_from_keypress(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<String> {
    if !(modifiers.command() || modifiers.control() || modifiers.alt() || modifiers.shift()) {
        return None;
    }
    let key = hotkey_token_for_key(key)?;
    let mut parts: Vec<&str> = Vec::new();
    if modifiers.command() {
        parts.push("cmd");
    }
    if modifiers.control() {
        parts.push("ctrl");
    }
    if modifiers.alt() {
        parts.push("alt");
    }
    if modifiers.shift() {
        parts.push("shift");
    }
    parts.push(key);
    Some(parts.join("+"))
}

pub(crate) fn shortcut_cancelled_by_keypress(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> bool {
    matches!(
        key,
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
    ) && !modifiers.command()
        && !modifiers.control()
        && !modifiers.alt()
        && !modifiers.shift()
}

pub(crate) fn hotkey_token_for_key(key: &iced::keyboard::Key) -> Option<&'static str> {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key;

    match key {
        Key::Character(c) => {
            let mut chars = c.chars();
            let ch = chars.next()?;
            if chars.next().is_none() && ch.is_ascii_alphanumeric() {
                Some(match ch.to_ascii_lowercase() {
                    'a' => "a",
                    'b' => "b",
                    'c' => "c",
                    'd' => "d",
                    'e' => "e",
                    'f' => "f",
                    'g' => "g",
                    'h' => "h",
                    'i' => "i",
                    'j' => "j",
                    'k' => "k",
                    'l' => "l",
                    'm' => "m",
                    'n' => "n",
                    'o' => "o",
                    'p' => "p",
                    'q' => "q",
                    'r' => "r",
                    's' => "s",
                    't' => "t",
                    'u' => "u",
                    'v' => "v",
                    'w' => "w",
                    'x' => "x",
                    'y' => "y",
                    'z' => "z",
                    '0' => "0",
                    '1' => "1",
                    '2' => "2",
                    '3' => "3",
                    '4' => "4",
                    '5' => "5",
                    '6' => "6",
                    '7' => "7",
                    '8' => "8",
                    '9' => "9",
                    _ => return None,
                })
            } else {
                None
            }
        }
        Key::Named(Named::Enter) => Some("enter"),
        Key::Named(Named::Escape) => Some("escape"),
        Key::Named(Named::Tab) => Some("tab"),
        Key::Named(Named::Space) => Some("space"),
        Key::Named(Named::Backspace) => Some("backspace"),
        Key::Named(Named::F1) => Some("f1"),
        Key::Named(Named::F2) => Some("f2"),
        Key::Named(Named::F3) => Some("f3"),
        Key::Named(Named::F4) => Some("f4"),
        Key::Named(Named::F5) => Some("f5"),
        Key::Named(Named::F6) => Some("f6"),
        Key::Named(Named::F7) => Some("f7"),
        Key::Named(Named::F8) => Some("f8"),
        Key::Named(Named::F9) => Some("f9"),
        Key::Named(Named::F10) => Some("f10"),
        Key::Named(Named::F11) => Some("f11"),
        Key::Named(Named::F12) => Some("f12"),
        _ => None,
    }
}
