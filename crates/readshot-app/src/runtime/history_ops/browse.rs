//! History window messages: opening, closing, search, selection, keys
//! and Clear All.
//! Moved out of `handle_history_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `HistoryMsg::OpenRequested`.
pub(super) fn on_open_history(state: &mut App) -> Task<Message> {
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
        open_task.map(|a| Message::History(HistoryMsg::WindowReady(a))),
        history_list_task(state.coordinator.clone()),
    ])
}

/// `HistoryMsg::Closed`.
pub(super) fn on_history_closed(state: &mut App) -> Task<Message> {
    let id = state.history_window_id.take();
    state.history_records.clear();
    crate::runtime::history_images::trim_history_image_cache();
    state.history_status = None;
    // An open from it that is still waiting is cancelled.
    cancel_history_open(state, "History was closed before it opened");
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

/// `HistoryMsg::SearchChanged`.
pub(super) fn on_search_changed(state: &mut App, q: String) -> Task<Message> {
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

/// `HistoryMsg::Select`.
pub(super) fn on_select(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    if state.history_delete_pending != Some(id) {
        state.history_delete_pending = None;
    }
    if history_record_visible(&state.history_records, &state.history_search, id) {
        state.history_selected_id = Some(id);
    }
    Task::none()
}

/// `HistoryMsg::SelectPrevious`.
pub(super) fn on_select_previous(state: &mut App) -> Task<Message> {
    state.history_delete_pending = None;
    state.history_selected_id = adjacent_history_selection(
        &state.history_records,
        &state.history_search,
        state.history_selected_id,
        -1,
    );
    Task::none()
}

/// `HistoryMsg::SelectNext`.
pub(super) fn on_select_next(state: &mut App) -> Task<Message> {
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

/// `HistoryMsg::KeyboardShortcut`.
pub(super) fn on_keyboard_shortcut(
    state: &mut App,
    window_id: iced::window::Id,
    action: HistoryKeyboardAction,
) -> Task<Message> {
    if state.history_window_id != Some(window_id) {
        return Task::none();
    }
    match action {
        // Moving through the list hands keyboard focus from the
        // search field to the list, so ⌘C / ⌘⌫ / ⌦ reach History.
        HistoryKeyboardAction::Previous => Task::batch([
            update(state, Message::History(HistoryMsg::SelectPrevious)),
            unfocus_history_search(),
        ]),
        HistoryKeyboardAction::Next => Task::batch([
            update(state, Message::History(HistoryMsg::SelectNext)),
            unfocus_history_search(),
        ]),
        HistoryKeyboardAction::Open => Task::batch([
            update(state, Message::History(HistoryMsg::OpenSelected)),
            unfocus_history_search(),
        ]),
        HistoryKeyboardAction::Delete => {
            update(state, Message::History(HistoryMsg::DeleteSelected))
        }
        HistoryKeyboardAction::CopyText => match state.history_selected_id {
            Some(id) => update(state, Message::History(HistoryMsg::CopyText(id))),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        HistoryKeyboardAction::CopyImage => match state.history_selected_id {
            Some(id) => update(state, Message::History(HistoryMsg::CopyImage(id))),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        HistoryKeyboardAction::FocusSearch => focus_history_search(),
        HistoryKeyboardAction::Reveal => match state.history_selected_id {
            Some(id) => update(state, Message::History(HistoryMsg::Reveal(id))),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        HistoryKeyboardAction::Pin => match state.history_selected_id {
            Some(id) => update(state, Message::History(HistoryMsg::Pin(id))),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        HistoryKeyboardAction::CopyVisibleText => update(
            state,
            Message::History(HistoryMsg::CopyVisibleTextRequested),
        ),
        HistoryKeyboardAction::ClearAll => {
            if state.history_records.is_empty() {
                Task::none()
            } else if state.history_clear_all_pending {
                update(state, Message::History(HistoryMsg::ClearAllConfirmed))
            } else {
                update(state, Message::History(HistoryMsg::ClearAllRequested))
            }
        }
        // One esc, one step back: an armed prompt first, then the
        // search.
        HistoryKeyboardAction::Escape => {
            if state.history_clear_all_pending {
                update(state, Message::History(HistoryMsg::ClearAllCancelled))
            } else if state.history_delete_pending.is_some() {
                update(state, Message::History(HistoryMsg::DeleteCancelled))
            } else {
                update(
                    state,
                    Message::History(HistoryMsg::SearchChanged(String::new())),
                )
            }
        }
    }
}

/// `HistoryMsg::ClearAllRequested`.
pub(super) fn on_clear_all_requested(state: &mut App) -> Task<Message> {
    // First click arms the destructive prompt. The button
    // flips to "Confirm Clear · Cancel" so the user has a
    // visible second step before everything is wiped. Arming
    // Clear All also cancels any pending single-row delete.
    state.history_delete_pending = None;
    state.history_clear_all_pending = true;
    Task::none()
}

/// `HistoryMsg::ClearAllConfirmed`.
pub(super) fn on_clear_all_confirmed(state: &mut App) -> Task<Message> {
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
