//! Key labels for chips, tooltips and status lines, in the platform's
//! own form: the macOS glyphs (`⇧⌘C`, `↵`, `⌫`,
//! lowercase `esc`) on macOS, the Ctrl form elsewhere, so Windows and
//! Linux are never told to press ⌘. One name per key everywhere.

use super::ui_kit::platform_key;

const MAC: bool = cfg!(target_os = "macos");

pub(crate) const UNDO_KEY: &str = platform_key("⌘Z", "Ctrl+Z");
pub(crate) const REDO_KEY: &str = platform_key("⇧⌘Z", "Ctrl+Shift+Z");
pub(crate) const COPY_IMAGE_KEY: &str = platform_key("⌘C", "Ctrl+C");
pub(crate) const COPY_TEXT_KEY: &str = platform_key("⇧⌘C", "Ctrl+Shift+C");
pub(crate) const SAVE_KEY: &str = platform_key("⌘S", "Ctrl+S");
pub(crate) const PIN_KEY: &str = platform_key("⌘P", "Ctrl+P");
pub(crate) const CLOSE_KEY: &str = platform_key("⌘W", "Ctrl+W");
pub(crate) const ZOOM_OUT_KEY: &str = platform_key("⌘−", "Ctrl+-");
pub(crate) const ZOOM_IN_KEY: &str = platform_key("⌘=", "Ctrl+=");
pub(crate) const ZOOM_ACTUAL_KEY: &str = platform_key("⌘0", "Ctrl+0");
/// Fit to View: Ctrl+Shift off macOS, where Ctrl+Alt is AltGr.
pub(crate) const ZOOM_FIT_KEY: &str = platform_key("⌥⌘0", "Ctrl+Shift+0");
pub(crate) const SCROLL_CAPTURE_KEY: &str = platform_key("⌘R", "Ctrl+R");
/// Commits the inline text editor (plain ↵ is a new line).
pub(crate) const COMMIT_TEXT_KEY: &str = platform_key("⌘↵", "Ctrl+Enter");
pub(crate) const ESC_KEY: &str = platform_key("esc", "Esc");
pub(crate) const SHIFT_ESC_KEY: &str = platform_key("⇧esc", "Shift+Esc");
pub(crate) const RETURN_KEY: &str = platform_key("↵", "Enter");
/// Removes the selection (either delete key works).
pub(crate) const DELETE_KEY: &str = platform_key("⌫", "Delete");
pub(crate) const OPTION_KEY: &str = platform_key("⌥", "Alt");
pub(crate) const SHIFT_KEY: &str = platform_key("⇧", "Shift");

// Image menu and the Resize dialog.
pub(crate) const ROTATE_LEFT_KEY: &str = platform_key("⌘[", "Ctrl+[");
pub(crate) const ROTATE_RIGHT_KEY: &str = platform_key("⌘]", "Ctrl+]");
/// Resize dialog: next unit (px / %).
pub(crate) const RESIZE_UNIT_KEY: &str = platform_key("⌥P", "Alt+P");
/// Resize dialog: lock the aspect ratio.
pub(crate) const RESIZE_LOCK_KEY: &str = platform_key("⌥L", "Alt+L");

// History's own bindings (`history_key_message`, runtime.rs). Delete is
// ⌘⌫ (⌦ also works, and is the key elsewhere; plain ⌫ edits the search
// field); it deletes for good, it does not move to the Trash. Copy Image
// and Copy Text are the editor's keys ([`COPY_IMAGE_KEY`],
// [`COPY_TEXT_KEY`]), Pin is [`PIN_KEY`].
pub(crate) const HISTORY_DELETE_KEY: &str = platform_key("⌘⌫", "Delete");
pub(crate) const HISTORY_SEARCH_KEY: &str = platform_key("⌘F", "Ctrl+F");
pub(crate) const HISTORY_REVEAL_KEY: &str = platform_key("⌥⌘R", "Ctrl+Shift+R");
pub(crate) const HISTORY_COPY_VISIBLE_KEY: &str = platform_key("⌥⇧⌘C", "Ctrl+Shift+A");
pub(crate) const HISTORY_CLEAR_ALL_KEY: &str = platform_key("⇧⌘⌫", "Ctrl+Shift+Backspace");

// Settings window: one key per pane (sidebar order), then each
// control's own key.
pub(crate) const SETTINGS_PANE_KEYS: [&str; 7] = [
    platform_key("⌘1", "Ctrl+1"),
    platform_key("⌘2", "Ctrl+2"),
    platform_key("⌘3", "Ctrl+3"),
    platform_key("⌘4", "Ctrl+4"),
    platform_key("⌘5", "Ctrl+5"),
    platform_key("⌘6", "Ctrl+6"),
    platform_key("⌘7", "Ctrl+7"),
];
pub(crate) const SETTINGS_RECORD_HOTKEY_KEY: &str = platform_key("⌘K", "Ctrl+K");
pub(crate) const SETTINGS_RETENTION_KEY: &str = platform_key("⌘R", "Ctrl+R");
pub(crate) const SETTINGS_FORMAT_KEY: &str = platform_key("⌘F", "Ctrl+F");
pub(crate) const SETTINGS_DEBUG_LOGGING_KEY: &str = platform_key("⌘D", "Ctrl+D");
pub(crate) const SETTINGS_LAUNCH_AT_LOGIN_KEY: &str = platform_key("⌘L", "Ctrl+L");
/// Reset All Settings, and its confirmation.
pub(crate) const SETTINGS_RESET_ALL_KEY: &str = platform_key("⇧⌘R", "Ctrl+Shift+R");
/// Uninstall Readshot, and its confirmation.
pub(crate) const SETTINGS_UNINSTALL_KEY: &str = platform_key("⇧⌘U", "Ctrl+Shift+U");

