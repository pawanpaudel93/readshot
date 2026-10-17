//! Off-thread document composes (#80 / #100). Near the 256 MP budget a
//! resize, an alignment or a gap preview composes and flattens ~1 GB
//! each; done on the UI thread that freezes the editor. Documents of at
//! least [`crate::editor::ASYNC_COMPOSE_MIN_PIXELS`] output pixels are
//! composed on a blocking worker instead; smaller ones stay synchronous.
//!
//! The editor keeps showing the previous image until the result lands.
//! Every request is tagged (`seq`, the history generation, the present
//! state and, for a gap preview, the slider drag's baseline). Gap
//! previews coalesce: one compose in flight, the newest value wins.
//!
//! A resize or alignment is never dropped and never lands under an open
//! interaction: every editor message that could start a drag or draft,
//! set `busy`, undo, output or otherwise move history first settles it
//! on the UI thread ([`settle_before`]; only an allow-list of passive
//! messages may wait). So a landing meets the state it was made for, and
//! is one undo step.

use super::*;

use iced::Task;

use crate::app::{App, Message};
use crate::editor::{ComposeKind, ComposeRequest, ComposedDoc, ComposedOutput, EditorSession};
use readshot_ui::editor::Snapshot;

/// Whether moving to `next` composes off the UI thread.
pub(crate) fn is_large(ed: &EditorSession, next: &Snapshot) -> bool {
    let (w, h) = next.doc.geometry.size();
    u64::from(w) * u64::from(h) >= ed.async_compose_min_pixels
}

/// Compose and flatten `next` (blocking; runs on a worker). The geometry
/// was validated when the request was made.
pub(crate) fn compose(next: Snapshot) -> ComposedDoc {
    let base = next.doc.geometry.compose();
    let flattened = readshot_core::render(&base, &next.annotations);
    let handle = iced::widget::image::Handle::from_rgba(
        flattened.width(),
        flattened.height(),
        flattened.clone().into_raw(),
    );
    ComposedDoc {
        next,
        base,
        flattened,
        handle,
    }
}

/// Start composing `next` for `kind` off the UI thread. It replaces any
/// request still running: that one's result will be dropped.
pub(crate) fn spawn(
    ed: &mut EditorSession,
    kind: ComposeKind,
    next: Snapshot,
    drag_baseline: Option<Snapshot>,
) -> Task<Message> {
    // Every message that can start a compose settles a pending resize /
    // alignment first (`settle_before`): only a gap compose is replaced.
    debug_assert!(
        ed.compose_job
            .as_ref()
            .is_none_or(|j| matches!(j.kind, ComposeKind::GapPreview(_))),
        "a pending step would be lost"
    );
    ed.compose_seq += 1;
    let seq = ed.compose_seq;
    let request = ComposeRequest {
        seq,
        kind,
        generation: ed.model.history_generation(),
        present: ed.model.snapshot(),
        drag_baseline,
        next,
    };
    let job = request.next.clone();
    ed.compose_job = Some(request);
    let session = ed.session_id;
    let work = async move {
        tokio::task::spawn_blocking(move || compose(job))
            .await
            .map_err(|e| tracing::warn!(target: "readshot::editor", "compose worker: {e}"))
            .ok()
    };
    Task::perform(work, move |doc| Message::EditorComposed {
        session,
        seq,
        output: ComposedOutput::new(doc),
    })
}

/// Whether a gap compose is running or queued for the open slider drag.
pub(crate) fn gap_outstanding(ed: &EditorSession) -> bool {
    ed.gap_queued.is_some()
        || ed
            .compose_job
            .as_ref()
            .is_some_and(|j| matches!(j.kind, ComposeKind::GapPreview(_)))
}

/// The newest gap value requested but not shown yet, dropping every
/// outstanding gap compose (its result will be ignored).
pub(crate) fn take_outstanding_gap(ed: &mut EditorSession) -> Option<u32> {
    let running = match ed.compose_job.as_ref().map(|j| j.kind) {
        Some(ComposeKind::GapPreview(gap)) => {
            ed.compose_job = None;
            Some(gap)
        }
        _ => None,
    };
    ed.gap_queued.take().or(running)
}

/// Drop any outstanding gap compose and a deferred release commit (the
/// slider drag they belong to is being cancelled).
pub(crate) fn cancel_gap_compose(ed: &mut EditorSession) {
    let _ = take_outstanding_gap(ed);
    ed.gap_commit_on_land = false;
}

