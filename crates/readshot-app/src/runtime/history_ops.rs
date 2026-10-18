//! History-browser message handling, extracted from `runtime.rs`.
//! `use super::*` pulls in the runtime's full scope (helpers, iced
//! imports, sibling-module re-exports) so the moved arms compile
//! unchanged.

use super::*;

mod actions;
mod browse;
mod records;

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
        } => records::on_record_save_failed(state, session, record_id, error),
        Message::HistoryRecordPersisted(result) => records::on_record_persisted(state, result),

        Message::OpenHistoryRequested => browse::on_open_history(state),
        Message::HistoryWindowReady(id) => {
            // Window settings already register at open-time; the
            // ready callback just records the id in case iced hands
            // back a different one.
            state.history_window_id = Some(id);
            // Typing right away searches, like Spotlight or Finder.
            Task::batch([window::gain_focus(id), focus_history_search()])
        }
        Message::HistoryListLoaded(result) => records::on_list_loaded(state, result),
        Message::HistoryClosed => browse::on_history_closed(state),
        Message::HistoryStatusDismissed => {
            state.history_status = None;
            Task::none()
        }
        Message::HistorySearchChanged(q) => browse::on_search_changed(state, q),
        Message::HistorySelect(id) => browse::on_select(state, id),
        Message::HistorySelectPrevious => browse::on_select_previous(state),
        Message::HistorySelectNext => browse::on_select_next(state),
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
            browse::on_keyboard_shortcut(state, window_id, action)
        }
        Message::HistoryClearAllRequested => browse::on_clear_all_requested(state),
        Message::HistoryClearAllCancelled => {
            state.history_clear_all_pending = false;
            Task::none()
        }
        Message::HistoryClearAllConfirmed => browse::on_clear_all_confirmed(state),
        Message::HistoryOpenInEditor(id) => {
            state.history_delete_pending = None;
            open_history_record(state, id)
        }
        Message::HistoryReopenedAfterSync {
            record_id,
            result,
            unsaved,
        } => records::on_reopened_after_sync(state, record_id, result, unsaved),
        Message::HistoryReveal(id) => actions::on_reveal(state, id),
        Message::HistoryCopyImage(id) => actions::on_copy_image(state, id),
        Message::HistoryCopyImageDone(result) => actions::on_copy_image_done(state, result),
        Message::HistoryCopyText(id) => actions::on_copy_text(state, id),
        Message::HistoryCopyTextDone(result) => actions::on_copy_text_done(state, result),
        Message::HistoryCopyVisibleTextRequested => actions::on_copy_visible_text(state),
        Message::HistoryCopyVisibleTextDone(result) => {
            actions::on_copy_visible_text_done(state, result)
        }
        Message::HistoryPin(id) => actions::on_pin(state, id),
        Message::HistoryPinReady(result) => actions::on_pin_ready(state, result),
        Message::HistoryDelete(id) => actions::on_delete(state, id),
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

