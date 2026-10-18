//! Editor Text panel: recognition of the base image,
//! the Text tab, line selection and copy, smart redact and the smart
//! highlighter snap. Positions come from the OCR engine; without them
//! (Windows, Linux today) only the list and copying are offered.

use super::*;

use std::collections::BTreeSet;
use std::ops::Range;

use iced::widget::{
    column, container, mouse_area, pick_list, row, scrollable, text, toggler, Space,
};
use iced::{Alignment, Color, Element, Length, Task, Theme};

use crate::app::{App, Message};
use crate::editor::{EditorSession, EditorToastAction, TextRecognition};
use crate::icons::AppIcon;
use readshot_core::geom::RectLike;
use readshot_core::redact::{
    is_covered, locate, redaction_annotation, summary, Located, RedactMatch, RedactStyle,
};
use readshot_core::text_lines::{TextDocument, TextLine};

/// Rebuild the Text panel's document for the current geometry: every
/// part's recognition (part px) mapped into document px and merged.
/// Clears hover and selection (line indices may have moved). While a
/// part has no recognition there is no document (a combined document
/// has text only once every part is recognized): the scrub then stores
/// no text rather than text it can't mask.
pub(crate) fn refresh_text_for_geometry(ed: &mut EditorSession) {
    use readshot_core::document::{map_text_document, merge_text_documents};
    let _ = resolve_baked_docs(&mut ed.text_panel);
    let g = std::sync::Arc::clone(ed.model.geometry());
    let tp = &mut ed.text_panel;
    tp.built_for = Some(std::sync::Arc::clone(&g));
    tp.hovered = None;
    tp.selected.clear();
    tp.anchor = None;
    let docs: Option<Vec<TextDocument>> = g
        .parts
        .iter()
        .map(|p| {
            Some(map_text_document(
                tp.part_docs.get(&p.id)?,
                &g.part_affine(p.id)?,
            ))
        })
        .collect();
    match docs {
        Some(docs) if !docs.is_empty() => {
            let doc = merge_text_documents(&docs);
            tp.matches = if doc.positioned {
                readshot_core::redact::scan(&doc)
            } else {
                Vec::new()
            };
            tp.recognition = TextRecognition::Ready(std::sync::Arc::new(doc));
        }
        _ => {
            tp.matches.clear();
            if matches!(tp.recognition, TextRecognition::Ready(_)) {
                tp.recognition = TextRecognition::Idle;
            }
        }
    }
}

/// Give every baked part (a transformed document baked into one part
/// before a combine) whose source parts are all
/// recognized its text: their recognitions mapped through the geometry
/// it was baked from, merged. Baked px are that geometry's document px,
/// so the boxes land exactly; the turned pixels are never recognized
///. Repeats until nothing changes, for a bake of a bake. `true`
/// when a baked part got its text.
pub(crate) fn resolve_baked_docs(tp: &mut crate::editor::TextPanelState) -> bool {
    use readshot_core::document::{map_text_document, merge_text_documents};
    let mut any = false;
    loop {
        let mut resolved = Vec::new();
        for (baked, src) in &tp.bake_sources {
            if tp.part_docs.contains_key(baked) {
                continue;
            }
            let docs: Option<Vec<TextDocument>> = src
                .parts
                .iter()
                .map(|p| {
                    Some(map_text_document(
                        tp.part_docs.get(&p.id)?,
                        &src.part_affine(p.id)?,
                    ))
                })
                .collect();
            if let Some(docs) = docs.filter(|d| !d.is_empty()) {
                resolved.push((*baked, merge_text_documents(&docs)));
            }
        }
        if resolved.is_empty() {
            return any;
        }
        any = true;
        for (id, doc) in resolved {
            tp.part_docs.insert(id, std::sync::Arc::new(doc));
        }
    }
}

/// Release the bake sources (each a whole earlier document) no held
/// state can reach any more: a baked part no undo/redo state, open
/// preview or slider baseline uses, nor any bake source still kept
/// (a bake of a bake). Part ids are never reused, so a dropped part never
/// comes back. Runs on every history change (`sync_editor_history`).
pub(crate) fn prune_bake_sources(ed: &mut EditorSession) {
    if ed.text_panel.bake_sources.is_empty() {
        return;
    }
    let mut live = ed.model.referenced_part_ids();
    if let Some(b) = &ed.snapshot_drag_baseline {
        live.extend(b.doc.geometry.parts.iter().map(|p| p.id));
    }
    let mut pending: Vec<_> = live.iter().copied().collect();
    while let Some(id) = pending.pop() {
        if let Some(src) = ed.text_panel.bake_sources.get(&id) {
            for p in &src.parts {
                if live.insert(p.id) {
                    pending.push(p.id);
                }
            }
        }
    }
    let tp = &mut ed.text_panel;
    tp.bake_sources.retain(|id, _| live.contains(id));
}

/// The next part to recognize, upright in its own pixels: the first of
/// `parts` with no recognition, or for a baked part, the first of its
/// source parts that has none.
fn next_unrecognized_part(
    tp: &crate::editor::TextPanelState,
    parts: &[readshot_core::document::Part],
) -> Option<readshot_core::document::Part> {
    for p in parts {
        if tp.part_docs.contains_key(&p.id) {
            continue;
        }
        if let Some(src) = tp.bake_sources.get(&p.id) {
            // Source ids are older than the baked one: no cycles. A baked
            // part's own (turned) pixels are never recognized: its text
            // comes from its sources only, or it has none (fail closed).
            if let Some(q) = next_unrecognized_part(tp, &src.parts) {
                return Some(q);
            }
            continue;
        }
        return Some(p.clone());
    }
    None
}

/// Original part px → document px of `g`, through any bakes: the
/// original may only survive inside a baked part (`bake_sources`).
pub(crate) fn original_part_affine(
    tp: &crate::editor::TextPanelState,
    g: &readshot_core::document::DocGeometry,
) -> Option<readshot_core::document::Affine> {
    if let Some(m) = g.part_affine(readshot_core::document::ORIGINAL_PART) {
        return Some(m);
    }
    g.parts.iter().find_map(|p| {
        let src = tp.bake_sources.get(&p.id)?;
        let inner = original_part_affine(tp, src)?;
        Some(inner.then(&g.part_affine(p.id)?))
    })
}

/// [`refresh_text_for_geometry`] when the panel's document was built for
/// another geometry than the present one (a transform, a combine, or an
/// undo/redo across one), so its word boxes never lag the frame the
/// redactions are in.
pub(crate) fn refresh_text_if_stale(ed: &mut EditorSession) {
    let stale = ed
        .text_panel
        .built_for
        .as_ref()
        .is_some_and(|g| !std::sync::Arc::ptr_eq(g, ed.model.geometry()));
    if stale {
        refresh_text_for_geometry(ed);
    }
}

/// The panel's positioned document, but only while its boxes are in the
/// present frame. Read-only callers (view, canvas marks, snap) can't
/// rebuild it; they get nothing rather than boxes in another frame.
pub(crate) fn current_positioned_document(ed: &EditorSession) -> Option<&TextDocument> {
    let current = ed
        .text_panel
        .built_for
        .as_ref()
        .is_none_or(|g| std::sync::Arc::ptr_eq(g, ed.model.geometry()));
    current
        .then(|| ed.text_panel.positioned_document())
        .flatten()
}