/// `Message::EditorComposed`.
pub(crate) fn handle_composed(
    state: &mut App,
    session: u64,
    seq: u64,
    output: ComposedOutput,
) -> Task<Message> {
    let coord = state.coordinator.clone();
    let Some(ed) = state.editor.as_mut().filter(|ed| ed.session_id == session) else {
        return Task::none();
    };
    // Superseded by a newer request (or settled already): never applied.
    let Some(request) = ed.compose_job.take_if(|j| j.seq == seq) else {
        return Task::none();
    };
    let doc = output.take();
    match request.kind {
        ComposeKind::Resize { .. } | ComposeKind::Align(_) => {
            // Every message that sets `busy` settles a pending step first
            // (`settle_before`), so this can't happen; if it did, keep the
            // step pending rather than change the document under an
            // output. It is settled with the next interaction.
            if ed.busy {
                debug_assert!(false, "a compose landed while busy");
                ed.compose_job = Some(request);
                return Task::none();
            }
            apply_request(ed, &coord, request, doc);
            Task::none()
        }
        ComposeKind::GapPreview(gap) => {
            if interaction_open(ed) {
                // Unreachable once `settle_before` runs: never install a
                // frame under an open interaction; keep the value instead.
                tracing::warn!(target: "readshot::editor", "gap compose landed under an interaction");
                ed.gap_queued.get_or_insert(gap);
                return Task::none();
            }
            let fresh = ed.model.history_generation() == request.generation
                && ed.model.snapshot() == request.present;
            let same_drag = !ed.snapshot_drag_cancelled
                && ed.snapshot_drag_baseline.is_some()
                && ed.snapshot_drag_baseline == request.drag_baseline;
            match doc {
                Some(doc) if fresh && same_drag => {
                    let ComposedDoc {
                        next,
                        base,
                        flattened,
                        handle,
                    } = doc;
                    if ed.model.preview_precomposed(next, base, flattened).is_ok() {
                        ed.install_image(handle);
                        ed.layout_gap_previewed_at = Some(std::time::Instant::now());
                    }
                }
                None if fresh && same_drag => {
                    // The worker failed: show the value on the UI thread.
                    ed.gap_queued.get_or_insert(gap);
                    super::combine::flush_gap_preview(ed);
                }
                _ => {}
            }
            after_gap_landed(ed, &coord)
        }
    }
}

/// A drag, draft or slider preview a landing frame must not slide under.
fn interaction_open(ed: &EditorSession) -> bool {
    ed.move_drag.is_some()
        || ed.canvas_drag_active
        || ed.crop_draft.is_some()
        || ed.pending_text.is_some()
        || ed.style_drag_baseline.is_some()
        || ed.width_drag_baseline.is_some()
}

/// Whether `message` may be handled while a compose is still pending,
/// as `(resize_or_align, gap)`. Everything else settles the pending work
/// first ([`settle_before`]), so a landing never meets an open drag,
/// draft, output, undo or file picker.
fn may_wait(message: &Message) -> (bool, bool) {
    use readshot_ui::CanvasMessage;
    match message {
        Message::EditorComposed { .. }
        | Message::EditorStatusTick
        | Message::EditorStatusDismissed
        | Message::EditorWindowReady(..)
        | Message::EditorSaved(..)
        | Message::EditorCopyImageDone(..)
        | Message::EditorCopyTextDone(..)
        | Message::EditorCopyLinesDone(..)
        | Message::EditorHistoryWritten { .. }
        | Message::EditorTextRecognized { .. }
        | Message::EditorCanvas(CanvasMessage::EyedropperHover(_))
        | Message::EditorZoomIn
        | Message::EditorZoomOut
        | Message::EditorZoomActual
        | Message::EditorZoomFit
        | Message::EditorPanelTabSelected(..)
        | Message::EditorResizeWidthChanged(..)
        | Message::EditorResizeHeightChanged(..)
        | Message::EditorResizeUnitChanged(..)
        | Message::EditorResizeLockToggled(..)
        | Message::EditorDiscardDisarmed
        | Message::PinWindowReady(..)
        | Message::PinClosePressed(..)
        | Message::PinDragRequested(..)
        | Message::PinOpacityChanged(..)
        | Message::PinLockToggled(..)
        | Message::PinHovered(..)
        | Message::PinKeyPressed { .. } => (true, true),
        // The gap slider's own ticks and release drive its compose.
        Message::EditorLayoutGapPreview(..)
        | Message::EditorLayoutGapFlush { .. }
        | Message::EditorSnapshotSliderCommit => (false, true),
        _ => (false, false),
    }
}

/// Run before an editor message is handled: unless `message` may wait,
/// a pending resize / alignment is applied now (composed here if its
/// worker hasn't returned) and a gap compose is shown now (a released
/// drag committed). The async path stays for the common case — apply,
/// then wait — and nothing ever lands under, or is lost to, a later
/// interaction.
pub(crate) fn settle_before(state: &mut App, message: &Message) {
    let (step_may_wait, gap_may_wait) = may_wait(message);
    let coord = state.coordinator.clone();
    let Some(ed) = state.editor.as_mut() else {
        return;
    };
    if !step_may_wait {
        settle_pending_step(ed, &coord);
    }
    if !gap_may_wait {
        settle_gap(ed, &coord);
    }
}

