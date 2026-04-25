//! Global hotkey wiring for Readshot.
//!
//! The user's preferred shortcut is stored in
//! [`Preferences::capture_hotkey`](readshot_core::Preferences::capture_hotkey)
//! as a string like `"ctrl+shift+x"`. This module:
//!
//! 1. Parses that string into a typed [`HotkeySpec`].
//! 2. Adapts the spec to `global_hotkey::hotkey::HotKey` so the
//!    composition root can register it with the
//!    `GlobalHotKeyManager`.
//! 3. Bridges hotkey events from the manager into iced's
//!    `Subscription` machinery (the actual subscription lives in
//!    Task 16; this module exposes the typed message it emits).
//!
//! Parsing is intentionally lenient: case-insensitive, allows
//! `+`/`-`/space as separators, and accepts both
//! Apple-style names ("cmd", "option", "return") and PC-style names
//! ("ctrl", "alt", "enter") for the same modifier so the user's
//! preferences file is portable across machines.

use global_hotkey::hotkey::{Code, HotKey, Modifiers};

/// Modifier bitset using our own enum so the parser doesn't depend
/// on the `global-hotkey` crate's exact representation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HotkeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool, // ⌘ / Win / Super
}

/// Parsed shortcut. Caller decides at registration time whether the
/// `meta` modifier maps to ⌘ (macOS), Windows-key, or Super (Linux).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HotkeySpec {
    pub modifiers: HotkeyModifiers,
    pub code: Code,
}

/// Errors from [`parse`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HotkeyParseError {
    #[error("hotkey string is empty")]
    Empty,
    #[error("hotkey is missing a key — only modifiers found: `{0}`")]
    NoKey(String),
    #[error("unknown key or modifier token: `{0}`")]
    UnknownToken(String),
}

/// Parse a string like `"ctrl+shift+x"` into a [`HotkeySpec`]. Tokens
/// are matched case-insensitively. Synonyms accepted:
///
/// * `cmd` / `command` / `meta` / `super` / `win` → meta
/// * `ctrl` / `control` → control
/// * `alt` / `opt` / `option` → alt
/// * `shift` → shift
/// * letters `a..z`, digits `0..9`, function keys `f1..f24`, plus a
///   small set of named keys (`enter`/`return`, `escape`/`esc`, `tab`,
///   `space`, `backspace`).
pub fn parse(input: &str) -> Result<HotkeySpec, HotkeyParseError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(HotkeyParseError::Empty);
    }

    let mut modifiers = HotkeyModifiers::default();
    let mut key_token: Option<&str> = None;

    // Allow `+`, `-`, or whitespace as separators. We split on any of
    // them and treat each chunk as either a modifier or the (single)
    // main key.
    let tokens: Vec<&str> = input
        .split(|c: char| matches!(c, '+' | '-') || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .collect();

    for token in &tokens {
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.control = true,
            "shift" => modifiers.shift = true,
            "alt" | "opt" | "option" => modifiers.alt = true,
            "cmd" | "command" | "meta" | "super" | "win" => modifiers.meta = true,
            _ => {
                if key_token.is_some() {
                    return Err(HotkeyParseError::UnknownToken((*token).into()));
                }
                key_token = Some(token);
            }
        }
    }

    let key = match key_token {
        Some(k) => k,
        None => return Err(HotkeyParseError::NoKey(input.to_string())),
    };

    let code = parse_key_code(key).ok_or_else(|| HotkeyParseError::UnknownToken(key.into()))?;

    Ok(HotkeySpec { modifiers, code })
}

fn parse_key_code(token: &str) -> Option<Code> {
    let lower = token.to_ascii_lowercase();
    // Letters
    if lower.len() == 1 {
        let c = lower.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some(letter_to_code(c));
        }
        if c.is_ascii_digit() {
            return Some(digit_to_code(c));
        }
    }
    // Function keys
    if let Some(stripped) = lower.strip_prefix('f') {
        if let Ok(n) = stripped.parse::<u8>() {
            if (1..=24).contains(&n) {
                return Some(function_key_to_code(n));
            }
        }
    }
    // Named keys
    Some(match lower.as_str() {
        "enter" | "return" => Code::Enter,
        "escape" | "esc" => Code::Escape,
        "tab" => Code::Tab,
        "space" => Code::Space,
        "backspace" | "delete" => Code::Backspace,
        _ => return None,
    })
}