/// Start recognizing the first part of the document that has no
/// recognition yet, unless one is already running (one request at a
/// time) or every part is done. Each part is recognized in its own
/// pixels (not the flattened output), so its boxes map exactly into
/// annotation coordinates and redactions can't hide text from the list.
/// Never sets `busy`.
pub(crate) fn request_text_recognition(state: &mut App) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if matches!(
        ed.text_panel.recognition,
        TextRecognition::Recognizing { .. }
    ) {
        return Task::none();
    }
    if resolve_baked_docs(&mut ed.text_panel) {
        refresh_text_for_geometry(ed);
    }
    let Some(part) = next_unrecognized_part(&ed.text_panel, &ed.model.geometry().parts) else {
        return Task::none(); // every part is recognized
    };
    ed.text_panel.recognition = TextRecognition::Recognizing {
        since: std::time::Instant::now(),
    };
    ed.text_panel.recognizing_part = Some(part.id);
    if ed.toast_action == Some(EditorToastAction::RetryRecognition) {
        ed.clear_status();
        ed.toast_action = None;
    }
    let session = ed.session_id;
    let (history_record, history_cut) = recognition_history_target(ed);
    // Only a shared handle here: the full-size pixel copy the engine
    // takes ownership of (~80 MB on a 5K capture) is made in the task,
    // off the UI thread.
    let part_image = std::sync::Arc::clone(&part.image);
    let part_id = part.id;
    let languages = state.preferences.ocr_languages.clone();
    let coord = state.coordinator.clone();
    Task::perform(
        async move {
            let req = readshot_ocr::OCRRequest {
                image: image::RgbaImage::clone(&part_image),
                languages,
                use_language_correction: true,
            };
            let result = coord
                .recognise_document(req)
                .await
                .map_err(|e| e.to_string());
            match &history_record {
                Some(record) => {
                    store_recognized_text(&coord, record, history_cut.as_ref(), result).await
                }
                None => result,
            }
        },
        move |result| Message::EditorTextRecognized {
            session,
            part: part_id,
            result,
        },
    )
}

/// The History record (and crop) the recognition task stores its text
/// for, if any. The task stores masked text in History itself (so it
/// lands after the editor closes) only while the document is the original frame:
/// part px are document px there. Otherwise the editor-side write in
/// `EditorTextRecognized` does it, once every part is recognized. The
/// record carries
/// the frame id, so History drops the text if it holds another frame
/// by the time it lands.
///
/// A scrub that bakes a Crop stores the crop, in a frame of its own:
/// the text is then cut to the crop (and pre-masked with what the
/// scrub hides) before History masks it with the stored redactions.
///
/// Nothing is stored for a reopened scrubbed record whose baked-in
/// redactions are unknown (no stored redactions): History could only mask
/// with the redactions added since, and the recognition of the opened
/// image may read a word under an old weak blur or pixelate.
pub(crate) fn recognition_history_target(
    ed: &EditorSession,
) -> (Option<readshot_core::CaptureRecord>, Option<HistoryCut>) {
    if ed.model.geometry().is_original() {
        let frame = history_geometry_id(ed);
        match super::editor_ops::history_crop(ed) {
            // A reopened scrub whose baked-in redactions are unknown: its
            // recognition may read words under them that nothing can mask
            // (a weak blur), so none of it is stored (fail closed).
            None if super::editor_ops::history_hide(ed).is_none() => (None, None),
            None => (
                ed.source_record.clone().map(|mut r| {
                    r.geometry_id = frame;
                    r
                }),
                None,
            ),
            Some(crop) => match super::editor_ops::history_hide(ed) {
                Some(hide) => (
                    ed.source_record.clone().map(|mut r| {
                        r.geometry_id =
                            Some(crate::coordinator::cropped_frame_id(frame.as_deref(), crop));
                        r
                    }),
                    Some((crop, hide)),
                ),
                // Inherited redactions can't be placed: store nothing.
                None => (None, None),
            },
        }
    } else {
        (None, None)
    }
}

/// History's image is this crop of the document, hiding these annotations
/// ([`store_recognized_text`]).
pub(crate) type HistoryCut = (
    readshot_core::render::OutputCrop,
    Vec<readshot_core::Annotation>,
);

/// After the editor's recognition of a History capture: if the record
/// was scrubbed, store the text masked with its redactions. Runs in the
/// recognition task, not the editor, so it lands even when the editor
/// closed meanwhile. An unredacted record is left alone. Passes `result`
/// through.
///
/// `cut`: History's image is this crop of the document, hiding these
/// annotations; only the text inside the crop is stored
/// ([`super::editor_ops::cropped_history_document`]), and none without
/// positions.
pub(crate) async fn store_recognized_text(
    coord: &crate::coordinator::CaptureCoordinator,
    record: &readshot_core::CaptureRecord,
    cut: Option<&HistoryCut>,
    result: Result<TextDocument, String>,
) -> Result<TextDocument, String> {
    let for_history = match (&result, cut) {
        (Ok(doc), None) => Some(doc.clone()),
        (Ok(doc), Some((crop, hide))) => {
            super::editor_ops::cropped_history_document(doc, hide, *crop)
        }
        (Err(_), _) => None,
    };
    if let Some(doc) = for_history {
        let (coord, record) = (coord.clone(), record.clone());
        let written =
            tokio::task::spawn_blocking(move || coord.record_recognized_text(&record, &doc, false))
                .await;
        match written {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "scrubbed text update failed: {e}")
            }
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "scrubbed text worker: {e}")
            }
        }
    }
    result
}

/// History reopen: an engine without line positions could only
/// reproduce the stored `ocr_text`, so the panel is built from it with
/// no OCR. Engines with positions (Apple Vision) recognize again.
pub(crate) fn seed_text_from_history(ed: &mut EditorSession, engine_reports_positions: bool) {
    if engine_reports_positions {
        return;
    }
    let Some(text) = ed
        .source_record
        .as_ref()
        .and_then(|r| r.ocr_text.as_deref())
    else {
        return;
    };
    let doc = readshot_core::text_lines::TextDocument::from_plain(text);
    // The stored text is the opened image's: the original part's.
    ed.text_panel.part_docs.insert(
        readshot_core::document::ORIGINAL_PART,
        std::sync::Arc::new(doc),
    );
    refresh_text_for_geometry(ed);
}

