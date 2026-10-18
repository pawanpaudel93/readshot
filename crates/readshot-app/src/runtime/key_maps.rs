//! Keyboard maps: editor keys to messages, and the per-window maps of
//! the pointer-only windows (pin, Settings, Welcome, scroll capture).
//! Each is a pure `key -> …` function, so it is tested without iced.

use super::*;

use crate::app::{Message, SettingsPane};

/// Map a single character to a tool. Used by the editor keyboard
/// subscription so muscle-memory shortcuts work without modifiers.
pub(crate) fn tool_for_key(c: &str) -> Option<readshot_ui::editor::ToolState> {
    use readshot_ui::editor::ToolState as T;
    match c.to_ascii_lowercase().as_str() {
        "v" => Some(T::Select),
        "r" => Some(T::Rectangle),
        "o" => Some(T::Ellipse),
        "l" => Some(T::Line),
        "a" => Some(T::Arrow),
        "p" => Some(T::Pen),
        "h" => Some(T::Highlighter),
        "t" => Some(T::Text),
        "b" => Some(T::Blur),
        "x" => Some(T::Pixelate),
        "s" => Some(T::Spotlight),
        "n" => Some(T::NumberedPin),
        "e" => Some(T::Stamp),
        "m" => Some(T::Magnifier),
        "c" => Some(T::Crop),
        _ => None,
    }
}

