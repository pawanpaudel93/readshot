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
    pub window_registered: bool,
}

/// Outcome of registering the system-wide hotkeys: the id→action map and
/// whether the user's capture and window-capture chords were registered.
pub(crate) struct HotkeyActions {
    pub actions: HashMap<u32, GlobalHotkeyAction>,
    pub capture_registered: bool,
    pub window_registered: bool,
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
/// this is the native glyph form "⇧⌘X" (modifiers in ⌃⌥⇧⌘ order, no
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
    // Apple order ⌃⌥⇧⌘.
    let mut out = String::new();
    if m.control {
        out.push('\u{2303}'); // ⌃
    }
    if m.alt {
        out.push('\u{2325}'); // ⌥
    }
    if m.shift {
        out.push('\u{21E7}'); // ⇧
    }
    if m.meta {
        out.push('\u{2318}'); // ⌘
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
            tracing::warn!(target: readshot_core::log::cat::HOTKEY, "GlobalHotKeyManager init failed: {e}");
            return None;
        }
    };
    let HotkeyActions {
        actions,
        capture_registered,
        window_registered,
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
            window_registered,
        })
    }
}

/// The History and Settings shortcuts. They are app shortcuts, handled
/// only while a Readshot window has the keyboard
/// (`app_window_shortcut_message`), never registered with the OS: a
/// system-wide ⌘Y / ⌘, would take Redo and every app's Preferences away
/// from other apps for as long as Readshot runs.
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

/// If the capture chord collides with one of the app shortcuts (History,
/// Settings), return the human name of the action it clashes with. A
/// system-wide capture chord equal to one of them would fire from inside
/// Readshot's own windows too, so it is rejected rather than shadowing
/// the app shortcut.
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
                    target: readshot_core::log::cat::HOTKEY,
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
                            target: readshot_core::log::cat::HOTKEY,
                            "registered global hotkey: {}",
                            prefs.capture_hotkey,
                        );
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: readshot_core::log::cat::HOTKEY,
                            "failed to register `{}`: {e}",
                            prefs.capture_hotkey,
                        );
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                target: readshot_core::log::cat::HOTKEY,
                "could not parse capture_hotkey `{}`: {e}",
                prefs.capture_hotkey,
            );
        }
    }
    let window_registered = register_window_hotkey(prefs, &mut actions, &mut register);
    HotkeyActions {
        actions,
        capture_registered,
        window_registered,
    }
}

/// Why the window-capture chord can't be used, if it can't: it equals an
/// app shortcut or the capture chord (it would shadow either). `None`
/// when it is unassigned or doesn't parse (the hint says that instead).
pub(crate) fn window_hotkey_conflict(prefs: &Preferences) -> Option<String> {
    let chord = prefs.window_capture_hotkey.trim();
    let spec = hotkey::parse(chord).ok()?;
    if let Some(reserved) = capture_hotkey_conflict(chord) {
        return Some(format!("the {reserved} shortcut"));
    }
    let capture = hotkey::parse(&prefs.capture_hotkey).ok();
    (capture == Some(spec)).then(|| "the capture shortcut".to_string())
}

/// Register the window-capture chord when one is set and usable.
fn register_window_hotkey<F>(
    prefs: &Preferences,
    actions: &mut HashMap<u32, GlobalHotkeyAction>,
    register: &mut F,
) -> bool
where
    F: FnMut(HotKey) -> Result<(), String>,
{
    let chord = prefs.window_capture_hotkey.trim();
    // Only where window capture exists, the same condition as the
    // Settings row: a chord left in a copied or hand-edited file must not
    // take a key from other apps for a feature that isn't there.
    if chord.is_empty() || !crate::tray::WINDOW_CAPTURE_IN_MENU {
        return false;
    }
    let Ok(spec) = hotkey::parse(chord) else {
        tracing::warn!(target: readshot_core::log::cat::HOTKEY, "could not parse window_capture_hotkey `{chord}`");
        return false;
    };
    if let Some(clash) = window_hotkey_conflict(prefs) {
        tracing::warn!(target: readshot_core::log::cat::HOTKEY, "window capture hotkey `{chord}` is {clash}; not registering");
        return false;
    }
    let hotkey = spec.to_global_hotkey();
    let id = hotkey.id();
    match register(hotkey) {
        Ok(()) => {
            actions.insert(id, GlobalHotkeyAction::CaptureWindow);
            true
        }
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HOTKEY, "failed to register `{chord}`: {e}");
            false
        }
    }
}