/// Settle everything off-thread compose holds back: the pending step,
/// then the gap preview (committing a released drag).
pub(crate) fn settle_all(ed: &mut EditorSession, coord: &crate::coordinator::CaptureCoordinator) {
    settle_pending_step(ed, coord);
    settle_gap(ed, coord);
}

/// Show an outstanding gap value now; commit a drag released while it
/// composed.
fn settle_gap(ed: &mut EditorSession, coord: &crate::coordinator::CaptureCoordinator) {
    if gap_outstanding(ed) {
        super::combine::flush_gap_preview(ed);
    }
    if ed.gap_commit_on_land {
        commit_snapshot_drag(ed, coord);
    }
}

/// Apply a pending resize or alignment now, on the UI thread.
pub(crate) fn settle_pending_step(
    ed: &mut EditorSession,
    coord: &crate::coordinator::CaptureCoordinator,
) {
    if let Some(request) = ed
        .compose_job
        .take_if(|j| !matches!(j.kind, ComposeKind::GapPreview(_)))
    {
        apply_request(ed, coord, request, None);
    }
}

/// Apply a resize / alignment request as one undo step; never dropped.
/// `doc` is its composed result, if the worker returned one. An open
/// interaction is settled first, in order (the step came first, so it
/// is never installed under one). When the document moved since the
/// request, the same change is made to the document as it is now.
fn apply_request(
    ed: &mut EditorSession,
    coord: &crate::coordinator::CaptureCoordinator,
    request: ComposeRequest,
    doc: Option<ComposedDoc>,
) {
    if interaction_open(ed) {
        cancel_editor_previews(ed);
        commit_pending_editor_text(ed, coord);
        apply_crop_draft(ed, coord);
    }
    let (action, verb) = match request.kind {
        ComposeKind::Resize { .. } => ("resize", "Resized"),
        ComposeKind::Align(_) => ("align", "Aligned"),
        ComposeKind::GapPreview(_) => return,
    };
    let fresh = ed.model.history_generation() == request.generation
        && ed.model.snapshot() == request.present;
    let applied = if fresh {
        let doc = doc.unwrap_or_else(|| compose(request.next));
        apply_step(ed, doc)
    } else {
        let cropped = ed.last_crop().is_some();
        let result = match request.kind {
            ComposeKind::Resize { sx, sy, .. } => ed.model.scale_by(sx, sy),
            ComposeKind::Align(align) => {
                let gap = ed.model.geometry().gap;
                ed.model.set_layout(gap, align)
            }
            ComposeKind::GapPreview(_) => Ok(false),
        };
        match result {
            Ok(changed) => {
                ed.refresh_image();
                changed
            }
            Err(e) => {
                ed.set_status(geometry_error_status(action, cropped, e));
                false
            }
        }
    };
    if applied {
        let status = match request.kind {
            ComposeKind::Align(align) => {
                format!("{verb} to {}. ⌘Z to undo.", align.label().to_lowercase())
            }
            _ => {
                let (w, h) = ed.effective_image_size();
                format!("{verb} to {w} × {h} px. ⌘Z to undo.")
            }
        };
        land_step(ed, coord, status);
    }
}

/// Push a landed resize / alignment as one undo step.
fn apply_step(ed: &mut EditorSession, doc: ComposedDoc) -> bool {
    let ComposedDoc {
        next,
        base,
        flattened,
        handle,
    } = doc;
    match ed.model.push_precomposed(next, base, flattened) {
        Ok(()) => {
            ed.install_image(handle);
            true
        }
        Err(e) => {
            tracing::warn!(target: "readshot::editor", "landed compose refused: {e}");
            false
        }
    }
}

/// What a synchronous geometry change does after the image refresh.
fn land_step(
    ed: &mut EditorSession,
    coord: &crate::coordinator::CaptureCoordinator,
    status: String,
) {
    ed.clear_eyedropper_loupe();
    ed.clear_discard_confirmation();
    ed.set_status(status);
    sync_editor_history(ed, coord);
}

/// A gap compose landed (applied or not): preview the value that queued
/// meanwhile, or commit a drag released while it ran.
fn after_gap_landed(
    ed: &mut EditorSession,
    coord: &crate::coordinator::CaptureCoordinator,
) -> Task<Message> {
    if ed.snapshot_drag_baseline.is_none() || ed.snapshot_drag_cancelled {
        ed.gap_queued = None;
        ed.gap_commit_on_land = false;
        return Task::none();
    }
    if let Some(gap) = ed.gap_queued.take() {
        return super::combine::preview_gap(ed, gap);
    }
    if std::mem::take(&mut ed.gap_commit_on_land) {
        commit_snapshot_drag(ed, coord);
    }
    Task::none()
}