pub(crate) fn editor_key_message(
    key: iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    status_ignored: bool,
) -> Option<Message> {
    use iced::keyboard::{key::Named, Key};

    let cmd = modifiers.command();
    // ⌃Tab / F6 walk the keyboard focus zones, even from a text field.
    if let Some(forward) = zone_cycle_key(&key, modifiers) {
        return Some(Message::Editor(EditorMsg::Focus(FocusMsg::ZoneCycle(
            forward,
        ))));
    }
    // ⌥⌘P / ⌥⌘I / ⌥⌘T / ⌥⌘B on macOS; Ctrl+Shift+P / R / T / B elsewhere,
    // where Ctrl+Alt is AltGr on many layouts and a desktop chord
    // (Ctrl+Alt+T opens a terminal on GNOME and KDE).
    if editor_alt_chord(&key, modifiers, "p") {
        return Some(Message::Editor(EditorMsg::TextPanel(
            TextPanelMsg::PanelToggled,
        )));
    }
    if editor_alt_chord(&key, modifiers, RESIZE_LETTER) {
        return Some(Message::Editor(EditorMsg::Image(ImageMsg::ResizeOpen)));
    }
    // The new chords never fire while a text field has the caret
    // (`status_ignored` false): a field's own ⇧⌘V pastes, and must not
    // also flip the image.
    if status_ignored && editor_alt_chord(&key, modifiers, "t") {
        return Some(Message::Editor(EditorMsg::Focus(FocusMsg::PanelTabKey(
            crate::editor::PanelTab::Text,
        ))));
    }
    if status_ignored && editor_alt_chord(&key, modifiers, "b") {
        return Some(Message::Editor(EditorMsg::Focus(FocusMsg::PanelTabKey(
            crate::editor::PanelTab::Beautify,
        ))));
    }
    match (&key, cmd, modifiers.shift()) {
        (Key::Character(c), true, true)
            if status_ignored && !modifiers.alt() && c.eq_ignore_ascii_case("i") =>
        {
            return Some(Message::Editor(EditorMsg::Focus(FocusMsg::ImageMenuKey)));
        }
        (Key::Character(c), true, true)
            if status_ignored && !modifiers.alt() && c.eq_ignore_ascii_case("h") =>
        {
            return Some(Message::Editor(EditorMsg::Image(ImageMsg::Flip(
                readshot_core::document::FlipAxis::Horizontal,
            ))));
        }
        (Key::Character(c), true, true)
            if status_ignored && !modifiers.alt() && c.eq_ignore_ascii_case("v") =>
        {
            return Some(Message::Editor(EditorMsg::Image(ImageMsg::Flip(
                readshot_core::document::FlipAxis::Vertical,
            ))));
        }
        (Key::Character(c), true, false) if c.eq_ignore_ascii_case("z") => {
            return Some(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Undo,
            )));
        }
        (Key::Character(c), true, true) if c.eq_ignore_ascii_case("z") => {
            return Some(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::Redo,
            )));
        }
        (Key::Character(c), true, false) if status_ignored && c.eq_ignore_ascii_case("c") => {
            return Some(Message::Editor(EditorMsg::CopyImageRequested));
        }
        (Key::Character(c), true, true) if status_ignored && c.eq_ignore_ascii_case("c") => {
            return Some(Message::Editor(EditorMsg::CopyTextRequested));
        }
        (Key::Character(c), true, false) if status_ignored && c.eq_ignore_ascii_case("p") => {
            return Some(Message::Editor(EditorMsg::PinRequested));
        }
        (Key::Character(c), true, false) if c.eq_ignore_ascii_case("s") => {
            return Some(Message::Editor(EditorMsg::SaveRequested));
        }
        (Key::Character(c), true, false) if c.eq_ignore_ascii_case("w") => {
            return Some(Message::Editor(EditorMsg::DiscardRequested));
        }
        (Key::Character(c), true, false) if status_ignored && !modifiers.alt() && c == "[" => {
            return Some(Message::Editor(EditorMsg::Image(ImageMsg::Rotate {
                clockwise: false,
            })));
        }
        (Key::Character(c), true, false) if status_ignored && !modifiers.alt() && c == "]" => {
            return Some(Message::Editor(EditorMsg::Image(ImageMsg::Rotate {
                clockwise: true,
            })));
        }
        (Key::Character(c), true, _) if c == "+" || c == "=" => {
            return Some(Message::Editor(EditorMsg::ZoomIn));
        }
        (Key::Character(c), true, false) if c == "-" => {
            return Some(Message::Editor(EditorMsg::ZoomOut));
        }
        (Key::Character(c), true, false) if c == "0" => {
            return Some(Message::Editor(EditorMsg::ZoomActual));
        }
        (Key::Named(Named::Escape), _, _) if status_ignored => {
            return Some(Message::Editor(EditorMsg::DiscardRequested));
        }
        // The annotation canvas captures every Escape (publishing
        // `Cancelled` for any live drag), so this is a real Esc press.
        // EditorTextCancel is its single cancel action: it closes an open
        // popover, else exits the eyedropper, else drops the crop draft, else cancels the text draft,
        // else clears the selection.
        (Key::Named(Named::Escape), _, _) => return Some(Message::Editor(EditorMsg::TextCancel)),
        _ => {}
    }

    // ⌥ + arrow resizes the crop draft or the selection from its
    // bottom-right edge (a selected magnifier: moves only its bubble).
    if status_ignored && modifiers.alt() && !cmd && !modifiers.control() {
        let step = if modifiers.shift() { 10.0 } else { 1.0 };
        match key {
            Key::Named(Named::ArrowLeft) => {
                return Some(Message::Editor(EditorMsg::ResizeByKeys(-step, 0.0)))
            }
            Key::Named(Named::ArrowRight) => {
                return Some(Message::Editor(EditorMsg::ResizeByKeys(step, 0.0)))
            }
            Key::Named(Named::ArrowUp) => {
                return Some(Message::Editor(EditorMsg::ResizeByKeys(0.0, -step)))
            }
            Key::Named(Named::ArrowDown) => {
                return Some(Message::Editor(EditorMsg::ResizeByKeys(0.0, step)))
            }
            _ => {}
        }
    }
    if status_ignored && !cmd && !modifiers.alt() && !modifiers.control() {
        // Tab / ⇧Tab walk the annotations, so a keyboard user can select
        // one (and then move, resize or delete it) without a click.
        if matches!(key, Key::Named(Named::Tab)) {
            return Some(Message::Editor(EditorMsg::CycleSelection(
                if modifiers.shift() { -1 } else { 1 },
            )));
        }
        if matches!(
            key,
            Key::Named(Named::Delete) | Key::Named(Named::Backspace)
        ) {
            return Some(Message::Editor(EditorMsg::DeleteSelected));
        }
        if matches!(key, Key::Named(Named::Enter)) {
            return Some(Message::Editor(EditorMsg::EditSelectedText));
        }
        // Arrow keys nudge the selected annotation a pixel at a time
        // (Shift = 10px for coarse positioning). The runtime no-ops when
        // nothing is selected, so this is safe to emit unconditionally.
        let nudge_step = if modifiers.shift() { 10.0 } else { 1.0 };
        if matches!(key, Key::Named(Named::ArrowLeft)) {
            return Some(Message::Editor(EditorMsg::NudgeSelected(-nudge_step, 0.0)));
        }
        if matches!(key, Key::Named(Named::ArrowRight)) {
            return Some(Message::Editor(EditorMsg::NudgeSelected(nudge_step, 0.0)));
        }
        if matches!(key, Key::Named(Named::ArrowUp)) {
            return Some(Message::Editor(EditorMsg::NudgeSelected(0.0, -nudge_step)));
        }
        if matches!(key, Key::Named(Named::ArrowDown)) {
            return Some(Message::Editor(EditorMsg::NudgeSelected(0.0, nudge_step)));
        }
        if let Key::Character(c) = &key {
            if c.eq_ignore_ascii_case("i") {
                return Some(Message::Editor(EditorMsg::EyedropperToggled));
            }
            if let Some(t) = tool_for_key(c.as_str()) {
                return Some(Message::Editor(EditorMsg::Toolbar(
                    readshot_ui::ToolbarMessage::SelectTool(t),
                )));
            }
            if let Some(n) = c.chars().next().and_then(|ch| ch.to_digit(10)) {
                if (1..=9).contains(&n) {
                    return Some(Message::Editor(EditorMsg::Toolbar(
                        readshot_ui::ToolbarMessage::SetLineWidth(n as f32),
                    )));
                }
            }
            match c.as_str() {
                "[" => return Some(Message::Editor(EditorMsg::WidthBump(-1.0))),
                "]" => return Some(Message::Editor(EditorMsg::WidthBump(1.0))),
                "," => return Some(Message::Editor(EditorMsg::ColorCycle(-1))),
                "." => return Some(Message::Editor(EditorMsg::ColorCycle(1))),
                _ => {}
            }
        }
    }

    None
}

