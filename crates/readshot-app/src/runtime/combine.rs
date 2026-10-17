//! Combine captures (spec §3.11, decision D4). Add Capture reuses the
//! capture overlay while the editor window is hidden; Add Image from File
//! uses the system picker. Each addition is one undo step.
//!
//! Every geometry change goes through the validated `EditorState` APIs;
//! a refused one becomes a status. A new part has no recognition: History
//! keeps no text for the combined document until every part is
//! recognized, each once and upright in its own pixels. Adding to a
//! transformed document bakes it into one part first, whose text is its
//! source parts' recognitions mapped through the bake
//! ([`super::text_panel::resolve_baked_docs`]), never a recognition of
//! the turned pixels.

use super::*;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{window, Task};
use readshot_core::document::{Join, MAX_DOC_PIXELS, MAX_DOC_SIDE};

use crate::app::{App, CaptureIntent, CombineCapture, Message};
use crate::editor::EditorSession;

/// Gap slider previews recompose the whole document, so they are
/// throttled to this interval; the newest value is applied on the
/// trailing edge.
pub(crate) const GAP_PREVIEW_INTERVAL: Duration = Duration::from_millis(50);

/// Largest image file Add Image from File reads, about the document's
/// pixel budget as RGBA: anything bigger can't fit once decoded.
const MAX_PART_FILE_BYTES: u64 = MAX_DOC_PIXELS * 4;

fn show_editor(id: Option<window::Id>) -> Task<Message> {
    match id {
        Some(id) => {
            window::set_mode::<Message>(id, window::Mode::Windowed).chain(window::gain_focus(id))
        }
        None => Task::none(),
    }
}

/// Append `image` to the editor's document; returns follow-up tasks
/// (recognition of the new part). `what` is "capture" or "image".
fn append(state: &mut App, image: image::RgbaImage, join: Join, what: &str) -> Task<Message> {
    let coord = state.coordinator.clone();
    let panel_open = state.preferences.editor_panel_open;
    let Some(ed) = state.editor.as_mut() else {
        return Task::none();
    };
    let old = Arc::clone(ed.model.geometry());
    // Same test as `EditorState::add_part`: a transformed document is
    // baked into one fresh part first (D4).
    let baking = !old.transform.is_identity();
    let cropped = ed.last_crop().is_some();
    match ed.model.add_part(image, join) {
        Ok(_) => {
            if baking {
                if let Some(baked) = ed.model.geometry().parts.first() {
                    ed.text_panel.bake_sources.insert(baked.id, old);
                }
                let _ = super::text_panel::resolve_baked_docs(&mut ed.text_panel);
            }
            ed.refresh_image();
            ed.clear_eyedropper_loupe();
            ed.clear_discard_confirmation();
            let side = match join {
                Join::Below => "below",
                Join::Beside => "beside",
            };
            ed.set_status(format!("Added {what} {side}. ⌘Z to undo."));
            // Rebuilds the word boxes for the new frame (no document while
            // the new part is unrecognized) and rewrites History.
            sync_editor_history(ed, &coord);
            // Recognize the new part when text is in use: panel open, the
            // earlier parts recognized or being recognized, a History
            // record that keeps text, or a redaction that needs masked text.
            let wanted = panel_open
                || !ed.text_panel.part_docs.is_empty()
                || matches!(
                    ed.text_panel.recognition,
                    crate::editor::TextRecognition::Recognizing { .. }
                )
                || ed.source_record.is_some()
                || readshot_core::scrub::has_redaction(ed.model.annotations());
            if wanted {
                super::text_panel::request_text_recognition(state)
            } else {
                Task::none()
            }
        }
        Err(e) => {
            ed.set_status(geometry_error_status(
                &format!("add the {what}"),
                cropped,
                e,
            ));
            Task::none()
        }
    }
}

/// `RegionCaptureCompleted(Ok)` with intent `CombineIntoEditor`: append
/// to the session that asked, and show its window again. Never opens an
/// editor; a result for a session that is gone is dropped.
pub(crate) fn finish_combine_capture(state: &mut App, image: image::RgbaImage) -> Task<Message> {
    let Some(cc) = state.combine_capture.take() else {
        return Task::none();
    };
    let Some(window_id) = state
        .editor
        .as_ref()
        .filter(|ed| ed.session_id == cc.session)
        .map(|ed| ed.window_id)
    else {
        return Task::none();
    };
    let follow = append(state, image, cc.join, "capture");
    Task::batch([show_editor(window_id), follow])
}

