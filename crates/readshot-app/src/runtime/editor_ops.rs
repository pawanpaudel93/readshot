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
        ed.set_info(format!(
            "Crop selected at {w}\u{a0}×\u{a0}{h}\u{a0}px. Drag the handles to adjust; {DELETE_KEY} or {UNDO_KEY} cancels."
        ));
    }
}

/// How to work the selected annotation: sentences, keys named as in
/// chips. Shown as a guide while the selection lasts (pointer or Tab).
pub(crate) fn editor_selected_hint(kind: &str, text_editable: bool) -> String {
    match (kind, text_editable) {
        ("Magnifier", _) => format!(
            "Magnifier selected. Drag the bubble to move it, the center to change what it shows, \
             or the ring to resize it. {DELETE_KEY} removes it."
        ),
        ("Crop", _) => format!(
            "Crop selected. Drag the frame or its handles; arrows nudge. {DELETE_KEY} restores the full image."
        ),
        (_, true) => format!(
            "{kind} selected. Edit it in place; color and size apply to it. Arrows nudge, {DELETE_KEY} removes it."
        ),
        _ => format!(
            "{kind} selected. Drag to move it, arrows nudge, handles resize. {DELETE_KEY} removes it."
        ),
    }
}

/// The focus query's answer for a canvas Tab: the field's own answer, or
/// "not focused" when no widget answered.
pub(crate) fn tab_field_focused(answers: &[bool]) -> bool {
    answers.first().copied().unwrap_or(false)
}

