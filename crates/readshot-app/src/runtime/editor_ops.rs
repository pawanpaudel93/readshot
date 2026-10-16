// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use crate::coordinator::CaptureCoordinator;

/// Apply a committed annotation to the editor session. Crop is
/// stored as a regular `Annotation` rather than baked into the base
/// image, so undo / redo work uniformly: the renderer translates
/// every other annotation by the crop offset, and removing the Crop
/// annotation (via undo) brings the full pre-crop image back. The
/// canvas's cursor mapping consults `effective_image_size` +
/// `crop_offset` so clicks after a crop still hit the right base
/// pixel.
pub(crate) fn handle_commit_annotation(
    ed: &mut crate::editor::EditorSession,
    annotation: readshot_core::Annotation,
) {
    if matches!(annotation, readshot_core::Annotation::NumberedPin { .. }) {
        ed.next_pin_number = ed.next_pin_number.saturating_add(1);
    }
    let cropped = matches!(annotation, readshot_core::Annotation::Crop { .. });
    ed.model.commit_annotation(annotation);
    ed.refresh_image();
    if cropped {
        let (w, h) = ed.effective_image_size();
        ed.set_status(format!(
            "Crop selected at {w} × {h}px. Drag handles to adjust; Delete or ⌘Z cancels."
        ));
    }
}

pub(crate) fn editor_selected_hint(kind: &str, text_editable: bool) -> String {
    match (kind, text_editable) {
        ("Crop", _) => {
            "Selected Crop — drag frame or handles · arrows nudge · Delete restores full image"
                .to_string()
        }
        (_, true) => {
            format!(
                "Selected {kind} — edit inline · color/size apply here · arrows nudge · ⌫ removes"
            )
        }
        _ => {
            format!(
                "Selected {kind} — drag to move · arrows nudge · handles resize · drag elsewhere to draw"
            )
        }
    }
}

/// Returns whether the color was applied to a selection (an undo step).
pub(crate) fn apply_editor_color(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
    color: readshot_core::Rgba,
) -> bool {
    cancel_editor_previews(ed);
    ed.model.set_color(color);
    if ed.model.apply_color_to_selected(color) {
        ed.refresh_image();
        ed.set_status("Updated selected annotation color. ⌘Z to undo.");
        sync_editor_history(ed, coord);
        return true;
    }
    false
}

pub(crate) fn apply_editor_line_width(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
    width: f32,
) {
    cancel_editor_previews(ed);
    ed.model.set_line_width(width);
    if ed.model.apply_line_width_to_selected(width) {
        ed.refresh_image();
        ed.set_status("Updated selected annotation size. ⌘Z to undo.");
        sync_editor_history(ed, coord);
    }
}

pub(crate) fn preview_editor_line_width(ed: &mut crate::editor::EditorSession, width: f32) {
    ed.model.set_line_width(width);
    if ed.model.selected_annotation().is_none() {
        ed.width_drag_baseline = None;
        return;
    }
    ed.clear_discard_confirmation();
    let baseline = ed
        .width_drag_baseline
        .get_or_insert_with(|| ed.model.annotations().to_vec())
        .clone();
    ed.model.preview_line_width_selected_from(&baseline, width);
    ed.refresh_image();
}

pub(crate) fn apply_editor_style(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
    msg: readshot_ui::editor::StyleMessage,
) {
    cancel_editor_previews(ed);
    ed.model.set_style(msg);
    if ed.model.apply_style_to_selected(msg) {
        ed.refresh_image();
        ed.set_status("Updated selected annotation. ⌘Z to undo.");
        sync_editor_history(ed, coord);
    }
}

/// The text size `[` / `]` step from: the selected text's px, or the
/// tool's text size when the Text tool is active with nothing selected.
/// `None` means the keys bump a width instead.
pub(crate) fn bump_text_px(model: &readshot_ui::editor::EditorState) -> Option<f32> {
    match model
        .selected_annotation()
        .and_then(|i| model.annotations().get(i))
    {
        Some(readshot_core::Annotation::Text { size, .. }) => Some(*size),
        Some(_) => None,
        None if model.active_tool() == readshot_ui::editor::ToolState::Text => {
            Some(model.tool_style().text_size.px())
        }
        None => None,
    }
}

pub(crate) fn preview_editor_style(
    ed: &mut crate::editor::EditorSession,
    msg: readshot_ui::editor::StyleMessage,
) {
    // Snapshot once per slider drag: the tool style always, and the
    // annotations when something is selected.
    if ed.style_drag_baseline.is_none() {
        ed.style_drag_baseline = Some(crate::editor::StyleDrag {
            annotations: ed
                .model
                .selected_annotation()
                .map(|_| ed.model.annotations().to_vec()),
            tool_style: ed.model.tool_style(),
        });
    }
    ed.model.set_style(msg);
    let baseline = ed
        .style_drag_baseline
        .as_ref()
        .and_then(|drag| drag.annotations.clone());
    if let Some(baseline) = baseline {
        ed.clear_discard_confirmation();
        ed.model.preview_style_selected_from(&baseline, msg);
        ed.refresh_image();
    }
}

pub(crate) fn commit_pending_editor_text(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) -> bool {
    let Some(pending) = ed.pending_text.take() else {
        return false;
    };
    // Drop the inline editor's backing store together with the draft so
    // the two never disagree once the draft is committed/dismissed.
    ed.text_edit_content = None;
    let trimmed = pending.content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Some(edit_index) = pending.edit_index {
        if ed.model.replace_text_at(edit_index, trimmed.to_string()) {
            ed.refresh_image();
            ed.set_status("Updated text. ⌘Z to undo.");
            sync_editor_history(ed, coord);
            return true;
        }
        return false;
    }

    let style = ed.model.tool_style();
    let annotation = readshot_core::Annotation::Text {
        content: trimmed.to_string(),
        origin: pending.origin,
        color: ed.model.current_color(),
        font_family: "system-ui".to_string(),
        size: style.text_size.px(),
        style: style.text_style(),
    };
    ed.model.commit_annotation(annotation);
    ed.refresh_image();
    ed.set_status("Added text. ⌘Z to undo.");
    sync_editor_history(ed, coord);
    true
}

