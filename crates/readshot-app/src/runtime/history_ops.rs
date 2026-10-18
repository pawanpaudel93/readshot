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
            if state.history_window_id.is_some() {
                return history_list_task(state.coordinator.clone());
            }
            Task::none()
        }
        Message::HistoryRecordSaveFailed {
            session,
            record_id,
            error,
        } => {
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
                Some(ed) => ed.set_warning(history_save_failure(&error, true)),
                None => state.last_capture_status = Some(history_save_failure(&error, false)),
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
                Err(e) => {
                    tracing::warn!(target: readshot_core::log::cat::HISTORY, "persist failed: {e}")
                }
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
            register_window(state, id, WindowKind::History);
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
        Message::HistoryClosed => {
            let id = state.history_window_id.take();
            state.history_records.clear();
            super::history_images::trim_history_image_cache();
            // A deferred open from it is cancelled (said on arrival).
            state.awaiting_reopen = None;
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
        Message::HistoryStatusDismissed => {
            state.history_status = None;
            Task::none()
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
            // ↓ past the last rendered row shows the next page, so Show
            // More has a keyboard path and the selection stays on screen.
            if let Some(id) = state.history_selected_id {
                let at = visible_history_ids(&state.history_records, &state.history_search)
                    .iter()
                    .position(|v| *v == id);
                if at.is_some_and(|i| i >= state.history_page_limit.max(1)) {
                    state.history_page_limit = state
                        .history_page_limit
                        .saturating_add(crate::app::HISTORY_PAGE_SIZE);
                }
            }
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
        Message::HistoryCapturedShortcut(window_id, action) => {
            if state.history_search.is_empty() {
                update(state, Message::HistoryKeyboardShortcut(window_id, action))
            } else {
                Task::none()
            }
        }
        Message::HistoryKeyboardShortcut(window_id, action) => {
            if state.history_window_id != Some(window_id) {
                return Task::none();
            }
            match action {
                // Moving through the list hands keyboard focus from the
                // search field to the list, so ⌘C / ⌘⌫ / ⌦ reach History.
                HistoryKeyboardAction::Previous => Task::batch([
                    update(state, Message::HistorySelectPrevious),
                    unfocus_history_search(),
                ]),
                HistoryKeyboardAction::Next => Task::batch([
                    update(state, Message::HistorySelectNext),
                    unfocus_history_search(),
                ]),
                HistoryKeyboardAction::Open => Task::batch([
                    update(state, Message::HistoryOpenSelected),
                    unfocus_history_search(),
                ]),
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
                HistoryKeyboardAction::Reveal => match state.history_selected_id {
                    Some(id) => update(state, Message::HistoryReveal(id)),
                    None => {
                        state.history_status = Some("No history capture selected.".into());
                        Task::none()
                    }
                },
                HistoryKeyboardAction::Pin => match state.history_selected_id {
                    Some(id) => update(state, Message::HistoryPin(id)),
                    None => {
                        state.history_status = Some("No history capture selected.".into());
                        Task::none()
                    }
                },
                HistoryKeyboardAction::CopyVisibleText => {
                    update(state, Message::HistoryCopyVisibleTextRequested)
                }
                HistoryKeyboardAction::ClearAll => {
                    if state.history_records.is_empty() {
                        Task::none()
                    } else if state.history_clear_all_pending {
                        update(state, Message::HistoryClearAllConfirmed)
                    } else {
                        update(state, Message::HistoryClearAllRequested)
                    }
                }
                // One esc, one step back: an armed prompt first, then the
                // search.
                HistoryKeyboardAction::Escape => {
                    if state.history_clear_all_pending {
                        update(state, Message::HistoryClearAllCancelled)
                    } else if state.history_delete_pending.is_some() {
                        update(state, Message::HistoryDeleteCancelled)
                    } else {
                        update(state, Message::HistorySearchChanged(String::new()))
                    }
                }
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
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "clear history failed: {e}");
                state.history_status = Some(friendly_history_error("clear the history", &e));
                return Task::none();
            }
            let n = state.history_records.len();
            state.history_records.clear();
            state.history_search.clear();
            state.history_selected_id = None;
            state.history_status = Some(format!(
                "Deleted {} permanently.",
                counted(n, "capture", "captures")
            ));
            Task::none()
        }
        Message::HistoryOpenInEditor(id) => {
            state.history_delete_pending = None;
            // An editor of this record (open, or closed but still
            // composing) has edits History may not have yet: they land
            // and are written first, then the record opens from disk with
            // them. Never dropped, never opened stale.
            let of_record = |ed: &crate::editor::EditorSession| {
                ed.source_record.as_ref().is_some_and(|r| r.id == id)
            };
            let open_here = state.editor.as_ref().is_some_and(of_record);
            if open_here || state.retiring_editors.iter().any(of_record) {
                // The open editor holds what History doesn't keep: ask
                // first (the unsaved-work prompt), then reopen.
                if let Some(ed) = state
                    .editor
                    .as_mut()
                    .filter(|ed| of_record(ed) && has_state_history_drops(ed))
                {
                    ed.arm_discard_prompt_then(
                        "This window has changes History doesn’t keep (Beautify, or a crop not applied yet). Press Discard to drop them and reopen the capture from History, or keep editing.",
                        crate::editor::AfterDiscard::Reopen(id),
                    );
                    return window_focus_of_editor(state);
                }
                return begin_reopen(state, id);
            }
            // Another record: any earlier wait is superseded.
            state.pending_history_open = None;
            state.awaiting_reopen = None;
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
        Message::HistoryReopenedAfterSync {
            record_id,
            result,
            unsaved,
        } => {
            let not_saved = |reason: &Option<String>| {
                reason.as_ref().map_or(String::new(), |r| {
                    format!(" Your last edits to it weren’t all saved to History ({r}).")
                })
            };
            if state.awaiting_reopen != Some(record_id) {
                // Cancelled while it waited: never open a window the user
                // walked away from.
                if state.history_window_id.is_none() {
                    state.last_capture_status = Some(format!(
                        "Didn’t open the capture: History was closed while your previous edits were being saved.{}",
                        not_saved(&unsaved)
                    ));
                }
                return Task::none();
            }
            state.awaiting_reopen = None;
            state.history_status = None;
            let loaded = match result {
                Ok(loaded) => loaded,
                Err(e) if e == REDACTION_NOT_SAVED => {
                    state.history_status = Some(format!(
                        "Couldn’t open the capture yet — History is still saving its redaction{}. Readshot keeps trying; open it again in a moment.",
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
            // Another editor became current meanwhile and has unsaved
            // work: never replace it silently; ask first.
            if let Some(ed) = state
                .editor
                .as_mut()
                .filter(|ed| super::editor_ops::editor_has_unsaved_work(ed))
            {
                let (image, record) = loaded;
                ed.arm_discard_prompt_then(
                    "The capture from History is ready, but this window has unsaved edits. Press Discard to close them and open it, or keep editing.",
                    crate::editor::AfterDiscard::Open(Box::new((image, record, unsaved))),
                );
                return Task::none();
            }
            open_reopened(state, loaded, unsaved)
        }
        Message::HistoryOpenInEditorReady(result) => match result {
            Ok((image, record)) => {
                let mut editor = crate::editor::EditorSession::from_history(image, record);
                super::text_panel::seed_text_from_history(
                    &mut editor,
                    state.coordinator.engine_reports_positions(),
                );
                open_editor_window_replacing(state, editor, None)
            }
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "open in editor failed: {e}");
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
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "reveal failed: {e}");
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
                    tracing::warn!(target: readshot_core::log::cat::HISTORY, "copy image failed: {e}");
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
        Message::HistoryCopyTextDone(result) => {
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
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "pin failed: {e}");
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
        tracing::warn!(target: readshot_core::log::cat::HISTORY, "delete failed: {e}");
        state.history_status = Some(friendly_history_error("delete this capture", &e));
        return Task::none();
    }
    // Reload the list so the deleted record disappears.
    history_list_task(state.coordinator.clone())
}

/// A redacted capture whose text was dropped on purpose
/// ([`readshot_core::history::text_allowed`]): not every redaction is
/// known, so no text is kept for it, now or later. History says so rather
/// than "No text yet", which would suggest waiting or retrying.
pub(crate) fn history_text_hidden(r: &readshot_core::CaptureRecord) -> bool {
    r.redacted && !readshot_core::history::text_allowed(r)
}

/// The detail pane's line for a record with no text, and the History
/// copy refusal.
pub(crate) const HIDDEN_TEXT_NOTE: &str =
    "Text hidden: this capture is redacted, so Readshot keeps no text for it.";
pub(crate) const HIDDEN_TEXT_LIST_LABEL: &str = "Text hidden (redacted)";

/// Short recovery next-steps appended to a friendly error so a failure
/// never leaves the user at a dead end. Kept as module consts so every
/// call site draws from the same small vocabulary.
pub(crate) const RETRY_HINT: &str = "Try again.";
/// For a record whose History folder is a link out of History.
pub(crate) const OUTSIDE_ROOT_HINT: &str =
    "Replace that link with a real folder (or move the folder back into History), then try again.";
pub(crate) const SAVE_HINT: &str = "Pick a different folder in Settings and try again.";
pub(crate) const CAPTURE_PERMISSION_HINT: &str =
    "Check Screen Recording permission in System Settings and try again.";
pub(crate) const REOPEN_HISTORY_HINT: &str = "Close History and open it again.";
pub(crate) const RESTART_HINT: &str = "Quit and reopen Readshot, then try again.";

/// The status for a capture History couldn't store: the reason when one
/// is recognisable, and the way to keep the image (in the editor, Save
/// with its key, from the app's key labels; elsewhere, capture again).
pub(crate) fn history_save_failure(raw: &str, in_editor: bool) -> String {
    let reason = io_reason_hint(raw);
    let fix = match reason {
        Some("the disk is full") => "Free some space",
        Some("permission denied" | "the location is read-only") => {
            "Make sure Readshot can write to its History folder"
        }
        _ => "Try again",
    };
    let next = if in_editor {
        format!(
            "{fix}, or press {} to save this image to a file.",
            super::keys::SAVE_KEY
        )
    } else {
        format!("{fix}, then capture again.")
    };
    compose_friendly("save the capture to History", reason, Some(&next))
}

/// Where the log file lives, for errors whose cause only the log knows
/// (mirrors `main.rs`), with the home folder shortened to `~`.
pub(crate) fn log_file_location() -> String {
    let Some(path) = directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")
        .map(|d| d.data_local_dir().join("readshot.log"))
    else {
        return "readshot.log in the temporary folder".to_string();
    };
    let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Turn a typed [`readshot_core::HistoryError`] into a short, human-readable status
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
            return compose_friendly(
                action,
                Some("the history data looks damaged"),
                Some(RESTART_HINT),
            );
        }
        // Retrying can't help: the folder must be fixed first.
        HistoryError::OutsideRoot(_) => {
            return compose_friendly(
                action,
                Some("its folder in History is a link to a folder outside History"),
                Some(OUTSIDE_ROOT_HINT),
            );
        }
    };
    compose_friendly(action, reason, Some(RETRY_HINT))
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
/// A recovery next-step is always appended as a second sentence ("Try
/// again." when the caller has none). With neither a reason nor a
/// caller's step, it also names the log file, the only place the cause
/// is recorded.
fn compose_friendly(action: &str, reason: Option<&str>, next_step: Option<&str>) -> String {
    let mut msg = match reason {
        Some(reason) => format!("Couldn’t {action} — {reason}."),
        None => format!("Couldn’t {action}."),
    };
    msg.push(' ');
    msg.push_str(next_step.unwrap_or(RETRY_HINT));
    if reason.is_none() && next_step.is_none() {
        msg.push_str(&format!(
            " If it keeps failing, see {}.",
            log_file_location()
        ));
    }
    msg
}

/// Drop keyboard focus from the search field. iced 0.14 exposes no
/// public unfocus task, but `focus` unfocuses every focusable whose id
/// doesn't match, so targeting an id nothing owns clears focus.
fn unfocus_history_search() -> Task<Message> {
    iced::widget::operation::focus(iced::widget::Id::new("history-list"))
}

fn focus_history_search() -> Task<Message> {
    iced::widget::operation::focus(iced::widget::Id::new(
        crate::runtime::view::HISTORY_SEARCH_ID,
    ))
}

/// Why [`load_after_writes`] didn't open a record: its newest redaction
/// isn't fully written to History yet (the write is retried).
pub(crate) const REDACTION_NOT_SAVED: &str = "redaction not saved to History yet";

/// Open History record `id` once every History write queued for it is
/// done ([`load_after_writes`]), off the UI thread; the answer is
/// `HistoryReopenedAfterSync`.
pub(crate) fn reopen_after_sync(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    let coord = state.coordinator.clone();
    let root = state.history_root.clone();
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || load_after_writes(&coord, root, id))
                .await
                .unwrap_or_else(|e| (Err(e.to_string()), None))
        },
        move |(result, unsaved)| Message::HistoryReopenedAfterSync {
            record_id: id,
            result,
            unsaved,
        },
    )
}

