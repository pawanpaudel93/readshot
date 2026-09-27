// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.

use std::collections::HashMap;

use global_hotkey::{
    hotkey::{Code, HotKey},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};

use readshot_core::Preferences;
use readshot_ui::hotkey;

use crate::app::{App, GlobalHotkeyAction};

pub(crate) struct HotkeyRegistration {
    pub manager: GlobalHotKeyManager,
    pub actions: HashMap<u32, GlobalHotkeyAction>,
    pub capture_registered: bool,
    /// Human names of any *fixed* hotkeys (History, Settings) that could
    /// not be registered with the OS. Surfaced in the settings window so
    /// a failure isn't silent.
    pub failed_fixed: Vec<&'static str>,
}

/// Outcome of registering the full hotkey set: the id→action map, whether
/// the user's capture chord was registered, and the names of any fixed
/// hotkeys that failed to register.
pub(crate) struct HotkeyActions {
    pub actions: HashMap<u32, GlobalHotkeyAction>,
    pub capture_registered: bool,
    pub failed_fixed: Vec<&'static str>,
}

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

/// Render a hotkey string like "cmd+shift+x" as a human label. On macOS
/// this is the native glyph form "⌘⇧X" (modifiers in ⌘⇧⌥⌃ order, no
/// separators); elsewhere it's "Ctrl+Shift+X". Named
/// keys render with their proper symbol ("," "↩" "←" …) instead of a
/// raw token like "COMMA".
///
/// The whole string is parsed with the *real* parser
/// ([`readshot_ui::hotkey::parse`]) so this can never disagree with what
/// is actually registered. If parsing fails, the trimmed input is
/// returned so the user still sees *something* rather than an empty
/// label.
pub fn pretty_hotkey(s: &str) -> String {
    match hotkey::parse(s) {
        Ok(spec) => format_chord(spec.modifiers, pretty_key_label(spec.code)),
        Err(_) => s.trim().to_string(),
    }
}

#[cfg(target_os = "macos")]
fn format_chord(m: hotkey::HotkeyModifiers, key: &str) -> String {
    // ⌘⇧⌥⌃ order, matching the glyphs Readshot has always shown.
    let mut out = String::new();
    if m.meta {
        out.push('\u{2318}'); // ⌘
    }
    if m.shift {
        out.push('\u{21E7}'); // ⇧
    }
    if m.alt {
        out.push('\u{2325}'); // ⌥
    }
    if m.control {
        out.push('\u{2303}'); // ⌃
    }
    out.push_str(key);
    out
}

#[cfg(not(target_os = "macos"))]
fn format_chord(m: hotkey::HotkeyModifiers, key: &str) -> String {
    // Ctrl+Shift+Alt+Super order — Ctrl first is the PC convention.
    let mut parts: Vec<&str> = Vec::new();
    if m.control {
        parts.push("Ctrl");
    }
    if m.shift {
        parts.push("Shift");
    }
    if m.alt {
        parts.push("Alt");
    }
    if m.meta {
        parts.push("Super");
    }
    let mut out = parts.join("+");
    if !out.is_empty() {
        out.push('+');
    }
    out.push_str(key);
    out
}