pub(crate) fn editor_has_unsaved_work(ed: &crate::editor::EditorSession) -> bool {
    ed.has_output_changes() || pending_text_has_unsaved_work(ed)
}

pub(crate) fn pending_text_has_unsaved_work(ed: &crate::editor::EditorSession) -> bool {
    let Some(pending) = ed.pending_text.as_ref() else {
        return false;
    };
    let trimmed = pending.content.trim();
    let Some(edit_index) = pending.edit_index else {
        return !trimmed.is_empty();
    };
    match ed.model.annotations().get(edit_index) {
        Some(readshot_core::Annotation::Text { content, .. }) => content != trimmed,
        _ => !trimmed.is_empty(),
    }
}

pub(crate) fn editor_text_commit_label(is_editing: bool) -> &'static str {
    if is_editing {
        "Update"
    } else {
        "Add"
    }
}

pub(crate) fn cancel_editor_previews(ed: &mut crate::editor::EditorSession) {
    let mut refresh = false;
    if let Some(drag) = ed.move_drag.take() {
        let restore_image_handle =
            drag.moved && can_preview_drag_annotation(&drag.baseline, drag.selected_index);
        ed.model.cancel_preview_from_baseline(drag.baseline);
        refresh |= restore_image_handle;
    }
    if let Some(baseline) = ed.width_drag_baseline.take() {
        ed.model.cancel_preview_from_baseline(baseline);
        refresh = true;
    }
    if let Some(drag) = ed.style_drag_baseline.take() {
        ed.model.set_tool_style(drag.tool_style);
        if let Some(baseline) = drag.annotations {
            ed.model.cancel_preview_from_baseline(baseline);
            refresh = true;
        }
    }
    if refresh {
        ed.refresh_image();
    }
}

/// Close the open popover, if any; returns whether one was open. A
/// line-width slider preview still live in the Width popover is
/// cancelled (Esc semantics), so its baseline can't linger and revert
/// the width later.
pub(crate) fn close_editor_popover(ed: &mut crate::editor::EditorSession) -> bool {
    let Some(was) = ed.popover.take() else {
        return false;
    };
    if was == crate::editor::EditorPopover::Width {
        if let Some(baseline) = ed.width_drag_baseline.take() {
            ed.model.cancel_preview_from_baseline(baseline);
            ed.refresh_image();
        }
    }
    true
}

pub(crate) fn sync_editor_history(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) {
    ed.clear_discard_confirmation();
    // Every history change passes here. A redaction's toast Undo is
    // only valid while that redaction is the newest step: once anything
    // else changes history, drop it for good (depth alone could match
    // again after undo + a different edit, and Undo would remove that).
    if let Some(crate::editor::EditorToastAction::UndoRedaction { undo_depth }) = ed.toast_action {
        if undo_depth != ed.model.undo_depth() {
            ed.toast_action = None;
        }
    }
    write_editor_history(ed, coord);
}

/// Write the editor's state to its History record, if it has one.
///
/// - Any redaction ([`readshot_core::scrub::has_redaction`]): **scrub**.
///   History gets the flattened output as its image (rendered and
///   encoded on the writer thread) and the text with every hidden word
///   masked; the annotations are baked in, so the sidecar stores none.
/// - No redaction left after a scrub (all undone): **restore** the
///   original image and text, both still in memory.
/// - Otherwise the plain sidecar sync (annotations only).
///
/// Everything is queued on the coordinator's ordered background writer
/// (the writes fsync, and this runs on every commit/undo/redo); rapid
/// rewrites coalesce to the newest. A rewrite's outcome comes back as
/// `Message::EditorHistoryWritten` via [`history_write_followups`].
pub(crate) fn write_editor_history(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) {
    let Some(record) = ed.source_record.as_mut() else {
        return;
    };
    record.annotation_model = ed.model.annotations().to_vec();
    let record = record.clone();
    let annotations = ed.model.annotations();
    if readshot_core::scrub::has_redaction(annotations) {
        let mut scrubbed = record;
        scrubbed.ocr_text = scrubbed_history_text(ed, coord.engine_reports_positions());
        scrubbed.annotation_model = Vec::new();
        scrubbed.redacted = true;
        let bake = annotations.to_vec();
        ed.history_scrubbed = true;
        ed.history_write =
            Some(coord.rewrite_history_async(scrubbed, ed.model.shared_base(), bake));
    } else if ed.history_scrubbed {
        let mut restored = record;
        restored.ocr_text = original_history_text(ed);
        ed.history_scrubbed = false;
        ed.history_write =
            Some(coord.rewrite_history_async(restored, ed.model.shared_base(), Vec::new()));
    } else {
        coord.update_history_async(record);
    }
}

/// The capture's unredacted text: what History held when the editor
/// opened, or (a fresh capture, whose text lands later) the Text panel's
/// recognition of the same base image.
fn original_history_text(ed: &crate::editor::EditorSession) -> Option<String> {
    ed.source_record
        .as_ref()
        .and_then(|r| r.ocr_text.clone())
        .or_else(|| ed.text_panel.document().map(|d| d.layout_text.clone()))
}

/// The text History keeps while redactions are present: the recognized
/// text with every hidden word masked, using the Text panel's document
/// (word boxes) when it has positions.
///
/// Without a positioned document:
/// - an engine that reports positions (Apple Vision) will produce one, so
///   store **no** text for now (fail closed) — masking only the detected
///   items would keep a hand-redacted name readable. The precise text is
///   stored when recognition lands, editor open or not
///   (`text_panel::store_recognized_text`);
/// - an engine without positions never will, so mask every detected item
///   in the known text (the documented fallback).
///
/// `None` also when no text is known at all.
fn scrubbed_history_text(
    ed: &crate::editor::EditorSession,
    engine_reports_positions: bool,
) -> Option<String> {
    use readshot_core::scrub::{hidden_spans, masked_layout_text};
    use readshot_core::text_lines::TextDocument;
    let annotations = ed.model.annotations();
    let mask = |doc: &TextDocument| masked_layout_text(doc, &hidden_spans(doc, annotations));
    if let Some(doc) = ed.text_panel.positioned_document() {
        return Some(mask(doc));
    }
    if engine_reports_positions {
        return None;
    }
    match ed.text_panel.document() {
        Some(doc) => Some(mask(doc)),
        None => original_history_text(ed).map(|t| mask(&TextDocument::from_plain(&t))),
    }
}