pub(crate) fn handle_text_panel_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::EditorTextRecognize => {
            if let Some(ed) = state.editor.as_mut() {
                ed.text_panel.user_requested = true;
            }
            request_text_recognition(state)
        }
        Message::EditorTextRecognized {
            session,
            part,
            result,
        } => {
            let panel_open = state.preferences.editor_panel_open;
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.session_id != session {
                return Task::none();
            }
            // A late result for another part than the running request's
            // (the capture's own OCR, say) leaves that request be: its
            // state, and whether the user asked for it.
            let other_running = matches!(
                ed.text_panel.recognition,
                TextRecognition::Recognizing { .. }
            ) && ed.text_panel.recognizing_part.is_some_and(|p| p != part);
            let user_requested =
                !other_running && std::mem::take(&mut ed.text_panel.user_requested);
            match result {
                Ok(doc) => {
                    ed.text_panel
                        .part_docs
                        .insert(part, std::sync::Arc::new(doc));
                    if !other_running {
                        ed.text_panel.recognizing_part = None;
                        ed.text_panel.recognition = TextRecognition::Idle;
                    }
                    refresh_text_for_geometry(ed);
                    // Redacted before the text was known, or History holds a
                    // transformed/combined document whose text is missing:
                    // write again, now with the full (masked) text.
                    let ready = ed.text_panel.document().is_some();
                    if ready && readshot_core::scrub::has_redaction(ed.model.annotations()) {
                        write_editor_history(ed, &state.coordinator);
                    } else if ready
                        && !ed.model.geometry().is_original()
                        && ed.source_record.is_some()
                    {
                        ed.history_frame_written = crate::editor::HistoryFrame::Unknown;
                        write_editor_history(ed, &state.coordinator);
                    }
                    if !ready && !other_running {
                        // Another part still needs recognition.
                        if user_requested {
                            ed.text_panel.user_requested = true;
                        }
                        return request_text_recognition(state);
                    }
                }
                Err(e) if other_running => {
                    tracing::warn!(
                        target: readshot_core::log::cat::EDITOR,
                        "text recognition of part {part} failed: {e}"
                    );
                }
                Err(e) => {
                    tracing::warn!(target: readshot_core::log::cat::EDITOR, "text recognition failed: {e}");
                    ed.text_panel.recognizing_part = None;
                    ed.text_panel.recognition = TextRecognition::Failed(e.clone());
                    // Don't clobber a Saving…/Copying… toast; the panel's
                    // Try Again state still offers the retry. A background
                    // OCR nobody is looking at (panel closed, not asked
                    // for) fails quietly, as history OCR always did.
                    if !ed.busy && (panel_open || user_requested) {
                        ed.set_status_with_action(
                            super::history_ops::friendly_string_error_with(
                                "recognize the text",
                                &e,
                                None,
                            ),
                            EditorToastAction::RetryRecognition,
                        );
                    }
                }
            }
            Task::none()
        }
        Message::EditorPanelToggled => {
            let Some(narrow) = state
                .editor
                .as_ref()
                .map(|ed| ed.window_width < PANEL_DOCK_MIN_WINDOW)
            else {
                return Task::none();
            };
            let opening = if narrow {
                // Narrow windows: a temporary overlay; the remembered
                // preference is untouched.
                match state.editor.as_mut() {
                    Some(ed) => {
                        ed.panel_overlay_open = !ed.panel_overlay_open;
                        ed.panel_overlay_open
                    }
                    None => false,
                }
            } else {
                // Toggle what is shown: an overlay opened in a narrow
                // window that was then widened shows docked, so it counts.
                let shown = state.preferences.editor_panel_open
                    || state
                        .editor
                        .as_ref()
                        .is_some_and(|ed| ed.panel_overlay_open);
                let open = !shown;
                state.set_editor_panel_open(open);
                if let Some(ed) = state.editor.as_mut() {
                    ed.panel_overlay_open = false;
                }
                open
            };
            if let Some(ed) = state.editor.as_mut() {
                ed.text_panel.hovered = None;
                if opening {
                    // The panel takes over; a popover left open over the
                    // canvas would otherwise eat the next Esc.
                    close_editor_popover(ed);
                }
            }
            if opening {
                request_text_recognition(state)
            } else {
                Task::none()
            }
        }
        Message::EditorPanelOverlayDismissed => {
            if let Some(ed) = state.editor.as_mut() {
                close_panel_overlay(ed);
            }
            Task::none()
        }
        Message::EditorPanelTabSelected(tab) => {
            state.set_editor_panel_tab(tab);
            // The Beautify tab opens showing the current background.
            if tab == crate::editor::PanelTab::Beautify {
                if let Some(ed) = state.editor.as_mut() {
                    ed.beautify_hex_draft = None;
                }
            }
            Task::none()
        }
        Message::EditorOpenTextSettings => {
            let _ = update(
                state,
                Message::SettingsPaneSelected(crate::app::SettingsPane::TextRecognition),
            );
            update(state, Message::OpenSettingsRequested)
        }
        Message::EditorTextLineClicked(index) => {
            let (shift, command) = (state.editor_shift_held, state.editor_command_held);
            if let Some(ed) = state.editor.as_mut() {
                let in_range = ed
                    .text_panel
                    .document()
                    .is_some_and(|d| index < d.lines.len());
                if in_range {
                    let tp = &mut ed.text_panel;
                    click_selection(&mut tp.selected, &mut tp.anchor, index, shift, command);
                }
            }
            Task::none()
        }
        Message::EditorCopyLines => {
            let remove_breaks = state.preferences.editor_copy_remove_line_breaks;
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            let indices: Vec<usize> = ed.text_panel.selected.iter().copied().collect();
            if indices.is_empty() {
                return Task::none();
            }
            let Some(text) = copy_lines_text(ed, &indices, remove_breaks) else {
                if ed.text_panel.document().is_some() {
                    ed.set_status(COPY_TEXT_WITHHELD);
                }
                return Task::none();
            };
            let n = indices.len();
            ed.set_status("Copying text…");
            Task::perform(copy_cached_text(text), move |r| {
                Message::EditorCopyLinesDone(r.map(|_| n).map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyLinesDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.set_status(match result {
                    Ok(n) => format!("Copied {}.", counted(n, "line", "lines")),
                    Err(e) => {
                        tracing::warn!(target: readshot_core::log::cat::EDITOR, "copy lines failed: {e}");
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
        Message::EditorRemoveLineBreaksToggled(on) => {
            state.set_copy_remove_line_breaks(on);
            Task::none()
        }
        Message::EditorRedactStyleChanged(style) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.text_panel.redact_style = style;
            }
            Task::none()
        }
        Message::EditorRedactAll => redact(state, None),
        Message::EditorRedactLine(index) => redact(state, Some(index)),
        Message::EditorHistoryWritten { session, result } => {
            match result {
                // Superseded by a newer rewrite, which reports instead.
                None => Task::none(),
                Some(Ok(())) => {
                    // The record's image and text changed: an open
                    // History window must show and search the new ones.
                    if state.history_window_id.is_some() {
                        history_list_task(state.coordinator.clone())
                    } else {
                        Task::none()
                    }
                }
                Some(Err(crate::coordinator::HistoryRewriteError::RedactedElsewhere)) => {
                    // History keeps the redacted record another window
                    // made; this editor can never write it back unredacted.
                    // It stops writing to it, and says how to keep its
                    // own version.
                    if let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) {
                        ed.source_record = None;
                        ed.set_warning(format!(
                            "History keeps the redacted version of this capture (it was redacted in another window), so this change wasn’t stored there. Press {} to save this version to a file.",
                            super::keys::SAVE_KEY
                        ));
                    }
                    Task::none()
                }
                Some(Err(crate::coordinator::HistoryRewriteError::Failed(e))) => {
                    tracing::warn!(target: readshot_core::log::cat::HISTORY, "history rewrite failed: {e}");
                    if let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) {
                        // What's on disk is unknown now: the next sync
                        // rewrites the whole record again.
                        ed.history_scrubbed = true;
                        ed.history_frame_written = crate::editor::HistoryFrame::Unknown;
                        if !ed.busy {
                            ed.set_status(super::history_ops::friendly_string_error_with(
                                "update this capture in History",
                                &e,
                                Some("Readshot will keep trying."),
                            ));
                        }
                    }
                    Task::none()
                }
            }
        }
        other => unreachable!("non-text-panel message routed to the text panel handler: {other:?}"),
    }
}

/// How well existing annotations cover a redaction rect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Coverage {
    None,
    /// Only a Blur or Pixelate (any strength) contains it: weaker than a
    /// solid block, so it doesn't count as redacted when Solid is chosen.
    Weak,
    /// An opaque, square-cornered filled rectangle contains it.
    Solid,
}

pub(crate) fn coverage(rect: RectLike, annotations: &[readshot_core::Annotation]) -> Coverage {
    use readshot_core::Annotation as A;
    let covered_by = |pred: fn(&A) -> bool| {
        annotations
            .iter()
            .any(|a| pred(a) && is_covered(rect, std::slice::from_ref(a)))
    };
    if covered_by(A::is_solid_redaction) {
        Coverage::Solid
    } else if covered_by(A::is_redaction) {
        Coverage::Weak
    } else {
        Coverage::None
    }
}

/// Smart-redact bookkeeping for the current document and style. Empty
/// without line positions; matches that can't be placed are listed in
/// `unlocatable`, never silently dropped.
#[derive(Debug, Default)]
pub(crate) struct RedactState {
    /// Still to redact with the chosen style, each with the rect its
    /// redaction gets (span-precise, padded, whole-pixel).
    pub pending: Vec<(RedactMatch, RectLike)>,
    /// Covered only by a Blur/Pixelate (pending too when Solid is chosen).
    pub weak: Vec<RedactMatch>,
    /// Covered by any redaction (solid or weak).
    pub covered: Vec<RedactMatch>,
    /// Detected but can't be placed on the image.
    pub unlocatable: Vec<RedactMatch>,
}

