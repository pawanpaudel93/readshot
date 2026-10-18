//! History record messages: saves, the list load and reopening a
//! record after its writes.
//! Moved out of `handle_history_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `Message::HistoryRecordSaveFailed`.
pub(super) fn on_record_save_failed(
    state: &mut App,
    session: Option<u64>,
    record_id: readshot_core::Uuid,
    error: String,
) -> Task<Message> {
    // The record will never exist: no editor writes to it any more,
    // and nothing parked for its save (a frame rewrite, a scrub,
    // each holding a document image) waits for it until quit.
    state.coordinator.forget_unsaved_record(record_id);
    for ed in state
        .editor
        .iter_mut()
        .chain(state.retiring_editors.iter_mut())
    {
        if ed.source_record.as_ref().is_some_and(|r| r.id == record_id) {
            ed.source_record = None;
        }
    }
    // Tell the user where they are looking: the editor opened for
    // this capture, else the capture status (menu bar,
    // notification).
    match state
        .editor
        .as_mut()
        .filter(|ed| Some(ed.session_id) == session)
    {
        Some(ed) => ed.set_status(StatusTone::Error, history_save_failure(&error, true)),
        None => state.last_capture_status = Some(history_save_failure(&error, false)),
    }
    Task::none()
}

/// `Message::HistoryRecordPersisted`.
pub(super) fn on_record_persisted(state: &mut App, result: Result<bool, String>) -> Task<Message> {
    match result {
        Ok(true) => {
            // OCR completed for the just-saved record. Refresh
            // the browser if it's open so the new text is
            // searchable / visible.
            if state.history_window_id.is_some() {
                return history_list_task(state.coordinator.clone());
            }
        }
        // `Ok(false)` = history off, or OCR failed/empty. The
        // row (if any) already appeared via `HistoryRecordSaved`
        // and its text won't change, so there's nothing to
        // refresh here.
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "persist failed: {e}")
        }
    }
    Task::none()
}

/// `Message::HistoryListLoaded`.
pub(super) fn on_list_loaded(
    state: &mut App,
    result: Result<Vec<readshot_core::CaptureRecord>, String>,
) -> Task<Message> {
    match result {
        Ok(records) => {
            let previous = state.history_selected_id;
            state.history_records = records;
            state.history_selected_id = preferred_history_selection(
                &state.history_records,
                &state.history_search,
                previous,
            );
            state.history_status = None;
        }
        Err(e) => {
            state.history_records.clear();
            state.history_selected_id = None;
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "history list failed: {e}");
            state.history_status = Some(friendly_string_error_with(
                "read the history",
                &e,
                Some(REOPEN_HISTORY_HINT),
            ));
        }
    }
    Task::none()
}

/// `Message::HistoryReopenedAfterSync`: the record's writes landed; open it.
pub(super) fn on_reopened_after_sync(
    state: &mut App,
    record_id: readshot_core::Uuid,
    result: Result<(image::RgbaImage, readshot_core::CaptureRecord), String>,
    unsaved: Option<String>,
) -> Task<Message> {
    if state.awaiting_reopen != Some(record_id) {
        // Cancelled while it waited (said then): never open a
        // window the user walked away from. Writes that failed are
        // still named.
        if let Some((_, said)) = state
            .cancelled_history_open
            .take_if(|(id, _)| *id == record_id)
        {
            if unsaved.is_some() {
                show_open_status(state, format!("{said}{}", not_saved(&unsaved)));
            }
        }
        return Task::none();
    }
    state.awaiting_reopen = None;
    state.history_status = None;
    let unsaved = match (state.history_open_note.take(), unsaved) {
        (Some(a), Some(b)) => Some(format!("{a}; {b}")),
        (a, b) => a.or(b),
    };
    let loaded = match result {
        Ok(loaded) => loaded,
        Err(e) if e == REDACTION_NOT_SAVED => {
            state.history_status = Some(format!(
                        "Couldn’t open the capture yet — History couldn’t finish hiding its redaction{}. Its image is kept; open it again in a moment.",
                        unsaved.as_ref().map_or(String::new(), |r| format!(" ({r})"))
                    ));
            return Task::none();
        }
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "reopen failed: {e}");
            state.history_status = Some(format!(
                "{}{}",
                friendly_string_error_with("open the capture", &e, Some(RETRY_HINT)),
                not_saved(&unsaved)
            ));
            return Task::none();
        }
    };
    // The current editor (of another capture) has unsaved work:
    // never replace it silently; ask first. Discard then opens the
    // record again through the same path (nothing outstanding: no
    // wait), never from this copy.
    if let Some(ed) = state
        .editor
        .as_mut()
        .filter(|ed| crate::runtime::editor_ops::editor_has_unsaved_work(ed))
    {
        ed.arm_discard_prompt_then(
                    "The capture from History is ready, but this window has unsaved edits. Press Discard to close them and open it, or keep editing.",
                    crate::editor::AfterDiscard::Open(record_id, unsaved),
                );
        return Task::none();
    }
    open_reopened(state, loaded, unsaved)
}
