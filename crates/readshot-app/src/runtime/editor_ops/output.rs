//! Editor output messages: Save, Copy Image, Copy Text, Pin and Discard.
//! Moved out of `handle_editor_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `EditorMsg::SaveRequested`: render the output and ask where to save it.
pub(super) fn on_save_requested(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    // The output must match what is on screen.
    settle_editor_for_output(ed, &state.coordinator);
    let out = match editor_output(ed) {
        Ok(out) => out,
        Err(e) => {
            ed.set_status(
                StatusTone::Error,
                crate::runtime::beautify_panel::beautify_refusal("save", e),
            );
            return Task::none();
        }
    };
    ed.busy = true;
    ed.set_status(StatusTone::Progress, "Choose a save location…");
    let session = ed.session_id;
    let seed = preferred_save_seed_dir(&state.preferences, &state.last_save_dir);
    let template = state.preferences.filename_template.clone();
    Task::perform(
        async move {
            let img = out.render_off_thread().await?;
            save_image_via_picker(img, seed, template)
                .await
                .map_err(|e| e.to_string())
        },
        move |result| Message::Editor(EditorMsg::Saved { session, result }),
    )
}

/// `EditorMsg::Saved`: the save picker's result.
pub(super) fn on_saved(
    state: &mut App,
    session: u64,
    result: Result<Option<std::path::PathBuf>, String>,
) -> Task<Message> {
    // Remember the parent directory of any successful save —
    // next save's picker will seed itself there instead of
    // bouncing back to ~/Desktop.
    if let Ok(Some(path)) = &result {
        if let Some(parent) = path.parent() {
            state.last_save_dir = Some(parent.to_path_buf());
        }
    }
    // Only the editor that saved: another one opened meanwhile
    // was never saved and must not be marked clean.
    if let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) {
        ed.busy = false;
        let saved = matches!(&result, Ok(Some(_)));
        if saved {
            ed.mark_output_clean();
        }
        let (tone, status) = match result {
            Ok(Some(p)) => (StatusTone::Success, format!("Saved to {}.", p.display())),
            Ok(None) => (StatusTone::Notice, "Save cancelled.".into()),
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::EDITOR, "save failed: {e}");
                (
                    StatusTone::Error,
                    crate::runtime::history_ops::friendly_string_error_with(
                        "save the image",
                        &e,
                        Some(crate::runtime::history_ops::SAVE_HINT),
                    ),
                )
            }
        };
        ed.set_status(tone, status);
    }
    Task::none()
}

/// `EditorMsg::CopyImageRequested`.
pub(super) fn on_copy_image_requested(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    // The output must match what is on screen.
    settle_editor_for_output(ed, &state.coordinator);
    let out = match editor_output(ed) {
        Ok(out) => out,
        Err(e) => {
            ed.set_status(
                StatusTone::Error,
                crate::runtime::beautify_panel::beautify_refusal("copy", e),
            );
            return Task::none();
        }
    };
    ed.busy = true;
    ed.set_status(
        StatusTone::Progress,
        if ed.model.beautify().is_identity() {
            "Copying…".to_string()
        } else {
            "Copying beautified image…".to_string()
        },
    );
    let session = ed.session_id;
    Task::perform(
        async move {
            let img = out.render_off_thread().await?;
            copy_image_to_clipboard(img)
                .await
                .map_err(|e| e.to_string())
        },
        move |result| Message::Editor(EditorMsg::CopyImageDone { session, result }),
    )
}

/// `EditorMsg::CopyImageDone`.
pub(super) fn on_copy_image_done(
    state: &mut App,
    session: u64,
    result: Result<(), String>,
) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) {
        ed.busy = false;
        let copied = result.is_ok();
        if copied {
            ed.mark_output_clean();
        }
        let (tone, status) = match result {
            Ok(()) => (StatusTone::Success, "Copied image to clipboard.".into()),
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::EDITOR, "copy image failed: {e}");
                (
                    StatusTone::Error,
                    crate::runtime::history_ops::friendly_string_error_with(
                        "copy to the clipboard",
                        &e,
                        Some(crate::runtime::history_ops::RETRY_HINT),
                    ),
                )
            }
        };
        ed.set_status(tone, status);
    }
    Task::none()
}

/// `EditorMsg::CopyTextRequested`: Copy All.
pub(super) fn on_copy_text_requested(state: &mut App) -> Task<Message> {
    let remove_breaks = state.preferences.editor_copy_remove_line_breaks;
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    // Previews first: one restored after the text commit would
    // drop the text (the `image_menu::prepare` order).
    cancel_editor_previews(ed);
    commit_pending_editor_text(ed, &state.coordinator);
    // Copy All (⇧⌘C, also the action bar's Copy Text):
    // the panel's recognized text only when it is ready and every
    // annotation is text-neutral (whitelist). Otherwise recognize
    // the output, so typed text is never dropped and redacted,
    // scribbled-over or cropped-away text never reaches the
    // clipboard. The pending draft was committed just above, so
    // it counts.
    // Any redaction (drawn now, or baked into a reopened scrubbed
    // record): the output is recognized and masked
    // (`copy_all_mask`), or nothing is copied when it can't be.
    let mask = copy_all_mask(ed);
    if matches!(mask, CopyAllMask::Withhold) {
        ed.set_status(
            StatusTone::Notice,
            crate::runtime::text_panel::COPY_TEXT_WITHHELD,
        );
        return Task::none();
    }
    let neutral = matches!(mask, CopyAllMask::None)
        && readshot_core::redact::panel_text_matches_output(ed.model.annotations());
    let cached = neutral
        .then(|| ed.text_panel.document())
        .flatten()
        .map(|doc| readshot_core::text_lines::copy_all(doc, remove_breaks));
    ed.busy = true;
    let session = ed.session_id;
    if let Some(text) = cached {
        ed.set_status(StatusTone::Progress, "Copying text…");
        return Task::perform(
            crate::runtime::text_panel::copy_cached_text(text),
            move |r| {
                Message::Editor(EditorMsg::CopyTextDone {
                    session,
                    result: r.map_err(|e| e.to_string()),
                })
            },
        );
    }
    ed.set_status(StatusTone::Progress, "Recognizing text…");
    let img = ed.model.flatten();
    let coord = state.coordinator.clone();
    let prefs = state.preferences.clone();
    Task::perform(
        ocr_then_copy(coord, img, prefs, remove_breaks, mask),
        move |r| {
            Message::Editor(EditorMsg::CopyTextDone {
                session,
                result: r.map_err(|e| e.to_string()),
            })
        },
    )
}