/// Run [`load_after_writes`] for record `id` off the UI thread; the answer
/// is `HistoryReopenedAfterSync`.
fn reopen_after_sync(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
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

/// The one way a History record opens in an editor: the row's Open
/// button, a double-click, Enter (`HistoryOpenSelected`) all send
/// `HistoryOpenInEditor`, which lands here; an unsaved-work prompt's
/// Discard goes on through [`begin_history_open`]. Nothing else reads
/// History (its cached list or its files) to open an editor.
///
/// - An open of `id` already under way is joined: a second click starts
///   nothing (and never a second, stale open).
/// - The open editor of `id` holding what History doesn't keep asks first.
/// - Otherwise [`begin_history_open`].
pub(crate) fn open_history_record(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    if state.awaiting_reopen == Some(id) {
        return Task::none();
    }
    if let Some(ed) = state
        .editor
        .as_mut()
        .filter(|ed| editor_of(ed, id) && has_state_history_drops(ed))
    {
        ed.arm_discard_prompt_then(
            "This window has changes History doesn’t keep (Beautify, or a crop not applied yet). Press Discard to drop them and reopen the capture from History, or keep editing.",
            crate::editor::AfterDiscard::Reopen(id),
        );
        return window_focus_of_editor(state);
    }
    begin_history_open(state, id, None)
}

fn editor_of(ed: &crate::editor::EditorSession, id: readshot_core::Uuid) -> bool {
    ed.source_record.as_ref().is_some_and(|r| r.id == id)
}

/// Open History record `id` (any earlier open waiting is superseded):
/// close (retire) its open editor, so its History edits are written; once
/// no editor of it is still finishing, wait off the UI thread until every
/// History write of it is done or cancelled, then read it fresh from disk
/// ([`load_after_writes`]); the answer, `HistoryReopenedAfterSync`, opens
/// it. History says "Finishing…" while there is something to wait for.
/// `note` names earlier edits that weren't saved (said when it opens).
pub(crate) fn begin_history_open(
    state: &mut App,
    id: readshot_core::Uuid,
    note: Option<String>,
) -> Task<Message> {
    let waits = state.editor.as_ref().is_some_and(|ed| editor_of(ed, id))
        || state.retiring_editors.iter().any(|ed| editor_of(ed, id))
        || state.coordinator.record_writes_outstanding(id);
    state.pending_history_open = Some(id);
    state.awaiting_reopen = Some(id);
    state.history_open_note = note;
    state.cancelled_history_open = None;
    if waits {
        state.history_status = Some("Finishing your previous edits to this capture…".into());
    }
    let mut tasks = Vec::new();
    if let Some(ed) = state.editor.take_if(|ed| editor_of(ed, id)) {
        if let Some(window) = ed.window_id {
            state.windows.forget(window);
            tasks.push(window::close(window));
        }
        state.release_editor_modifiers();
        tasks.push(super::compose_jobs::retire_editor(state, ed));
    }
    // Nothing left to wait for here (no editor of it, or it finished at
    // once and already started the load): start it.
    if state.pending_history_open == Some(id)
        && !state.retiring_editors.iter().any(|ed| editor_of(ed, id))
    {
        tasks.push(load_when_finished(state, id));
    }
    Task::batch(tasks)
}

/// The last editor of record `id` has finished: start the open waiting
/// for it, if there is one.
pub(crate) fn load_when_finished(state: &mut App, id: readshot_core::Uuid) -> Task<Message> {
    if state.pending_history_open != Some(id) {
        return Task::none();
    }
    state.pending_history_open = None;
    reopen_after_sync(state, id)
}

/// Another editor became current (a new capture) or History closed while
/// an open waited: it is cancelled, and the user is told why.
pub(crate) fn cancel_history_open(state: &mut App, why: &str) {
    let Some(id) = state.awaiting_reopen.take() else {
        return;
    };
    state.pending_history_open = None;
    let said = format!("Didn’t open the capture: {why}.");
    let note = state.history_open_note.take();
    show_open_status(state, format!("{said}{}", not_saved(&note)));
    state.cancelled_history_open = Some((id, said));
}

/// A status about a History open: in History when it is open, else the
/// capture status (menu bar, notification).
fn show_open_status(state: &mut App, message: String) {
    if state.history_window_id.is_some() {
        state.history_status = Some(message);
    } else {
        state.last_capture_status = Some(message);
    }
}

fn not_saved(reason: &Option<String>) -> String {
    reason.as_ref().map_or(String::new(), |r| {
        format!(" Your last edits to it weren’t all saved to History ({r}).")
    })
}

/// Open the editor on a record [`load_after_writes`] read, naming any
/// edits that weren't saved to History.
pub(crate) fn open_reopened(
    state: &mut App,
    loaded: (image::RgbaImage, readshot_core::CaptureRecord),
    unsaved: Option<String>,
) -> Task<Message> {
    let (image, record) = loaded;
    let mut editor = crate::editor::EditorSession::from_history(image, record);
    super::text_panel::seed_text_from_history(
        &mut editor,
        state.coordinator.engine_reports_positions(),
    );
    let opened = open_editor_window_replacing(state, editor, None);
    if let (Some(reason), Some(ed)) = (unsaved, state.editor.as_mut()) {
        ed.set_status(StatusTone::Warning, format!(
            "Your last edits in the previous window weren’t all saved to History ({reason}). This is the capture as History has it."
        ));
    }
    opened
}
