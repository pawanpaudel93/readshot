//! Editor toolbar and style messages: tool, color, width, style
//! sliders, the hex field, the eyedropper toggle and Beautify presets.
//! Moved out of `handle_editor_message`; `use super::*` keeps the
//! arms' names resolving as they did there.

use super::*;

/// `Message::EditorBeautifyPreset`: a built-in frame preset.
pub(super) fn on_beautify_preset(
    state: &mut App,
    preset: readshot_core::beautify::BuiltinPreset,
) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        if ed.busy {
            return Task::none();
        }
        cancel_editor_previews(ed);
        ed.snapshot_drag_cancelled = false;
        ed.clear_discard_confirmation();
        close_editor_popover(ed);
        if ed.model.set_beautify(preset.settings()) {
            ed.set_status(match preset {
                readshot_core::beautify::BuiltinPreset::None => {
                    format!("Frame removed. {UNDO_HINT}")
                }
                p => format!("Frame set to {}. {UNDO_HINT}", p.label()),
            });
        }
    }
    Task::none()
}

/// `Message::EditorSnapshotSliderCommit`: a Beautify or gap slider released.
pub(super) fn on_snapshot_slider_commit(state: &mut App) -> Task<Message> {
    if let Some(ed) = state.editor.as_mut() {
        // Release ends the drag: a cancelled one commits nothing.
        ed.snapshot_drag_cancelled = false;
        return crate::runtime::combine::release_gap_slider(ed, &state.coordinator);
    }
    Task::none()
}

/// `Message::EditorToolbar`: tool, color, width, Undo and Redo.
pub(super) fn on_toolbar(state: &mut App, msg: readshot_ui::ToolbarMessage) -> Task<Message> {
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
            if t != readshot_ui::editor::ToolState::Crop {
                ed.crop_draft = None;
            }
            if t != readshot_ui::editor::ToolState::Text {
                commit_pending_editor_text(ed, &state.coordinator);
            }
            let changed = ed.model.active_tool() != t;
            ed.model.set_tool(t);
            // The tool's how-to, now that the options bar has
            // taken the old hint line: a notice when
            // the tool is chosen. Select keeps its hint line.
            if changed
                && !crate::runtime::editor_options::visible_option_controls(ed).is_empty()
                && hint_may_show(ed)
            {
                ed.set_info(crate::runtime::view::editor_compact_hint(t));
            }
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
            // Through the one close path, so the keyboard focus goes
            // back to the control that opened the popover.
            close_editor_popover(ed);
        }
        readshot_ui::ToolbarMessage::SetLineWidth(w) => {
            apply_editor_line_width(ed, &state.coordinator, w);
        }
        readshot_ui::ToolbarMessage::Undo | readshot_ui::ToolbarMessage::Redo
            if ed.crop_draft.is_some() =>
        {
            // One step per press: an open crop draft is dropped and
            // history stays put, so Undo never lands beneath it.
            ed.crop_draft = None;
            ed.set_status("Cancelled the crop.");
        }
        readshot_ui::ToolbarMessage::Undo | readshot_ui::ToolbarMessage::Redo
            if (ed.snapshot_drag_baseline.is_some() && !ed.gap_commit_on_land)
                || ed.style_drag_baseline.is_some() =>
        {
            // One step per press: an open slider drag ends (back to
            // its value before the drag) and history stays put.
            // Covers the Beautify and the options-bar style sliders.
            cancel_editor_previews(ed);
            ed.set_status("Cancelled the slider change.");
        }
        readshot_ui::ToolbarMessage::Undo => {
            crate::runtime::image_menu::before_geometry_change(ed, &state.coordinator);
            let kind = crate::editor::ComposeKind::Undo;
            if let Some(task) = crate::runtime::compose_jobs::queue_history_move(ed, kind) {
                return task;
            }
            crate::runtime::compose_jobs::apply_step_now(ed, &state.coordinator, kind, None);
        }
        readshot_ui::ToolbarMessage::Redo => {
            crate::runtime::image_menu::before_geometry_change(ed, &state.coordinator);
            let kind = crate::editor::ComposeKind::Redo;
            if let Some(task) = crate::runtime::compose_jobs::queue_history_move(ed, kind) {
                return task;
            }
            crate::runtime::compose_jobs::apply_step_now(ed, &state.coordinator, kind, None);
        }
    }
    // Only tools that show a "new" dot are worth remembering;
    // others would just grow the prefs file.
    if let Some(t) = selected_tool.filter(|t| readshot_ui::editor::toolbar::NEW_TOOLS.contains(t)) {
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
        return crate::runtime::text_panel::request_text_recognition(state);
    }
    Task::none()
}

/// `Message::EditorLineWidthPreview`.
pub(super) fn on_line_width_preview(state: &mut App, width: f32) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    preview_editor_line_width(ed, width);
    Task::none()
}

/// `Message::EditorLineWidthCommit`.
pub(super) fn on_line_width_commit(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    if let Some(baseline) = ed.width_drag_baseline.take() {
        if ed.model.commit_preview_from_baseline(baseline) {
            edit_landed(
                ed,
                &state.coordinator,
                Some(format!("Updated selected annotation size. {UNDO_HINT}")),
            );
        } else {
            ed.refresh_image();
        }
    }
    Task::none()
}

/// `Message::EditorStyle`.
pub(super) fn on_style(state: &mut App, msg: readshot_ui::editor::StyleMessage) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    if let (readshot_ui::editor::style::StyleMessage::CropAspect(a), Some(d)) = (msg, ed.crop_draft)
    {
        // The draft owns the aspect while it is open: refit it
        // (centred) and leave any selected committed crop alone.
        cancel_editor_previews(ed);
        ed.model.set_style(msg);
        if let Some(r) = a.ratio() {
            ed.crop_draft = Some(readshot_ui::editor::style::fit_aspect(d, r));
        }
        return Task::none();
    }
    apply_editor_style(ed, &state.coordinator, msg);
    Task::none()
}

/// `Message::EditorStylePreview`.
pub(super) fn on_style_preview(
    state: &mut App,
    msg: readshot_ui::editor::StyleMessage,
) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    preview_editor_style(ed, msg);
    Task::none()
}

/// `Message::EditorHexChanged`: keep only what a hex color may hold.
pub(super) fn on_hex_changed(state: &mut App, raw: String) -> Task<Message> {
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

/// `Message::EditorEyedropperToggled`.
pub(super) fn on_eyedropper_toggled(state: &mut App) -> Task<Message> {
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

/// `Message::EditorHexSubmitted`.
pub(super) fn on_hex_submitted(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy {
        return Task::none();
    }
    let Some(color) = readshot_core::Rgba::from_hex(&ed.hex_draft) else {
        ed.set_status("Couldn’t read that color. Use a hex value like #4F46E5.");
        return Task::none();
    };
    apply_editor_color(ed, &state.coordinator, color);
    close_editor_popover(ed);
    state.remember_custom_color(color);
    Task::none()
}

/// `Message::EditorStyleCommit`: a style slider released.
pub(super) fn on_style_commit(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    // Release ends the drag: a cancelled one commits nothing.
    ed.style_drag_cancelled = false;
    if ed.busy {
        return Task::none();
    }
    // Release keeps the new tool style; only the selection needs
    // turning into one undo step.
    if commit_style_drag(ed, &state.coordinator) {
        ed.set_status(format!("Updated selected annotation. {UNDO_HINT}"));
    }
    Task::none()
}