/// Human label for a single key code: glyphs for the named editing keys,
/// bare characters for letters/digits, "F1".."F24" for function keys.
fn pretty_key_label(code: Code) -> &'static str {
    match code {
        Code::KeyA => "A",
        Code::KeyB => "B",
        Code::KeyC => "C",
        Code::KeyD => "D",
        Code::KeyE => "E",
        Code::KeyF => "F",
        Code::KeyG => "G",
        Code::KeyH => "H",
        Code::KeyI => "I",
        Code::KeyJ => "J",
        Code::KeyK => "K",
        Code::KeyL => "L",
        Code::KeyM => "M",
        Code::KeyN => "N",
        Code::KeyO => "O",
        Code::KeyP => "P",
        Code::KeyQ => "Q",
        Code::KeyR => "R",
        Code::KeyS => "S",
        Code::KeyT => "T",
        Code::KeyU => "U",
        Code::KeyV => "V",
        Code::KeyW => "W",
        Code::KeyX => "X",
        Code::KeyY => "Y",
        Code::KeyZ => "Z",
        Code::Digit0 => "0",
        Code::Digit1 => "1",
        Code::Digit2 => "2",
        Code::Digit3 => "3",
        Code::Digit4 => "4",
        Code::Digit5 => "5",
        Code::Digit6 => "6",
        Code::Digit7 => "7",
        Code::Digit8 => "8",
        Code::Digit9 => "9",
        Code::F1 => "F1",
        Code::F2 => "F2",
        Code::F3 => "F3",
        Code::F4 => "F4",
        Code::F5 => "F5",
        Code::F6 => "F6",
        Code::F7 => "F7",
        Code::F8 => "F8",
        Code::F9 => "F9",
        Code::F10 => "F10",
        Code::F11 => "F11",
        Code::F12 => "F12",
        Code::F13 => "F13",
        Code::F14 => "F14",
        Code::F15 => "F15",
        Code::F16 => "F16",
        Code::F17 => "F17",
        Code::F18 => "F18",
        Code::F19 => "F19",
        Code::F20 => "F20",
        Code::F21 => "F21",
        Code::F22 => "F22",
        Code::F23 => "F23",
        Code::F24 => "F24",
        Code::Comma => ",",
        Code::Enter => "\u{21A9}",  // ↩
        Code::Escape => "\u{238B}", // ⎋
        Code::Tab => "\u{21E5}",    // ⇥
        Code::Space => "Space",
        Code::Backspace => "\u{232B}",  // ⌫
        Code::Delete => "\u{2326}",     // ⌦
        Code::ArrowLeft => "\u{2190}",  // ←
        Code::ArrowRight => "\u{2192}", // →
        Code::ArrowUp => "\u{2191}",    // ↑
        Code::ArrowDown => "\u{2193}",  // ↓
        Code::Home => "Home",
        Code::End => "End",
        Code::PageUp => "PgUp",
        Code::PageDown => "PgDn",
        Code::Insert => "Ins",
        // Any other code the parser might yield in future: best-effort.
        _ => "?",
    }
}

pub(crate) fn register_default_hotkey(prefs: &Preferences) -> Option<HotkeyRegistration> {
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(target: "readshot::hotkey", "GlobalHotKeyManager init failed: {e}");
            return None;
        }
    };
    let HotkeyActions {
        actions,
        capture_registered,
        failed_fixed,
    } = register_hotkey_actions(prefs, |hotkey| {
        manager.register(hotkey).map_err(|e| e.to_string())
    });
    if actions.is_empty() {
        None
    } else {
        Some(HotkeyRegistration {
            manager,
            actions,
            capture_registered,
            failed_fixed,
        })
    }
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

/// If the capture chord collides with one of the fixed global hotkeys,
/// return the human name of the reserved action it clashes with. Fixed
/// hotkeys (History, Settings) always win, so a capture chord equal to
/// one of them must be rejected rather than silently clobbering it.
pub(crate) fn capture_hotkey_conflict(capture: &str) -> Option<&'static str> {
    let spec = hotkey::parse(capture).ok()?;
    let clashes = |fixed: &str| hotkey::parse(fixed).map(|f| f == spec).unwrap_or(false);
    if clashes(history_hotkey()) {
        Some("History")
    } else if clashes(settings_hotkey()) {
        Some("Settings")
    } else {
        None
    }
}

