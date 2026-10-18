// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.
use super::*;

use crate::coordinator::CaptureCoordinator;

mod canvas;
mod chrome;
mod keyboard;
mod output;
mod pins;
mod style;
mod text_edit;

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
        ed.set_status(StatusTone::Info, format!(
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

/// A key's message that a focused text field should own: Undo, Redo
/// and an Up/Down nudge (a single-line field ignores those keys).
fn is_field_key(message: &Message) -> bool {
    matches!(
        message,
        Message::EditorToolbar(
            readshot_ui::ToolbarMessage::Undo | readshot_ui::ToolbarMessage::Redo
        )
    ) || matches!(message, Message::EditorNudgeSelected(dx, _) if *dx == 0.0)
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
        && !matches!(
            ed.status.as_ref().and_then(|s| s.guide),
            Some(KeyGuide::Selection)
        );
    !(ed.status_is_error()
        || ed.status_is_warning()
        || ed.visible_toast_action().is_some()
        || ed.discard_armed()
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
        edit_landed(
            ed,
            coord,
            Some(format!("Updated selected annotation color. {UNDO_HINT}")),
        );
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
        edit_landed(
            ed,
            coord,
            Some(format!("Updated selected annotation size. {UNDO_HINT}")),
        );
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
        edit_landed(
            ed,
            coord,
            Some(format!("Updated selected annotation. {UNDO_HINT}")),
        );
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
    let (w, h) = ed.effective_image_size();
    let status = format!("Cropped to {w}\u{a0}×\u{a0}{h}\u{a0}px. {UNDO_HINT}");
    edit_landed(ed, coord, Some(status));
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
            edit_landed(ed, coord, Some(format!("Updated text. {UNDO_HINT}")));
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
    edit_landed(ed, coord, Some(format!("Added text. {UNDO_HINT}")));
    true
}

/// An annotation edit just became an undo step on the model: show it,
/// say so as a success (`None`: no toast, as for rapid arrow-key
/// nudges) and write History. Geometry steps land through `compose_jobs` instead.
pub(crate) fn edit_landed(
    ed: &mut crate::editor::EditorSession,
    coord: &CaptureCoordinator,
    status: Option<String>,
) {
    ed.refresh_image();
    if let Some(status) = status {
        ed.set_status(StatusTone::Success, status);
    }
    sync_editor_history(ed, coord);
}

pub(crate) fn editor_has_unsaved_work(ed: &crate::editor::EditorSession) -> bool {
    // A history step still composing is an edit too.
    ed.has_output_changes()
        || pending_text_has_unsaved_work(ed)
        || super::compose_jobs::steps_pending(ed)
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
        // A kept gap is a geometry step: it lands as one, as it does when
        // its drag is released while composing off the UI thread.
        let gap = Some(ed.model.geometry().gap).filter(|g| *g != baseline.doc.geometry.gap);
        if ed.model.commit_preview_snapshot(baseline) {
            match gap {
                Some(gap) => super::compose_jobs::land_gap_commit(ed, coord, gap),
                None => {
                    ed.clear_discard_confirmation();
                    edit_landed(ed, coord, None);
                }
            }
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
    // else changes history, drop it for good (the generation changes on
    // every push, undo and redo, even at the history cap).
    if let Some(crate::editor::EditorToastAction::UndoRedaction { generation }) = ed.toast_action()
    {
        if generation != ed.model.history_generation() {
            ed.clear_toast_action();
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
        ed.history_write = Some(coord.rewrite_history_as(
            Some(ed.session_id),
            scrubbed,
            ed.model.shared_base(),
            bake,
        ));
    } else if ed.history_scrubbed || ed.history_frame_written != known {
        // Restore after a scrub, or the frame changed: the image itself
        // must change, with the annotations still editable.
        let mut restored = record;
        restored.ocr_text = history_plain_text(ed);
        // A reopened scrubbed record stays a scrub (`redacted` as loaded):
        // its image still has the redactions baked in.
        ed.history_scrubbed = false;
        ed.history_frame_written = known;
        ed.history_write = Some(coord.rewrite_history_as(
            Some(ed.session_id),
            restored,
            ed.model.shared_base(),
            Vec::new(),
        ));
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
        // while a part has no recognition yet.
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
/// text rather than text nothing could mask).
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
        } => keyboard::on_key_pressed(state, window, key, modifiers, status_ignored),
        Message::EditorStatusTick => chrome::on_status_tick(state),
        Message::EditorPopoverToggled(which) => chrome::on_popover_toggled(state, which),
        Message::EditorPopoverClosed => chrome::on_popover_closed(state),
        Message::EditorWindowReady(id) => chrome::on_window_ready(state, id),

        Message::EditorSaveRequested => output::on_save_requested(state),
        Message::EditorSaved { session, result } => output::on_saved(state, session, result),

        Message::EditorCopyImageRequested => output::on_copy_image_requested(state),
        Message::EditorCopyImageDone { session, result } => {
            output::on_copy_image_done(state, session, result)
        }

        Message::EditorBeautifyPreset(preset) => style::on_beautify_preset(state, preset),

        Message::EditorSnapshotSliderCommit => style::on_snapshot_slider_commit(state),

        Message::EditorCopyTextRequested => output::on_copy_text_requested(state),
        Message::EditorCopyTextDone { session, result } => {
            output::on_copy_text_done(state, session, result)
        }

        Message::EditorDiscardDisarmed => chrome::on_discard_disarmed(state),
        Message::EditorStatusDismissed => chrome::on_status_dismissed(state),

        Message::EditorDiscardRequested => output::on_discard_requested(state),

        // Toolbar selections — tool / colour / line-width changes
        // and undo/redo. Tool/colour/width changes don't need a
        // re-render (annotations haven't changed), but undo/redo do.
        Message::EditorToolbar(msg) => style::on_toolbar(state, msg),

        Message::EditorLineWidthPreview(width) => style::on_line_width_preview(state, width),

        Message::EditorLineWidthCommit => style::on_line_width_commit(state),

        Message::EditorStyle(msg) => style::on_style(state, msg),
        Message::EditorStylePreview(msg) => style::on_style_preview(state, msg),
        Message::EditorHexChanged(raw) => style::on_hex_changed(state, raw),
        Message::EditorEyedropperToggled => style::on_eyedropper_toggled(state),
        Message::EditorHexSubmitted => style::on_hex_submitted(state),
        Message::EditorStyleCommit => style::on_style_commit(state),

        // Canvas events — mostly drag previews (which don't need
        // their own state mutation in this cut; the canvas's internal
        // DrawState already drives the live painting) and one-shot
        // commits which mutate the model.
        Message::EditorCanvas(msg) => canvas::on_canvas(state, msg),

        Message::EditorEditSelectedText => text_edit::on_edit_selected_text(state),

        Message::EditorDeleteSelected => text_edit::on_delete_selected(state),

        Message::EditorNudgeSelected(dx, dy) => keyboard::on_nudge_selected(state, dx, dy),

        Message::EditorTextChanged(content) => text_edit::on_text_changed(state, content),
        Message::EditorTextAction(action) => text_edit::on_text_action(state, action),
        Message::EditorTextCommit => text_edit::on_text_commit(state),
        Message::EditorCropApply => text_edit::on_crop_apply(state),
        Message::EditorCropCancel => text_edit::on_crop_cancel(state),
        Message::EditorTextCancel => text_edit::on_text_cancel(state),
        Message::EditorWidthBump(delta) => keyboard::on_width_bump(state, delta),
        Message::EditorColorCycle(dir) => keyboard::on_color_cycle(state, dir),
        Message::EditorZoomIn => chrome::on_zoom_in(state),
        Message::EditorZoomOut => chrome::on_zoom_out(state),
        Message::EditorZoomActual => chrome::on_zoom_actual(state),
        Message::EditorZoomFit => chrome::on_zoom_fit(state),

        Message::EditorPinRequested => output::on_pin_requested(state),
        Message::EditorPinRendered {
            result,
            display_bounds,
        } => pins::on_pin_rendered(state, result, display_bounds),
        Message::PinWindowReady(id, handle) => pins::on_pin_window_ready(state, id, handle),
        Message::PinClosePressed(id) => pins::on_pin_close_pressed(state, id),
        Message::PinHovered(id, true) => pins::on_pin_hovered(state, id),
        Message::PinHovered(id, false) => pins::on_pin_unhovered(state, id),
        Message::PinDragRequested(id) => window::drag(id),
        Message::PinOpacityChanged(id, opacity) => pins::on_pin_opacity_changed(state, id, opacity),
        Message::PinLockToggled(id) => pins::on_pin_lock_toggled(state, id),
        Message::PinKeyPressed {
            window,
            key,
            modifiers,
        } => pins::on_pin_key_pressed(state, window, key, modifiers),
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
        Message::EditorCycleSelection(step) => keyboard::on_cycle_selection(state, step),
        Message::EditorFieldKeyResolved {
            session,
            message,
            field_focused,
        } => keyboard::on_field_key_resolved(state, session, message, field_focused),
        Message::EditorTabResolved {
            step,
            field_focused,
        } => keyboard::on_tab_resolved(state, step, field_focused),
        Message::EditorResizeByKeys(dx, dy) => keyboard::on_resize_by_keys(state, dx, dy),
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
/// word under a weak baked blur — and every magnifier bubble that shows
/// one ([`readshot_core::scrub::magnified_redactions`]). Moved into the
/// output's px (a Crop cuts it). [`CopyAllMask::Withhold`] when the baked-in ones are unknown.
pub(crate) fn copy_all_mask(ed: &crate::editor::EditorSession) -> CopyAllMask {
    let annotations = ed.model.annotations();
    if !source_scrubbed(ed) && !readshot_core::scrub::has_redaction(annotations) {
        return CopyAllMask::None;
    }
    let Some(mut hide) = history_hide(ed) else {
        return CopyAllMask::Withhold;
    };
    // A magnifier bubble whose source shows a redaction shows a magnified
    // copy of what it hides: its bubble is masked too.
    let magnified = readshot_core::scrub::magnified_redactions(&hide);
    hide.extend(magnified);
    let base = ed.model.base();
    CopyAllMask::Hide(
        match readshot_core::render::output_crop(base.width(), base.height(), annotations) {
            Some(crop) => readshot_core::scrub::redactions_in_crop(&hide, crop),
            None => hide.into_iter().filter(|a| a.is_redaction()).collect(),
        },
    )
}

/// Open a pin window showing `img`. Its kind and image handle are
/// registered eagerly so the first `view` paints the pin instead of the
/// "(no capture)" fallback.
fn open_pin_window(
    state: &mut App,
    img: image::RgbaImage,
    display_bounds: Option<(f32, f32, f32, f32)>,
) -> Task<Message> {
    let settings = pin_window_settings((img.width(), img.height()), display_bounds);
    let handle = rgba_handle(img);
    let (id, open_task) = window::open(settings);
    state.windows.register(id, WindowKind::Pin);
    state
        .pins
        .insert(id, crate::app::PinState::new(handle.clone()));
    open_task.map(move |opened| Message::PinWindowReady(opened, handle.clone()))
}

/// Ask iced whether the Beautify hex field has the caret before `key`
/// (Undo, Redo, an Up/Down nudge) acts; the answer is
/// `EditorFieldKeyResolved`. Collected, so a query no widget answers
/// still resolves, as "not focused": a key is never dropped.
fn ask_hex_field(session: u64, key: Message) -> Task<Message> {
    let message = Box::new(key);
    iced::widget::operation::is_focused(iced::widget::Id::new(super::key_focus::BEAUTIFY_HEX_ID))
        .collect()
        .map(move |answers| Message::EditorFieldKeyResolved {
            session,
            message: message.clone(),
            field_focused: tab_field_focused(&answers),
        })
}

/// Whether a text field owns Undo, Redo and Up/Down now: the Color or
/// Resize popover (its field may have focus), an inline text draft, or a
/// typed Beautify hex draft.
fn field_owns_keys(state: &App) -> bool {
    state.editor.as_ref().is_some_and(|ed| {
        matches!(
            ed.popover,
            Some(crate::editor::EditorPopover::Color | crate::editor::EditorPopover::Resize)
        ) || ed.pending_text.is_some()
            || ed.beautify_hex_draft.is_some()
    })
}

/// C from the keyboard (the Crop tool picked by its key).
fn is_crop_key(message: &Message) -> bool {
    matches!(
        message,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
            readshot_ui::editor::ToolState::Crop
        ))
    )
}

/// After C from the keyboard: a crop draft to move and size with the
/// arrows, since the pointer is not there to drag one (WCAG 2.1.1 /
/// 2.5.7). The toolbar button still waits for a drag.
fn keyboard_crop_follow_up(state: &mut App) {
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
