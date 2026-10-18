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
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        ),
        A::RetryRecognition => ("Retry", None, Message::EditorTextRecognize),
        A::RestorePreset => ("Undo", None, Message::EditorBeautifyRestorePreset),
    }
}

/// Errors and warnings never auto-dismiss, so their toast carries a
/// Dismiss button.
pub(crate) fn toast_is_dismissable(kind: ToastKind) -> bool {
    matches!(kind, ToastKind::Error | ToastKind::Warn)
}

/// Toast kind for the editor status line: the tone its writer chose.
pub(crate) fn editor_toast_kind(tone: crate::editor::StatusTone) -> ToastKind {
    use crate::editor::StatusTone;
    match tone {
        StatusTone::Success => ToastKind::Ok,
        StatusTone::Notice | StatusTone::Info | StatusTone::Progress => ToastKind::Info,
        StatusTone::Warning | StatusTone::DiscardPrompt => ToastKind::Warn,
        StatusTone::Error => ToastKind::Error,
    }
}

/// The editor tone for a plain status string from outside the editor (a
/// notification that could not be shown): the kind [`status_toast_kind`]
/// gives it. One that reports work starting ("Checking for updates…") is
/// a notice: the editor will never see that work end, so it must not
/// wait for it.
pub(crate) fn plain_status_tone(status: &str) -> crate::editor::StatusTone {
    use crate::editor::StatusTone;
    if status.ends_with('…') && !crate::editor::status_str_is_error(status) {
        return StatusTone::Notice;
    }
    match status_toast_kind(status) {
        ToastKind::Ok => StatusTone::Success,
        ToastKind::Info => StatusTone::Notice,
        ToastKind::Warn => StatusTone::Warning,
        ToastKind::Error => StatusTone::Error,
    }
}

/// Toast kind for a settled status line (History, Welcome, editor):
/// failures are Error; "No …", "Text hidden…", "… not found" and
/// "… cancelled" are neutral notices (Info) rather than successes; the
/// rest are Ok.
pub(crate) fn status_toast_kind(status: &str) -> ToastKind {
    if crate::editor::status_str_is_error(status) {
        return ToastKind::Error;
    }
    let lower = status.to_lowercase();
    if lower.starts_with("no ")
        || lower.starts_with("text hidden")
        || lower.contains("not found")
        || lower.contains("cancelled")
    {
        ToastKind::Info
    } else {
        ToastKind::Ok
    }
}
