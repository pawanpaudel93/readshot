// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use crate::coordinator::CaptureCoordinator;

/// Map an editor line-width to a text point-size. The Text tool
/// shares the line-width slider so the user has one knob — small
/// width = small text, big width = headline. The clamp keeps text
/// readable on either end.
pub(crate) fn text_size_from_line_width(line_width: f32) -> f32 {
    (line_width * 4.0 + 8.0).clamp(12.0, 96.0)
}

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
            "Selected Crop — drag frame or handles · Delete restores full image".to_string()
        }
        (_, true) => {
            format!("Selected {kind} — edit inline · color/size apply here · Delete removes")
        }
        _ => {
            format!(
                "Selected {kind} — drag to move · arrows nudge · handles resize · drag elsewhere to draw"
            )
        }
    }
}

pub(crate) fn apply_editor_color(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
    color: readshot_core::Rgba,
) {
    cancel_editor_previews(ed);
    ed.model.set_color(color);
    if ed.model.apply_color_to_selected(color) {
        ed.refresh_image();
        ed.set_status("Updated selected annotation color. ⌘Z to undo.");
        sync_editor_history(ed, coord);
    }
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

pub(crate) fn commit_pending_editor_text(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) -> bool {
    let Some(pending) = ed.pending_text.take() else {
        return false;
    };
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

    let annotation = readshot_core::Annotation::Text {
        content: trimmed.to_string(),
        origin: pending.origin,
        color: ed.model.current_color(),
        font_family: "system-ui".to_string(),
        // Tie text size to the line-width slider so it's discoverable
        // without a separate control.
        size: text_size_from_line_width(ed.model.current_line_width()),
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
    if refresh {
        ed.refresh_image();
    }
}

pub(crate) fn sync_editor_history(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) {
    ed.clear_discard_confirmation();
    let Some(record) = ed.source_record.as_mut() else {
        return;
    };
    record.annotation_model = ed.model.annotations().to_vec();
    // Queued on the coordinator's ordered background writer: the
    // sidecar write fsyncs, and this runs on every commit/undo/redo —
    // a synchronous write would jank the UI thread. Failures are
    // logged by the worker.
    coord.update_history_async(record.clone());
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
            match editor_key_message(key, modifiers, status_ignored) {
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
            // that the user has already walked away from.
            if let Some(ed) = state.editor.as_mut() {
                if !ed.status_is_in_progress()
                    && ed
                        .status_set_at
                        .is_some_and(|t| t.elapsed() > crate::editor::STATUS_AUTO_DISMISS)
                {
                    ed.status = None;
                    ed.status_set_at = None;
                }
                if let Some(t) = ed.discard_pending_at {
                    if t.elapsed() > crate::editor::DISCARD_CONFIRM_WINDOW {
                        ed.discard_pending_at = None;
                    }
                }
            }
            Task::none()
        }
        Message::EditorWindowReady(id) => {
            if let Some(ed) = &mut state.editor {
                ed.window_id = Some(id);
            }
            Task::none()
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
                    Ok(Some(p)) => format!("Saved to {}", p.display()),
                    Ok(None) => "Save cancelled.".into(),
                    Err(e) => format!("Save failed: {e}"),
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
            if ed.frame_style == EditorFrameStyle::None {
                ed.set_status("Copying…");
            } else {
                ed.set_status(format!(
                    "Copying image with {} frame…",
                    ed.frame_style.label()
                ));
            }
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
                    Ok(()) => "Copied to clipboard.".into(),
                    Err(e) => format!("Copy failed: {e}"),
                });
            }
            Task::none()
        }

        Message::EditorFrameStyleChanged(style) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                cancel_editor_previews(ed);
                ed.clear_discard_confirmation();
                ed.frame_style = style;
                ed.set_status(match style {
                    EditorFrameStyle::None => "Frame removed.".into(),
                    _ => format!("Frame set to {}.", style.label()),
                });
            }
            Task::none()
        }

        Message::EditorCopyTextRequested => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            commit_pending_editor_text(ed, &state.coordinator);
            cancel_editor_previews(ed);
            ed.busy = true;
            ed.set_status("Recognising text…");
            let img = ed.model.flatten();
            let coord = state.coordinator.clone();
            let prefs = state.preferences.clone();
            Task::perform(ocr_then_copy(coord, img, prefs), |r| {
                Message::EditorCopyTextDone(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyTextDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                ed.set_status(match result {
                    Ok(text) if text.is_empty() => "No text recognised.".into(),
                    Ok(text) => format!(
                        "Copied {} character{} of text.",
                        text.chars().count(),
                        if text.chars().count() == 1 { "" } else { "s" },
                    ),
                    Err(e) => format!("Copy text failed: {e}"),
                });
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
                        ed.status =
                            Some("Discard unsaved edits? Click Discard again to confirm.".into());
                        ed.status_set_at = Some(now);
                        return Task::none();
                    }
                }
            }
            let id = state.editor.as_ref().and_then(|e| e.window_id);
            state.editor = None;
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
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            match msg {
                readshot_ui::ToolbarMessage::SelectTool(t) => {
                    cancel_editor_previews(ed);
                    if t != readshot_ui::editor::ToolState::Text {
                        commit_pending_editor_text(ed, &state.coordinator);
                    }
                    ed.model.set_tool(t);
                }
                readshot_ui::ToolbarMessage::SelectColor(c) => {
                    apply_editor_color(ed, &state.coordinator, c);
                }
                readshot_ui::ToolbarMessage::SetLineWidth(w) => {
                    apply_editor_line_width(ed, &state.coordinator, w);
                }
                readshot_ui::ToolbarMessage::Undo => {
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    if ed.model.undo() {
                        ed.refresh_image();
                        ed.set_status("Undid edit. ⌘⇧Z to redo.");
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
            match msg {
                readshot_ui::CanvasMessage::DragStarted
                | readshot_ui::CanvasMessage::DragMoved(_)
                | readshot_ui::CanvasMessage::PolylineMoved(_) => {
                    // Preview-only events. The canvas's own State holds
                    // the drag points; a redraw is automatic.
                }
                readshot_ui::CanvasMessage::Cancelled => {
                    cancel_editor_previews(ed);
                }
                readshot_ui::CanvasMessage::SelectPressed(p) => {
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    if let Some(handle) = ed.model.resize_handle_at(p) {
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
                    } else if let Some(selected_index) = ed.model.select_at(p) {
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
                    ed.pending_text = Some(crate::editor::PendingText {
                        origin: p,
                        content: String::new(),
                        edit_index: None,
                    });
                }
                readshot_ui::CanvasMessage::CommitAnnotation(annotation) => {
                    cancel_editor_previews(ed);
                    handle_commit_annotation(ed, annotation);
                    sync_editor_history(ed, &state.coordinator);
                }
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
                ed.pending_text = Some(crate::editor::PendingText {
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
                ed.pending_text = None;
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
                    pending.content = content;
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
                if ed.busy {
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
                ed.pending_text = None;
            }
            Task::none()
        }
        Message::EditorWidthBump(delta) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                let next = ed.model.current_line_width() + delta;
                apply_editor_line_width(ed, &state.coordinator, next);
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
                ed.zoom = ed.zoom.zoom_in();
            }
            Task::none()
        }
        Message::EditorZoomInFromDisplayScale(scale) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.zoom = ed.zoom.zoom_in_from_display_scale(scale);
            }
            Task::none()
        }
        Message::EditorZoomOut => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.zoom = ed.zoom.zoom_out();
            }
            Task::none()
        }
        Message::EditorZoomOutFromDisplayScale(scale) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.zoom = ed.zoom.zoom_out_from_display_scale(scale);
            }
            Task::none()
        }
        Message::EditorZoomActual => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.zoom = ed.actual_size_zoom();
            }
            Task::none()
        }
        Message::EditorZoomFit => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.zoom = crate::editor::EditorZoom::Fit;
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
            window::close(id)
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
        other => unreachable!("non-editor message routed to the editor handler: {other:?}"),
    }
}