/// The combine capture ended without an image (error, esc, no display):
/// show the editor again, unchanged. `status` is a complete, friendly
/// message for the editor's toast (`None` for a plain cancel).
pub(crate) fn abort_combine_capture(state: &mut App, status: Option<&str>) -> Task<Message> {
    // Past its selection (`OverlaySelected` marked it) the capture is in
    // flight: it must land in this editor or fail there via
    // `fail_combine_capture`. Aborting now would let it fall back to the
    // Editor intent and replace the editor.
    if combine_selection_made(state) {
        return Task::none();
    }
    fail_combine_capture(state, status)
}

/// The combine's overlay selection was made, so its capture is in flight.
pub(crate) fn combine_selection_made(state: &App) -> bool {
    state.combine_capture.is_some_and(|cc| cc.selected)
}

/// End the combine without an image whether or not its selection was
/// made: `RegionCaptureCompleted(Err)` is the in-flight capture's own end.
pub(crate) fn fail_combine_capture(state: &mut App, status: Option<&str>) -> Task<Message> {
    let Some(cc) = state.combine_capture.take() else {
        return Task::none();
    };
    // A stale intent would make the next ordinary capture auto-confirm
    // into a combine that no longer exists, and drop it.
    if state.pending_intent == Some(CaptureIntent::CombineIntoEditor) {
        state.pending_intent = None;
    }
    let Some(ed) = state
        .editor
        .as_mut()
        .filter(|ed| ed.session_id == cc.session)
    else {
        return Task::none();
    };
    if let Some(status) = status {
        ed.set_status(status);
    }
    show_editor(ed.window_id)
}

/// Read and decode a picked file as an untrusted input: its size and
/// the image's dimensions are checked before the pixels are allocated.
/// Errors are short, user-facing reasons (the raw error is logged).
pub(crate) fn load_part_file(
    path: &std::path::Path,
    budget: PartBudget,
) -> Result<image::RgbaImage, String> {
    use std::io::Read;
    let io_err = |what: &str, e: std::io::Error| {
        tracing::warn!(target: "readshot::editor", "add image: {what}: {e}");
        history_ops::io_reason_hint(&e.to_string())
            .unwrap_or("the file couldn't be read")
            .to_string()
    };
    let meta = std::fs::metadata(path).map_err(|e| io_err("metadata", e))?;
    // A FIFO, device or directory can report any length and block (or
    // fail) on read: only regular files are opened.
    if !meta.is_file() {
        return Err("that isn't an image file".into());
    }
    if meta.len() > MAX_PART_FILE_BYTES {
        return Err("the file is too large".into());
    }
    // Bounded even if the file grows after the check.
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| io_err("open", e))?
        .take(MAX_PART_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_err("read", e))?;
    if bytes.len() as u64 > MAX_PART_FILE_BYTES {
        return Err("the file is too large".into());
    }
    decode_part_image(&bytes, budget)
}

/// The document an added file joins (Add Image from File appends below):
/// its layout size once any transform is baked, and its gap. Lets the
/// file's header be checked against the combined size before decoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PartBudget {
    pub doc_width: u32,
    pub doc_height: u32,
    pub gap: u32,
}

impl PartBudget {
    /// The budget for appending below `g` (mirrors `add_part`: a
    /// transformed document is baked into one part of its output size).
    pub(crate) fn below(g: &readshot_core::document::DocGeometry) -> Self {
        let (doc_width, doc_height) = if g.transform.is_identity() {
            let l = g.layout();
            (l.width, l.height)
        } else {
            g.size()
        };
        Self {
            doc_width,
            doc_height,
            gap: g.gap.min(readshot_core::document::MAX_GAP),
        }
    }

    /// Whether a `w` × `h` part appended below fits the document limits.
    fn check(&self, w: u32, h: u32) -> Result<(), String> {
        let cw = self.doc_width.max(w);
        let ch = u64::from(self.doc_height) + u64::from(self.gap) + u64::from(h);
        if u64::from(cw) > u64::from(MAX_DOC_SIDE) || ch > u64::from(MAX_DOC_SIDE) {
            return Err(format!(
                "the combined image would be {cw} × {ch} px, and the limit is {MAX_DOC_SIDE} px on a side"
            ));
        }
        let pixels = u64::from(cw) * ch;
        if pixels > MAX_DOC_PIXELS {
            return Err(format!(
                "the combined image would be {pixels} px, over the {MAX_DOC_PIXELS} px limit"
            ));
        }
        Ok(())
    }
}

