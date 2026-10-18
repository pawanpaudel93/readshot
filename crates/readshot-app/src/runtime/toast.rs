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

/// Toast kind for the editor status line. Armed discard wins (amber),
/// then errors, then in-flight work, then the tone the status was set
/// with (`EditorSession::set_warning`: refusals, partial failures;
/// `set_info`: instructions); the rest is classified from the wording.
pub(crate) fn editor_toast_kind(
    status: &str,
    in_progress: bool,
    discard_armed: bool,
    tone: crate::editor::StatusTone,
) -> ToastKind {
    use crate::editor::StatusTone;
    let error = crate::editor::status_str_is_error(status);
    if discard_armed {
        ToastKind::Warn
    } else if in_progress && !error {
        ToastKind::Info
    } else if tone == StatusTone::Warning && !error {
        ToastKind::Warn
    } else if tone == StatusTone::Info && !error {
        ToastKind::Info
    } else {
        status_toast_kind(status)
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