/// Classify every match. With Solid chosen only a solid block counts as
/// redacted (a blur can sometimes be reversed); with Blur or Pixelate
/// chosen any existing redaction counts, so nothing is doubled up.
pub(crate) fn redact_state(ed: &EditorSession) -> RedactState {
    let Some(doc) = current_positioned_document(ed) else {
        return RedactState::default();
    };
    let solid = ed.text_panel.redact_style == RedactStyle::Solid;
    let (w, h) = ed.image_size();
    // The one reviewed placement path: `redact::locate` (span-precise,
    // padded, whole-pixel rects; unplaceable matches reported).
    let Located { rects, unlocatable } = locate(doc, &ed.text_panel.matches, w, h);
    let mut out = RedactState {
        unlocatable,
        ..RedactState::default()
    };
    // What the output hides: the redactions as drawn. Built once: it
    // clones every annotation, and the panel view calls this per frame.
    let drawn = ed.model.drawn_annotations();
    for (m, rect) in rects {
        let cov = coverage(rect, &drawn);
        if cov != Coverage::None {
            out.covered.push(m.clone());
        }
        if cov == Coverage::Weak {
            out.weak.push(m.clone());
        }
        let done = match cov {
            Coverage::Solid => true,
            Coverage::Weak => !solid,
            Coverage::None => false,
        };
        if !done {
            out.pending.push((m, rect));
        }
    }
    out
}

fn items(n: usize) -> String {
    counted(n, "item", "items")
}

/// Toast text after a redaction of `n` items with `missed` that couldn't
/// be located.
pub(crate) fn redact_status(n: usize, missed: usize) -> String {
    let pronoun = if missed == 1 { "it" } else { "them" };
    match (n, missed) {
        (0, 0) => "No sensitive items left to redact.".to_string(),
        (0, m) => format!(
            "Couldn’t locate {}. Cover {pronoun} by hand.",
            counted(m, "sensitive item", "sensitive items")
        ),
        (n, 0) => format!("Redacted {}.", items(n)),
        (n, m) => format!(
            "Redacted {}. {m} more couldn’t be located; cover {pronoun} by hand.",
            items(n)
        ),
    }
}

/// Redact the pending items (all, or one line's) as one undo step and
/// toast "Redacted N items" with Undo. Items that can't be located are
/// counted in the toast.
fn redact(state: &mut App, only_line: Option<usize>) -> Task<Message> {
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    if ed.busy || ed.text_panel.positioned_document().is_none() {
        return Task::none();
    }
    cancel_editor_previews(ed);
    // The preview may have moved the frame; boxes must be in this one.
    refresh_text_if_stale(ed);
    commit_pending_editor_text(ed, &state.coordinator);
    let in_scope = |m: &RedactMatch| only_line.is_none_or(|line| m.line == line);
    let style = ed.text_panel.redact_style;
    let rs = redact_state(ed);
    let annotations: Vec<readshot_core::Annotation> = rs
        .pending
        .into_iter()
        .filter(|(m, _)| in_scope(m))
        .map(|(_, rect)| redaction_annotation(rect, style))
        .collect();
    let missed = rs.unlocatable.into_iter().filter(|m| in_scope(m)).count();
    let n = ed.model.commit_annotations(annotations);
    let status = redact_status(n, missed);
    if n == 0 {
        ed.set_status(status);
        return Task::none();
    }
    ed.refresh_image();
    sync_editor_history(ed, &state.coordinator);
    let generation = ed.model.history_generation();
    let action = EditorToastAction::UndoRedaction { generation };
    if missed > 0 {
        // Partial: the items left to cover by hand must not read as done.
        ed.set_warning_with_action(status, action);
    } else {
        ed.set_status_with_action(status, action);
    }
    Task::none()
}

/// Line selection: a click selects one line (clicking the
/// only selected line clears it); ⇧ selects the range from the anchor
/// (added to the selection when ⌘ is held too); ⌘ toggles one line.
pub(crate) fn click_selection(
    selected: &mut BTreeSet<usize>,
    anchor: &mut Option<usize>,
    index: usize,
    shift: bool,
    command: bool,
) {
    if shift {
        let from = anchor.unwrap_or(index);
        let (lo, hi) = if from <= index {
            (from, index)
        } else {
            (index, from)
        };
        if !command {
            selected.clear();
        }
        selected.extend(lo..=hi);
        anchor.get_or_insert(index);
    } else if command {
        if !selected.remove(&index) {
            selected.insert(index);
        }
        *anchor = Some(index);
    } else if selected.len() == 1 && selected.contains(&index) {
        selected.clear();
        *anchor = None;
    } else {
        selected.clear();
        selected.insert(index);
        *anchor = Some(index);
    }
}

/// Status when a copy is refused because its text can't be masked.
pub(crate) const COPY_TEXT_WITHHELD: &str =
    "Text hidden: Readshot can't tell which words the redactions cover, so nothing was copied.";

/// Copy N Lines text: the lines at `indices` with every word a
/// redaction hides masked (`█` per char): the redactions drawn now and,
/// for a reopened scrubbed record, the ones baked into its image
/// ([`super::editor_ops::history_hide`]; recognition may read a word
/// under a weak baked blur). `None` without recognized text, or when the
/// text can't be masked (baked-in redactions unknown, or no positions
/// while a redaction exists): withheld, never copied unmasked.
pub(crate) fn copy_lines_text(
    ed: &EditorSession,
    indices: &[usize],
    remove_breaks: bool,
) -> Option<String> {
    use readshot_core::scrub::{hidden_spans, mask_document};
    let doc = ed.text_panel.document()?;
    // Masked like the exported image: the redactions as drawn.
    let hide = super::editor_ops::history_hide(ed)?;
    let spans = hidden_spans(doc, &hide)?;
    Some(if spans.is_empty() {
        readshot_core::text_lines::copy_lines(doc, indices, remove_breaks)
    } else {
        readshot_core::text_lines::copy_lines(&mask_document(doc, &spans), indices, remove_breaks)
    })
}

pub(crate) fn copy_lines_label(n: usize) -> String {
    match n {
        0 => "Copy Lines".to_string(),
        1 => "Copy 1 Line".to_string(),
        n => format!("Copy {n} Lines"),
    }
}

/// How a run of a Text-panel line is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Run {
    Plain,
    /// A sensitive span still to redact (underlined).
    Marked,
    /// A span already covered on the image (muted).
    Redacted,
}

/// `text` split into runs; `marks` and `redacted` are char ranges
/// (`marks` wins where both apply). A run boundary never splits a
/// grapheme cluster ([`clusters`]): a cluster any char of which is
/// marked (or redacted) is marked whole, so a combining accent, a ZWJ
/// emoji or a flag is never drawn in two pieces.
pub(crate) fn line_spans(
    text: &str,
    marks: &[Range<usize>],
    redacted: &[Range<usize>],
) -> Vec<(String, Run)> {
    let mut out: Vec<(String, Run)> = Vec::new();
    for (chars, cluster) in clusters(text) {
        let hit = |ranges: &[Range<usize>]| {
            ranges
                .iter()
                .any(|r| r.start < chars.end && chars.start < r.end)
        };
        let run = if hit(marks) {
            Run::Marked
        } else if hit(redacted) {
            Run::Redacted
        } else {
            Run::Plain
        };
        match out.last_mut() {
            Some((s, r)) if *r == run => s.push_str(cluster),
            _ => out.push((cluster.to_string(), run)),
        }
    }
    out
}

/// A char that attaches to the one before it in a cluster: combining
/// marks (the combining blocks and the common Hebrew, Arabic, Indic and
/// Thai marks), variation selectors, emoji skin-tone modifiers, tag
/// characters and the zero-width joiner. An approximation of Unicode's
/// extended grapheme clusters, enough for display.
fn extends_cluster(c: char) -> bool {
    matches!(
        u32::from(c),
        0x0300..=0x036F
            | 0x0483..=0x0489
            | 0x0591..=0x05BD
            | 0x05BF
            | 0x05C1..=0x05C2
            | 0x05C4..=0x05C5
            | 0x05C7
            | 0x0610..=0x061A
            | 0x064B..=0x065F
            | 0x0670
            | 0x06D6..=0x06DC
            | 0x06DF..=0x06E4
            | 0x06E7..=0x06E8
            | 0x06EA..=0x06ED
            | 0x0900..=0x0903
            | 0x093A..=0x094F
            | 0x0951..=0x0957
            | 0x0962..=0x0963
            | 0x0981..=0x0983
            | 0x09BC..=0x09D7
            | 0x0E31
            | 0x0E34..=0x0E3A
            | 0x0E47..=0x0E4E
            | 0x1AB0..=0x1AFF
            | 0x1DC0..=0x1DFF
            | 0x200C..=0x200D
            | 0x20D0..=0x20FF
            | 0xFE00..=0xFE0F
            | 0xFE20..=0xFE2F
            | 0x1F3FB..=0x1F3FF
            | 0xE0020..=0xE007F
            | 0xE0100..=0xE01EF
    )
}

