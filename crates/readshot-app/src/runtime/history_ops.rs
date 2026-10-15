//! History-browser message handling, extracted from `runtime.rs`.
//! `use super::*` pulls in the runtime's full scope (helpers, iced
//! imports, sibling-module re-exports) so the moved arms compile
//! unchanged.

use super::*;

/// Every history message arm, extracted from `runtime::update` so the
/// top-level dispatcher stays navigable. Routed from `update`'s
/// grouped arm; the trailing `unreachable!` only fires if a
/// non-history message is mis-routed here.
pub(crate) fn handle_history_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::HistoryRecordSaved => {
            // The record's PNG + sidecar have landed on disk (OCR is
            // still pending). Refresh an open browser so the new row
            // shows immediately instead of only once OCR finishes.
            // Skipped when the browser is closed so a background
            // capture never triggers a pointless list read.
            if state.history_window_id.is_some() {
                return history_list_task(state.coordinator.clone());
            }
            Task::none()
        }
        Message::HistoryRecordPersisted(result) => {
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
                Err(e) => tracing::warn!(target: "readshot::history", "persist failed: {e}"),
            }
            Task::none()
        }

        Message::OpenHistoryRequested => {
            // Single-instance — if the window already exists just
            // refocus it and refresh the list.
            if let Some(id) = state.history_window_id {
                return Task::batch([
                    window::gain_focus(id),
                    focus_history_search(),
                    history_list_task(state.coordinator.clone()),
                ]);
            }
            let (id, open_task) = window::open(history_window_settings());
            state.windows.register(id, WindowKind::History);
            state.history_window_id = Some(id);
            state.history_status = None;
            state.history_delete_pending = None;
            state.history_page_limit = crate::app::HISTORY_PAGE_SIZE;
            Task::batch([
                open_task.map(Message::HistoryWindowReady),
                history_list_task(state.coordinator.clone()),
            ])
        }
        Message::HistoryWindowReady(id) => {
            // Window settings already register at open-time; the
            // ready callback just records the id in case iced hands
            // back a different one.
            state.history_window_id = Some(id);
            // Typing right away searches, like Spotlight or Finder.
            Task::batch([window::gain_focus(id), focus_history_search()])
        }
        Message::HistoryListLoaded(result) => {
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
                    state.history_status = Some(format!("Couldn't read history: {e}"));
                }
            }
            Task::none()
        }
        Message::HistoryClosed => {
            let id = state.history_window_id.take();
            state.history_records.clear();
            state.history_status = None;
            state.history_selected_id = None;
            state.history_delete_pending = None;
            state.history_page_limit = crate::app::HISTORY_PAGE_SIZE;
            match id {
                Some(id) => {
                    state.windows.forget(id);
                    window::close(id)
                }
                None => Task::none(),
            }
        }
        Message::HistorySearchChanged(q) => {
            // A search change (Escape maps here too) cancels an armed
            // delete and resets paging so the first page of the new
            // result set is shown.
            state.history_delete_pending = None;
            state.history_page_limit = crate::app::HISTORY_PAGE_SIZE;
            state.history_search = q;
            state.history_selected_id = preferred_history_selection(
                &state.history_records,
                &state.history_search,
                state.history_selected_id,
            );
            Task::none()
        }
        Message::HistorySelect(id) => {
            if state.history_delete_pending != Some(id) {
                state.history_delete_pending = None;
            }
            if history_record_visible(&state.history_records, &state.history_search, id) {
                state.history_selected_id = Some(id);
            }
            Task::none()
        }
        Message::HistorySelectPrevious => {
            state.history_delete_pending = None;
            state.history_selected_id = adjacent_history_selection(
                &state.history_records,
                &state.history_search,
                state.history_selected_id,
                -1,
            );
            Task::none()
        }
        Message::HistorySelectNext => {
            state.history_delete_pending = None;
            state.history_selected_id = adjacent_history_selection(
                &state.history_records,
                &state.history_search,
                state.history_selected_id,
                1,
            );
            Task::none()
        }
        Message::HistoryOpenSelected => match state.history_selected_id {
            Some(id) => update(state, Message::HistoryOpenInEditor(id)),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        Message::HistoryDeleteSelected => match state.history_selected_id {
            Some(id) => update(state, Message::HistoryDelete(id)),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        Message::HistoryKeyboardShortcut(window_id, action) => {
            if state.history_window_id != Some(window_id) {
                return Task::none();
            }
            match action {
                HistoryKeyboardAction::Previous => update(state, Message::HistorySelectPrevious),
                HistoryKeyboardAction::Next => update(state, Message::HistorySelectNext),
                HistoryKeyboardAction::Open => update(state, Message::HistoryOpenSelected),
                HistoryKeyboardAction::Delete => update(state, Message::HistoryDeleteSelected),
                HistoryKeyboardAction::CopyText => match state.history_selected_id {
                    Some(id) => update(state, Message::HistoryCopyText(id)),
                    None => {
                        state.history_status = Some("No history capture selected.".into());
                        Task::none()
                    }
                },
                HistoryKeyboardAction::CopyImage => match state.history_selected_id {
                    Some(id) => update(state, Message::HistoryCopyImage(id)),
                    None => {
                        state.history_status = Some("No history capture selected.".into());
                        Task::none()
                    }
                },
                HistoryKeyboardAction::FocusSearch => focus_history_search(),
            }
        }
        Message::HistoryClearAllRequested => {
            // First click arms the destructive prompt. The button
            // flips to "Confirm Clear · Cancel" so the user has a
            // visible second step before everything is wiped. Arming
            // Clear All also cancels any pending single-row delete.
            state.history_delete_pending = None;
            state.history_clear_all_pending = true;
            Task::none()
        }
        Message::HistoryClearAllCancelled => {
            state.history_clear_all_pending = false;
            Task::none()
        }
        Message::HistoryClearAllConfirmed => {
            state.history_clear_all_pending = false;
            state.history_delete_pending = None;
            if let Err(e) = state.coordinator.clear_history() {
                tracing::warn!(target: "readshot::history", "clear history failed: {e}");
                state.history_status = Some(friendly_history_error("clear the history", &e));
                return Task::none();
            }
            let n = state.history_records.len();
            state.history_records.clear();
            state.history_search.clear();
            state.history_selected_id = None;
            state.history_status = Some(format!(
                "Cleared {n} capture{}.",
                if n == 1 { "" } else { "s" }
            ));
            Task::none()
        }
        Message::HistoryOpenInEditor(id) => {
            state.history_delete_pending = None;
            let Some((path, record)) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| (history_png_path(root, r), r.clone()))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            Task::perform(
                async move { load_png_async(path).await.map(|img| (img, record)) },
                |r| Message::HistoryOpenInEditorReady(r.map_err(|e| e.to_string())),
            )
        }
        Message::HistoryOpenInEditorReady(result) => match result {
            Ok((image, record)) => {
                let editor = crate::editor::EditorSession::from_history(image, record);
                open_editor_window_replacing(state, editor, None)
            }
            Err(e) => {
                tracing::warn!(target: "readshot::history", "open in editor failed: {e}");
                state.history_status = Some(friendly_string_error_with(
                    "open the capture",
                    &e,
                    Some(RETRY_HINT),
                ));
                Task::none()
            }
        },
        Message::HistoryReveal(id) => {
            state.history_delete_pending = None;
            let Some(path) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| history_png_path(root, r))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            if let Err(e) = reveal_path(&path) {
                tracing::warn!(target: "readshot::history", "reveal failed: {e}");
                state.history_status = Some(friendly_string_error_with(
                    "reveal the file",
                    &e.to_string(),
                    Some(RETRY_HINT),
                ));
            }
            Task::none()
        }
        Message::HistoryCopyImage(id) => {
            state.history_delete_pending = None;
            let Some(path) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| history_png_path(root, r))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            Task::perform(
                async move {
                    let img = load_png_async(path).await.map_err(|e| e.to_string())?;
                    copy_image_to_clipboard(img)
                        .await
                        .map_err(|e| e.to_string())
                },
                Message::HistoryCopyImageDone,
            )
        }
        Message::HistoryCopyImageDone(result) => {
            state.history_status = Some(match result {
                Ok(()) => "Copied image to clipboard.".into(),
                Err(e) => {
                    tracing::warn!(target: "readshot::history", "copy image failed: {e}");
                    friendly_string_error("copy the image to the clipboard", &e)
                }
            });
            Task::none()
        }
        Message::HistoryCopyText(id) => {
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
                        Message::HistoryCopyTextDone,
                    )
                }
                _ => {
                    state.history_status = Some("No OCR text on this capture yet.".into());
                    Task::none()
                }
            }
        }
        Message::HistoryCopyTextDone(result) => {
            state.history_status = Some(match result {
                Ok(text) if text.is_empty() => "No OCR text on this capture yet.".into(),
                Ok(text) => {
                    let n = text.chars().count();
                    format!(
                        "Copied {n} character{} of text.",
                        if n == 1 { "" } else { "s" }
                    )
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::history", "copy text failed: {e}");
                    friendly_string_error_with("copy the text", &e, Some(RETRY_HINT))
                }
            });
            Task::none()
        }
        Message::HistoryCopyVisibleTextRequested => {
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
                Message::HistoryCopyVisibleTextDone,
            )
        }
        Message::HistoryCopyVisibleTextDone(result) => {
            state.history_status = Some(match result {
                Ok(count) => format!(
                    "Copied OCR text from {count} capture{}.",
                    if count == 1 { "" } else { "s" }
                ),
                Err(e) => {
                    tracing::warn!(target: "readshot::history", "copy visible text failed: {e}");
                    friendly_string_error_with("copy the visible text", &e, Some(RETRY_HINT))
                }
            });
            Task::none()
        }
        Message::HistoryPin(id) => {
            state.history_delete_pending = None;
            let Some(path) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| history_png_path(root, r))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            Task::perform(load_png_async(path), |r| {
                Message::HistoryPinReady(r.map_err(|e| e.to_string()))
            })
        }
        Message::HistoryPinReady(result) => match result {
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
                tracing::warn!(target: "readshot::history", "pin failed: {e}");
                state.history_status = Some(friendly_string_error_with(
                    "pin the capture",
                    &e,
                    Some(RETRY_HINT),
                ));
                Task::none()
            }
        },
        Message::HistoryDelete(id) => {
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
        Message::HistoryDeleteConfirmed(id) => delete_history_record(state, id),
        Message::HistoryDeleteCancelled => {
            state.history_delete_pending = None;
            Task::none()
        }
        Message::HistoryShowMore => {
            state.history_page_limit = state
                .history_page_limit
                .saturating_add(crate::app::HISTORY_PAGE_SIZE);
            Task::none()
        }
        other => unreachable!("non-history message routed to the history handler: {other:?}"),
    }
}

