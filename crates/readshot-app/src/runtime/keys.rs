//! Key labels for chips, tooltips and status lines, in the platform's
//! own form: the macOS glyphs (spec §2.1 rule 4: `⇧⌘C`, `↵`, `⌫`,
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
pub(crate) const ZOOM_OUT_KEY: &str = platform_key("⌘−", "Ctrl+−");
pub(crate) const ZOOM_IN_KEY: &str = platform_key("⌘=", "Ctrl+=");
pub(crate) const ZOOM_ACTUAL_KEY: &str = platform_key("⌘0", "Ctrl+0");
pub(crate) const SCROLL_CAPTURE_KEY: &str = platform_key("⌘R", "Ctrl+R");
/// Commits the inline text editor (plain ↵ is a new line).
pub(crate) const COMMIT_TEXT_KEY: &str = platform_key("⌘↵", "Ctrl+↵");
/// Removes the selection (either delete key works).
pub(crate) const DELETE_KEY: &str = platform_key("⌫", "Delete");
pub(crate) const OPTION_KEY: &str = platform_key("⌥", "Alt");
pub(crate) const SHIFT_KEY: &str = platform_key("⇧", "Shift");

/// The inline text editor's placeholder.
pub(crate) const TEXT_PLACEHOLDER: &str = if MAC {
    "Type text… (⌘↵ to commit, esc to cancel)"
} else {
    "Type text… (Ctrl+↵ to commit, esc to cancel)"
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
            SCROLL_CAPTURE_KEY,
            COMMIT_TEXT_KEY,
            UNDO_HINT,
            REDO_HINT,
        ] {
            assert_eq!(k.contains('⌘'), MAC, "{k}");
            assert_eq!(k.contains("Ctrl"), !MAC, "{k}");
        }
        assert!(UNDO_HINT.starts_with(UNDO_KEY));
        assert!(REDO_HINT.starts_with(REDO_KEY));
    }
}