/// Follow-ups after a message that may have written History: await the
/// newest rewrite's outcome, and when a redaction exists but the panel
/// has no recognition (never ran, or failed) and the engine reports
/// positions, start recognition. Its result is stored in History by the
/// task itself, so the precise masked text lands even if the editor
/// closes first.
pub(crate) fn history_write_followups(state: &mut App) -> Task<Message> {
    let positions = state.coordinator.engine_reports_positions();
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    let mut tasks = Vec::new();
    let wrote = ed.history_write.is_some();
    if let Some(rx) = ed.history_write.take() {
        let session = ed.session_id;
        tasks.push(Task::perform(rx, move |r| Message::EditorHistoryWritten {
            session,
            result: r.ok(),
        }));
    }
    // Idle: never recognized. Failed: retried whenever a scrub is
    // written, so a transient failure can't leave the text unmasked-or-
    // missing for good.
    let recognition_due = match ed.text_panel.recognition {
        crate::editor::TextRecognition::Idle => true,
        crate::editor::TextRecognition::Failed(_) => wrote,
        _ => false,
    };
    let needs_text = positions
        && ed.source_record.is_some()
        && recognition_due
        && readshot_core::scrub::has_redaction(ed.model.annotations());
    if needs_text {
        tasks.push(super::text_panel::request_text_recognition(state));
    }
    Task::batch(tasks)
}

