//! Editor Text panel (spec §3.6–§3.8): recognition of the base image,
//! the Text tab, line selection and copy, smart redact and the smart
//! highlighter snap. Positions come from the OCR engine; without them
//! (Windows, Linux today) only the list and copying are offered.

use super::*;

use iced::Task;

use crate::app::{App, Message};
use crate::editor::{EditorSession, EditorToastAction, TextRecognition};

/// Start recognizing the editor's base image unless it is already
/// running or done. The base (not the flattened output) is recognized,
/// so line boxes are in annotation coordinates and redactions can't
/// hide text from the list. Never sets `busy`.
pub(crate) fn request_text_recognition(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if matches!(
        ed.text_panel.recognition,
        TextRecognition::Recognizing { .. } | TextRecognition::Ready(_)
    ) {
        return Task::none();
    }
    ed.text_panel.recognition = TextRecognition::Recognizing {
        since: std::time::Instant::now(),
    };
    if ed.toast_action == Some(EditorToastAction::RetryRecognition) {
        ed.status = None;
        ed.status_set_at = None;
        ed.toast_action = None;
    }
    let session = ed.session_id;
    let req = ocr_request_for_image(ed.model.base().clone(), &state.preferences);
    let coord = state.coordinator.clone();
    Task::perform(
        async move { coord.recognise_document(req).await },
        move |r| Message::EditorTextRecognized {
            session,
            result: r.map_err(|e| e.to_string()),
        },
    )
}

/// History reopen: an engine without line positions could only
/// reproduce the stored `ocr_text`, so the panel is built from it with
/// no OCR. Engines with positions (Apple Vision) recognize again.
pub(crate) fn seed_text_from_history(ed: &mut EditorSession, engine_reports_positions: bool) {
    if engine_reports_positions {
        return;
    }
    let Some(text) = ed
        .source_record
        .as_ref()
        .and_then(|r| r.ocr_text.as_deref())
    else {
        return;
    };
    let doc = readshot_core::text_lines::TextDocument::from_plain(text);
    ed.text_panel.recognition = TextRecognition::Ready(std::sync::Arc::new(doc));
}

pub(crate) fn handle_text_panel_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::EditorTextRecognize => request_text_recognition(state),
        Message::EditorTextRecognized { session, result } => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.session_id != session {
                return Task::none();
            }
            match result {
                Ok(doc) => {
                    let tp = &mut ed.text_panel;
                    tp.matches = if doc.positioned {
                        readshot_core::redact::scan(&doc)
                    } else {
                        Vec::new()
                    };
                    tp.hovered = None;
                    tp.selected.clear();
                    tp.anchor = None;
                    tp.recognition = TextRecognition::Ready(std::sync::Arc::new(doc));
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::editor", "text recognition failed: {e}");
                    ed.text_panel.recognition = TextRecognition::Failed(e.clone());
                    // Don't clobber a Saving…/Copying… toast; the panel's
                    // Try Again state still offers the retry.
                    if !ed.busy {
                        ed.set_status_with_action(
                            super::history_ops::friendly_string_error_with(
                                "recognize the text",
                                &e,
                                None,
                            ),
                            EditorToastAction::RetryRecognition,
                        );
                    }
                }
            }
            Task::none()
        }
        other => unreachable!("non-text-panel message routed to the text panel handler: {other:?}"),
    }
}