/// `EditorMsg::CopyTextDone`.
pub(super) fn on_copy_text_done(
    state: &mut App,
    session: u64,
    result: Result<String, String>,
) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) {
        ed.busy = false;
        let (tone, status) = match result {
            Err(e) if e == crate::runtime::text_panel::COPY_TEXT_WITHHELD => {
                (StatusTone::Notice, e)
            }
            Ok(text) if text.is_empty() => (StatusTone::Notice, "No text recognized.".into()),
            Ok(text) => (
                StatusTone::Success,
                format!(
                    "Copied {} of text.",
                    counted(text.chars().count(), "character", "characters")
                ),
            ),
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::EDITOR, "copy text failed: {e}");
                (
                    StatusTone::Error,
                    crate::runtime::history_ops::friendly_string_error_with(
                        "copy the text",
                        &e,
                        Some(crate::runtime::history_ops::RETRY_HINT),
                    ),
                )
            }
        };
        ed.set_status(tone, status);
    }
    Task::none()
}

/// `EditorMsg::DiscardRequested`: the two-stage Discard.
pub(super) fn on_discard_requested(state: &mut App) -> Task<Message> {
    if state.editor.as_ref().is_some_and(|ed| ed.busy) {
        return Task::none();
    }
    // Two-stage discard guards against accidental data loss.
    // Clean editors close immediately. Editors with committed
    // annotations or non-empty draft text require a second
    // click before closing; the arm waits for an answer.
    let dirty = state
        .editor
        .as_ref()
        .map(editor_has_unsaved_work)
        .unwrap_or(false);
    if dirty {
        if let Some(ed) = state.editor.as_mut() {
            let now = std::time::Instant::now();
            let armed = ed.discard_armed();
            if !armed {
                ed.arm_discard_prompt(now);
                return Task::none();
            }
        }
    }
    // A History reopen that asked first: confirmed now.
    let after = state.editor.as_mut().and_then(|ed| ed.after_discard.take());
    if let Some(crate::editor::AfterDiscard::Reopen(record)) = after {
        // Only what History doesn't keep is dropped: the editor's
        // History edits are written before the record reopens.
        return crate::runtime::history_ops::begin_history_open(state, record, None);
    }
    let id = state.editor.as_ref().and_then(|e| e.window_id);
    // A step still queued or composing is abandoned with it.
    if let Some(ed) = state.editor.take() {
        crate::runtime::compose_jobs::drop_editor(ed);
    }
    state.release_editor_modifiers();
    let close = if let Some(id) = id {
        state.windows.forget(id);
        window::close(id)
    } else {
        Task::none()
    };
    match after {
        Some(crate::editor::AfterDiscard::Open(record, unsaved)) => Task::batch([
            close,
            crate::runtime::history_ops::begin_history_open(state, record, unsaved),
        ]),
        _ => close,
    }
}

/// `EditorMsg::PinRequested`: the output opens as a pin and the editor closes.
pub(super) fn on_pin_requested(state: &mut App) -> Task<Message> {
    // Snapshot the editor's currently-flattened image, open
    // a fresh pin window with that image, then close the
    // editor. The pin lives independently from there on.
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    // The output must match what is on screen.
    settle_editor_for_output(ed, &state.coordinator);
    let out = match editor_output(ed) {
        Ok(out) => out,
        Err(e) => {
            ed.set_status(
                StatusTone::Error,
                crate::runtime::beautify_panel::beautify_refusal("pin", e),
            );
            return Task::none();
        }
    };
    let display_bounds = ed.source_display_bounds;
    // Close the editor window if any.
    let editor_id = ed.window_id;
    if let Some(ed) = state.editor.take() {
        crate::runtime::compose_jobs::drop_editor(ed);
    }
    state.release_editor_modifiers();
    let mut tasks: Vec<Task<Message>> = Vec::new();
    if let Some(id) = editor_id {
        state.windows.forget(id);
        tasks.push(window::close(id));
    }
    if out.is_identity() {
        // Nothing to render: the flattened image opens at once.
        if let Ok(img) = out.render() {
            tasks.push(open_pin_window(state, img, display_bounds));
        }
    } else {
        // A beautified canvas (up to the pixel budget) is filled
        // off the UI thread; the pin opens when it is ready.
        tasks.push(Task::perform(out.render_off_thread(), move |result| {
            Message::Editor(EditorMsg::PinRendered {
                result,
                display_bounds,
            })
        }));
    }
    Task::batch(tasks)
}