// --- Keyboard maps for pointer-only windows (WCAG 2.1.1) ------------
//
// iced 0.14 buttons, togglers and pick-lists cannot take keyboard focus
// (no `Focused` status, no key handling in `iced_widget` button.rs) and
// iced ships no a11y feature, so every pointer-only control the app owns
// needs an explicit per-window keyboard map — the same shape the editor
// (`editor_key_message`), History (runtime subscription) and overlay
// (`overlay.rs`) already use. Each map is a pure `key -> …` fn so it can
// be unit-tested without iced.

/// Slider step / bounds for a pin's image opacity. Shared by the
/// [`pin_view`] slider and the keyboard `-`/`+` adjust so they can't
/// drift apart.
pub(crate) const PIN_OPACITY_MIN: f32 = 0.2;
pub(crate) const PIN_OPACITY_MAX: f32 = 1.0;
pub(crate) const PIN_OPACITY_STEP: f32 = 0.05;

/// Keyboard command for a focused pin window. Resolved against live pin
/// state (current opacity, window position) in the runtime so the map
/// itself stays pure and testable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum PinKeyAction {
    Close,
    ToggleLock,
    OpacityUp,
    OpacityDown,
    /// Nudge the window by `(dx, dy)` logical pixels.
    Nudge(f32, f32),
}