/// Read History record `id` and its image for an editor, once every
/// History write queued for it is done or cancelled
/// ([`crate::coordinator::CaptureCoordinator::settle_record_writes`]: none
/// of them lands after this read). Blocking, no timeout: run it off the UI
/// thread. The second value names writes that failed or were refused
/// (they never land now). A redaction whose unredacted image History
/// couldn't hide yet is never opened.
pub(crate) fn load_after_writes(
    coord: &crate::coordinator::CaptureCoordinator,
    root: Option<std::path::PathBuf>,
    id: readshot_core::Uuid,
) -> (
    Result<(image::RgbaImage, readshot_core::CaptureRecord), String>,
    Option<String>,
) {
    let settled = coord.settle_record_writes(id);
    let unsaved = (!settled.failures.is_empty()).then(|| settled.failures.join("; "));
    if settled.exposed || settled.redaction_due {
        return (Err(REDACTION_NOT_SAVED.to_string()), unsaved);
    }
    let loaded = (|| {
        let record = coord
            .history_list()
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| "the capture is no longer in History".to_string())?;
        let root = root.ok_or_else(|| "History isn’t available".to_string())?;
        let img = image::ImageReader::open(history_png_path(&root, &record))
            .map_err(|e| e.to_string())?
            .with_guessed_format()
            .map_err(|e| e.to_string())?
            .decode()
            .map_err(|e| e.to_string())?
            .to_rgba8();
        Ok((img, record))
    })();
    (loaded, unsaved)
}