/// Whether a how-to notice may take the status line: never over an
/// error, a warning, an armed Discard, a job in progress, or a live
/// crop or focus-zone guide.
pub(crate) fn hint_may_show(ed: &crate::editor::EditorSession) -> bool {
    use crate::editor::KeyGuide;
    let live_guide = ed.key_guide_live() == Some(true)
        && !matches!(ed.key_guide, Some((_, KeyGuide::Selection)));
    !(ed.status_is_error()
        || ed.status_is_warning()
        || ed.visible_toast_action().is_some()
        || ed.discard_pending_at.is_some()
        || ed.status_is_in_progress()
        || live_guide)
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
        ed.set_status(format!("Updated selected annotation color. {UNDO_HINT}"));
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
        ed.set_status(format!("Updated selected annotation size. {UNDO_HINT}"));
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
        ed.set_status(format!("Updated selected annotation. {UNDO_HINT}"));
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
    if ed.style_drag_cancelled {
        return;
    }
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

/// Commit an open crop draft as one undo step. Returns whether one was open.
pub(crate) fn apply_crop_draft(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) -> bool {
    let Some(rect) = ed.crop_draft.take() else {
        return false;
    };
    ed.model
        .commit_annotation(readshot_core::Annotation::Crop { rect });
    ed.refresh_image();
    let (w, h) = ed.effective_image_size();
    ed.set_status(format!(
        "Cropped to {w}\u{a0}×\u{a0}{h}\u{a0}px. {UNDO_HINT}"
    ));
    sync_editor_history(ed, coord);
    true
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
            ed.set_status(format!("Updated text. {UNDO_HINT}"));
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
    ed.set_status(format!("Added text. {UNDO_HINT}"));
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

/// Whether an options-bar or Beautify slider preview is live (held,
/// or moved by key or Ctrl+wheel and not committed yet). An Escape
/// cancels it and does nothing else.
fn slider_preview_live(ed: &crate::editor::EditorSession) -> bool {
    ed.style_drag_baseline.is_some()
        || (ed.snapshot_drag_baseline.is_some() && !ed.gap_commit_on_land)
}

pub(crate) fn cancel_editor_previews(ed: &mut crate::editor::EditorSession) {
    // A gap drag already released (its commit waiting on an off-thread
    // compose) is kept, not cancelled: settled now as its step, with the
    // running compose's result.
    super::compose_jobs::settle_released_gap(ed);
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
    // Only a slider still held keeps sending ticks that must be ignored;
    // a key or Ctrl+wheel preview has none, and its flag would swallow
    // the next drag.
    if let Some(drag) = ed.style_drag_baseline.take() {
        ed.style_drag_cancelled |= ed.pointer_held;
        ed.model.set_tool_style(drag.tool_style);
        if let Some(baseline) = drag.annotations {
            ed.model.cancel_preview_from_baseline(baseline);
            refresh = true;
        }
    }
    // A coalesced gap tick (or an off-thread gap compose) belongs to the
    // drag being dropped.
    ed.layout_gap_pending = None;
    super::compose_jobs::cancel_gap_compose(ed);
    if let Some(baseline) = ed.snapshot_drag_baseline.take() {
        ed.model.cancel_preview_snapshot(baseline);
        ed.snapshot_drag_cancelled |= ed.pointer_held;
        refresh = true;
    }
    if refresh {
        ed.refresh_image();
    }
    // A cancelled geometry preview puts the document back in another
    // frame: OCR boxes built meanwhile must follow it.
    super::text_panel::refresh_text_if_stale(ed);
}

/// Keep an open snapshot slider preview as one undo step. Slider
/// keys and Ctrl+wheel preview without ever sending a release, so
/// anything that outputs the image settles it first.
pub(crate) fn commit_snapshot_drag(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) {
    // The gap slider's newest value may still be waiting on its throttle
    // or composing off the UI thread: shown now, so it is what is kept.
    super::combine::flush_gap_preview(ed);
    ed.gap_commit_on_land = false;
    if let Some(baseline) = ed.snapshot_drag_baseline.take() {
        if ed.model.commit_preview_snapshot(baseline) {
            ed.clear_discard_confirmation();
            ed.refresh_image();
            sync_editor_history(ed, coord);
        }
    }
}

/// Keep an open style preview (an options-bar slider, which arrow keys
/// drive without ever sending a release) as one undo step, as its
/// release would. Returns whether the selection changed.
pub(crate) fn commit_style_drag(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) -> bool {
    let Some(baseline) = ed.style_drag_baseline.take().and_then(|d| d.annotations) else {
        return false;
    };
    let changed = ed.model.commit_preview_from_baseline(baseline);
    ed.refresh_image();
    if changed {
        ed.clear_discard_confirmation();
        sync_editor_history(ed, coord);
    }
    changed
}

/// Settle the editor before Save, Copy or Pin, so the output matches the
/// screen: slider previews are kept (snapshot, then style), any other
/// preview is dropped, and only then is the text draft committed — a
/// preview restored after it would put back annotations without the
/// text (the [`super::image_menu::prepare`] order). An open crop draft is
/// applied last.
pub(crate) fn settle_editor_for_output(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) {
    // A resize / alignment still composing came before anything below.
    super::compose_jobs::settle_all(ed, coord);
    commit_snapshot_drag(ed, coord);
    commit_style_drag(ed, coord);
    cancel_editor_previews(ed);
    commit_pending_editor_text(ed, coord);
    apply_crop_draft(ed, coord);
    sync_owed_history(ed, coord);
    // The output renders the present: reuse a render of it in flight.
    super::compose_jobs::settle_flatten(ed);
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
    if was == crate::editor::EditorPopover::Resize {
        ed.resize_draft = None;
    }
    // Keyboard focus goes back to the control that opened the popover
    // (an options-bar Color or Width), or to the canvas when the popover
    // was its own zone (Image menu, Resize).
    match ed.key_focus_return.take() {
        Some(back) => ed.key_focus = Some(back),
        None => {
            if ed.key_focus.is_some_and(|f| {
                matches!(
                    f.zone,
                    crate::editor::FocusZone::ImageMenu | crate::editor::FocusZone::Resize
                )
            }) {
                ed.key_focus = None;
            }
        }
    }
    true
}

/// Run the History sync a step taken inside `cancel_editor_previews`
/// still owes (see `EditorSession::history_sync_owed`).
pub(crate) fn sync_owed_editor_history(state: &mut App) {
    let coord = state.coordinator.clone();
    if let Some(ed) = state.editor.as_mut() {
        sync_owed_history(ed, &coord);
    }
}

/// [`sync_owed_editor_history`] for a session at hand: call it before
/// the session is dropped (close, replace, pin).
pub(crate) fn sync_owed_history(ed: &mut crate::editor::EditorSession, coord: &CaptureCoordinator) {
    if ed.history_sync_owed {
        sync_editor_history(ed, coord);
    }
}

pub(crate) fn sync_editor_history(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) {
    ed.clear_discard_confirmation();
    ed.history_sync_owed = false;
    // Every history change passes here. A redaction's toast Undo is
    // only valid while that redaction is the newest step: once anything
    // else changes history, drop it for good (depth alone could match
    // again after undo + a different edit, and Undo would remove that).
    if let Some(crate::editor::EditorToastAction::UndoRedaction { undo_depth }) = ed.toast_action {
        if undo_depth != ed.model.undo_depth() {
            ed.toast_action = None;
        }
    }
    super::text_panel::prune_bake_sources(ed);
    write_editor_history(ed, coord);
}

/// Write the editor's state to its History record, if it has one.
/// History stores the unbeautified document in its current frame
/// ([`history_geometry_id`]); every write carries that frame id.
///
/// - Any redaction ([`readshot_core::scrub::has_redaction`]): **scrub**.
///   History gets the flattened output as its image (rendered and
///   encoded on the writer thread) and the text with every hidden word
///   masked; the annotations are baked in, so the sidecar stores none.
/// - No redaction, but History may hold a scrub (all undone), or its
///   image is in another frame than the editor's (rotate, resize,
///   combine, or an undo back to the original), or its state is unknown
///   after a failed write: **rewrite** the image as the current document
///   with the unmasked text, annotations still editable.
/// - Otherwise the plain sidecar sync (annotations only).
///
/// The Text panel's word boxes are rebuilt first when the geometry moved,
/// so the masking always uses boxes in the redactions' frame.
///
/// Everything is queued on the coordinator's ordered background writer
/// (the writes fsync, and this runs on every commit/undo/redo); rapid
/// rewrites coalesce to the newest. A rewrite's outcome comes back as
/// `Message::EditorHistoryWritten` via [`history_write_followups`].
pub(crate) fn write_editor_history(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
) {
    // A geometry change (or an undo across one) moves every OCR box.
    super::text_panel::refresh_text_if_stale(ed);
    let frame = history_geometry_id(ed);
    let Some(record) = ed.source_record.as_mut() else {
        return;
    };
    record.annotation_model = ed.model.annotations().to_vec();
    record.geometry_id = frame.clone();
    // History stores the document's current base (rotated, resized,
    // combined…), so its size is the base's. A rewrite also sets it from
    // what it renders; a plain sync must not put the opened size back.
    record.width_px = ed.model.base().width();
    record.height_px = ed.model.base().height();
    let mut record = record.clone();
    // A reopened scrubbed record keeps its baked-in redactions, mapped
    // into this frame, so text recognized later is still masked with
    // them (none when they can't be placed: the text then stays as
    // masked as it is, or absent).
    // Complete only when every redaction already in the image is known:
    // the opened image was never scrubbed, or its own list was complete.
    // Unknown (`None`): the new redactions are stored, but never as a
    // complete list, so no text is ever masked with them.
    let inherited = inherited_redactions(ed);
    let complete = inherited.is_some();
    record.redactions = inherited.unwrap_or_default();
    // A restore of a never-scrubbed image is a clean, unredacted record.
    record.redactions_complete = record.redacted && complete;
    let annotations = ed.model.annotations();
    let known = crate::editor::HistoryFrame::Known(frame);
    if readshot_core::scrub::has_redaction(annotations) {
        let mut scrubbed = record;
        scrubbed.ocr_text = scrubbed_history_text(ed);
        scrubbed.annotation_model = Vec::new();
        scrubbed.redacted = true;
        scrubbed.redactions_complete = complete;
        // Redactions as drawn (whole pixels, a resize's fringe included):
        // the stored image hides those, and so the stored redactions say.
        let bake = ed.model.drawn_annotations();
        ed.history_scrubbed = true;
        ed.history_frame_written = known;
        ed.history_write =
            Some(coord.rewrite_history_async(scrubbed, ed.model.shared_base(), bake));
    } else if ed.history_scrubbed || ed.history_frame_written != known {
        // Restore after a scrub, or the frame changed: the image itself
        // must change, with the annotations still editable.
        let mut restored = record;
        restored.ocr_text = history_plain_text(ed);
        // A reopened scrubbed record stays a scrub (`redacted` as loaded):
        // its image still has the redactions baked in.
        ed.history_scrubbed = false;
        ed.history_frame_written = known;
        ed.history_write =
            Some(coord.rewrite_history_async(restored, ed.model.shared_base(), Vec::new()));
    } else {
        // `None` keeps the stored text: it may be the merged text of
        // several parts, or text masked since the editor opened.
        record.ocr_text = None;
        coord.update_history_async(record);
    }
}

/// The opened image is a scrubbed History record: redactions are baked
/// into its pixels, so recognition of it may still read weakly hidden
/// words and its raw text must never be stored.
fn source_scrubbed(ed: &crate::editor::EditorSession) -> bool {
    ed.source_record.as_ref().is_some_and(|r| r.redacted)
}

/// The redactions baked into the opened image (a scrubbed record's
/// stored ones, in its own px = the original part's), mapped into the
/// present document frame. Empty when the record isn't scrubbed; `None`
/// when they can't be placed (no stored redactions, a list not known complete, or the original part
/// isn't in the document): callers then fail closed.
fn inherited_redactions(
    ed: &crate::editor::EditorSession,
) -> Option<Vec<readshot_core::Annotation>> {
    let Some(record) = ed.source_record.as_ref().filter(|r| r.redacted) else {
        return Some(Vec::new());
    };
    // Only a list known to hold every baked-in redaction can be used.
    if !record.redactions_complete || !readshot_core::scrub::has_redaction(&record.redactions) {
        return None;
    }
    // Through any bake (Add Capture on a transformed document): the
    // original part then lives inside the baked one.
    let m = super::text_panel::original_part_affine(&ed.text_panel, ed.model.geometry())?;
    let mapped: Vec<_> = record
        .redactions
        .iter()
        .map(|a| readshot_core::document::map_annotation(a, &m))
        .collect();
    // Mapping is exact; mask with the whole pixels they cover.
    Some(ed.model.geometry().cover_redactions(&mapped))
}

/// `doc` with every word hidden by `hide` masked; `None` when it can't be
/// masked ([`readshot_core::scrub::hidden_spans`]) and must be withheld.
fn masked_text(
    doc: &readshot_core::text_lines::TextDocument,
    hide: &[readshot_core::Annotation],
) -> Option<String> {
    readshot_core::scrub::masked_text(doc, hide)
}

/// History's id for the document frame the editor shows now (D2): the
/// opened image's own frame while the geometry is untouched, else this
/// session's nonce plus the geometry fingerprint.
pub(crate) fn history_geometry_id(ed: &crate::editor::EditorSession) -> Option<String> {
    match ed.model.geometry().fingerprint() {
        None => ed.history_geometry_base.clone(),
        Some(f) => Some(format!("{}:{f}", ed.history_nonce)),
    }
}

/// Text for a rewrite without (new) redactions: the panel's (merged)
/// document when it covers several parts, else the capture's text. For a
/// reopened scrubbed record never the panel's raw text: its document
/// masked with the inherited redactions, else the stored (masked) text.
fn history_plain_text(ed: &crate::editor::EditorSession) -> Option<String> {
    if source_scrubbed(ed) {
        if let (Some(doc), Some(hide)) = (
            super::text_panel::current_positioned_document(ed),
            inherited_redactions(ed),
        ) {
            return masked_text(doc, &hide);
        }
        // The stored masked text is the opened image's alone.
        return only_the_original_part(ed)
            .then(|| original_history_text(ed))
            .flatten();
    }
    if !only_the_original_part(ed) {
        // Combined (or baked): the merged text of every part, or none
        // while a part has no recognition yet (decision D2).
        return ed.text_panel.document().map(|doc| doc.layout_text.clone());
    }
    original_history_text(ed)
}

/// The capture's unredacted text: what History held when the editor
/// opened, or (a fresh capture, whose text lands later) the Text panel's
/// recognition of the same base image — never for a scrubbed record,
/// whose stored text is the only masked one.
fn original_history_text(ed: &crate::editor::EditorSession) -> Option<String> {
    if source_scrubbed(ed) {
        return ed.source_record.as_ref().and_then(|r| r.ocr_text.clone());
    }
    ed.source_record
        .as_ref()
        .and_then(|r| r.ocr_text.clone())
        .or_else(|| ed.text_panel.document().map(|d| d.layout_text.clone()))
}

/// The text History keeps while redactions are present: the recognized
/// text with every hidden word masked, using the Text panel's document
/// (word boxes). Only positioned text can be masked
/// ([`readshot_core::scrub::hidden_spans`]), so without a positioned
/// document **no** text is stored (fail closed):
/// - an engine that reports positions (Apple Vision) will produce one; the
///   precise text is stored when recognition lands, editor open or not
///   (`text_panel::store_recognized_text`);
/// - an engine without positions (Windows, Linux) never will: nothing can
///   tell which words a hand-drawn redaction hides, so the record keeps no
///   text at all (History shows "Text hidden", MCP serves `null`). Masking
///   only the detected items would keep a hand-redacted name readable.
///   Recognizing the baked image again is no fix either: a weak Blur or
///   Pixelate can leave words recognition still reads.
///
/// `None` also when no text is known at all, and whenever the document
/// has a part other than the opened image with no recognition yet (no
/// text rather than text nothing could mask: decision D2).
pub(crate) fn scrubbed_history_text(ed: &crate::editor::EditorSession) -> Option<String> {
    // The editor's redactions plus, for a reopened scrubbed record, the
    // ones baked into its image (recognition may still read words under a
    // weak blur).
    let inherited = inherited_redactions(ed);
    let mut hide = ed.model.drawn_annotations();
    hide.extend(inherited.iter().flatten().cloned());
    let doc = super::text_panel::current_positioned_document(ed)?;
    inherited.as_ref()?;
    if let Some(crop) = history_crop(ed) {
        // The stored image is the crop: only text whose boxes are inside
        // it may be stored.
        return cropped_history_document(doc, &hide, crop).map(|d| d.layout_text);
    }
    masked_text(doc, &hide)
}

/// The crop History's image gets from a write now: a scrub bakes every
/// annotation, a Crop included
/// ([`CaptureCoordinator::rewrite_history_async`]); every other write
/// stores the uncropped document. `None`: no crop.
pub(crate) fn history_crop(
    ed: &crate::editor::EditorSession,
) -> Option<readshot_core::render::OutputCrop> {
    let annotations = ed.model.annotations();
    if !readshot_core::scrub::has_redaction(annotations) {
        return None;
    }
    let base = ed.model.base();
    readshot_core::render::output_crop(base.width(), base.height(), annotations)
}

/// What a scrub hides: the editor's annotations plus, for a reopened
/// scrubbed record, the redactions baked into its image. `None` when
/// those can't be placed ([`inherited_redactions`]): fail closed.
pub(crate) fn history_hide(
    ed: &crate::editor::EditorSession,
) -> Option<Vec<readshot_core::Annotation>> {
    let mut hide = ed.model.drawn_annotations();
    hide.extend(inherited_redactions(ed)?);
    Some(hide)
}

/// `doc` (document px) as History may store it for an image cropped to
/// `crop`: words hidden by `hide` masked, then cut to the words wholly
/// inside the crop ([`readshot_core::scrub::document_in_crop`]) in the
/// cropped image's px, then masked again with `hide` moved into those px
/// (a redaction clipped by the crop still hides what it hid before). The
/// one path every cropped History text takes. `None` without positions
/// (fail closed).
pub(crate) fn cropped_history_document(
    doc: &readshot_core::text_lines::TextDocument,
    hide: &[readshot_core::Annotation],
    crop: readshot_core::render::OutputCrop,
) -> Option<readshot_core::text_lines::TextDocument> {
    use readshot_core::scrub::{document_in_crop, hidden_spans, mask_document, redactions_in_crop};
    let premasked = mask_document(doc, &hidden_spans(doc, hide)?);
    let cut = document_in_crop(&premasked, crop)?;
    let hide = redactions_in_crop(hide, crop);
    Some(mask_document(&cut, &hidden_spans(&cut, &hide)?))
}

/// The document is made of the opened image alone (perhaps transformed),
/// so the capture's own text is all the text it has.
fn only_the_original_part(ed: &crate::editor::EditorSession) -> bool {
    let parts = &ed.model.geometry().parts;
    parts.len() == 1 && parts[0].id == readshot_core::document::ORIGINAL_PART
}

/// Follow-ups after a message that may have written History: await the
/// newest rewrite's outcome, and when a redaction exists but the panel
/// has no recognition (never ran, or failed) and the engine reports
/// positions, start recognition. While the document is the original
/// frame its result is stored in History by the task itself, so the
/// precise masked text lands even if the editor closes first; in any
/// other frame only the editor stores it (on `EditorTextRecognized`), and
/// History keeps no text until then.
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

/// Not while a popover or a text field has the keys: Tab out of a hex
/// field must not move the selection under the value.
fn tab_may_cycle(state: &App) -> bool {
    state.editor.as_ref().is_some_and(|ed| {
        !(ed.busy
            || ed.pending_text.is_some()
            || ed.move_drag.is_some()
            || ed.popover.is_some()
            || ed.beautify_hex_draft.is_some())
    })
}

/// Whether the Beautify tab (with its hex field) is on screen.
pub(crate) fn beautify_hex_on_screen(state: &App) -> bool {
    state.editor.as_ref().is_some_and(|ed| {
        matches!(
            editor_panel_mode(state, ed),
            PanelMode::Docked | PanelMode::Overlay
        ) && crate::editor::PanelTab::from_id(&state.preferences.editor_panel_tab)
            == crate::editor::PanelTab::Beautify
    })
}

/// Tab on the canvas: select the next (previous) annotation.
fn cycle_selection_by_tab(state: &mut App, step: i32) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    let n = ed.model.annotations().len();
    let Some(next) = cycle_selection_index(n, ed.model.selected_annotation(), step) else {
        return Task::none();
    };
    cancel_editor_previews(ed);
    ed.model.select_index(Some(next));
    if let Some(kind) = ed.model.selected_kind_label() {
        ed.set_guide(
            format!(
                "{kind} selected ({} of {n}). Arrows move it, {DELETE_KEY} removes it, {ESC_KEY} deselects.",
                next + 1
            ),
            crate::editor::KeyGuide::Selection,
        );
    }
    // Takes the caret out of any other text field, so typing goes to
    // the canvas keys.
    super::key_focus::unfocus_text_fields()
}

/// The annotation Tab (`step` 1) or ⇧Tab (-1) selects next among `n`,
/// wrapping; from no selection Tab starts at the first and ⇧Tab at the
/// last. `None` when there are none.
pub(crate) fn cycle_selection_index(n: usize, current: Option<usize>, step: i32) -> Option<usize> {
    if n == 0 {
        return None;
    }
    let n = n as i64;
    let next = match current {
        Some(c) => (c as i64 + i64::from(step.signum())).rem_euclid(n),
        None if step < 0 => n - 1,
        None => 0,
    };
    Some(next as usize)
}

/// The handle ⌥ + arrow drags on a selected annotation: a box's
/// bottom-right corner, a line's end, a magnifier's bubble (so the bubble
/// moves away from its source without a drag). Text, pins, stamps and
/// freehand strokes have none.
pub(crate) fn keyboard_resize_handle(
    a: &readshot_core::Annotation,
) -> Option<readshot_ui::editor::ResizeHandle> {
    use readshot_core::Annotation as A;
    use readshot_ui::editor::ResizeHandle as H;
    match a {
        A::Rectangle { .. }
        | A::Ellipse { .. }
        | A::Blur { .. }
        | A::Pixelate { .. }
        | A::Spotlight { .. }
        | A::Crop { .. } => Some(H::SouthEast),
        A::Line { .. } | A::Arrow { .. } | A::Magnifier { .. } => Some(H::End),
        _ => None,
    }
}

/// The part of the base image on screen (the current crop, or all of
/// it), in base pixels: where a crop draft may go.
pub(crate) fn visible_frame(ed: &crate::editor::EditorSession) -> readshot_core::RectLike {
    let (x, y) = ed.crop_offset();
    let (w, h) = ed.effective_image_size();
    readshot_core::RectLike::new(x, y, w as f32, h as f32)
}

/// Smallest side a keyboard-sized crop draft keeps.
const MIN_KEY_CROP: f32 = 8.0;

/// A crop draft started from the keyboard (C with no draft): the visible
/// image inset by a tenth on each side, at the chosen aspect when there
/// is one.
pub(crate) fn keyboard_crop_draft(
    frame: readshot_core::RectLike,
    ratio: Option<f32>,
) -> readshot_core::RectLike {
    let inset = readshot_core::RectLike::new(
        frame.x + frame.width * 0.1,
        frame.y + frame.height * 0.1,
        (frame.width * 0.8).max(1.0),
        (frame.height * 0.8).max(1.0),
    );
    match ratio {
        Some(r) => readshot_ui::editor::style::fit_aspect(inset, r),
        None => inset,
    }
}

/// `draft` moved by `(dx, dy)`, kept inside `frame`.
pub(crate) fn move_crop_draft(
    draft: readshot_core::RectLike,
    dx: f32,
    dy: f32,
    frame: readshot_core::RectLike,
) -> readshot_core::RectLike {
    let max_x = (frame.x + frame.width - draft.width).max(frame.x);
    let max_y = (frame.y + frame.height - draft.height).max(frame.y);
    readshot_core::RectLike::new(
        (draft.x + dx).clamp(frame.x, max_x),
        (draft.y + dy).clamp(frame.y, max_y),
        draft.width,
        draft.height,
    )
}

/// `draft` grown (or shrunk) from its bottom-right edge by `(dx, dy)`,
/// inside `frame` and at least [`MIN_KEY_CROP`] a side. With an aspect
/// `ratio` the other side follows (the horizontal step leads).
pub(crate) fn resize_crop_draft(
    draft: readshot_core::RectLike,
    dx: f32,
    dy: f32,
    ratio: Option<f32>,
    frame: readshot_core::RectLike,
) -> readshot_core::RectLike {
    let max_w = (frame.x + frame.width - draft.x).max(MIN_KEY_CROP);
    let max_h = (frame.y + frame.height - draft.y).max(MIN_KEY_CROP);
    let (w, h) = match ratio {
        Some(r) => {
            let lead = if dx != 0.0 { dx } else { dy * r };
            let mut w = (draft.width + lead).clamp(MIN_KEY_CROP, max_w);
            let mut h = w / r;
            if h > max_h {
                h = max_h;
                w = h * r;
            }
            (w, h.max(MIN_KEY_CROP))
        }
        None => (
            (draft.width + dx).clamp(MIN_KEY_CROP, max_w),
            (draft.height + dy).clamp(MIN_KEY_CROP, max_h),
        ),
    };
    readshot_core::RectLike::new(draft.x, draft.y, w, h)
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
            // An Escape during a live canvas drag ends the drag only,
            // whichever of its two messages comes first.
            if let Some(ed) = state.editor.as_mut() {
                let escape = matches!(
                    key,
                    iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
                );
                let seen = ed.escape_ended_drag.take();
                if escape {
                    if seen == Some(crate::editor::EscapeSeenBy::Canvas) {
                        return Task::none(); // the canvas already spent it
                    }
                    // Captured by the canvas (`!status_ignored`): its
                    // `DragEscaped` follows and must do nothing more.
                    if !status_ignored && ed.canvas_drag_in_progress() {
                        ed.canvas_drag_active = false;
                        cancel_editor_previews(ed);
                        ed.escape_ended_drag = Some(crate::editor::EscapeSeenBy::Key);
                        return Task::none();
                    }
                }
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
            // The keyboard focus outside the canvas (Image menu, options
            // bar, side panel, Resize) gets the key first.
            if let Some(task) =
                super::key_focus::handle_focus_key(state, &key, modifiers, status_ignored)
            {
                return task;
            }
            let message = editor_key_message(key, modifiers, status_ignored);
            // C from the keyboard: a crop draft to move and size with the
            // arrows, since the pointer is not there to drag one (WCAG
            // 2.1.1 / 2.5.7). The toolbar button still waits for a drag.
            let crop_key = matches!(
                message,
                Some(Message::EditorToolbar(
                    readshot_ui::ToolbarMessage::SelectTool(readshot_ui::editor::ToolState::Crop)
                ))
            );
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
                matches!(
                    ed.popover,
                    Some(
                        crate::editor::EditorPopover::Color | crate::editor::EditorPopover::Resize
                    )
                ) || ed.pending_text.is_some()
            }) {
                return Task::none();
            }
            let task = match message {
                Some(message) => update(state, message),
                None => Task::none(),
            };
            if crop_key {
                if let Some(ed) = state.editor.as_mut() {
                    if !ed.busy
                        && ed.crop_draft.is_none()
                        && ed.model.active_tool() == readshot_ui::editor::ToolState::Crop
                    {
                        let ratio = ed.model.effective_style().crop_aspect.ratio();
                        ed.crop_draft = Some(keyboard_crop_draft(visible_frame(ed), ratio));
                        ed.set_guide(
                            format!(
                                "Arrows move the crop, {OPTION_KEY} + arrows resize it ({SHIFT_KEY} for 10\u{a0}px). {RETURN_KEY} crops, {ESC_KEY} cancels."
                            ),
                            crate::editor::KeyGuide::CropDraft,
                        );
                    }
                }
            }
            task
        }
        Message::EditorStatusTick => {
            // Auto-dismiss stale, non-in-progress status pills *here* —
            // the authoritative place — so the `EditorStatusTick`
            // subscription (gated on `status.is_some() &&
            // !status_is_in_progress()`) actually stops firing once a
            // toast has expired. Previously the dismissal only happened
            // locally in `editor_view`, which can't clear `ed.status`, so
            // the 1 Hz tick + redraw ran for the entire life of the
            // editor window. An armed Discard is sticky: it waits for
            // Keep Editing, Discard or the next edit (WCAG 2.2.1).
            if let Some(ed) = state.editor.as_mut() {
                if !ed.status_is_in_progress()
                    && !ed.status_is_sticky()
                    && ed
                        .status_set_at
                        .is_some_and(|t| t.elapsed() > crate::editor::STATUS_AUTO_DISMISS)
                {
                    ed.status = None;
                    ed.status_set_at = None;
                    ed.toast_action = None;
                }
                // A key guide whose draft, zone or selection has ended
                // goes with it.
                if ed.key_guide_live() == Some(false) {
                    ed.status = None;
                    ed.status_set_at = None;
                    ed.key_guide = None;
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
            // The output must match what is on screen.
            settle_editor_for_output(ed, &state.coordinator);
            let out = match editor_output(ed) {
                Ok(out) => out,
                Err(e) => {
                    ed.set_warning(super::beautify_panel::beautify_refusal("save", e));
                    return Task::none();
                }
            };
            ed.busy = true;
            ed.set_status("Choose a save location…");
            let seed = preferred_save_seed_dir(&state.preferences, &state.last_save_dir);
            let template = state.preferences.filename_template.clone();
            Task::perform(
                async move {
                    let img = out.render_off_thread().await?;
                    save_image_via_picker(img, seed, template)
                        .await
                        .map_err(|e| e.to_string())
                },
                Message::EditorSaved,
            )
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
            // The output must match what is on screen.
            settle_editor_for_output(ed, &state.coordinator);
            let out = match editor_output(ed) {
                Ok(out) => out,
                Err(e) => {
                    ed.set_warning(super::beautify_panel::beautify_refusal("copy", e));
                    return Task::none();
                }
            };
            ed.busy = true;
            ed.set_status(if ed.model.beautify().is_identity() {
                "Copying…".to_string()
            } else {
                "Copying beautified image…".to_string()
            });
            Task::perform(
                async move {
                    let img = out.render_off_thread().await?;
                    copy_image_to_clipboard(img)
                        .await
                        .map_err(|e| e.to_string())
                },
                Message::EditorCopyImageDone,
            )
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

        Message::EditorSnapshotSliderCommit => {
            if let Some(ed) = state.editor.as_mut() {
                // Release ends the drag: a cancelled one commits nothing.
                ed.snapshot_drag_cancelled = false;
                return super::combine::release_gap_slider(ed, &state.coordinator);
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
            // Previews first: one restored after the text commit would
            // drop the text (the `image_menu::prepare` order).
            cancel_editor_previews(ed);
            commit_pending_editor_text(ed, &state.coordinator);
            // Copy All (spec §3.7, ⇧⌘C, also the action bar's Copy Text):
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
                ed.set_status(super::text_panel::COPY_TEXT_WITHHELD);
                return Task::none();
            }
            let neutral = matches!(mask, CopyAllMask::None)
                && readshot_core::redact::panel_text_matches_output(ed.model.annotations());
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
            Task::perform(ocr_then_copy(coord, img, prefs, remove_breaks, mask), |r| {
                Message::EditorCopyTextDone(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyTextDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                ed.set_status(match result {
                    Err(e) if e == super::text_panel::COPY_TEXT_WITHHELD => e,
                    Ok(text) if text.is_empty() => "No text recognized.".into(),
                    Ok(text) => format!(
                        "Copied {} of text.",
                        counted(text.chars().count(), "character", "characters")
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
            // click before closing; the arm waits for an answer.
            let dirty = state
                .editor
                .as_ref()
                .map(editor_has_unsaved_work)
                .unwrap_or(false);
            if dirty {
                if let Some(ed) = state.editor.as_mut() {
                    let now = std::time::Instant::now();
                    let armed = ed.discard_pending_at.is_some();
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
                    if t != readshot_ui::editor::ToolState::Crop {
                        ed.crop_draft = None;
                    }
                    if t != readshot_ui::editor::ToolState::Text {
                        commit_pending_editor_text(ed, &state.coordinator);
                    }
                    let changed = ed.model.active_tool() != t;
                    ed.model.set_tool(t);
                    // The tool's how-to, now that the options bar has
                    // taken the old hint line (spec §3.2): a notice when
                    // the tool is chosen. Select keeps its hint line.
                    if changed
                        && !super::editor_options::visible_option_controls(ed).is_empty()
                        && hint_may_show(ed)
                    {
                        ed.set_info(super::view::editor_compact_hint(t));
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
                    ed.crop_draft = None;
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    if let Some(task) = super::compose_jobs::queue_history_move(
                        ed,
                        crate::editor::ComposeKind::Undo,
                    ) {
                        return task;
                    }
                    if ed.model.undo() {
                        ed.refresh_image();
                        ed.set_status(format!("Undid edit. {REDO_HINT}"));
                        sync_editor_history(ed, &state.coordinator);
                    }
                }
                readshot_ui::ToolbarMessage::Redo => {
                    ed.crop_draft = None;
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    if let Some(task) = super::compose_jobs::queue_history_move(
                        ed,
                        crate::editor::ComposeKind::Redo,
                    ) {
                        return task;
                    }
                    if ed.model.redo() {
                        ed.refresh_image();
                        ed.set_status(format!("Redid edit. {UNDO_HINT}"));
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
                    ed.set_status(format!("Updated selected annotation size. {UNDO_HINT}"));
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
            if let (readshot_ui::editor::style::StyleMessage::CropAspect(a), Some(d)) =
                (msg, ed.crop_draft)
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
                ed.set_status("Couldn’t read that color. Use a hex value like #4F46E5.");
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
                    if let Some(eye) = ed.eyedropper.take() {
                        let shown =
                            readshot_core::eyedropper::displayed_base_point(p, ed.crop_offset());
                        if let Some(color) =
                            readshot_core::eyedropper::sample(ed.model.base(), shown)
                        {
                            match eye.target {
                                crate::editor::EyedropperTarget::Annotation => {
                                    let undoable =
                                        apply_editor_color(ed, &state.coordinator, color);
                                    ed.set_status(if undoable {
                                        format!("Picked {}. {UNDO_HINT}", color.to_hex())
                                    } else {
                                        format!("Picked {}.", color.to_hex())
                                    });
                                    picked = Some(color);
                                }
                                crate::editor::EyedropperTarget::BeautifyBackground => {
                                    // A pick replaces whatever was typed.
                                    ed.beautify_hex_draft = None;
                                    cancel_editor_previews(ed);
                                    ed.snapshot_drag_cancelled = false;
                                    let mut s = ed.model.beautify();
                                    s.apply(readshot_core::beautify::BeautifyEdit::Background(
                                        readshot_core::beautify::Background::Solid {
                                            rgba: color.to_rgba8(),
                                        },
                                    ));
                                    if ed.model.set_beautify(s) {
                                        ed.clear_discard_confirmation();
                                        ed.set_status(format!(
                                            "Background set to {}. {UNDO_HINT}",
                                            color.to_hex()
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
                readshot_ui::CanvasMessage::DragStarted => {
                    // A shape/freehand drag is now live; Escape should cancel
                    // it rather than trigger discard.
                    ed.canvas_drag_active = true;
                    ed.escape_ended_drag = None;
                    // The frame its points are in: the one shown, which is
                    // the present one (a press in another was dropped).
                    ed.drag_started_in =
                        Some(crate::editor::CanvasDragFrame::Live(ed.shown_frame()));
                }
                readshot_ui::CanvasMessage::DragMoved(_)
                | readshot_ui::CanvasMessage::PolylineMoved(_) => {
                    // Preview-only events. The canvas's own State holds
                    // the drag points; a redraw is automatic.
                }
                readshot_ui::CanvasMessage::Cancelled => {
                    ed.canvas_drag_active = false;
                    ed.drag_started_in = None;
                    // One Esc = one step: a live slider preview cancelled
                    // here is this Escape's step, so its key press (which
                    // follows) must not go on to the selection or draft.
                    let slider = slider_preview_live(ed);
                    cancel_editor_previews(ed);
                    if slider {
                        ed.escape_ended_drag = Some(crate::editor::EscapeSeenBy::Canvas);
                    }
                }
                readshot_ui::CanvasMessage::DragEscaped => {
                    ed.drag_started_in = None;
                    // This Escape ends the live drag only (a re-drag over
                    // an open crop draft brings the draft back; a Select
                    // move or resize goes back, still selected). The next
                    // Esc does the rest. The key press of the same Escape
                    // may come before or after this.
                    if ed.escape_ended_drag.take() == Some(crate::editor::EscapeSeenBy::Key) {
                        // The key already ended the re-drag.
                        ed.canvas_drag_active = false;
                        return Task::none();
                    }
                    ed.escape_ended_drag = ed
                        .canvas_drag_in_progress()
                        .then_some(crate::editor::EscapeSeenBy::Canvas);
                    ed.canvas_drag_active = false;
                    cancel_editor_previews(ed);
                }
                readshot_ui::CanvasMessage::SelectPressed(p) => {
                    ed.escape_ended_drag = None;
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    // A press mapped from the frame shown before hit-tests
                    // at the scale it was made at, in present document px.
                    let scale = ed
                        .press_remap
                        .as_ref()
                        .filter(|r| r.view_seq == ed.canvas_view_seq.get())
                        .map_or_else(|| ed.render_scale.get(), |r| r.scale);
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
                    } else if let Some(selected_index) = {
                        let before = ed.model.selected_annotation();
                        let picked = ed.model.select_at_scaled(p, scale);
                        // A new selection explains itself while it lasts
                        // (as a Tab selection does).
                        if picked.is_some() && picked != before && hint_may_show(ed) {
                            if let Some(kind) = ed.model.selected_kind_label() {
                                ed.set_guide(
                                    editor_selected_hint(
                                        kind,
                                        ed.model.selected_text_edit().is_some(),
                                    ),
                                    crate::editor::KeyGuide::Selection,
                                );
                            }
                        }
                        picked
                    } {
                        // A press on an unselected magnifier's bubble,
                        // source centre or rim grabs that part on this
                        // first drag too, as the second drag would; only
                        // elsewhere does it move the whole magnifier.
                        let handle = (ed.model.selected_kind_label() == Some("Magnifier"))
                            .then(|| ed.model.resize_handle_at_scaled(p, scale))
                            .flatten();
                        let kind = match handle {
                            Some(handle) => crate::editor::MoveDragKind::Resize(handle),
                            None => crate::editor::MoveDragKind::Move,
                        };
                        ed.move_drag = Some(crate::editor::MoveDrag {
                            baseline: ed.model.annotations().to_vec(),
                            selected_index,
                            start: p,
                            moved: false,
                            kind,
                        });
                    } else {
                        ed.move_drag = None;
                    }
                }
                readshot_ui::CanvasMessage::SelectDragged(p) => {
                    let mut background_without_selected = None;
                    // A redaction dragged on a document small enough to
                    // render cheaply (under 4 MP) is rendered where it is
                    // on each tick, so its real effect (pixelate, blur)
                    // shows while it moves; on a bigger one the canvas
                    // covers it opaquely and the release renders it.
                    let small = ed.renders_redaction_drags_live();
                    let mut render_present = false;
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
                            let redaction = drag
                                .baseline
                                .get(drag.selected_index)
                                .is_some_and(readshot_core::Annotation::is_redaction);
                            if small && redaction {
                                render_present = true;
                            } else if !drag.moved
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
                        ed.show_without_dragged(index);
                    }
                    if render_present {
                        ed.refresh_image();
                    }
                }
                readshot_ui::CanvasMessage::SelectReleased => {
                    let Some(drag) = ed.move_drag.take() else {
                        return Task::none();
                    };
                    if !drag.moved {
                        // A preview tick that changed nothing (a refused
                        // resize) still opened an entry; close it so it
                        // can't pass for a later preview from the same
                        // baseline.
                        ed.model.end_annotation_preview();
                        return Task::none();
                    }
                    if !ed.model.commit_preview_from_baseline(drag.baseline) {
                        ed.refresh_image();
                        return Task::none();
                    }
                    ed.refresh_image();
                    let verb = match drag.kind {
                        crate::editor::MoveDragKind::Move => "Moved",
                        crate::editor::MoveDragKind::Resize(_) => "Resized",
                    };
                    ed.set_status(format!("{verb} annotation. {UNDO_HINT}"));
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
                    // A drag's shape is in the frame it began in: if the
                    // frame changed since (a pending step landed on its
                    // press, or mid-drag), it would land somewhere else.
                    // Pins and stamps commit on their own press (no drag),
                    // in the frame on screen; a leftover drag frame never
                    // applies to them.
                    let from_drag = !matches!(
                        annotation,
                        readshot_core::Annotation::NumberedPin { .. }
                            | readshot_core::Annotation::Stamp { .. }
                    );
                    let stale = match ed.drag_started_in.take() {
                        Some(crate::editor::CanvasDragFrame::Dropped) => from_drag,
                        Some(crate::editor::CanvasDragFrame::Live(g)) => {
                            from_drag
                                && ed.canvas_drag_active
                                && !crate::editor::same_frame(&g, ed.model.geometry())
                        }
                        None => false,
                    };
                    if stale {
                        ed.canvas_drag_active = false;
                        return Task::none();
                    }
                    ed.canvas_drag_active = false;
                    cancel_editor_previews(ed);
                    if let readshot_core::Annotation::Crop { rect } = annotation {
                        // Crop is a draft until Apply ↵ (spec §3.10). A tool
                        // switched mid-drag must not leave a draft behind.
                        if ed.model.active_tool() == readshot_ui::editor::ToolState::Crop {
                            ed.crop_draft = Some(rect);
                            ed.set_guide(
                                format!("Press {RETURN_KEY} to crop, {ESC_KEY} to cancel."),
                                crate::editor::KeyGuide::CropDraft,
                            );
                        } else {
                            ed.crop_draft = None;
                        }
                        return Task::none();
                    }
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
            // ↵ applies an open crop draft, unless a text field has focus
            // or a text draft is being typed (those own Enter).
            if state.editor.as_ref().is_some_and(|ed| {
                ed.crop_draft.is_some()
                    && ed.pending_text.is_none()
                    && !matches!(
                        ed.popover,
                        Some(
                            crate::editor::EditorPopover::Color
                                | crate::editor::EditorPopover::Resize
                        )
                    )
            }) {
                return handle_editor_message(state, Message::EditorCropApply);
            }
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
                ed.set_status(format!("Deleted annotation. {UNDO_HINT}"));
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
            // An open crop draft owns the arrows: they move it (the
            // keyboard alternative to dragging it, WCAG 2.5.7).
            if let Some(draft) = ed.crop_draft {
                let frame = visible_frame(ed);
                ed.crop_draft = Some(move_crop_draft(draft, dx, dy, frame));
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
        Message::EditorCropApply => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            // Same order as `image_menu::prepare`: end slider/drag
            // previews, then commit any pending text, then the crop.
            cancel_editor_previews(ed);
            commit_pending_editor_text(ed, &state.coordinator);
            apply_crop_draft(ed, &state.coordinator);
            Task::none()
        }
        Message::EditorCropCancel => {
            if let Some(ed) = state.editor.as_mut() {
                ed.crop_draft = None;
            }
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
                // Esc chain: popover → overlay panel → eyedropper → crop draft → slider →
                // text draft → selection → panel lines.
                if ed.eyedropper.take().is_some() {
                    return Task::none();
                }
                if ed.crop_draft.take().is_some() {
                    return Task::none();
                }
                if ed.busy {
                    return Task::none();
                }
                // One Esc = one step: a live options-bar slider drag is
                // cancelled on its own, keeping the selection (and the bar).
                if slider_preview_live(ed) {
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
                let next = ed.zoom.zoom_in_from_display_scale(ed.render_scale.get());
                ed.clear_eyedropper_loupe();
                return super::view::zoom_keeping_center(ed, next);
            }
            Task::none()
        }
        Message::EditorZoomOut => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                // See `EditorZoomIn`.
                let next = ed.zoom.zoom_out_from_display_scale(ed.render_scale.get());
                ed.clear_eyedropper_loupe();
                return super::view::zoom_keeping_center(ed, next);
            }
            Task::none()
        }
        Message::EditorZoomActual => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                let next = ed.actual_size_zoom();
                ed.clear_eyedropper_loupe();
                return super::view::zoom_keeping_center(ed, next);
            }
            Task::none()
        }
        Message::EditorZoomFit => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.busy {
                    return Task::none();
                }
                ed.clear_eyedropper_loupe();
                return super::view::zoom_keeping_center(ed, crate::editor::EditorZoom::Fit);
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
            // The output must match what is on screen.
            settle_editor_for_output(ed, &state.coordinator);
            // A pin opens at once, so its Beautify render stays here; the
            // limit check keeps it within the document budget.
            let img = match editor_output_image(ed) {
                Ok(img) => img,
                Err(e) => {
                    ed.set_warning(super::beautify_panel::beautify_refusal("pin", e));
                    return Task::none();
                }
            };
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
        msg @ (Message::EditorBeautifySavedPresetApplied(..)
        | Message::EditorBeautifyEdit(..)
        | Message::EditorBeautifySliderPreview(..)
        | Message::EditorBeautifySavePreset
        | Message::EditorBeautifyDeletePreset(..)
        | Message::EditorBeautifyRestorePreset
        | Message::EditorBeautifyHexChanged(..)
        | Message::EditorBeautifyHexSubmitted
        | Message::EditorBeautifyPickBackground) => {
            super::beautify_panel::handle_beautify_message(state, msg)
        }
        msg @ (Message::EditorRotate { .. }
        | Message::EditorFlip(..)
        | Message::EditorResizeOpen
        | Message::EditorResizeWidthChanged(..)
        | Message::EditorResizeHeightChanged(..)
        | Message::EditorResizeUnitChanged(..)
        | Message::EditorResizeLockToggled(..)
        | Message::EditorResizeApply) => super::image_menu::handle_image_message(state, msg),
        msg @ (Message::EditorCombineCaptureRequested(..)
        | Message::EditorAddImageFromFile
        | Message::EditorPartLoaded { .. }
        | Message::EditorLayoutGapPreview(..)
        | Message::EditorLayoutGapFlush { .. }
        | Message::EditorLayoutAlign(..)) => super::combine::handle_combine_message(state, msg),
        Message::EditorComposed {
            session,
            seq,
            output,
        } => super::compose_jobs::handle_composed(state, session, seq, output),
        msg @ (Message::EditorImageMenuKey
        | Message::EditorPanelTabKey(..)
        | Message::EditorFocusZoneCycle(..)) => super::key_focus::handle_focus_message(state, msg),
        Message::EditorCycleSelection(step) => {
            if !tab_may_cycle(state) {
                return Task::none();
            }
            // A click can leave the caret in the Beautify hex field without
            // any message reaching the app. While that field is on screen,
            // ask iced whether it has the caret (one round trip) before
            // moving the selection.
            if beautify_hex_on_screen(state) {
                // Collected, so a query no widget answers (the tab closed
                // before the next frame) still resolves, as "not focused":
                // a Tab is never dropped.
                return iced::widget::operation::is_focused(iced::widget::Id::new(
                    super::key_focus::BEAUTIFY_HEX_ID,
                ))
                .collect()
                .map(move |answers| Message::EditorTabResolved {
                    step,
                    field_focused: tab_field_focused(&answers),
                });
            }
            cycle_selection_by_tab(state, step)
        }
        Message::EditorTabResolved {
            step,
            field_focused,
        } => {
            if field_focused {
                // Tab leaves the field; the selection stays.
                return super::key_focus::unfocus_text_fields();
            }
            if !tab_may_cycle(state) {
                return Task::none();
            }
            cycle_selection_by_tab(state, step)
        }
        Message::EditorResizeByKeys(dx, dy) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy || ed.pending_text.is_some() || ed.move_drag.is_some() {
                return Task::none();
            }
            if let Some(draft) = ed.crop_draft {
                let frame = visible_frame(ed);
                let ratio = ed.model.effective_style().crop_aspect.ratio();
                ed.crop_draft = Some(resize_crop_draft(draft, dx, dy, ratio, frame));
                return Task::none();
            }
            let Some(handle) = ed
                .model
                .selected_annotation()
                .and_then(|i| ed.model.annotations().get(i))
                .and_then(keyboard_resize_handle)
            else {
                return Task::none();
            };
            cancel_editor_previews(ed);
            if ed.model.resize_selected_by(handle, dx, dy) {
                ed.refresh_image();
                sync_editor_history(ed, &state.coordinator);
            }
            Task::none()
        }
        other => unreachable!("non-editor message routed to the editor handler: {other:?}"),
    }
}

/// What Copy All may put on the clipboard.
#[derive(Debug)]
pub(crate) enum CopyAllMask {
    /// No redaction anywhere: the text as recognized.
    None,
    /// Mask the words these redactions (output px) hide.
    Hide(Vec<readshot_core::Annotation>),
    /// Redactions that can't be placed: nothing may be copied.
    Withhold,
}

/// How Copy All masks its recognition of the output: with every
/// redaction drawn now **and**, for a reopened scrubbed record, the ones
/// baked into its image ([`history_hide`]) — recognition can still read a
/// word under a weak baked blur. Moved into the output's px (a Crop cuts
/// it). [`CopyAllMask::Withhold`] when the baked-in ones are unknown.
pub(crate) fn copy_all_mask(ed: &crate::editor::EditorSession) -> CopyAllMask {
    let annotations = ed.model.annotations();
    if !source_scrubbed(ed) && !readshot_core::scrub::has_redaction(annotations) {
        return CopyAllMask::None;
    }
    let Some(hide) = history_hide(ed) else {
        return CopyAllMask::Withhold;
    };
    let base = ed.model.base();
    CopyAllMask::Hide(
        match readshot_core::render::output_crop(base.width(), base.height(), annotations) {
            Some(crop) => readshot_core::scrub::redactions_in_crop(&hide, crop),
            None => hide.into_iter().filter(|a| a.is_redaction()).collect(),
        },
    )
}