/// The Indic viramas (halant): a consonant after one is part of the
/// same conjunct.
fn is_virama(c: char) -> bool {
    matches!(
        u32::from(c),
        0x094D | 0x09CD | 0x0A4D | 0x0ACD | 0x0B4D | 0x0BCD | 0x0C4D | 0x0CCD | 0x0D4D | 0x0DCA
    )
}

fn is_regional_indicator(c: char) -> bool {
    matches!(u32::from(c), 0x1F1E6..=0x1F1FF)
}

/// `text` as grapheme clusters, each with its char range: a base char
/// plus every char that extends it ([`extends_cluster`]), the char after
/// a zero-width joiner (ZWJ emoji sequences), and regional indicators in
/// pairs (flags).
fn clusters(text: &str) -> Vec<(Range<usize>, &str)> {
    let mut out: Vec<(Range<usize>, &str)> = Vec::new();
    // (char start, byte start, regional indicators so far, after a ZWJ)
    let mut current: Option<(usize, usize, usize, bool)> = None;
    for (i, (byte, c)) in text.char_indices().enumerate() {
        let joins = current.is_some_and(|(_, _, ri, zwj)| {
            zwj || extends_cluster(c) || is_virama(c) || (ri == 1 && is_regional_indicator(c))
        });
        match current.as_mut() {
            Some((_, _, ri, zwj)) if joins => {
                if is_regional_indicator(c) {
                    *ri += 1;
                }
                // A ZWJ or a virama joins the next char (ZWJ emoji; an
                // Indic conjunct such as क्ष, Unicode's GB9c).
                *zwj = c == '\u{200D}' || is_virama(c);
            }
            _ => {
                if let Some((start, from, _, _)) = current {
                    out.push((start..i, &text[from..byte]));
                }
                current = Some((i, byte, usize::from(is_regional_indicator(c)), false));
            }
        }
    }
    if let Some((start, from, _, _)) = current {
        out.push((start..text.chars().count(), &text[from..]));
    }
    out
}

/// Copy already-recognized text; returns it for the toast.
pub(crate) async fn copy_cached_text(text: String) -> Result<String, ClipboardError> {
    if !text.is_empty() {
        copy_text_to_clipboard(text.clone()).await?;
    }
    Ok(text)
}

/// Close the narrow-window overlay panel; returns whether it was open
/// (and shown as an overlay: a wide window docks it, so it is no Esc
/// step there). Hover is dropped only when an overlay actually closes.
pub(crate) fn close_panel_overlay(ed: &mut EditorSession) -> bool {
    if !ed.panel_overlay_open || ed.window_width >= PANEL_DOCK_MIN_WINDOW {
        return false;
    }
    ed.panel_overlay_open = false;
    ed.text_panel.hovered = None;
    true
}

/// Bar widths of the recognizing skeleton, as fractions of the panel's
/// inner width (mockup section 5).
const SKELETON_WIDTHS: [f32; 5] = [0.70, 0.90, 0.55, 0.80, 0.62];
const SKELETON_PERIOD_MS: u128 = 1200;

/// Opacity of the recognizing skeleton: a 1.2 s triangle-wave pulse
/// between 0.55 and 1 (no trig), or a constant 1 under Reduce Motion.
pub(crate) fn skeleton_alpha(elapsed_ms: u128, reduce_motion: bool) -> f32 {
    if reduce_motion {
        return 1.0;
    }
    let t = (elapsed_ms % SKELETON_PERIOD_MS) as f32 / SKELETON_PERIOD_MS as f32;
    let tri = if t < 0.5 { t * 2.0 } else { 2.0 - t * 2.0 };
    0.55 + 0.45 * tri
}

/// Body of the Text tab for every recognition state.
pub(crate) fn text_tab<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Element<'a, Message> {
    use crate::editor::{FocusItem as F, FocusZone};
    let lit = |item: F| super::key_focus::is_focused(ed, FocusZone::Panel, item);
    let body: Element<'a, Message> = match &ed.text_panel.recognition {
        TextRecognition::Idle | TextRecognition::Recognizing { .. } => {
            recognizing_view(ed, state.reduce_motion)
        }
        TextRecognition::Failed(_) => empty_state(
            AppIcon::Alert,
            p,
            "Couldn’t read text",
            "Text recognition didn’t finish.",
            Some(focus_ring(
                bar_button(
                    text("Try Again").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Secondary,
                    ButtonSize::Sm,
                    Some(Message::EditorTextRecognize),
                ),
                lit(F::TextRetry),
            )),
        ),
        TextRecognition::Ready(doc) if doc.is_empty() => empty_state(
            AppIcon::ScanText,
            p,
            "No text found",
            "This capture has no readable text. Recognition languages are in Settings › Text Recognition.",
            Some(focus_ring(
                bar_button(
                    text("Open Settings").size(ButtonSize::Sm.text_size()).into(),
                    ButtonKind::Secondary,
                    ButtonSize::Sm,
                    Some(Message::EditorOpenTextSettings),
                ),
                lit(F::TextOpenSettings),
            )),
        ),
        TextRecognition::Ready(doc) => lines_view(state, ed, doc, p),
    };
    container(body)
        .padding(SP3)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn recognizing_view<'a>(ed: &EditorSession, reduce_motion: bool) -> Element<'a, Message> {
    let elapsed = match ed.text_panel.recognition {
        TextRecognition::Recognizing { since } => since.elapsed().as_millis(),
        _ => 0,
    };
    let alpha = skeleton_alpha(elapsed, reduce_motion);
    let inner = PANEL_WIDTH - 2.0 * SP3;
    let mut col = column![text("Recognizing text…").size(TYPE_CAPTION).style(text_t2)].spacing(SP3);
    for fraction in SKELETON_WIDTHS {
        col = col.push(
            container(
                Space::new()
                    .width(Length::Fixed(inner * fraction))
                    .height(Length::Fixed(10.0)),
            )
            .style(move |theme: &Theme| {
                let p = palette_for(theme);
                container::Style {
                    background: Some(
                        Color {
                            a: p.border.a * alpha,
                            ..p.border
                        }
                        .into(),
                    ),
                    border: iced::Border {
                        radius: R_CHIP.into(),
                        ..iced::Border::default()
                    },
                    ..container::Style::default()
                }
            }),
        );
    }
    col.into()
}

fn redact_card_style(theme: &Theme) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: Some(Color { a: 0.08, ..p.warn }.into()),
        border: iced::Border {
            radius: R_GROUP.into(),
            width: 1.0,
            color: Color { a: 0.35, ..p.warn },
        },
        ..container::Style::default()
    }
}

fn chip_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(palette_for(theme).hover.into()),
        border: iced::Border {
            radius: R_GROUP.into(),
            ..iced::Border::default()
        },
        ..container::Style::default()
    }
}

/// Title of the redact card: pending plus unlocatable items, or "All …
/// redacted" once nothing is left.
/// Only says "redacted" when every item is under a solid block; items
/// left blurred or pixelated (the chosen weaker style) are named as such.
pub(crate) fn redact_card_title(pending: usize, unlocatable: usize, weak: usize) -> String {
    match (pending + unlocatable, weak) {
        (0, 0) => "All detected items redacted".to_string(),
        (0, _) => "All detected items blurred or pixelated".to_string(),
        (n, _) => counted(n, "sensitive item", "sensitive items"),
    }
}