/// Actually remove a single capture from disk and refresh the list.
/// Clears the armed-delete guard first so the confirm button reverts
/// regardless of outcome.
fn delete_history_record(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    state.history_delete_pending = None;
    if let Err(e) = state.coordinator.delete_history(id) {
        tracing::warn!(target: "readshot::history", "delete failed: {e}");
        state.history_status = Some(friendly_history_error("delete this capture", &e));
        return Task::none();
    }
    // Reload the list so the deleted record disappears.
    history_list_task(state.coordinator.clone())
}

/// Short recovery next-steps appended to a friendly error so a failure
/// never leaves the user at a dead end. Kept as module consts so every
/// call site draws from the same small vocabulary.
pub(crate) const RETRY_HINT: &str = "Try again.";
pub(crate) const SAVE_HINT: &str = "Pick a different folder in Settings and try again.";
pub(crate) const CONNECTION_HINT: &str = "Check your connection and try again.";
pub(crate) const CAPTURE_PERMISSION_HINT: &str =
    "Check Screen Recording permission in System Settings and try again.";

/// Turn a typed [`HistoryError`] into a short, human-readable status
/// line. The raw error is logged separately via tracing; the user sees
/// plain language plus a hint (permission denied / not found) when the
/// underlying io error exposes one.
pub(crate) fn friendly_history_error(action: &str, e: &readshot_core::HistoryError) -> String {
    use readshot_core::HistoryError;
    let reason = match e {
        HistoryError::Io(msg) => io_reason_hint(msg),
        // Parse/serialise failures mean the on-disk history index is
        // damaged — surface that rather than a raw serde string.
        HistoryError::Parse(_) | HistoryError::Serialize(_) => {
            Some("the history data looks damaged")
        }
    };
    compose_friendly(action, reason, None)
}