/// Which scroll-capture esc chords are global hotkeys right now:
/// `(esc, ⇧esc)`. The in-window HUD map leaves only those to the hotkey.
pub(crate) fn scroll_escape_globals(state: &App) -> (bool, bool) {
    let has = |a: GlobalHotkeyAction| {
        state
            .scroll_escape_hotkeys
            .iter()
            .any(|h| state.hotkey_actions.get(&h.id()) == Some(&a))
    };
    (
        has(GlobalHotkeyAction::ScrollStop),
        has(GlobalHotkeyAction::ScrollDiscard),
    )
}

/// The HUD's in-window key map minus the esc chords that are global
/// hotkeys (`plain`: esc, `shift`: ⇧esc), so one press is never seen
/// twice (a doubled ⇧esc would confirm a discard).
pub(crate) fn scroll_key_message_beside_globals(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    plain: bool,
    shift: bool,
) -> Option<crate::app::Message> {
    if matches!(
        key,
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
    ) && (if modifiers.shift() { shift } else { plain })
    {
        return None;
    }
    super::scroll_key_message(key, modifiers)
}

/// While a scrolling capture runs (and is not stitching), esc and ⇧esc
/// are global hotkeys: the user is scrolling another app, so the HUD's
/// in-window keys would never arrive (WCAG 2.1.1). They are released as
/// soon as the session ends, whichever way it ends. Called after every
/// update; a no-op when nothing changed.
pub(crate) fn sync_scroll_escape_hotkeys(state: &mut App) {
    let Some(manager) = state.hotkey_manager.take() else {
        return;
    };
    sync_scroll_escape_hotkeys_with(
        state,
        |h| manager.register(h).map_err(|e| e.to_string()),
        |h| {
            let _ = manager.unregister(h);
        },
    );
    state.hotkey_manager = Some(manager);
}

/// [`sync_scroll_escape_hotkeys`] with the OS calls passed in (tests).
pub(crate) fn sync_scroll_escape_hotkeys_with<R, U>(
    state: &mut App,
    mut register: R,
    mut unregister: U,
) where
    R: FnMut(HotKey) -> Result<(), String>,
    U: FnMut(HotKey),
{
    let want = state.scroll_session.as_ref().is_some_and(|s| !s.stopping);
    // Registration is tried once per session: if the OS refuses a chord
    // it is not retried (and logged) on every update.
    if want == state.scroll_escape_tried {
        return;
    }
    state.scroll_escape_tried = want;
    if want {
        use global_hotkey::hotkey::Modifiers;
        for (mods, action) in [
            (None, GlobalHotkeyAction::ScrollStop),
            (Some(Modifiers::SHIFT), GlobalHotkeyAction::ScrollDiscard),
        ] {
            let hotkey = HotKey::new(mods, Code::Escape);
            match register(hotkey) {
                Ok(()) => {
                    state.hotkey_actions.insert(hotkey.id(), action);
                    state.scroll_escape_hotkeys.push(hotkey);
                }
                Err(e) => tracing::warn!(
                    target: readshot_core::log::cat::HOTKEY,
                    "couldn't register the scrolling-capture esc key: {e}"
                ),
            }
        }
    } else {
        for hotkey in std::mem::take(&mut state.scroll_escape_hotkeys) {
            unregister(hotkey);
            state.hotkey_actions.remove(&hotkey.id());
        }
    }
}