/// Decode image bytes, refusing anything over the document limits
/// before allocating its pixels.
pub(crate) fn decode_part_image(
    bytes: &[u8],
    budget: PartBudget,
) -> Result<image::RgbaImage, String> {
    const NOT_AN_IMAGE: &str = "that file isn't an image Readshot can read";
    let reader = || {
        image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| {
                tracing::warn!(target: "readshot::editor", "add image: format: {e}");
                NOT_AN_IMAGE.to_string()
            })
    };
    let (w, h) = reader()?.into_dimensions().map_err(|e| {
        tracing::warn!(target: "readshot::editor", "add image: header: {e}");
        NOT_AN_IMAGE.to_string()
    })?;
    if w == 0 || h == 0 {
        return Err("the image is empty".into());
    }
    if w > MAX_DOC_SIDE || h > MAX_DOC_SIDE {
        return Err(format!(
            "the image is {w} × {h} px, and the limit is {MAX_DOC_SIDE} px on a side"
        ));
    }
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_DOC_PIXELS {
        return Err(format!(
            "image too large: {pixels} px is over the {MAX_DOC_PIXELS} px limit"
        ));
    }
    // The document it joins must still fit: refuse before decoding, not
    // after a large allocation that `add_part` then rejects.
    budget.check(w, h)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DOC_SIDE);
    limits.max_image_height = Some(MAX_DOC_SIDE);
    limits.max_alloc = Some(MAX_DOC_PIXELS * 4);
    let mut decoder = reader()?;
    decoder.limits(limits);
    let img = decoder.decode().map_err(|e| {
        tracing::warn!(target: "readshot::editor", "add image: decode: {e}");
        match e {
            image::ImageError::Limits(_) => "the image is too large to open".to_string(),
            _ => NOT_AN_IMAGE.to_string(),
        }
    })?;
    // A decoder may report other dimensions than its header did.
    if img.width() > MAX_DOC_SIDE
        || img.height() > MAX_DOC_SIDE
        || u64::from(img.width()) * u64::from(img.height()) > MAX_DOC_PIXELS
    {
        return Err("the image is too large to open".into());
    }
    Ok(img.into_rgba8())
}

/// The editor's toast when an Add Capture's capture failed (`raw` is
/// logged by the caller).
pub(crate) fn combine_capture_failed_status(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let next_step = if lower.contains("permission") || lower.contains("denied") {
        history_ops::CAPTURE_PERMISSION_HINT
    } else {
        history_ops::RETRY_HINT
    };
    history_ops::friendly_string_error_with("add the capture", raw, Some(next_step))
}

async fn pick_and_decode_image(
    budget: PartBudget,
) -> Result<Option<Arc<image::RgbaImage>>, String> {
    let Some(file) = rfd::AsyncFileDialog::new()
        .add_filter(
            "Images",
            &["png", "jpg", "jpeg", "gif", "bmp", "tif", "tiff", "webp"],
        )
        .pick_file()
        .await
    else {
        return Ok(None);
    };
    let path: PathBuf = file.path().to_path_buf();
    tokio::task::spawn_blocking(move || {
        load_part_file(&path, budget).map(|img| Some(Arc::new(img)))
    })
    .await
    .map_err(|e| {
        tracing::warn!(target: "readshot::editor", "add image worker: {e}");
        "the file couldn't be read".to_string()
    })?
}

/// Preview the gap slider's `gap` over the drag's baseline (no undo
/// step; release commits). A refused layout changes nothing.
fn preview_gap(ed: &mut EditorSession, gap: u32) {
    let Some(base) = ed.snapshot_drag_baseline.clone() else {
        return;
    };
    let next = base.doc.geometry.with_layout(gap, base.doc.geometry.align);
    let cropped = ed.last_crop().is_some();
    match ed.model.preview_geometry(&base, next) {
        Ok(()) => ed.refresh_image(),
        Err(e) => ed.set_status(geometry_error_status("change the gap", cropped, e)),
    }
    ed.layout_gap_previewed_at = Some(Instant::now());
}

/// Apply a coalesced gap value that hasn't been previewed yet, while its
/// drag is still open. Release and every output path
/// (`commit_snapshot_drag`) call this first, so what is kept is the
/// value the slider shows.
pub(crate) fn flush_gap_preview(ed: &mut EditorSession) {
    if let Some(gap) = ed.layout_gap_pending.take() {
        if ed.snapshot_drag_baseline.is_some() && !ed.snapshot_drag_cancelled {
            preview_gap(ed, gap);
        }
    }
}