/// Map a key press in a focused pin window to a [`PinKeyAction`].
/// Esc / ⌘W close, `L` toggles lock, `-`/`+`/`=` step opacity, and the
/// arrow keys nudge the window (1 px, Shift = 10 px, as in the overlay
/// and editor).
pub(crate) fn pin_key_action(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<PinKeyAction> {
    use iced::keyboard::{key::Named, Key};
    if matches!(key, Key::Named(Named::Escape)) {
        return Some(PinKeyAction::Close);
    }
    if modifiers.command() {
        // ⌘W closes; no other ⌘-chord is a pin command.
        if let Key::Character(c) = key {
            if c.eq_ignore_ascii_case("w") {
                return Some(PinKeyAction::Close);
            }
        }
        return None;
    }
    if modifiers.alt() || modifiers.control() {
        return None;
    }
    // Arrow keys move the borderless pin (which otherwise only drags).
    let step = if modifiers.shift() { 10.0 } else { 1.0 };
    match key {
        Key::Named(Named::ArrowLeft) => return Some(PinKeyAction::Nudge(-step, 0.0)),
        Key::Named(Named::ArrowRight) => return Some(PinKeyAction::Nudge(step, 0.0)),
        Key::Named(Named::ArrowUp) => return Some(PinKeyAction::Nudge(0.0, -step)),
        Key::Named(Named::ArrowDown) => return Some(PinKeyAction::Nudge(0.0, step)),
        // `+` is Shift+`=` on most layouts, so the opacity keys ignore
        // Shift; Lock is a plain letter so it must not fire on Shift+L.
        Key::Character(c) => match c.as_str() {
            "+" | "=" => return Some(PinKeyAction::OpacityUp),
            "-" | "_" => return Some(PinKeyAction::OpacityDown),
            _ if !modifiers.shift() && c.eq_ignore_ascii_case("l") => {
                return Some(PinKeyAction::ToggleLock)
            }
            _ => {}
        },
        _ => {}
    }
    None
}

/// Keyboard command for the settings window. Toggles / cycles are
/// resolved against the live `Preferences` in the runtime (so the map
/// doesn't need to know the current pick-list value); the destructive
/// commands resolve against the two-stage arm state so a single
/// keypress can never wipe settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettingsKeyAction {
    Close,
    ToggleLaunchAtLogin,
    ToggleDebugLogging,
    CycleRetention,
    CycleFormat,
    /// Arm "Reset All Settings"; a second press confirms.
    ResetAll,
    /// Arm "Uninstall Readshot"; a second press confirms.
    Uninstall,
    /// ⌘1–⌘7: show that sidebar page.
    SelectPane(SettingsPane),
    /// ⌘K: record a new capture shortcut.
    RecordShortcut,
}

impl SettingsKeyAction {
    /// The pane that shows the setting this shortcut changes. The
    /// runtime switches there first so a change is never off-screen.
    pub(crate) fn owning_pane(self) -> Option<SettingsPane> {
        match self {
            Self::ToggleLaunchAtLogin | Self::ResetAll | Self::Uninstall => Some(SettingsPane::App),
            Self::ToggleDebugLogging => Some(SettingsPane::Diagnostics),
            Self::CycleRetention | Self::RecordShortcut => Some(SettingsPane::Capture),
            Self::CycleFormat => Some(SettingsPane::Export),
            Self::Close | Self::SelectPane(_) => None,
        }
    }
}