pub(crate) fn refresh_hotkey_registration(state: &mut App) {
    // Drop the old manager first — that releases the OS-level chord.
    // Only then try the new one; if the new chord fails to parse or
    // conflicts with another app, the field stays `None`.
    state.hotkey_manager = None;
    state.hotkey_actions.clear();
    // Dropping the manager released the scrolling-capture esc keys too;
    // the next update registers them again if a session is running.
    state.scroll_escape_hotkeys.clear();
    state.scroll_escape_tried = false;
    state.capture_hotkey_registered = false;
    state.window_hotkey_registered = false;
    if let Some(registration) = register_default_hotkey(&state.preferences) {
        state.hotkey_manager = Some(registration.manager);
        state.hotkey_actions = registration.actions;
        state.capture_hotkey_registered = registration.capture_registered;
        state.window_hotkey_registered = registration.window_registered;
    }
}

pub(crate) fn set_hotkey_registration_notice(state: &mut App) {
    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    state.settings_hotkey_error = None;
    state.settings_hotkey_status = None;
    if hotkey::parse(&state.preferences.capture_hotkey).is_err() {
        state.settings_hotkey_error =
            Some("Couldn’t read that shortcut. Press Record and try another.".to_string());
    } else if let Some(reserved) = capture_hotkey_conflict(&state.preferences.capture_hotkey) {
        state.settings_hotkey_error = Some(format!(
            "{pretty} is reserved for the {reserved} shortcut. Choose a different shortcut."
        ));
    } else if state.capture_hotkey_registered {
        state.settings_hotkey_status = Some(format!("{pretty} is ready."));
    } else {
        state.settings_hotkey_error = Some(format!(
            "Another app may be using {pretty}. Choose a different shortcut."
        ));
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
    let window = state.preferences.window_capture_hotkey.trim();
    tray.set_window_capture_hotkey(state.window_hotkey_registered.then_some(window));
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
/// The token for the logo key (⌘ on macOS, the Windows / Super key
/// elsewhere); the parser maps both spellings to the same modifier.
const LOGO_TOKEN: &str = if cfg!(target_os = "macos") {
    "cmd"
} else {
    "super"
};

/// The shortcut string for a recorded key press: exactly the physical
/// modifiers held. Not `Modifiers::command()`, which is Ctrl off macOS
/// and would record Ctrl+Shift+X as "cmd+ctrl+shift+x", registering
/// Super+Ctrl+Shift+X instead of the chord the user pressed.
pub(crate) fn shortcut_string_from_keypress(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<String> {
    if !(modifiers.logo() || modifiers.control() || modifiers.alt() || modifiers.shift()) {
        return None;
    }
    let key = hotkey_token_for_key(key)?;
    let mut parts: Vec<&str> = Vec::new();
    if modifiers.logo() {
        parts.push(LOGO_TOKEN);
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
    ) && !modifiers.logo()
        && !modifiers.control()
        && !modifiers.alt()
        && !modifiers.shift()
}

/// True for keys that are *only* modifiers (Shift, Ctrl, …). While the
/// recorder is armed the user typically holds a modifier before pressing
/// the real key; those intermediate presses must be ignored instead of
/// being reported as an invalid shortcut (which is what left the recorder
/// feeling "stuck").
pub(crate) fn is_modifier_key(key: &iced::keyboard::Key) -> bool {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key;
    matches!(
        key,
        Key::Named(
            Named::Shift
                | Named::Control
                | Named::Alt
                | Named::AltGraph
                | Named::Super
                | Named::Meta
                | Named::Hyper
                | Named::Fn
                | Named::Symbol
        )
    )
}

/// Map an iced key press to the parser token it represents, or `None`
/// when the key can't be part of a shortcut. This is the recorder half
/// of the recorder/parser contract; every token returned here is one
/// [`readshot_ui::hotkey::parse`] accepts (enforced by the tests below,
/// cross-checked against [`readshot_ui::hotkey::RECORDABLE_TOKENS`]).
pub(crate) fn hotkey_token_for_key(key: &iced::keyboard::Key) -> Option<&'static str> {
    use iced::keyboard::Key;

    match key {
        Key::Character(c) => {
            let mut chars = c.chars();
            let ch = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            character_token(ch)
        }
        Key::Named(named) => named_key_token(*named),
        _ => None,
    }
}

fn character_token(ch: char) -> Option<&'static str> {
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
        // The comma key reports as a character, not a `Named` key.
        ',' => "comma",
        _ => return None,
    })
}

