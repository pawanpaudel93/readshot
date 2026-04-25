//! Bottom action bar — the buttons that decide what happens with a
//! finished annotation: Copy Image / Copy Text (OCR) / Save / Pin /
//! Discard.
//!
//! The bar publishes [`ActionMessage`]s that the composition root
//! routes onward — Save → Exporter, Copy Image → ClipboardWriter,
//! Copy Text → OCREngine + ClipboardWriter, Pin → a new pin window,
//! Discard → close editor.

/// Public messages the bar publishes. Keep the names stable; they
/// appear in spec §3.10 verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionMessage {
    CopyImage,
    /// Copy the recognised text from the current capture; uses the
    /// platform OCR engine via the trait set up in Task 10.
    CopyText,
    Save,
    /// Pin the current capture as a floating window so the user can
    /// reference it while working in another app. Implementation in
    /// Task 16.
    Pin,
    Discard,
    /// Triggered by ⌘/Ctrl+Z / ⌘/Ctrl+Shift+Z. The action bar
    /// surfaces these so they live alongside the visible buttons.
    Undo,
    Redo,
}

/// Display-ordered list. Keeps the bar's layout stable across
/// releases.
pub const BUTTON_ORDER: [ActionMessage; 5] = [
    ActionMessage::CopyImage,
    ActionMessage::CopyText,
    ActionMessage::Save,
    ActionMessage::Pin,
    ActionMessage::Discard,
];

/// Default keyboard shortcut for each action, in a platform-neutral
/// "Ctrl+X" notation. Task 6's hotkey wiring on macOS swaps "Ctrl"
/// to "Cmd" automatically; toolbar tooltips show the localised form.
pub fn default_shortcut(message: ActionMessage) -> Option<&'static str> {
    Some(match message {
        ActionMessage::CopyImage => "Ctrl+C",
        ActionMessage::CopyText => "Ctrl+Shift+C",
        ActionMessage::Save => "Ctrl+S",
        ActionMessage::Pin => "Ctrl+P",
        ActionMessage::Discard => "Esc",
        ActionMessage::Undo => "Ctrl+Z",
        ActionMessage::Redo => "Ctrl+Shift+Z",
    })
}

/// Default labels that appear on each button. Plain ASCII — Task 16
/// wires localisation later if it becomes a goal.
pub fn default_label(message: ActionMessage) -> &'static str {
    match message {
        ActionMessage::CopyImage => "Copy Image",
        ActionMessage::CopyText => "Copy Text",
        ActionMessage::Save => "Save",
        ActionMessage::Pin => "Pin",
        ActionMessage::Discard => "Discard",
        ActionMessage::Undo => "Undo",
        ActionMessage::Redo => "Redo",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_order_lists_the_five_visible_actions() {
        assert_eq!(BUTTON_ORDER.len(), 5);
        // Undo / Redo are keyboard-only; they don't appear on the
        // visible button row but still emit ActionMessage variants.
        assert!(!BUTTON_ORDER.contains(&ActionMessage::Undo));
        assert!(!BUTTON_ORDER.contains(&ActionMessage::Redo));
    }

    #[test]
    fn every_action_has_a_label_and_default_shortcut() {
        for action in [
            ActionMessage::CopyImage,
            ActionMessage::CopyText,
            ActionMessage::Save,
            ActionMessage::Pin,
            ActionMessage::Discard,
            ActionMessage::Undo,
            ActionMessage::Redo,
        ] {
            assert!(
                !default_label(action).is_empty(),
                "label missing for {action:?}"
            );
            assert!(
                default_shortcut(action).is_some(),
                "shortcut missing for {action:?}"
            );
        }
    }

    #[test]
    fn shortcut_for_save_is_ctrl_s() {
        assert_eq!(default_shortcut(ActionMessage::Save), Some("Ctrl+S"));
    }
}