pub(crate) fn handle_combine_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::EditorCombineCaptureRequested(join) => {
            if state.welcome.should_show()
                || state.capture_in_flight
                || state.overlay_opening
                || state.combine_capture.is_some()
                || state.scroll_session.is_some()
                || state.windows.kinds().any(|k| k == WindowKind::Overlay)
            {
                return Task::none();
            }
            let coord = state.coordinator.clone();
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            super::image_menu::prepare(ed, &coord);
            state.combine_capture = Some(CombineCapture {
                session: ed.session_id,
                join,
                selected: false,
            });
            // Hidden, so the capture can't contain the editor itself.
            let hide = match ed.window_id {
                Some(id) => window::set_mode::<Message>(id, window::Mode::Hidden),
                None => Task::none(),
            };
            state.pending_intent = Some(CaptureIntent::CombineIntoEditor);
            hide.chain(Task::done(Message::OpenOverlayRequested))
        }
        Message::EditorAddImageFromFile => {
            let coord = state.coordinator.clone();
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            super::image_menu::prepare(ed, &coord);
            ed.busy = true;
            ed.set_status("Choose an image…");
            let session = ed.session_id;
            let budget = PartBudget::below(ed.model.geometry());
            Task::perform(pick_and_decode_image(budget), move |result| {
                Message::EditorPartLoaded { session, result }
            })
        }
        Message::EditorPartLoaded { session, result } => {
            let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) else {
                return Task::none();
            };
            ed.busy = false;
            match result {
                Ok(Some(img)) => append(state, Arc::unwrap_or_clone(img), Join::Below, "image"),
                Ok(None) => {
                    ed.status = None;
                    ed.status_set_at = None;
                    Task::none()
                }
                Err(e) => {
                    ed.set_status(format!("Couldn't add the image — {e}."));
                    Task::none()
                }
            }
        }
        Message::EditorLayoutGapPreview(gap) => {
            let coord = state.coordinator.clone();
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            // The slider is still held after Esc / undo cancelled its drag:
            // ticks until release must not start a new preview.
            if ed.snapshot_drag_cancelled {
                return Task::none();
            }
            if ed.snapshot_drag_baseline.is_none() {
                // Settle every other preview and draft before the baseline:
                // the parts are about to move under them.
                cancel_editor_previews(ed);
                commit_pending_editor_text(ed, &coord);
                ed.crop_draft = None;
                ed.snapshot_drag_baseline = Some(ed.model.snapshot());
                ed.layout_gap_previewed_at = None;
                ed.layout_gap_pending = None;
            }
            let since = ed.layout_gap_previewed_at.map(|t| t.elapsed());
            match since {
                Some(elapsed) if elapsed < GAP_PREVIEW_INTERVAL => {
                    // Coalesce: keep the newest value; one trailing flush.
                    if ed.layout_gap_pending.replace(gap).is_none() {
                        let session = ed.session_id;
                        let wait = GAP_PREVIEW_INTERVAL.saturating_sub(elapsed);
                        // Lazily: the timer is made when the task runs.
                        let wait = async move { tokio::time::sleep(wait).await };
                        return Task::perform(wait, move |_| Message::EditorLayoutGapFlush {
                            session,
                        });
                    }
                }
                _ => {
                    ed.layout_gap_pending = None;
                    preview_gap(ed, gap);
                }
            }
            Task::none()
        }
        Message::EditorLayoutGapFlush { session } => {
            if let Some(ed) = state
                .editor
                .as_mut()
                .filter(|ed| ed.session_id == session && !ed.busy)
            {
                flush_gap_preview(ed);
            }
            Task::none()
        }
        Message::EditorLayoutAlign(align) => {
            let coord = state.coordinator.clone();
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if ed.busy {
                return Task::none();
            }
            cancel_editor_previews(ed);
            ed.snapshot_drag_cancelled = false;
            commit_pending_editor_text(ed, &coord);
            ed.crop_draft = None;
            let gap = ed.model.geometry().gap;
            let cropped = ed.last_crop().is_some();
            match ed.model.set_layout(gap, align) {
                Ok(true) => {
                    ed.refresh_image();
                    ed.clear_eyedropper_loupe();
                    ed.set_status(format!(
                        "Aligned to {}. ⌘Z to undo.",
                        align.label().to_lowercase()
                    ));
                    sync_editor_history(ed, &coord);
                }
                Ok(false) => {}
                Err(e) => ed.set_status(geometry_error_status("align", cropped, e)),
            }
            Task::none()
        }
        other => unreachable!("non-combine message routed to the combine handler: {other:?}"),
    }
}