fn named_key_token(named: iced::keyboard::key::Named) -> Option<&'static str> {
    use iced::keyboard::key::Named;
    Some(match named {
        Named::Enter => "enter",
        Named::Escape => "escape",
        Named::Tab => "tab",
        Named::Space => "space",
        Named::Backspace => "backspace",
        Named::Delete => "delete",
        Named::Insert => "insert",
        Named::Home => "home",
        Named::End => "end",
        Named::PageUp => "pageup",
        Named::PageDown => "pagedown",
        Named::ArrowLeft => "left",
        Named::ArrowRight => "right",
        Named::ArrowUp => "up",
        Named::ArrowDown => "down",
        Named::F1 => "f1",
        Named::F2 => "f2",
        Named::F3 => "f3",
        Named::F4 => "f4",
        Named::F5 => "f5",
        Named::F6 => "f6",
        Named::F7 => "f7",
        Named::F8 => "f8",
        Named::F9 => "f9",
        Named::F10 => "f10",
        Named::F11 => "f11",
        Named::F12 => "f12",
        Named::F13 => "f13",
        Named::F14 => "f14",
        Named::F15 => "f15",
        Named::F16 => "f16",
        Named::F17 => "f17",
        Named::F18 => "f18",
        Named::F19 => "f19",
        Named::F20 => "f20",
        Named::F21 => "f21",
        Named::F22 => "f22",
        Named::F23 => "f23",
        Named::F24 => "f24",
        _ => return None,
    })
}

#[cfg(test)]
mod recorder_tests {
    use super::*;
    use iced::keyboard::key::Named;
    use iced::keyboard::Key;
    use std::collections::BTreeSet;

    /// Every `Named` key the recorder is expected to translate. Kept in
    /// sync with `named_key_token`; the coverage test below fails if the
    /// recorder starts emitting a token outside the parser's set.
    const NAMED_CANDIDATES: &[Named] = &[
        Named::Enter,
        Named::Escape,
        Named::Tab,
        Named::Space,
        Named::Backspace,
        Named::Delete,
        Named::Insert,
        Named::Home,
        Named::End,
        Named::PageUp,
        Named::PageDown,
        Named::ArrowLeft,
        Named::ArrowRight,
        Named::ArrowUp,
        Named::ArrowDown,
        Named::F1,
        Named::F2,
        Named::F3,
        Named::F4,
        Named::F5,
        Named::F6,
        Named::F7,
        Named::F8,
        Named::F9,
        Named::F10,
        Named::F11,
        Named::F12,
        Named::F13,
        Named::F14,
        Named::F15,
        Named::F16,
        Named::F17,
        Named::F18,
        Named::F19,
        Named::F20,
        Named::F21,
        Named::F22,
        Named::F23,
        Named::F24,
    ];