// Welcome window.
pub(crate) const WELCOME_RESTART_KEY: &str = platform_key("⌘R", "Ctrl+R");
pub(crate) const WELCOME_RESET_PERMISSION_KEY: &str = platform_key("⇧⌘R", "Ctrl+Shift+R");

/// The inline text editor's placeholder.
pub(crate) const TEXT_PLACEHOLDER: &str = if MAC {
    "Type text… (⌘↵ to commit, esc to cancel)"
} else {
    "Type text… (Ctrl+Enter to commit, Esc to cancel)"
};

/// The eyedropper's instruction line.
pub(crate) const EYEDROPPER_HINT: &str = if MAC {
    "Click the image to pick a color, or press esc to cancel."
} else {
    "Click the image to pick a color, or press Esc to cancel."
};

/// The tail every undoable change's status ends with.
pub(crate) const UNDO_HINT: &str = if MAC {
    "⌘Z to undo."
} else {
    "Ctrl+Z to undo."
};
pub(crate) const REDO_HINT: &str = if MAC {
    "⇧⌘Z to redo."
} else {
    "Ctrl+Shift+Z to redo."
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_labels_follow_the_platform() {
        for k in [
            UNDO_KEY,
            REDO_KEY,
            COPY_IMAGE_KEY,
            COPY_TEXT_KEY,
            SAVE_KEY,
            PIN_KEY,
            CLOSE_KEY,
            ZOOM_OUT_KEY,
            ZOOM_IN_KEY,
            ZOOM_ACTUAL_KEY,
            ZOOM_FIT_KEY,
            SCROLL_CAPTURE_KEY,
            COMMIT_TEXT_KEY,
            UNDO_HINT,
            REDO_HINT,
        ] {
            assert_eq!(k.contains('⌘'), MAC, "{k}");
            assert_eq!(k.contains("Ctrl"), !MAC, "{k}");
        }
        assert!(UNDO_HINT.starts_with(UNDO_KEY));
    }

    /// One naming rule per platform. macOS: glyphs and lowercase `esc`
    ///. Elsewhere: words joined with "+" ("Shift+Esc",
    /// "Ctrl+Enter"), never a Mac glyph.
    #[test]
    fn every_key_label_follows_its_platform_rule() {
        use crate::runtime::key_focus as kf;
        let mut labels = vec![
            UNDO_KEY,
            REDO_KEY,
            COPY_IMAGE_KEY,
            COPY_TEXT_KEY,
            SAVE_KEY,
            PIN_KEY,
            CLOSE_KEY,
            ZOOM_OUT_KEY,
            ZOOM_IN_KEY,
            ZOOM_ACTUAL_KEY,
            ZOOM_FIT_KEY,
            SCROLL_CAPTURE_KEY,
            COMMIT_TEXT_KEY,
            DELETE_KEY,
            OPTION_KEY,
            SHIFT_KEY,
            ESC_KEY,
            SHIFT_ESC_KEY,
            RETURN_KEY,
            ROTATE_LEFT_KEY,
            ROTATE_RIGHT_KEY,
            RESIZE_UNIT_KEY,
            RESIZE_LOCK_KEY,
            HISTORY_DELETE_KEY,
            HISTORY_SEARCH_KEY,
            HISTORY_REVEAL_KEY,
            HISTORY_COPY_VISIBLE_KEY,
            HISTORY_CLEAR_ALL_KEY,
            SETTINGS_RECORD_HOTKEY_KEY,
            SETTINGS_RETENTION_KEY,
            SETTINGS_FORMAT_KEY,
            SETTINGS_DEBUG_LOGGING_KEY,
            SETTINGS_LAUNCH_AT_LOGIN_KEY,
            SETTINGS_RESET_ALL_KEY,
            SETTINGS_UNINSTALL_KEY,
            WELCOME_RESTART_KEY,
            WELCOME_RESET_PERMISSION_KEY,
            crate::overlay::SHIFT_CHIP,
            crate::overlay::RESIZE_CHIP,
        ];
        for k in [
            kf::KEY_IMAGE_MENU,
            kf::KEY_FLIP_H,
            kf::KEY_FLIP_V,
            kf::KEY_TEXT_TAB,
            kf::KEY_BEAUTIFY_TAB,
            kf::KEY_PANEL,
            kf::KEY_RESIZE,
            kf::KEY_NEXT_ZONE,
        ] {
            labels.push(kf::key_label(k));
        }
        labels.extend(SETTINGS_PANE_KEYS);
        for l in labels {
            if MAC {
                assert!(
                    !l.contains("Ctrl") && !l.contains("Esc") && !l.contains("Enter"),
                    "{l}"
                );
            } else {
                assert!(!l.chars().any(|c| "⌘⇧⌥⌃↵⌫⌦".contains(c)), "{l}");
                assert!(!l.contains("esc"), "{l}");
                // Every part names a key: a word or a single symbol.
                for part in l.split('+') {
                    assert!(!part.is_empty(), "{l}");
                    assert!(
                        part.chars().next().unwrap().is_uppercase() || part.chars().count() <= 4,
                        "{l}"
                    );
                }
            }
        }
        for prose in [TEXT_PLACEHOLDER, EYEDROPPER_HINT, UNDO_HINT, REDO_HINT] {
            if !MAC {
                assert!(!prose.chars().any(|c| "⌘⇧⌥⌃↵⌫".contains(c)), "{prose}");
                assert!(!prose.contains(" esc "), "{prose}");
            }
        }
        assert!(REDO_HINT.starts_with(REDO_KEY));
    }
}
