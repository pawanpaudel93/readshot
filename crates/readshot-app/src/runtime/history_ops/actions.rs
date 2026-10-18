//! History actions on one record: reveal, copy, pin and delete.
//! Moved out of `handle_history_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// Record `id`'s image path for Reveal, Copy Image and Pin, or `None`
/// with the reason in History's status: not found, or withheld while its
/// redaction is being saved (nothing of it may be handed out then).
fn served_image(state: &mut App, id: readshot_core::Uuid) -> Option<PathBuf> {
    let found = state
        .history_root
        .as_ref()
        .zip(state.history_records.iter().find(|r| r.id == id));
    let Some((root, record)) = found else {
        state.history_status = Some("Capture not found.".into());
        return None;
    };
    let path = history_png_path(root, record);
    if path.is_none() {
        state.history_status = Some(WITHHELD_NOTE.into());
    }
    path
}

/// A record withheld while its redaction is saved.
pub(crate) const WITHHELD_NOTE: &str =
    "This capture’s redaction is still being saved, so its image is hidden. Try again in a moment.";

/// `HistoryMsg::Reveal`.
pub(super) fn on_reveal(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    state.history_delete_pending = None;
    let Some(path) = served_image(state, id) else {
        return Task::none();
    };
    if let Err(e) = reveal_path(&path) {
        tracing::warn!(target: readshot_core::log::cat::HISTORY, "reveal failed: {e}");
        state.history_status = Some(friendly_string_error_with(
            "reveal the file",
            &e.to_string(),
            Some(RETRY_HINT),
        ));
    }
    Task::none()
}

/// `HistoryMsg::CopyImage`.
pub(super) fn on_copy_image(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    state.history_delete_pending = None;
    let Some(path) = served_image(state, id) else {
        return Task::none();
    };
    Task::perform(
        async move {
            let img = load_png_async(path).await.map_err(|e| e.to_string())?;
            copy_image_to_clipboard(img)
                .await
                .map_err(|e| e.to_string())
        },
        |a| Message::History(HistoryMsg::CopyImageDone(a)),
    )
}

/// `HistoryMsg::CopyImageDone`.
pub(super) fn on_copy_image_done(state: &mut App, result: Result<(), String>) -> Task<Message> {
    state.history_status = Some(match result {
        Ok(()) => "Copied image to clipboard.".into(),
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "copy image failed: {e}");
            friendly_string_error("copy the image to the clipboard", &e)
        }
    });
    Task::none()
}

/// `HistoryMsg::CopyText`.
pub(super) fn on_copy_text(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    state.history_delete_pending = None;
    match state
        .history_records
        .iter()
        .find(|r| r.id == id)
        .and_then(|r| r.ocr_text.as_ref())
    {
        Some(text) if !text.is_empty() => {
            let text = text.clone();
            Task::perform(
                async move {
                    copy_text_to_clipboard(text.clone())
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(text)
                },
                |a| Message::History(HistoryMsg::CopyTextDone(a)),
            )
        }
        _ => {
            let hidden = state
                .history_records
                .iter()
                .find(|r| r.id == id)
                .is_some_and(history_text_hidden);
            state.history_status = Some(if hidden {
                HIDDEN_TEXT_NOTE.into()
            } else {
                "No OCR text on this capture yet.".into()
            });
            Task::none()
        }
    }
}

/// `HistoryMsg::CopyTextDone`.
pub(super) fn on_copy_text_done(state: &mut App, result: Result<String, String>) -> Task<Message> {
    state.history_status = Some(match result {
        Ok(text) if text.is_empty() => "No OCR text on this capture yet.".into(),
        Ok(text) => {
            let n = text.chars().count();
            format!("Copied {} of text.", counted(n, "character", "characters"))
        }
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "copy text failed: {e}");
            friendly_string_error_with("copy the text", &e, Some(RETRY_HINT))
        }
    });
    Task::none()
}

/// `HistoryMsg::CopyVisibleTextRequested`.
pub(super) fn on_copy_visible_text(state: &mut App) -> Task<Message> {
    let text = visible_history_text(&state.history_records, &state.history_search);
    if text.trim().is_empty() {
        state.history_status = Some("No OCR text in the visible captures.".into());
        return Task::none();
    }
    let count = visible_history_text_count(&state.history_records, &state.history_search);
    Task::perform(
        async move {
            copy_text_to_clipboard(text)
                .await
                .map_err(|e| e.to_string())?;
            Ok(count)
        },
        |a| Message::History(HistoryMsg::CopyVisibleTextDone(a)),
    )
}

/// `HistoryMsg::CopyVisibleTextDone`.
pub(super) fn on_copy_visible_text_done(
    state: &mut App,
    result: Result<usize, String>,
) -> Task<Message> {
    state.history_status = Some(match result {
        Ok(count) => format!(
            "Copied OCR text from {}.",
            counted(count, "capture", "captures")
        ),
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "copy visible text failed: {e}");
            friendly_string_error_with("copy the visible text", &e, Some(RETRY_HINT))
        }
    });
    Task::none()
}

/// `HistoryMsg::Pin`.
pub(super) fn on_pin(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    state.history_delete_pending = None;
    let Some(path) = served_image(state, id) else {
        return Task::none();
    };
    Task::perform(load_png_async(path), |r| {
        Message::History(HistoryMsg::PinReady(r.map_err(|e| e.to_string())))
    })
}

/// `HistoryMsg::PinReady`.
pub(super) fn on_pin_ready(
    state: &mut App,
    result: Result<image::RgbaImage, String>,
) -> Task<Message> {
    match result {
        Ok(image) => {
            let size = (image.width(), image.height());
            let handle = rgba_handle(image);
            let (wid, open_task) = window::open(pin_window_settings(size, None));
            state.windows.register(wid, WindowKind::Pin);
            state
                .pins
                .insert(wid, crate::app::PinState::new(handle.clone()));
            open_task.map(move |opened| Message::PinWindowReady(opened, handle.clone()))
        }
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "pin failed: {e}");
            state.history_status = Some(friendly_string_error_with(
                "pin the capture",
                &e,
                Some(RETRY_HINT),
            ));
            Task::none()
        }
    }
}

/// `HistoryMsg::Delete`: ask before deleting.
pub(super) fn on_delete(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    // Two-stage guard, mirroring "Clear All": the first press
    // arms the prompt (the row's button flips to "Confirm
    // delete?"); a second press for the same id confirms. This
    // also makes the Delete key confirm on a repeat press.
    if state.history_delete_pending == Some(id) {
        return delete_history_record(state, id);
    }
    state.history_clear_all_pending = false;
    state.history_delete_pending = Some(id);
    Task::none()
}