/// Every editor message arm, extracted from `runtime::update` so the
/// top-level dispatcher stays navigable. Covers the
/// editor session, its keyboard/canvas/toolbar flow, and the pin
/// windows it spawns. Routed from `update`'s
/// grouped arm; the trailing `unreachable!` only fires if a
/// non-editor message is mis-routed here.
pub(crate) fn handle_editor_message(state: &mut App, message: Message) -> Task<Message> {
    // An editor message is the only thing that can change what the
    // annotation canvas paints, so invalidate its geometry cache here.
    // The high-frequency background ticks (hotkey / tray / url drains)
    // are handled elsewhere and never reach this function, so the
    // canvas reuses cached geometry on those idle redraws instead of
    // re-tessellating every annotation ~20×/sec. Over-clearing (e.g. on
    // a Save result) is harmless — it only costs one extra tessellation
    // on the next draw.
    if let Some(ed) = state.editor.as_ref() {
        ed.canvas_cache.clear();
    }
    match message {
        Message::EditorKeyPressed {
            window,
            key,
            modifiers,
            status_ignored,
        } => {
            let editor_window = state.editor.as_ref().and_then(|ed| ed.window_id);
            if editor_window != Some(window) {
                return Task::none();
            }
            // An open popover owns Escape. In the running editor the
            // annotation canvas captures every Escape, so it arrives with
            // `!status_ignored` and `editor_key_message` maps it to
            // `EditorTextCancel`, which closes the popover first. This
            // branch only covers an Escape no widget captured, closing the
            // popover before it can fall through to discard.
            if status_ignored
                && matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                )
            {
                if let Some(ed) = state.editor.as_mut() {
                    if close_editor_popover(ed) || super::text_panel::close_panel_overlay(ed) {
                        return Task::none();
                    }
                }
            }
            // Fallback for an Escape no widget captured during a live
            // shape/freehand drag: bump the cancel sequence so the canvas
            // drops the drag on its next event instead of Escape reaching
            // the discard flow. (Normally the canvas captures Escape itself
            // and publishes `Cancelled`.)
            if status_ignored
                && matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                )
            {
                if let Some(ed) = state.editor.as_mut() {
                    if ed.canvas_drag_active {
                        ed.canvas_drag_active = false;
                        ed.drag_cancel_seq = ed.drag_cancel_seq.wrapping_add(1);
                        cancel_editor_previews(ed);
                        return Task::none();
                    }
                }
            }
            let message = editor_key_message(key, modifiers, status_ignored);
            // While the Color popover is open its hex field may have focus,
            // and an inline text draft owns its own field. iced's text_input
            // has no undo and doesn't capture Cmd+Z, so undo/redo would
            // silently rewind the drawing under the popover, or commit the
            // draft and then undo it (the typed text vanishes).
            if matches!(
                message,
                Some(Message::EditorToolbar(
                    readshot_ui::ToolbarMessage::Undo | readshot_ui::ToolbarMessage::Redo
                ))
            ) && state.editor.as_ref().is_some_and(|ed| {
                ed.popover == Some(crate::editor::EditorPopover::Color) || ed.pending_text.is_some()
            }) {
                return Task::none();
            }
            match message {
                Some(message) => update(state, message),
                None => Task::none(),
            }
        }
        Message::EditorStatusTick => {
            // Auto-dismiss stale, non-in-progress status pills *here* —
            // the authoritative place — so the `EditorStatusTick`
            // subscription (gated on `status.is_some() &&
            // !status_is_in_progress()`) actually stops firing once a
            // toast has expired. Previously the dismissal only happened
            // locally in `editor_view`, which can't clear `ed.status`, so
            // the 1 Hz tick + redraw ran for the entire life of the
            // editor window. Also expire stale "Discard? Click again"
            // arms so the editor doesn't keep listening for a confirm
            // that the user has already walked away from. The armed
            // toast's Keep Editing / Discard buttons expire with the arm
            // on purpose: the press-twice confirm is time-boxed.
            if let Some(ed) = state.editor.as_mut() {
                if !ed.status_is_in_progress()
                    && !ed.status_is_error()
                    && ed
                        .status_set_at
                        .is_some_and(|t| t.elapsed() > crate::editor::STATUS_AUTO_DISMISS)
                {
                    ed.status = None;
                    ed.status_set_at = None;
                    ed.toast_action = None;
                }
                if let Some(t) = ed.discard_pending_at {
                    if t.elapsed() > crate::editor::DISCARD_CONFIRM_WINDOW {
                        ed.discard_pending_at = None;
                    }
                }
            }
            Task::none()
        }
        Message::EditorPopoverToggled(which) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.popover == Some(which) {
                    close_editor_popover(ed);
                } else {
                    close_editor_popover(ed);
                    ed.popover = Some(which);
                    if which == crate::editor::EditorPopover::Color {
                        ed.hex_draft = ed.model.effective_color().to_hex();
                    }
                }
            }
            Task::none()
        }
        Message::EditorPopoverClosed => {
            if let Some(ed) = state.editor.as_mut() {
                close_editor_popover(ed);
            }
            Task::none()
        }
        Message::EditorWindowReady(id) => {
            if let Some(ed) = &mut state.editor {
                ed.window_id = Some(id);
            }
            // The OS can open the window smaller than requested on small
            // screens; the real width decides dock vs collapse.
            window::size(id).map(move |size| Message::EditorWindowResized(id, size.width))
        }

        Message::EditorSaveRequested => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            commit_pending_editor_text(ed, &state.coordinator);
            cancel_editor_previews(ed);
            ed.busy = true;
            ed.set_status("Choose a save location…");
            let img = editor_output_image(ed);
            let seed = preferred_save_seed_dir(&state.preferences, &state.last_save_dir);
            let template = state.preferences.filename_template.clone();
            Task::perform(save_image_via_picker(img, seed, template), |r| {
                Message::EditorSaved(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorSaved(result) => {
            // Remember the parent directory of any successful save —
            // next save's picker will seed itself there instead of
            // bouncing back to ~/Desktop.
            if let Ok(Some(path)) = &result {
                if let Some(parent) = path.parent() {
                    state.last_save_dir = Some(parent.to_path_buf());
                }
            }
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                let saved = matches!(&result, Ok(Some(_)));
                if saved {
                    ed.mark_output_clean();
                }
                ed.set_status(match result {
                    Ok(Some(p)) => format!("Saved to {}.", p.display()),
                    Ok(None) => "Save cancelled.".into(),
                    Err(e) => {
                        tracing::warn!(target: "readshot::editor", "save failed: {e}");
                        super::history_ops::friendly_string_error_with(
                            "save the image",
                            &e,
                            Some(super::history_ops::SAVE_HINT),
                        )
                    }
                });
            }
            Task::none()
        }

        Message::EditorCopyImageRequested => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            commit_pending_editor_text(ed, &state.coordinator);
            cancel_editor_previews(ed);
            ed.busy = true;
            ed.set_status(if ed.model.beautify().is_identity() {
                "Copying…".to_string()
            } else {
                "Copying beautified image…".to_string()
            });
            let img = editor_output_image(ed);
            Task::perform(copy_image_to_clipboard(img), |r| {
                Message::EditorCopyImageDone(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyImageDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                let copied = result.is_ok();
                if copied {
                    ed.mark_output_clean();
                }
                ed.set_status(match result {
                    Ok(()) => "Copied image to clipboard.".into(),
                    Err(e) => {
                        tracing::warn!(target: "readshot::editor", "copy image failed: {e}");
                        super::history_ops::friendly_string_error_with(
                            "copy to the clipboard",
                            &e,
                            Some(super::history_ops::RETRY_HINT),
                        )
                    }
                });
            }
            Task::none()
        }

        Message::EditorBeautifyPreset(preset) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                cancel_editor_previews(ed);
                ed.clear_discard_confirmation();
                close_editor_popover(ed);
                if ed.model.set_beautify(preset.settings()) {
                    ed.set_status(match preset {
                        readshot_core::beautify::BuiltinPreset::None => {
                            "Frame removed. ⌘Z to undo.".to_string()
                        }
                        p => format!("Frame set to {}. ⌘Z to undo.", p.label()),
                    });
                }
            }
            Task::none()
        }

        Message::EditorCopyTextRequested => {
            let remove_breaks = state.preferences.editor_copy_remove_line_breaks;
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            commit_pending_editor_text(ed, &state.coordinator);
            cancel_editor_previews(ed);
            // Copy All (spec §3.7, ⇧⌘C, also the action bar's Copy Text):
            // the panel's recognized text only when it is ready and every
            // annotation is text-neutral (whitelist). Otherwise recognize
            // the output, so typed text is never dropped and redacted,
            // scribbled-over or cropped-away text never reaches the
            // clipboard. The pending draft was committed just above, so
            // it counts.
            let neutral = readshot_core::redact::panel_text_matches_output(ed.model.annotations());
            let cached = neutral
                .then(|| ed.text_panel.document())
                .flatten()
                .map(|doc| readshot_core::text_lines::copy_all(doc, remove_breaks));
            ed.busy = true;
            if let Some(text) = cached {
                ed.set_status("Copying text…");
                return Task::perform(super::text_panel::copy_cached_text(text), |r| {
                    Message::EditorCopyTextDone(r.map_err(|e| e.to_string()))
                });
            }
            ed.set_status("Recognizing text…");
            let img = ed.model.flatten();
            let coord = state.coordinator.clone();
            let prefs = state.preferences.clone();
            Task::perform(ocr_then_copy(coord, img, prefs, remove_breaks), |r| {
                Message::EditorCopyTextDone(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyTextDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                ed.set_status(match result {
                    Ok(text) if text.is_empty() => "No text recognized.".into(),
                    Ok(text) => format!(
                        "Copied {} character{} of text.",
                        text.chars().count(),
                        if text.chars().count() == 1 { "" } else { "s" },
                    ),
                    Err(e) => {
                        tracing::warn!(target: "readshot::editor", "copy text failed: {e}");
                        super::history_ops::friendly_string_error_with(
                            "copy the text",
                            &e,
                            Some(super::history_ops::RETRY_HINT),
                        )
                    }
                });
            }
            Task::none()
        }

        Message::EditorDiscardDisarmed => {
            if let Some(ed) = state.editor.as_mut() {
                ed.discard_pending_at = None;
                ed.status = None;
                ed.status_set_at = None;
                ed.toast_action = None;
            }
            Task::none()
        }
        Message::EditorStatusDismissed => {
            if let Some(ed) = state.editor.as_mut() {
                ed.status = None;
                ed.status_set_at = None;
                ed.toast_action = None;
            }
            Task::none()
        }

        Message::EditorDiscardRequested => {
            if state.editor.as_ref().is_some_and(|ed| ed.busy) {
                return Task::none();
            }
            // Two-stage discard guards against accidental data loss.
            // Clean editors close immediately. Editors with committed
            // annotations or non-empty draft text require a second
            // click within `DISCARD_CONFIRM_WINDOW` before closing.
            let dirty = state
                .editor
                .as_ref()
                .map(editor_has_unsaved_work)
                .unwrap_or(false);
            if dirty {
                if let Some(ed) = state.editor.as_mut() {
                    let now = std::time::Instant::now();
                    let armed = ed
                        .discard_pending_at
                        .map(|t| now.duration_since(t) < crate::editor::DISCARD_CONFIRM_WINDOW)
                        .unwrap_or(false);
                    if !armed {
                        ed.discard_pending_at = Some(now);
                        // The armed prompt replaces the toast; its old
                        // button (e.g. a redaction's Undo) must not come
                        // back under this text once the arm expires.
                        ed.toast_action = None;
                        ed.status = Some(
                            "Unsaved edits. Press Discard again to close without saving.".into(),
                        );
                        ed.status_set_at = Some(now);
                        return Task::none();
                    }
                }
            }
            let id = state.editor.as_ref().and_then(|e| e.window_id);
            state.editor = None;
            state.release_editor_modifiers();
            if let Some(id) = id {
                state.windows.forget(id);
                window::close(id)
            } else {
                Task::none()
            }
        }

        // Toolbar selections — tool / colour / line-width changes
        // and undo/redo. Tool/colour/width changes don't need a
        // re-render (annotations haven't changed), but undo/redo do.
        Message::EditorToolbar(msg) => {
            let selected_tool = match msg {
                readshot_ui::ToolbarMessage::SelectTool(t) => Some(t),
                _ => None,
            };
            let picked_color = match msg {
                readshot_ui::ToolbarMessage::SelectColor(c) => Some(c),
                _ => None,
            };
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            match msg {
                readshot_ui::ToolbarMessage::SelectTool(t) => {
                    cancel_editor_previews(ed);
                    ed.eyedropper = None;
                    if t != readshot_ui::editor::ToolState::Text {
                        commit_pending_editor_text(ed, &state.coordinator);
                    }
                    ed.model.set_tool(t);
                    // macOS prewarms the emoji cache at startup (main).
                    // Elsewhere the cache reads a large font file, so it is
                    // built off the UI thread the first time Stamp is
                    // chosen: one thread per process, not per selection.
                    #[cfg(not(target_os = "macos"))]
                    {
                        static PREWARM: std::sync::Once = std::sync::Once::new();
                        if t == readshot_ui::editor::ToolState::Stamp {
                            PREWARM.call_once(|| {
                                std::thread::spawn(readshot_core::stamp::prewarm_emoji);
                            });
                        }
                    }
                }
                readshot_ui::ToolbarMessage::SelectColor(c) => {
                    apply_editor_color(ed, &state.coordinator, c);
                    ed.popover = None;
                }
                readshot_ui::ToolbarMessage::SetLineWidth(w) => {
                    apply_editor_line_width(ed, &state.coordinator, w);
                }
                readshot_ui::ToolbarMessage::Undo => {
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    if ed.model.undo() {
                        ed.refresh_image();
                        ed.set_status("Undid edit. ⇧⌘Z to redo.");
                        sync_editor_history(ed, &state.coordinator);
                    }
                }
                readshot_ui::ToolbarMessage::Redo => {
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    if ed.model.redo() {
                        ed.refresh_image();
                        ed.set_status("Redid edit. ⌘Z to undo.");
                        sync_editor_history(ed, &state.coordinator);
                    }
                }
            }
            // Only tools that show a "new" dot are worth remembering;
            // others would just grow the prefs file.
            if let Some(t) =
                selected_tool.filter(|t| readshot_ui::editor::toolbar::NEW_TOOLS.contains(t))
            {
                state.mark_editor_tool_seen(t.id());
            }
            if let Some(color) = picked_color {
                // Presets are ignored; a custom (Recent) color moves to the front.
                state.remember_custom_color(color);
            }
            if selected_tool == Some(readshot_ui::editor::ToolState::Highlighter)
                && state.coordinator.engine_reports_positions()
            {
                // Snap to text needs line positions; recognize now if the
                // panel hasn't already. Engines without positions can't
                // snap, so they skip the whole-image OCR.
                return super::text_panel::request_text_recognition(state);
            }
            Task::none()
        }

        Message::EditorLineWidthPreview(width) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            preview_editor_line_width(ed, width);
            Task::none()
        }

        Message::EditorLineWidthCommit => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            if let Some(baseline) = ed.width_drag_baseline.take() {
                if ed.model.commit_preview_from_baseline(baseline) {
                    ed.refresh_image();
                    ed.set_status("Updated selected annotation size. ⌘Z to undo.");
                    sync_editor_history(ed, &state.coordinator);
                } else {
                    ed.refresh_image();
                }
            }
            Task::none()
        }

        Message::EditorStyle(msg) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            apply_editor_style(ed, &state.coordinator, msg);
            Task::none()
        }
        Message::EditorStylePreview(msg) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            preview_editor_style(ed, msg);
            Task::none()
        }
        Message::EditorHexChanged(raw) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.hex_draft = raw
                    .chars()
                    .filter(|c| *c == '#' || c.is_ascii_hexdigit())
                    .enumerate()
                    // '#' is only valid as the first kept character.
                    .filter(|(i, c)| *c != '#' || *i == 0)
                    .map(|(_, c)| c)
                    .take(7)
                    .collect();
            }
            Task::none()
        }
        Message::EditorEyedropperToggled => {
            if let Some(ed) = state.editor.as_mut() {
                // A held drag (shape, freehand or Select move/resize)
                // can't be hijacked into a pick.
                if ed.busy || ed.canvas_drag_active || ed.move_drag.is_some() {
                    return Task::none();
                }
                close_editor_popover(ed);
                ed.eyedropper = match ed.eyedropper {
                    Some(_) => None,
                    None => Some(crate::editor::EyedropperState::default()),
                };
            }
            Task::none()
        }
        Message::EditorHexSubmitted => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            let Some(color) = readshot_core::Rgba::from_hex(&ed.hex_draft) else {
                ed.set_status("Couldn't read that color. Use a hex value like #4F46E5.");
                return Task::none();
            };
            apply_editor_color(ed, &state.coordinator, color);
            close_editor_popover(ed);
            state.remember_custom_color(color);
            Task::none()
        }
        Message::EditorStyleCommit => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            // Release keeps the new tool style; only the selection needs
            // turning into one undo step.
            if let Some(baseline) = ed.style_drag_baseline.take().and_then(|d| d.annotations) {
                if ed.model.commit_preview_from_baseline(baseline) {
                    ed.refresh_image();
                    ed.set_status("Updated selected annotation. ⌘Z to undo.");
                    sync_editor_history(ed, &state.coordinator);
                } else {
                    ed.refresh_image();
                }
            }
            Task::none()
        }

        // Canvas events — mostly drag previews (which don't need
        // their own state mutation in this cut; the canvas's internal
        // DrawState already drives the live painting) and one-shot
        // commits which mutate the model.
        Message::EditorCanvas(msg) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            let mut picked: Option<readshot_core::Rgba> = None;
            match msg {
                readshot_ui::CanvasMessage::EyedropperHover(p) => {
                    if ed.eyedropper.is_some() {
                        // Sample what the canvas shows: a fractional crop
                        // origin is truncated by the renderer.
                        let shown =
                            readshot_core::eyedropper::displayed_base_point(p, ed.crop_offset());
                        let pixels = readshot_core::eyedropper::sample_grid(
                            ed.model.base(),
                            shown,
                            readshot_ui::editor::LOUPE_RADIUS,
                        );
                        if let Some(eye) = ed.eyedropper.as_mut() {
                            eye.loupe = Some(readshot_ui::editor::Loupe { center: p, pixels });
                        }
                    }
                }
                readshot_ui::CanvasMessage::EyedropperPick(p) => {
                    // One pick, then the eyedropper exits. The source image
                    // is sampled, so annotations drawn over it never count.
                    if ed.eyedropper.take().is_some() {
                        let shown =
                            readshot_core::eyedropper::displayed_base_point(p, ed.crop_offset());
                        if let Some(color) =
                            readshot_core::eyedropper::sample(ed.model.base(), shown)
                        {
                            let undoable = apply_editor_color(ed, &state.coordinator, color);
                            ed.set_status(if undoable {
                                format!("Picked {}. ⌘Z to undo.", color.to_hex())
                            } else {
                                format!("Picked {}.", color.to_hex())
                            });
                            picked = Some(color);
                        }
                    }
                }
                readshot_ui::CanvasMessage::DragStarted => {
                    // A shape/freehand drag is now live; Escape should cancel
                    // it rather than trigger discard.
                    ed.canvas_drag_active = true;
                }
                readshot_ui::CanvasMessage::DragMoved(_)
                | readshot_ui::CanvasMessage::PolylineMoved(_) => {
                    // Preview-only events. The canvas's own State holds
                    // the drag points; a redraw is automatic.
                }
                readshot_ui::CanvasMessage::Cancelled => {
                    ed.canvas_drag_active = false;
                    cancel_editor_previews(ed);
                }
                readshot_ui::CanvasMessage::SelectPressed(p) => {
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    let scale = ed.render_scale.get();
                    if let Some(handle) = ed.model.resize_handle_at_scaled(p, scale) {
                        let Some(selected_index) = ed.model.selected_annotation() else {
                            return Task::none();
                        };
                        ed.move_drag = Some(crate::editor::MoveDrag {
                            baseline: ed.model.annotations().to_vec(),
                            selected_index,
                            start: p,
                            moved: false,
                            kind: crate::editor::MoveDragKind::Resize(handle),
                        });
                    } else if let Some(selected_index) = ed.model.select_at_scaled(p, scale) {
                        ed.move_drag = Some(crate::editor::MoveDrag {
                            baseline: ed.model.annotations().to_vec(),
                            selected_index,
                            start: p,
                            moved: false,
                            kind: crate::editor::MoveDragKind::Move,
                        });
                    } else {
                        ed.move_drag = None;
                    }
                }
                readshot_ui::CanvasMessage::SelectDragged(p) => {
                    let mut background_without_selected = None;
                    if let Some(drag) = ed.move_drag.as_mut() {
                        let dx = p.x - drag.start.x;
                        let dy = p.y - drag.start.y;
                        if !drag.moved && dx.abs() < 0.5 && dy.abs() < 0.5 {
                            return Task::none();
                        }
                        let changed = match drag.kind {
                            crate::editor::MoveDragKind::Move => {
                                ed.model.preview_move_selected_from(&drag.baseline, dx, dy)
                            }
                            crate::editor::MoveDragKind::Resize(handle) => ed
                                .model
                                .preview_resize_selected_from(&drag.baseline, handle, dx, dy),
                        };
                        if changed {
                            if !drag.moved
                                && can_preview_drag_annotation(&drag.baseline, drag.selected_index)
                            {
                                background_without_selected = Some(drag.selected_index);
                            }
                            drag.moved = true;
                            if refresh_image_during_select_drag(&drag.baseline, drag.selected_index)
                            {
                                ed.refresh_crop_drag_preview();
                            }
                            // For ordinary annotations, keep the flattened image
                            // handle stable while the pointer is moving.
                            // Re-uploading a full RGBA texture on every Select
                            // drag tick can briefly reveal the dark stage behind
                            // the image. Crop is the exception: it changes the
                            // rendered output geometry, so the texture must stay
                            // in sync with the preview dimensions.
                        }
                    }
                    if let Some(index) = background_without_selected {
                        ed.image_handle = editor_image_handle_without_annotation(ed, index);
                    }
                }
                readshot_ui::CanvasMessage::SelectReleased => {
                    let Some(drag) = ed.move_drag.take().filter(|drag| drag.moved) else {
                        return Task::none();
                    };
                    if !ed.model.commit_preview_from_baseline(drag.baseline) {
                        ed.refresh_image();
                        return Task::none();
                    }
                    ed.refresh_image();
                    let verb = match drag.kind {
                        crate::editor::MoveDragKind::Move => "Moved",
                        crate::editor::MoveDragKind::Resize(_) => "Resized",
                    };
                    ed.set_status(format!("{verb} annotation. ⌘Z to undo."));
                    sync_editor_history(ed, &state.coordinator);
                }
                readshot_ui::CanvasMessage::RequestText(p) => {
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    // Text tool clicked — open the inline text-input
                    // banner. The eventual Annotation::Text lands at
                    // exactly the click point regardless of how long
                    // the user takes to type.
                    ed.begin_text_edit(crate::editor::PendingText {
                        origin: p,
                        content: String::new(),
                        edit_index: None,
                    });
                }
                readshot_ui::CanvasMessage::CommitAnnotation(annotation) => {
                    ed.canvas_drag_active = false;
                    cancel_editor_previews(ed);
                    let annotation = super::text_panel::snap_highlight_annotation(ed, annotation);
                    handle_commit_annotation(ed, annotation);
                    sync_editor_history(ed, &state.coordinator);
                }
            }
            if let Some(color) = picked {
                state.remember_custom_color(color);
            }
            Task::none()
        }

        Message::EditorEditSelectedText => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            if let Some(edit) = ed.model.selected_text_edit() {
                cancel_editor_previews(ed);
                ed.clear_discard_confirmation();
                ed.begin_text_edit(crate::editor::PendingText {
                    origin: edit.origin,
                    content: edit.content,
                    edit_index: Some(edit.index),
                });
            }
            Task::none()
        }

        Message::EditorDeleteSelected => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            let deleting_pending_text = matches!(
                (
                    ed.pending_text
                        .as_ref()
                        .and_then(|pending| pending.edit_index),
                    ed.model.selected_annotation(),
                ),
                (Some(edit_index), Some(selected_index)) if edit_index == selected_index
            );
            cancel_editor_previews(ed);
            if deleting_pending_text {
                ed.clear_text_edit();
            }
            if ed.model.delete_selected_annotation() {
                ed.refresh_image();
                ed.set_status("Deleted annotation. ⌘Z to undo.");
                sync_editor_history(ed, &state.coordinator);
            }
            Task::none()
        }

        Message::EditorNudgeSelected(dx, dy) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            // Don't hijack the keys when another input mode owns them:
            // while a text draft/edit is open the arrows belong to the
            // text caret (iced's single-line input ignores Up/Down, so
            // they'd otherwise reach here and move the annotation being
            // typed), and during an in-progress mouse move/resize drag a
            // nudge would cancel the drag mid-gesture.
            if ed.pending_text.is_some() || ed.move_drag.is_some() {
                return Task::none();
            }
            // Settle any in-flight preview first, then move the selection
            // by the keyboard step as one undoable edit. No status toast:
            // arrow presses are rapid and would spam the chrome.
            cancel_editor_previews(ed);
            if ed.model.nudge_selected(dx, dy) {
                ed.refresh_image();
                sync_editor_history(ed, &state.coordinator);
            }
            Task::none()
        }

        Message::EditorTextChanged(content) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                if ed.pending_text.is_some() {
                    ed.clear_discard_confirmation();
                }
                if let Some(pending) = ed.pending_text.as_mut() {
                    pending.content = content.clone();
                }
                // Keep the multi-line backing store in sync so a
                // programmatic content change (e.g. tests) is reflected
                // if the inline editor re-renders.
                ed.text_edit_content =
                    Some(iced::widget::text_editor::Content::with_text(&content));
            }
            Task::none()
        }
        Message::EditorTextAction(action) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                if let Some(content) = ed.text_edit_content.as_mut() {
                    content.perform(action);
                    // Normalise line endings so the model (render /
                    // bounds) only ever sees `\n`, regardless of the
                    // platform default the widget inserts.
                    let text = content.text().replace("\r\n", "\n").replace('\r', "\n");
                    if let Some(pending) = ed.pending_text.as_mut() {
                        pending.content = text;
                    }
                    ed.clear_discard_confirmation();
                }
            }
            Task::none()
        }
        Message::EditorTextCommit => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            commit_pending_editor_text(ed, &state.coordinator);
            Task::none()
        }
        Message::EditorTextCancel => {
            if let Some(ed) = state.editor.as_mut() {
                if close_editor_popover(ed) {
                    return Task::none();
                }
                if super::text_panel::close_panel_overlay(ed) {
                    return Task::none();
                }
                // Esc chain: popover, then the overlay panel, then the eyedropper, then the text
                // draft / selection below, then the Text panel's line selection.
                if ed.eyedropper.take().is_some() {
                    return Task::none();
                }
                if ed.busy {
                    return Task::none();
                }
                // One Esc = one step: a live options-bar slider drag is
                // cancelled on its own, keeping the selection (and the bar).
                if ed.style_drag_baseline.is_some() {
                    cancel_editor_previews(ed);
                    return Task::none();
                }
                // Last step of the chain: a Text-panel line selection,
                // once the canvas has nothing left to cancel.
                if ed.pending_text.is_none()
                    && ed.model.selected_annotation().is_none()
                    && !ed.text_panel.selected.is_empty()
                {
                    ed.text_panel.selected.clear();
                    ed.text_panel.anchor = None;
                    return Task::none();
                }
                let keep_selection = ed
                    .pending_text
                    .as_ref()
                    .is_some_and(|pending| pending.edit_index.is_some());
                cancel_editor_previews(ed);
                if !keep_selection {
                    ed.model.clear_selection();
                }
                ed.clear_text_edit();
            }
            Task::none()
        }
        Message::EditorWidthBump(delta) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                // Text (selected, or the Text tool with nothing selected)
                // steps its size preset from the actual px; anything
                // else bumps the width the options bar shows.
                if let Some(px) = bump_text_px(&ed.model) {
                    if let Some(next) = readshot_ui::editor::TextSize::step_from_px(px, delta > 0.0)
                    {
                        apply_editor_style(
                            ed,
                            &state.coordinator,
                            readshot_ui::editor::StyleMessage::TextSize(next),
                        );
                    }
                } else {
                    let next = ed.model.effective_line_width() + delta;
                    apply_editor_line_width(ed, &state.coordinator, next);
                }
            }
            Task::none()
        }
        Message::EditorColorCycle(dir) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                let palette = readshot_ui::editor::toolbar::PALETTE;
                let current = ed.model.current_color();
                let idx = palette
                    .iter()
                    .position(|c| swatch_eq(*c, current))
                    .unwrap_or(0) as i32;
                let len = palette.len() as i32;
                let next = ((idx + dir).rem_euclid(len)) as usize;
                apply_editor_color(ed, &state.coordinator, palette[next]);
            }
            Task::none()
        }
        Message::EditorZoomIn => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                // Keyboard (⌘=) and the title-row "+" share this path. In
                // Fit mode the step starts from the scale the stage last
                // rendered at, resolved now: a view built before a window
                // resize would carry a stale scale.
                ed.zoom = ed.zoom.zoom_in_from_display_scale(ed.render_scale.get());
                ed.clear_eyedropper_loupe();
            }
            Task::none()
        }
        Message::EditorZoomOut => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                // See `EditorZoomIn`.
                ed.zoom = ed.zoom.zoom_out_from_display_scale(ed.render_scale.get());
                ed.clear_eyedropper_loupe();
            }
            Task::none()
        }
        Message::EditorZoomActual => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.zoom = ed.actual_size_zoom();
                ed.clear_eyedropper_loupe();
            }
            Task::none()
        }
        Message::EditorZoomFit => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.zoom = crate::editor::EditorZoom::Fit;
                ed.clear_eyedropper_loupe();
            }
            Task::none()
        }

        Message::EditorPinRequested => {
            // Snapshot the editor's currently-flattened image, open
            // a fresh pin window with that image, then close the
            // editor. The pin lives independently from there on.
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            commit_pending_editor_text(ed, &state.coordinator);
            cancel_editor_previews(ed);
            let img = editor_output_image(ed);
            let size = (img.width(), img.height());
            let handle = rgba_handle(img);
            // Close the editor window if any.
            let editor_id = ed.window_id;
            let pin_settings = editor_pin_window_settings(ed, size);
            state.editor = None;
            state.release_editor_modifiers();
            let mut tasks: Vec<Task<Message>> = Vec::new();
            if let Some(id) = editor_id {
                state.windows.forget(id);
                tasks.push(window::close(id));
            }
            // Open the pin window. We register both kind + image
            // handle eagerly so the first `view` call paints the
            // pin instead of the "(no capture)" fallback.
            let (id, open_task) = window::open(pin_settings);
            state.windows.register(id, WindowKind::Pin);
            state
                .pins
                .insert(id, crate::app::PinState::new(handle.clone()));
            tasks
                .push(open_task.map(move |opened| Message::PinWindowReady(opened, handle.clone())));
            Task::batch(tasks)
        }
        Message::PinWindowReady(id, handle) => {
            // The eager insert above already covers most cases; this
            // re-key handles the (rare) scenario where the iced
            // runtime hands us a different id than the one returned
            // by `window::open` synchronously. Idempotent insert.
            state
                .pins
                .entry(id)
                .or_insert_with(|| crate::app::PinState::new(handle));
            Task::none()
        }
        Message::PinClosePressed(id) => {
            state.windows.forget(id);
            state.pins.remove(&id);
            if state.hovered_pin == Some(id) {
                state.hovered_pin = None;
            }
            window::close(id)
        }
        Message::PinHovered(id, true) => {
            state.hovered_pin = Some(id);
            Task::none()
        }
        Message::PinHovered(id, false) => {
            if state.hovered_pin == Some(id) {
                state.hovered_pin = None;
            }
            Task::none()
        }
        Message::PinDragRequested(id) => window::drag(id),
        Message::PinOpacityChanged(id, opacity) => {
            if let Some(pin) = state.pins.get_mut(&id) {
                pin.opacity = opacity.clamp(0.2, 1.0);
            }
            Task::none()
        }
        Message::PinLockToggled(id) => {
            if let Some(pin) = state.pins.get_mut(&id) {
                pin.locked = !pin.locked;
            }
            Task::none()
        }
        Message::PinKeyPressed {
            window,
            key,
            modifiers,
        } => {
            // The keyboard subscription is global; only act when the
            // focused window is actually a live pin. Copy the opacity out
            // so the immutable pin borrow is dropped before `update`
            // needs `&mut state`.
            let Some(opacity) = state.pins.get(&window).map(|pin| pin.opacity) else {
                return Task::none();
            };
            match pin_key_action(&key, modifiers) {
                Some(PinKeyAction::Close) => update(state, Message::PinClosePressed(window)),
                Some(PinKeyAction::ToggleLock) => update(state, Message::PinLockToggled(window)),
                Some(PinKeyAction::OpacityUp) => {
                    let next = (opacity + PIN_OPACITY_STEP).clamp(PIN_OPACITY_MIN, PIN_OPACITY_MAX);
                    update(state, Message::PinOpacityChanged(window, next))
                }
                Some(PinKeyAction::OpacityDown) => {
                    let next = (opacity - PIN_OPACITY_STEP).clamp(PIN_OPACITY_MIN, PIN_OPACITY_MAX);
                    update(state, Message::PinOpacityChanged(window, next))
                }
                Some(PinKeyAction::Nudge(dx, dy)) => {
                    // iced exposes no relative move; read the current
                    // logical position, then move_to the offset point.
                    window::position(window).and_then(move |p| {
                        window::move_to(window, iced::Point::new(p.x + dx, p.y + dy))
                    })
                }
                None => Task::none(),
            }
        }
        msg @ (Message::EditorTextRecognize
        | Message::EditorTextRecognized { .. }
        | Message::EditorPanelToggled
        | Message::EditorPanelTabSelected(..)
        | Message::EditorPanelOverlayDismissed
        | Message::EditorOpenTextSettings
        | Message::EditorTextLineClicked(..)
        | Message::EditorCopyLines
        | Message::EditorCopyLinesDone(..)
        | Message::EditorRemoveLineBreaksToggled(..)
        | Message::EditorRedactStyleChanged(..)
        | Message::EditorRedactAll
        | Message::EditorRedactLine(..)
        | Message::EditorHistoryWritten { .. }) => {
            super::text_panel::handle_text_panel_message(state, msg)
        }
        other => unreachable!("non-editor message routed to the editor handler: {other:?}"),
    }
}