/// Map a raw io-error string to a short human reason, when it carries a
/// recognisable one. `None` for anything unclassified (the caller then
/// falls back to a generic message + the log). Covers the common
/// `io::ErrorKind` cases a user can actually act on.
pub(crate) fn io_reason_hint(raw: &str) -> Option<&'static str> {
    let lower = raw.to_lowercase();
    if lower.contains("permission denied") {
        Some("permission denied")
    } else if lower.contains("not found") || lower.contains("no such file") {
        Some("the file was already gone")
    } else if lower.contains("no space") || lower.contains("disk full") {
        Some("the disk is full")
    } else if lower.contains("read-only") || lower.contains("read only") {
        Some("the location is read-only")
    } else if lower.contains("timed out") || lower.contains("timeout") {
        Some("it timed out")
    } else {
        None
    }
}

/// Friendly status for the string-typed errors that come back from the
/// clipboard / file-manager tasks (they've already been `to_string`d
/// at the task boundary). The raw error is logged by the caller; this
/// returns plain text with a hint where one is recognisable.
pub(crate) fn friendly_string_error(action: &str, raw: &str) -> String {
    friendly_string_error_with(action, raw, None)
}

/// As [`friendly_string_error`], with an optional recovery next-step
/// appended as a second sentence (e.g. "Try again.").
pub(crate) fn friendly_string_error_with(
    action: &str,
    raw: &str,
    next_step: Option<&str>,
) -> String {
    compose_friendly(action, io_reason_hint(raw), next_step)
}