/// State an editor holds that History doesn't store: Beautify settings
/// not saved anywhere yet, or a crop draft not applied.
fn has_state_history_drops(ed: &crate::editor::EditorSession) -> bool {
    ed.crop_draft.is_some()
        || (!ed.model.beautify().clamped().is_identity() && ed.has_output_changes())
}

fn window_focus_of_editor(state: &App) -> Task<Message> {
    state
        .editor
        .as_ref()
        .and_then(|ed| ed.window_id)
        .map_or_else(Task::none, window::gain_focus)
}

/// Reopen History record `id` from disk once every editor of it has
/// landed and written its edits: closes (retires) the open editor of it,
/// shows "Finishing…" in History, and opens when they are done
/// ([`reopen_after_sync`]).
pub(crate) fn begin_reopen(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    let of_record =
        |ed: &crate::editor::EditorSession| ed.source_record.as_ref().is_some_and(|r| r.id == id);
    state.pending_history_open = Some(id);
    state.awaiting_reopen = Some(id);
    state.history_status = Some("Finishing your previous edits to this capture…".into());
    let mut tasks = Vec::new();
    if let Some(ed) = state.editor.take_if(|ed| of_record(ed)) {
        if let Some(window) = ed.window_id {
            state.windows.forget(window);
            tasks.push(window::close(window));
        }
        state.release_editor_modifiers();
        tasks.push(super::compose_jobs::retire_editor(state, ed));
    }
    // Nothing left to wait for (no editor of it, or it finished at once
    // and already started the open): start it here.
    if state.pending_history_open == Some(id) && !state.retiring_editors.iter().any(of_record) {
        state.pending_history_open = None;
        tasks.push(reopen_after_sync(state, id));
    }
    Task::batch(tasks)
}

/// Open a capture reopened after a wait, naming any edits that weren't
/// saved to History.
pub(crate) fn open_reopened(
    state: &mut App,
    loaded: (image::RgbaImage, readshot_core::CaptureRecord),
    unsaved: Option<String>,
) -> Task<Message> {
    let opened = update(state, Message::HistoryOpenInEditorReady(Ok(loaded)));
    if let (Some(reason), Some(ed)) = (unsaved, state.editor.as_mut()) {
        ed.set_warning(format!(
            "Your last edits in the previous window weren’t all saved to History ({reason}). This is the capture as History has it."
        ));
    }
    opened
}