    /// Collect the full set of tokens the recorder can produce from the
    /// character keys and the named keys above.
    fn recorder_emitted_tokens() -> BTreeSet<&'static str> {
        let mut out = BTreeSet::new();
        for ch in ('a'..='z').chain('0'..='9').chain([',']) {
            if let Some(tok) = hotkey_token_for_key(&Key::Character(ch.to_string().into())) {
                out.insert(tok);
            }
        }
        for named in NAMED_CANDIDATES {
            if let Some(tok) = hotkey_token_for_key(&Key::Named(*named)) {
                out.insert(tok);
            }
        }
        out
    }

    #[test]
    fn recorder_covers_exactly_the_parser_token_set() {
        let emitted = recorder_emitted_tokens();
        let expected: BTreeSet<&'static str> = readshot_ui::hotkey::RECORDABLE_TOKENS
            .iter()
            .copied()
            .collect();

        // Every parser-accepted token must be recordable …
        for tok in &expected {
            assert!(emitted.contains(tok), "token `{tok}` is not recordable");
        }
        // … and the recorder must not emit anything the parser rejects.
        for tok in &emitted {
            assert!(
                expected.contains(tok),
                "recorder emits `{tok}` which the parser does not accept"
            );
        }
        assert_eq!(emitted, expected);
    }

    #[test]
    fn every_recorded_token_round_trips_through_the_parser() {
        for tok in recorder_emitted_tokens() {
            assert!(
                readshot_ui::hotkey::parse(&format!("ctrl+{tok}")).is_ok(),
                "parser rejected recorded token `{tok}`"
            );
        }
    }

    #[test]
    fn pure_modifier_presses_are_ignored() {
        for named in [
            Named::Shift,
            Named::Control,
            Named::Alt,
            Named::Super,
            Named::Meta,
        ] {
            let key = Key::Named(named);
            assert!(
                is_modifier_key(&key),
                "{named:?} should count as a modifier"
            );
            assert!(
                hotkey_token_for_key(&key).is_none(),
                "{named:?} should not yield a token"
            );
        }
    }

    #[test]
    fn newly_supported_keys_record() {
        // Regression guard for #8: these were accepted by the parser but
        // previously unrecordable.
        assert_eq!(
            hotkey_token_for_key(&Key::Character(",".into())),
            Some("comma")
        );
        assert_eq!(
            hotkey_token_for_key(&Key::Named(Named::ArrowLeft)),
            Some("left")
        );
        assert_eq!(hotkey_token_for_key(&Key::Named(Named::F13)), Some("f13"));
        assert_eq!(
            hotkey_token_for_key(&Key::Named(Named::PageDown)),
            Some("pagedown")
        );
    }
}

#[cfg(test)]
mod pretty_hotkey_tests {
    use super::*;

    #[test]
    fn parse_failure_falls_back_to_trimmed_input() {
        assert_eq!(pretty_hotkey("  not a hotkey!!  "), "not a hotkey!!");
        assert_eq!(pretty_hotkey(""), "");
    }

    #[test]
    fn named_keys_render_symbols_not_raw_tokens() {
        // Whatever the platform, the raw token must never leak through.
        let comma = pretty_hotkey("cmd+comma");
        assert!(
            comma.ends_with(','),
            "expected trailing comma, got {comma:?}"
        );
        assert!(!comma.to_uppercase().contains("COMMA"));

        let left = pretty_hotkey("cmd+left");
        assert!(left.ends_with('\u{2190}'), "expected ←, got {left:?}");
        assert!(!left.to_uppercase().contains("LEFT"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_uses_glyph_form() {
        // ⇧⌘X (modifiers rendered in Apple ⌃⌥⇧⌘ order).
        assert_eq!(pretty_hotkey("cmd+shift+x"), "\u{21E7}\u{2318}X");
        // ⌃⌥⇧⌘, — full modifier stack with the comma key.
        assert_eq!(
            pretty_hotkey("ctrl+alt+shift+cmd+comma"),
            "\u{2303}\u{2325}\u{21E7}\u{2318},"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_uses_word_form() {
        assert_eq!(pretty_hotkey("ctrl+shift+x"), "Ctrl+Shift+X");
        assert_eq!(pretty_hotkey("ctrl+comma"), "Ctrl+,");
    }
}