/// Capture failures are the most visible errors in the app, so they get
/// a dedicated helper: a Screen Recording permission next-step when the
/// error looks permission-related (the common, fixable cause on macOS),
/// and a plain retry hint otherwise.
pub(crate) fn friendly_capture_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let next_step = if lower.contains("permission") || lower.contains("denied") {
        CAPTURE_PERMISSION_HINT
    } else {
        RETRY_HINT
    };
    friendly_string_error_with("capture the screen", raw, Some(next_step))
}

/// Assemble the final status line. With a recognised reason it reads
/// "Couldn't {action} — {reason}."; otherwise just "Couldn't {action}.".
/// A recovery next-step, when present, is appended as a second sentence
/// — and replaces the "See the log" tail, since the user now has a
/// concrete action (the full error still goes to the log).
fn compose_friendly(action: &str, reason: Option<&str>, next_step: Option<&str>) -> String {
    let mut msg = match reason {
        Some(reason) => format!("Couldn't {action} — {reason}."),
        None if next_step.is_some() => format!("Couldn't {action}."),
        None => format!("Couldn't {action}. See the log for details."),
    };
    if let Some(step) = next_step {
        msg.push(' ');
        msg.push_str(step);
    }
    msg
}

fn focus_history_search() -> Task<Message> {
    iced::widget::operation::focus(iced::widget::Id::new(
        crate::runtime::view::HISTORY_SEARCH_ID,
    ))
}