/// Size of the Text tab's safety and redaction notes: full sentences,
/// so the readable caption size, never Micro (11 px is for key chips and
/// labels).
pub(crate) const REDACT_NOTE_SIZE: f32 = TYPE_CAPTION;

/// The note for matches that can't be placed on the image.
pub(crate) fn unlocatable_note(m: usize) -> String {
    if m == 1 {
        "1 item couldn’t be located on the image. Cover it by hand.".to_string()
    } else {
        format!("{m} items couldn’t be located on the image. Cover them by hand.")
    }
}

/// Card note for items covered only by a Blur or Pixelate.
pub(crate) fn weak_note(weak: usize, style: RedactStyle) -> String {
    let (lead, verb) = if weak == 1 {
        ("1 item is".to_string(), "it")
    } else {
        (format!("{weak} items are"), "them")
    };
    if style == RedactStyle::Solid {
        format!("{lead} only blurred or pixelated (weaker). Redact adds a solid block over {verb}.")
    } else {
        format!("{lead} blurred or pixelated, which is weaker than a solid block.")
    }
}

/// "N sensitive items" card at the top of the Text tab:
/// style picker, Redact All, and a chip per kind. Hidden without
/// positions or matches; says "All … redacted" once everything is
/// covered. Redact All is Secondary: Copy Image is the view's primary.
/// Items that can't be located are named, never silently left out.
fn redact_card<'a>(
    ed: &'a EditorSession,
    rs: &RedactState,
    p: &'static Palette,
) -> Option<Element<'a, Message>> {
    ed.text_panel.positioned_document()?;
    if ed.text_panel.matches.is_empty() {
        return None;
    }
    let pending = &rs.pending;
    let n = pending.len();
    let unlocatable = &rs.unlocatable;
    let head = row![
        crate::icons::icon(AppIcon::Shield, p.warn, ICON_SIZE),
        text(redact_card_title(n, unlocatable.len(), rs.weak.len()))
            .size(TYPE_BODY)
            .font(semibold())
            .style(text_t1),
    ]
    .spacing(SP2)
    .align_y(Alignment::Center);
    use crate::editor::{FocusItem as F, FocusZone};
    let lit = |item: F| super::key_focus::is_focused(ed, FocusZone::Panel, item);
    let picker = focus_ring(
        pick_list(
            &RedactStyle::ALL[..],
            Some(ed.text_panel.redact_style),
            Message::EditorRedactStyleChanged,
        )
        .text_size(TYPE_CAPTION)
        .padding([4, 8])
        .width(Length::Fill),
        lit(F::TextRedactStyle),
    );
    let redact_all = focus_ring(
        bar_button(
            text("Redact All").size(ButtonSize::Sm.text_size()).into(),
            ButtonKind::Secondary,
            ButtonSize::Sm,
            (!ed.busy && n > 0).then_some(Message::EditorRedactAll),
        ),
        lit(F::TextRedactAll),
    );
    let mut chips = row![].spacing(SP2);
    for (kind, count) in summary(pending.iter().map(|(m, _)| m)) {
        chips = chips.push(
            container(text(kind.chip(count)).size(TYPE_MICRO).style(text_t2))
                .padding([3, 8])
                .style(chip_style),
        );
    }
    let mut body = column![
        head,
        row![picker, redact_all]
            .spacing(SP2)
            .align_y(Alignment::Center),
    ]
    .spacing(SP2);
    if ed.text_panel.redact_style != RedactStyle::Solid {
        body = body.push(
            text(
                "Pixelate and blur are weaker than a solid block: text can sometimes be recovered.",
            )
            .size(REDACT_NOTE_SIZE)
            .style(text_t2),
        );
    }
    if n > 0 {
        body = body.push(chips);
    }
    if !rs.weak.is_empty() {
        body = body.push(
            text(weak_note(rs.weak.len(), ed.text_panel.redact_style))
                .size(REDACT_NOTE_SIZE)
                .style(text_t2),
        );
    }
    if !unlocatable.is_empty() {
        let m = unlocatable.len();
        body = body.push(
            text(unlocatable_note(m))
                .size(REDACT_NOTE_SIZE)
                .style(text_t2),
        );
    }
    Some(
        container(body)
            .padding([10, 12])
            .width(Length::Fill)
            .style(redact_card_style)
            .into(),
    )
}

/// A selected line has the teal tint plus an `acc_text` outline, so it
/// differs from a hovered line by more than hue (WCAG 1.4.1).
pub(crate) fn line_row_style(theme: &Theme, selected: bool, hovered: bool) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: if selected {
            Some(p.acc_tint.into())
        } else if hovered {
            Some(p.hover.into())
        } else {
            None
        },
        border: iced::Border {
            radius: R_CHIP.into(),
            width: if selected { SELECTED_OUTLINE } else { 0.0 },
            color: if selected {
                p.acc_text
            } else {
                Color::TRANSPARENT
            },
        },
        ..container::Style::default()
    }
}

/// Room kept free at the end of a line row while its Redact button
/// shows: the Xs button ("Redact" at 11 px ≈ 36 px, plus 2 × 8 px
/// padding) and its 4 px inset, with a margin for wider fonts.
pub(crate) const REDACT_BUTTON_ROOM: f32 = 64.0;

/// One recognized line: hover outlines it on the image, click selects
/// (⇧ range, ⌘ toggle). `marks` (char ranges) are underlined;
/// `redacted` spans (already covered on the image) are muted and the
/// row gets a "redacted" marker, because copying still copies the
/// original text. With `redact_button` (hovered line with pending
/// matches) a Redact button sits on the row's trailing edge.
#[allow(clippy::too_many_arguments)]
fn line_row<'a>(
    index: usize,
    line: &'a TextLine,
    hovered: bool,
    keyboard_cursor: bool,
    selected: bool,
    marks: &[Range<usize>],
    redacted: &[Range<usize>],
    redact_button: bool,
    p: &'static Palette,
) -> Element<'a, Message> {
    let ink = if hovered || selected { p.t1 } else { p.t2 };
    let mark_bg = Color { a: 0.16, ..p.warn };
    let spans: Vec<iced::widget::text::Span<'a, (), iced::Font>> =
        line_spans(&line.text, marks, redacted)
            .into_iter()
            .map(|(segment, run)| {
                let s = iced::widget::span(segment);
                match run {
                    Run::Plain => s.color(ink),
                    Run::Marked => s.color(ink).underline(true).background(mark_bg),
                    Run::Redacted => s.color(p.t3),
                }
            })
            .collect();
    let mut inner = row![iced::widget::rich_text(spans)
        .size(TYPE_CAPTION)
        .width(Length::Fill)]
    .spacing(SP2)
    .align_y(Alignment::Center);
    if !redacted.is_empty() && !redact_button {
        inner = inner.push(tooltip_with_key(
            container(text("redacted").size(TYPE_MICRO).style(text_t3))
                .padding([1, 6])
                .style(chip_style),
            "Copied as ████",
            None,
            iced::widget::tooltip::Position::Top,
        ));
    }
    // The Redact button sits over the row's trailing edge; the text keeps
    // clear of it (it wraps before the button), so the button never hides
    // the match being checked. Reserving room, rather than putting the
    // taller button in the row, keeps the row's height on hover.
    let right = if redact_button {
        8.0 + REDACT_BUTTON_ROOM
    } else {
        8.0
    };
    let content = container(inner)
        .padding(iced::Padding {
            top: 6.0,
            bottom: 6.0,
            left: 8.0,
            right,
        })
        .width(Length::Fill)
        .style(move |theme: &Theme| line_row_style(theme, selected, hovered));
    let body: Element<'a, Message> = if redact_button {
        iced::widget::stack![
            content,
            container(tooltip_with_key(
                bar_button(
                    text("Redact").size(ButtonSize::Xs.text_size()).into(),
                    ButtonKind::Secondary,
                    ButtonSize::Xs,
                    Some(Message::EditorRedactLine(index)),
                ),
                "Redact This Line",
                Some("R"),
                iced::widget::tooltip::Position::Top,
            ))
            .width(Length::Fill)
            .align_x(Alignment::End)
            .padding([2, 4]),
        ]
        .into()
    } else {
        content.into()
    };
    let body = focus_ring(body, keyboard_cursor);
    mouse_area(body)
        .on_enter(Message::EditorTextLineHovered(index))
        .on_exit(Message::EditorTextLineUnhovered(index))
        .on_press(Message::EditorTextLineClicked(index))
        .interaction(iced::mouse::Interaction::Pointer)
        .into()
}