/// Map a key press in the settings window to a [`SettingsKeyAction`].
/// Every shortcut is ⌘-prefixed (so plain typing in the filename field
/// is never stolen) except Esc, which closes. The destructive commands
/// use ⇧⌘ to sit apart from the everyday toggles.
pub(crate) fn settings_key_action(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<SettingsKeyAction> {
    use iced::keyboard::{key::Named, Key};
    // Esc leaves the window (no modifier — matches the app convention).
    if matches!(key, Key::Named(Named::Escape))
        && !modifiers.command()
        && !modifiers.alt()
        && !modifiers.control()
    {
        return Some(SettingsKeyAction::Close);
    }
    // Everything else needs ⌘ (⌃ on non-macOS) and nothing exotic.
    if !modifiers.command() || modifiers.alt() || extra_command_modifier(modifiers) {
        return None;
    }
    let Key::Character(c) = key else {
        return None;
    };
    let c = c.to_ascii_lowercase();
    if modifiers.shift() {
        return match c.as_str() {
            "r" => Some(SettingsKeyAction::ResetAll),
            "u" => Some(SettingsKeyAction::Uninstall),
            _ => None,
        };
    }
    match c.as_str() {
        "w" => Some(SettingsKeyAction::Close),
        "l" => Some(SettingsKeyAction::ToggleLaunchAtLogin),
        "d" => Some(SettingsKeyAction::ToggleDebugLogging),
        "r" => Some(SettingsKeyAction::CycleRetention),
        "f" => Some(SettingsKeyAction::CycleFormat),
        "k" => Some(SettingsKeyAction::RecordShortcut),
        d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() => {
            SettingsPane::from_digit(d.as_bytes()[0] - b'0').map(SettingsKeyAction::SelectPane)
        }
        _ => None,
    }
}

/// Primary action of the welcome window for the currently-shown card.
/// Mirrors the cards' own buttons: Pending → Allow, Awaiting/Denied →
/// Open System Settings, Granted → Capture (disabled while one is in
/// flight, so `None` then). Enter triggers it.
pub(crate) fn welcome_primary_message(
    welcome: WelcomeState,
    capture_in_flight: bool,
) -> Option<Message> {
    match welcome {
        WelcomeState::Pending => Some(Message::App(AppMsg::GrantPermissionRequested)),
        WelcomeState::AwaitingGrant | WelcomeState::Denied => {
            Some(Message::App(AppMsg::OpenPermissionSettingsRequested))
        }
        WelcomeState::Granted => {
            (!capture_in_flight).then_some(Message::Capture(CaptureMsg::OpenOverlayRequested))
        }
    }
}

/// Map a key press in the welcome window: Enter activates the card's
/// primary action; ⌘R restarts Readshot and ⇧⌘R resets the permission
/// while those buttons are shown. Reset Permission is deliberately *not*
/// bound to a bare key (it is mildly destructive).
pub(crate) fn welcome_key_message(
    welcome: WelcomeState,
    capture_in_flight: bool,
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<Message> {
    use iced::keyboard::{key::Named, Key};
    if modifiers.command() && !modifiers.alt() && !extra_command_modifier(modifiers) {
        let Key::Character(c) = key else {
            return None;
        };
        if !c.eq_ignore_ascii_case("r") {
            return None;
        }
        let blocked = matches!(welcome, WelcomeState::AwaitingGrant | WelcomeState::Denied);
        return if modifiers.shift() {
            (blocked || welcome == WelcomeState::Pending)
                .then_some(Message::App(AppMsg::ResetPermissionRequested))
        } else {
            blocked.then_some(Message::App(AppMsg::RestartRequested))
        };
    }
    if modifiers.command() || modifiers.alt() || modifiers.control() || modifiers.shift() {
        return None;
    }
    match key {
        Key::Named(Named::Enter) => welcome_primary_message(welcome, capture_in_flight),
        _ => None,
    }
}

/// Map a key press while a scrolling-capture HUD is up. Esc stops &
/// stitches (as the Stop button); Shift+Esc or ⌘. discards the session
/// — the same `ScrollCaptureCancelRequested` the Cancel button sends,
/// which arms on the first press and confirms on the second.
pub(crate) fn scroll_key_message(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<Message> {
    use iced::keyboard::{key::Named, Key};
    match key {
        Key::Named(Named::Escape) if modifiers.shift() => {
            Some(Message::Scroll(ScrollMsg::CancelRequested))
        }
        Key::Named(Named::Escape) => Some(Message::Scroll(ScrollMsg::StopRequested)),
        Key::Character(c) if c.as_str() == "." && modifiers.command() => {
            Some(Message::Scroll(ScrollMsg::CancelRequested))
        }
        _ => None,
    }
}
