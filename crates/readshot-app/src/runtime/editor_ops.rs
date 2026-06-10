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
            "Selected Crop — drag or resize frame · Delete or ⌘Z restores full image".to_string()
        }
        (_, true) => {
            format!(
                "Selected {kind} — Enter edits text · color/size update selected · Delete removes"
            )
        }
        _ => {
            format!("Selected {kind} — drag to move · handles resize · color/size update selected · Delete removes")
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
        "Update Text"
    } else {
        "Add Text"
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