/// The recognized-text list with copy controls.
fn lines_view<'a>(
    state: &'a App,
    ed: &'a EditorSession,
    doc: &'a TextDocument,
    p: &'static Palette,
) -> Element<'a, Message> {
    let tp = &ed.text_panel;
    let count = doc.lines.len();
    let header = row![
        text("Recognized text")
            .size(TYPE_BODY)
            .font(semibold())
            .style(text_t1),
        Space::new().width(Length::Fill),
        text(counted(count, "line", "lines"))
            .size(TYPE_CAPTION)
            .style(text_t3),
    ]
    .align_y(Alignment::Center);
    use crate::editor::{FocusItem as F, FocusZone};
    let lit = |item: F| super::key_focus::is_focused(ed, FocusZone::Panel, item);
    let lines_focused = lit(F::TextLines);
    let rs = redact_state(ed);
    let mut list = column![].spacing(1);
    for (i, line) in doc.lines.iter().enumerate() {
        let marks: Vec<Range<usize>> = rs
            .pending
            .iter()
            .filter(|(m, _)| m.line == i)
            .map(|(m, _)| m.span.clone())
            .collect();
        let redacted: Vec<Range<usize>> = rs
            .covered
            .iter()
            .filter(|m| m.line == i)
            .map(|m| m.span.clone())
            .collect();
        let keyboard_cursor = lines_focused && tp.cursor == Some(i);
        // The keyboard cursor reads like hover: the line is outlined on
        // the image and its Redact button shows (R presses it).
        let hovered = tp.hovered == Some(i) || keyboard_cursor;
        list = list.push(line_row(
            i,
            line,
            hovered,
            keyboard_cursor,
            tp.selected.contains(&i),
            &marks,
            &redacted,
            hovered && !marks.is_empty() && !ed.busy,
            p,
        ));
    }
    let mut col = column![].spacing(SP2);
    if let Some(card) = redact_card(ed, &rs, p) {
        col = col.push(card);
    }
    col = col.push(header).push(
        scrollable(list)
            .id(iced::widget::Id::new(super::key_focus::TEXT_LINES_ID))
            .height(Length::Fill),
    );
    if !doc.positioned {
        col = col.push(
            text("This platform’s text recognition doesn’t report where lines are, so hover-to-locate and smart redact are off.")
                .size(REDACT_NOTE_SIZE)
                .style(text_t3),
        );
    }
    let toggle = focus_ring(
        toggler(state.preferences.editor_copy_remove_line_breaks)
            .label("Remove line breaks when copying")
            .text_size(TYPE_CAPTION)
            .size(16.0)
            .on_toggle(Message::EditorRemoveLineBreaksToggled),
        lit(F::TextLineBreaks),
    );
    let n = tp.selected.len();
    let copy_lines = focus_ring(
        bar_button(
            text(copy_lines_label(n))
                .size(ButtonSize::Sm.text_size())
                .into(),
            ButtonKind::Secondary,
            ButtonSize::Sm,
            (n > 0).then_some(Message::EditorCopyLines),
        )
        .width(Length::Fill),
        lit(F::TextCopyLines),
    );
    let copy_all = bar_button(
        row![
            text("Copy All").size(ButtonSize::Sm.text_size()),
            key_chip(COPY_TEXT_KEY),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        ButtonKind::Secondary,
        ButtonSize::Sm,
        (!ed.busy).then_some(Message::EditorCopyTextRequested),
    )
    .width(Length::Fill);
    let copy_all = focus_ring(copy_all, lit(F::TextCopyAll));
    col.push(toggle)
        .push(row![copy_lines, copy_all].spacing(SP2))
        .into()
}

/// Boxes the canvas marks for the Text panel: the hovered line and the
/// selected lines. Nothing when the panel is not on screen or the engine
/// gave no positions (hover-to-locate is macOS-only today).
pub(crate) fn text_marks(
    ed: &EditorSession,
    panel_visible: bool,
) -> (Option<RectLike>, Vec<RectLike>) {
    if !panel_visible {
        return (None, Vec::new());
    }
    let Some(doc) = current_positioned_document(ed) else {
        return (None, Vec::new());
    };
    let rect = |i: usize| doc.lines.get(i).and_then(|l| l.rect);
    (
        ed.text_panel
            .hovered
            .or(ed.text_panel.cursor)
            .and_then(rect),
        ed.text_panel
            .selected
            .iter()
            .filter_map(|&i| rect(i))
            .collect(),
    )
}