pub(crate) fn register_hotkey_actions<F>(prefs: &Preferences, mut register: F) -> HotkeyActions
where
    F: FnMut(HotKey) -> Result<(), String>,
{
    let mut actions = HashMap::new();
    let mut capture_registered = false;
    match hotkey::parse(&prefs.capture_hotkey) {
        Ok(spec) => {
            if let Some(reserved) = capture_hotkey_conflict(&prefs.capture_hotkey) {
                // Don't register a chord that would collide with (and
                // clobber) a reserved fixed hotkey — the fixed ones below
                // must keep working. The settings notice explains why.
                tracing::warn!(
                    target: "readshot::hotkey",
                    "capture hotkey `{}` conflicts with the {reserved} shortcut; not registering",
                    prefs.capture_hotkey,
                );
            } else {
                let hotkey = spec.to_global_hotkey();
                let id = hotkey.id();
                match register(hotkey) {
                    Ok(()) => {
                        actions.insert(id, GlobalHotkeyAction::Capture);
                        capture_registered = true;
                        tracing::info!(
                            target: "readshot::hotkey",
                            "registered global hotkey: {}",
                            prefs.capture_hotkey,
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "readshot::hotkey",
                            "failed to register `{}`: {e}",
                            prefs.capture_hotkey,
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "readshot::hotkey",
                "could not parse capture_hotkey `{}`: {e}",
                prefs.capture_hotkey,
            );
        }
    }
    let mut failed_fixed = Vec::new();
    if !register_fixed_global_hotkey(
        &mut actions,
        history_hotkey(),
        GlobalHotkeyAction::History,
        &mut register,
    ) {
        failed_fixed.push("History");
    }
    if !register_fixed_global_hotkey(
        &mut actions,
        settings_hotkey(),
        GlobalHotkeyAction::Settings,
        &mut register,
    ) {
        failed_fixed.push("Settings");
    }
    HotkeyActions {
        actions,
        capture_registered,
        failed_fixed,
    }
}

/// Register one fixed hotkey. Returns `true` on success, `false` if the
/// string didn't parse or the OS refused the registration — the caller
/// records failures so they can be surfaced to the user.
pub(crate) fn register_fixed_global_hotkey<F>(
    actions: &mut HashMap<u32, GlobalHotkeyAction>,
    hotkey_string: &str,
    action: GlobalHotkeyAction,
    register: &mut F,
) -> bool
where
    F: FnMut(HotKey) -> Result<(), String>,
{
    let Ok(spec) = hotkey::parse(hotkey_string) else {
        tracing::warn!(target: "readshot::hotkey", "fixed hotkey `{hotkey_string}` did not parse");
        return false;
    };
    let hotkey = spec.to_global_hotkey();
    let id = hotkey.id();
    if let Err(e) = register(hotkey) {
        tracing::warn!(
            target: "readshot::hotkey",
            "failed to register fixed hotkey `{hotkey_string}`: {e}",
        );
        return false;
    }
    actions.insert(id, action);
    true
}

pub(crate) fn refresh_hotkey_registration(state: &mut App) {
    // Drop the old manager first — that releases the OS-level chord.
    // Only then try the new one; if the new chord fails to parse or
    // conflicts with another app, the field stays `None`.
    state.hotkey_manager = None;
    state.hotkey_actions.clear();
    state.capture_hotkey_registered = false;
    state.fixed_hotkey_failures = Vec::new();
    if let Some(registration) = register_default_hotkey(&state.preferences) {
        state.hotkey_manager = Some(registration.manager);
        state.hotkey_actions = registration.actions;
        state.capture_hotkey_registered = registration.capture_registered;
        state.fixed_hotkey_failures = registration.failed_fixed;
    }
}

pub(crate) fn set_hotkey_registration_notice(state: &mut App) {
    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    state.settings_hotkey_error = None;
    state.settings_hotkey_status = None;
    if hotkey::parse(&state.preferences.capture_hotkey).is_err() {
        state.settings_hotkey_error = Some("That shortcut could not be read.".to_string());
    } else if let Some(reserved) = capture_hotkey_conflict(&state.preferences.capture_hotkey) {
        state.settings_hotkey_error = Some(format!(
            "{pretty} is reserved for the {reserved} shortcut. Pick another chord."
        ));
    } else if state.capture_hotkey_registered {
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
