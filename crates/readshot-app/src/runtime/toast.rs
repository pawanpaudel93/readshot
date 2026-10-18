//! The editor toast: what kind a status shows as, its button, and
//! whether it can be dismissed.

use super::*;

use crate::app::Message;

/// Label, key chip and message of a status-toast button.
pub(crate) fn toast_action_parts(
    action: crate::editor::EditorToastAction,
) -> (&'static str, Option<&'static str>, Message) {
    use crate::editor::EditorToastAction as A;
    match action {
        A::UndoRedaction { .. } => (
            "Undo",
            Some(UNDO_KEY),
            Message::Editor(EditorMsg::Toolbar(readshot_ui::ToolbarMessage::Undo)),
        ),
        A::RetryRecognition => (
            "Retry",
            None,
            Message::Editor(EditorMsg::TextPanel(TextPanelMsg::Recognize)),
        ),
        A::RestorePreset => (
            "Undo",
            None,
            Message::Editor(EditorMsg::Beautify(BeautifyMsg::RestorePreset)),
        ),
    }
}

/// Errors and warnings never auto-dismiss, so their toast carries a
/// Dismiss button.
pub(crate) fn toast_is_dismissable(kind: ToastKind) -> bool {
    matches!(kind, ToastKind::Error | ToastKind::Warn)
}

/// Toast kind for a status (the editor's, History's, the Welcome
/// window's): the tone its writer chose.
pub(crate) fn status_toast_kind(tone: crate::editor::StatusTone) -> ToastKind {
    use crate::editor::StatusTone;
    match tone {
        StatusTone::Success => ToastKind::Ok,
        StatusTone::Notice | StatusTone::Info | StatusTone::Progress => ToastKind::Info,
        StatusTone::Warning | StatusTone::DiscardPrompt => ToastKind::Warn,
        StatusTone::Error => ToastKind::Error,
    }
}