fn letter_to_code(c: char) -> Code {
    use Code::*;
    match c {
        'a' => KeyA, 'b' => KeyB, 'c' => KeyC, 'd' => KeyD, 'e' => KeyE,
        'f' => KeyF, 'g' => KeyG, 'h' => KeyH, 'i' => KeyI, 'j' => KeyJ,
        'k' => KeyK, 'l' => KeyL, 'm' => KeyM, 'n' => KeyN, 'o' => KeyO,
        'p' => KeyP, 'q' => KeyQ, 'r' => KeyR, 's' => KeyS, 't' => KeyT,
        'u' => KeyU, 'v' => KeyV, 'w' => KeyW, 'x' => KeyX, 'y' => KeyY,
        'z' => KeyZ,
        _ => unreachable!("called with non-letter: {c}"),
    }
}

fn digit_to_code(c: char) -> Code {
    use Code::*;
    match c {
        '0' => Digit0, '1' => Digit1, '2' => Digit2, '3' => Digit3, '4' => Digit4,
        '5' => Digit5, '6' => Digit6, '7' => Digit7, '8' => Digit8, '9' => Digit9,
        _ => unreachable!("called with non-digit: {c}"),
    }
}

fn function_key_to_code(n: u8) -> Code {
    use Code::*;
    match n {
        1 => F1, 2 => F2, 3 => F3, 4 => F4, 5 => F5, 6 => F6,
        7 => F7, 8 => F8, 9 => F9, 10 => F10, 11 => F11, 12 => F12,
        13 => F13, 14 => F14, 15 => F15, 16 => F16, 17 => F17, 18 => F18,
        19 => F19, 20 => F20, 21 => F21, 22 => F22, 23 => F23, 24 => F24,
        _ => unreachable!("function-key range is 1..=24"),
    }
}

impl HotkeySpec {
    /// Produce a `global_hotkey::hotkey::HotKey` ready to register
    /// with the `GlobalHotKeyManager`. The `meta` bit maps to the
    /// platform-appropriate modifier (Cmd / Win / Super) — that's
    /// what the upstream crate does internally.
    pub fn to_global_hotkey(&self) -> HotKey {
        let mut mods = Modifiers::empty();
        if self.modifiers.shift {
            mods |= Modifiers::SHIFT;
        }
        if self.modifiers.control {
            mods |= Modifiers::CONTROL;
        }
        if self.modifiers.alt {
            mods |= Modifiers::ALT;
        }
        if self.modifiers.meta {
            mods |= Modifiers::SUPER;
        }
        HotKey::new(Some(mods), self.code)
    }
}

/// Typed message the iced subscription publishes when the user's
/// global shortcut fires. The composition root maps it to a
/// "start capture" application message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyMessage {
    CapturePressed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_ctrl_shift_x() {
        let spec = parse("ctrl+shift+x").unwrap();
        assert!(spec.modifiers.control);
        assert!(spec.modifiers.shift);
        assert!(!spec.modifiers.alt);
        assert!(!spec.modifiers.meta);
        assert_eq!(spec.code, Code::KeyX);
    }

    #[test]
    fn parses_apple_style_synonyms() {
        let spec = parse("cmd+option+return").unwrap();
        assert!(spec.modifiers.meta);
        assert!(spec.modifiers.alt);
        assert_eq!(spec.code, Code::Enter);
    }

    #[test]
    fn parses_function_keys() {
        let spec = parse("ctrl+f5").unwrap();
        assert_eq!(spec.code, Code::F5);
    }

    #[test]
    fn rejects_empty_string() {
        assert_eq!(parse(""), Err(HotkeyParseError::Empty));
        assert_eq!(parse("   "), Err(HotkeyParseError::Empty));
    }

    #[test]
    fn rejects_modifier_only() {
        match parse("ctrl+shift") {
            Err(HotkeyParseError::NoKey(_)) => {}
            other => panic!("expected NoKey, got {other:?}"),
        }
    }

    #[test]
    fn rejects_two_keys() {
        match parse("a+b") {
            Err(HotkeyParseError::UnknownToken(_)) => {}
            other => panic!("expected UnknownToken, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_token() {
        match parse("bogus+x") {
            Err(HotkeyParseError::UnknownToken(_)) => {}
            other => panic!("expected UnknownToken, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_and_minus_separators_work() {
        assert_eq!(parse("ctrl shift x").unwrap().code, Code::KeyX);
        assert_eq!(parse("ctrl-shift-x").unwrap().code, Code::KeyX);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(parse("CTRL+SHIFT+X").unwrap().code, Code::KeyX);
        assert_eq!(parse("Ctrl+Shift+X").unwrap().code, Code::KeyX);
    }

    #[test]
    fn to_global_hotkey_maps_modifiers_correctly() {
        let spec = parse("cmd+shift+x").unwrap();
        let h = spec.to_global_hotkey();
        assert!(h.mods.contains(Modifiers::SUPER));
        assert!(h.mods.contains(Modifiers::SHIFT));
        assert!(!h.mods.contains(Modifiers::CONTROL));
        assert_eq!(h.key, Code::KeyX);
    }
}