/// Smart highlighter: with Snap to text on and line
/// positions available, a highlighter stroke over a text line becomes a
/// straight mark across the touched words, as tall as the line. Every
/// other annotation, and strokes off text, pass through unchanged.
pub(crate) fn snap_highlight_annotation(
    ed: &EditorSession,
    annotation: readshot_core::Annotation,
) -> readshot_core::Annotation {
    use readshot_core::Annotation;
    match annotation {
        Annotation::Highlighter {
            points,
            color,
            line_width,
        } => {
            // A sideways document turns text lines vertical; the snap
            // only knows horizontal lines, so the stroke stays as drawn.
            if ed.model.geometry().transform.is_sideways() {
                return Annotation::Highlighter {
                    points,
                    color,
                    line_width,
                };
            }
            let snapped = ed
                .model
                .tool_style()
                .snap_to_text
                .then(|| current_positioned_document(ed))
                .flatten()
                .and_then(|doc| readshot_core::text_lines::snap_highlight(doc, &points));
            match snapped {
                Some((points, width)) => Annotation::Highlighter {
                    points,
                    color,
                    line_width: width,
                },
                None => Annotation::Highlighter {
                    points,
                    color,
                    line_width,
                },
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_static_under_reduce_motion() {
        for ms in [0, 300, 600, 900, 1199, 5000] {
            assert_eq!(skeleton_alpha(ms, true), 1.0);
        }
    }

    #[test]
    fn skeleton_pulse_is_a_bounded_triangle_wave() {
        assert_eq!(skeleton_alpha(0, false), 0.55);
        assert!((skeleton_alpha(600, false) - 1.0).abs() < 1e-6);
        assert_eq!(skeleton_alpha(1200, false), 0.55);
        for ms in (0..2400).step_by(37) {
            let a = skeleton_alpha(ms, false);
            assert!((0.55..=1.0).contains(&a), "{ms}: {a}");
        }
    }

    use std::collections::BTreeSet;

    fn set(items: &[usize]) -> BTreeSet<usize> {
        items.iter().copied().collect()
    }

    #[test]
    fn click_selects_one_line_and_clicking_it_again_clears() {
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 2, false, false);
        assert_eq!((sel.clone(), anchor), (set(&[2]), Some(2)));
        click_selection(&mut sel, &mut anchor, 4, false, false);
        assert_eq!((sel.clone(), anchor), (set(&[4]), Some(4)));
        click_selection(&mut sel, &mut anchor, 4, false, false);
        assert_eq!((sel, anchor), (set(&[]), None));
    }

    #[test]
    fn shift_click_selects_the_range_from_the_anchor() {
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 3, false, false);
        click_selection(&mut sel, &mut anchor, 1, true, false);
        assert_eq!((sel.clone(), anchor), (set(&[1, 2, 3]), Some(3)));
        click_selection(&mut sel, &mut anchor, 5, true, false);
        assert_eq!(sel, set(&[3, 4, 5]));
        // ⇧ with no anchor selects just that line and anchors it.
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 2, true, false);
        assert_eq!((sel, anchor), (set(&[2]), Some(2)));
    }

    #[test]
    fn command_click_toggles_and_command_shift_adds_a_range() {
        let (mut sel, mut anchor) = (BTreeSet::new(), None);
        click_selection(&mut sel, &mut anchor, 1, false, true);
        click_selection(&mut sel, &mut anchor, 4, false, true);
        assert_eq!(sel, set(&[1, 4]));
        click_selection(&mut sel, &mut anchor, 1, false, true);
        assert_eq!((sel.clone(), anchor), (set(&[4]), Some(1)));
        click_selection(&mut sel, &mut anchor, 7, true, true);
        assert_eq!(sel, set(&[1, 2, 3, 4, 5, 6, 7]));
    }

    #[test]
    fn copy_lines_label_counts_lines() {
        assert_eq!(copy_lines_label(0), "Copy Lines");
        assert_eq!(copy_lines_label(1), "Copy 1 Line");
        assert_eq!(copy_lines_label(12), "Copy 12 Lines");
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn line_spans_mark_char_ranges() {
        assert_eq!(
            line_spans("Email a@b.co", &[6..12], &[]),
            vec![
                ("Email ".to_string(), Run::Plain),
                ("a@b.co".to_string(), Run::Marked)
            ]
        );
        assert_eq!(
            line_spans("é@x", &[1..2], &[]),
            vec![
                ("é".to_string(), Run::Plain),
                ("@".to_string(), Run::Marked),
                ("x".to_string(), Run::Plain)
            ]
        );
        assert!(line_spans("", &[], &[]).is_empty());
    }

    #[test]
    fn clusters_keep_marks_joiners_selectors_tones_and_flags_together() {
        let parts = |s: &str| -> Vec<String> {
            clusters(s)
                .into_iter()
                .map(|(_, c)| c.to_string())
                .collect()
        };
        // e + combining acute; a precomposed é stays one char.
        assert_eq!(parts("e\u{301}x"), ["e\u{301}", "x"]);
        // A ZWJ family is one cluster.
        let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        assert_eq!(parts(&format!("a{family}b")), ["a", family, "b"]);
        // Variation selector, skin tone, keycap.
        assert_eq!(parts("\u{2764}\u{FE0F}!"), ["\u{2764}\u{FE0F}", "!"]);
        assert_eq!(parts("\u{1F44B}\u{1F3FD}"), ["\u{1F44B}\u{1F3FD}"]);
        assert_eq!(parts("1\u{FE0F}\u{20E3}"), ["1\u{FE0F}\u{20E3}"]);
        // Regional indicators pair up: two flags, then a lone one.
        let (fr, de) = ("\u{1F1EB}\u{1F1F7}", "\u{1F1E9}\u{1F1EA}");
        assert_eq!(parts(&format!("{fr}{de}\u{1F1EB}")), [fr, de, "\u{1F1EB}"]);
        // Char ranges line up with char indices.
        let ranges: Vec<_> = clusters("e\u{301}ab").into_iter().map(|(r, _)| r).collect();
        assert_eq!(ranges, [0..2, 2..3, 3..4]);
        assert!(clusters("").is_empty());
        // An Indic conjunct (क्ष = ka + virama + ssa) is one cluster.
        assert_eq!(
            parts("\u{915}\u{94D}\u{937}a"),
            ["\u{915}\u{94D}\u{937}", "a"]
        );
        // Gurmukhi too (ਕ੍ਸ).
        assert_eq!(parts("\u{A15}\u{A4D}\u{A38}"), ["\u{A15}\u{A4D}\u{A38}"]);
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn line_spans_never_split_a_cluster() {
        // A detector span ending between e and its accent marks both.
        assert_eq!(
            line_spans("xe\u{301}y", &[0..2], &[]),
            vec![
                ("xe\u{301}".to_string(), Run::Marked),
                ("y".to_string(), Run::Plain)
            ]
        );
        // A redaction covering half a flag mutes the whole flag.
        let flag = "\u{1F1EB}\u{1F1F7}";
        assert_eq!(
            line_spans(&format!("{flag} a@b.co"), &[3..9], &[1..2]),
            vec![
                (flag.to_string(), Run::Redacted),
                (" ".to_string(), Run::Plain),
                ("a@b.co".to_string(), Run::Marked)
            ]
        );
        // A ZWJ family split by a mark is marked whole.
        let family = "\u{1F468}\u{200D}\u{1F469}";
        assert_eq!(
            line_spans(family, &[2..3], &[]),
            vec![(family.to_string(), Run::Marked)]
        );
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)]
    fn line_spans_mute_redacted_ranges_and_marks_win() {
        assert_eq!(
            line_spans("ab cd", &[3..5], &[0..2]),
            vec![
                ("ab".to_string(), Run::Redacted),
                (" ".to_string(), Run::Plain),
                ("cd".to_string(), Run::Marked)
            ]
        );
        assert_eq!(
            line_spans("ab", &[0..1], &[0..2]),
            vec![
                ("a".to_string(), Run::Marked),
                ("b".to_string(), Run::Redacted)
            ]
        );
    }

    #[test]
    fn redact_status_counts_items_and_reports_unlocatable_ones() {
        assert_eq!(redact_status(0, 0), "No sensitive items left to redact.");
        assert_eq!(redact_status(1, 0), "Redacted 1 item.");
        assert_eq!(redact_status(4, 0), "Redacted 4 items.");
        assert_eq!(
            redact_status(2, 1),
            "Redacted 2 items. 1 more couldn’t be located; cover it by hand."
        );
        assert_eq!(
            redact_status(0, 3),
            "Couldn’t locate 3 sensitive items. Cover them by hand."
        );
    }

    #[test]
    fn redact_card_title_counts_unlocatable_items_too() {
        assert_eq!(redact_card_title(0, 0, 0), "All detected items redacted");
        assert_eq!(redact_card_title(1, 0, 0), "1 sensitive item");
        assert_eq!(redact_card_title(0, 2, 0), "2 sensitive items");
        assert_eq!(redact_card_title(3, 1, 0), "4 sensitive items");
        // Weakly covered items never read as "redacted".
        assert_eq!(
            redact_card_title(0, 0, 2),
            "All detected items blurred or pixelated"
        );
        assert_eq!(redact_card_title(2, 0, 2), "2 sensitive items");
    }

    #[test]
    fn redaction_notes_are_readable_and_clear_the_redact_button() {
        const { assert!(REDACT_NOTE_SIZE >= TYPE_CAPTION && REDACT_NOTE_SIZE > TYPE_MICRO) };
        // "Redact" at Xs (11 px, ≈ 0.6 em a glyph) plus its padding and
        // inset fits the room the text keeps free.
        let button = "Redact".len() as f32 * ButtonSize::Xs.text_size() * 0.6
            + 2.0 * ButtonSize::Xs.padding_x()
            + 4.0;
        assert!(REDACT_BUTTON_ROOM >= button, "{button}");
        assert_eq!(
            unlocatable_note(1),
            "1 item couldn’t be located on the image. Cover it by hand."
        );
        assert_eq!(
            unlocatable_note(3),
            "3 items couldn’t be located on the image. Cover them by hand."
        );
    }

    #[test]
    fn weak_note_says_what_solid_redact_will_do() {
        assert_eq!(
            weak_note(1, RedactStyle::Solid),
            "1 item is only blurred or pixelated (weaker). Redact adds a solid block over it."
        );
        assert_eq!(
            weak_note(2, RedactStyle::Blur),
            "2 items are blurred or pixelated, which is weaker than a solid block."
        );
    }

    #[test]
    fn a_selected_line_has_an_outline_a_hovered_one_does_not() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let t = readshot_theme_for(mode);
            let p = palette_for(&t);
            let sel = line_row_style(&t, true, false);
            assert_eq!(sel.border.width, SELECTED_OUTLINE);
            assert_eq!(sel.border.color, p.acc_text);
            let hov = line_row_style(&t, false, true);
            assert_eq!(hov.border.width, 0.0);
            assert_eq!(hov.background, Some(p.hover.into()));
        }
    }
}
