//! Off-thread document composes (#80 / #100). Near the 256 MP budget a
//! resize, an alignment or a gap preview composes and flattens ~1 GB
//! each; done on the UI thread that freezes the editor. Documents of at
//! least [`crate::editor::ASYNC_COMPOSE_MIN_PIXELS`] output pixels are
//! composed on a blocking worker instead; smaller ones stay synchronous.
//!
//! The editor keeps showing the previous image until the result lands.
//! Every request is tagged (`seq`, the history generation, the present
//! state and, for a gap preview, the slider drag's baseline); a result
//! applies only while it is the newest request and nothing moved, so a
//! stale one is dropped and a rapid series of changes applies once, as
//! the newest value. A committed change is still one undo step, pushed
//! when its result lands.

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
    // A resize or alignment still composing is superseded: say so, or its
    // "…ing" status would stay up for good.
    if let Some(old) = ed.compose_job.take() {
        match old.kind {
            ComposeKind::Resize { .. } => ed.set_status("Resize cancelled."),
            ComposeKind::Align(_) => ed.set_status("Alignment cancelled."),
            ComposeKind::GapPreview(_) => {}
        }
    }
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
    // Superseded by a newer request (or cancelled): never applied.
    let Some(request) = ed.compose_job.take_if(|j| j.seq == seq) else {
        return Task::none();
    };
    // An output in flight settled every pending step before it started
    // (`settle_pending_step`); anything landing now would change the
    // document under it.
    let fresh = !ed.busy
        && ed.model.history_generation() == request.generation
        && ed.model.snapshot() == request.present;
    let Some(doc) = output.take() else {
        return match request.kind {
            ComposeKind::GapPreview(_) => after_gap_landed(ed, &coord),
            ComposeKind::Resize { .. } => {
                ed.set_status("Couldn't resize — please try again.");
                Task::none()
            }
            ComposeKind::Align(_) => {
                ed.set_status("Couldn't align — please try again.");
                Task::none()
            }
        };
    };
    match request.kind {
        ComposeKind::Resize { width, height } => {
            if !fresh {
                ed.set_status("Resize cancelled — the image changed. Try again.");
                return Task::none();
            }
            if apply_step(ed, doc) {
                land_step(
                    ed,
                    &coord,
                    format!("Resized to {width} × {height} px. ⌘Z to undo."),
                );
            }
            Task::none()
        }
        ComposeKind::Align(align) => {
            if !fresh {
                ed.set_status("Alignment cancelled — the image changed. Try again.");
                return Task::none();
            }
            if apply_step(ed, doc) {
                land_step(
                    ed,
                    &coord,
                    format!("Aligned to {}. ⌘Z to undo.", align.label().to_lowercase()),
                );
            }
            Task::none()
        }
        ComposeKind::GapPreview(_) => {
            let same_drag = !ed.snapshot_drag_cancelled
                && ed.snapshot_drag_baseline.is_some()
                && ed.snapshot_drag_baseline == request.drag_baseline;
            if fresh && same_drag {
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
            after_gap_landed(ed, &coord)
        }
    }
}

/// Before an output (Save, Copy, Pin): a resize or alignment still
/// composing is applied now, on the UI thread, so the output and the
/// screen both show it and a late landing can't change the document
/// after the output marked it clean. A stale one is dropped.
pub(crate) fn settle_pending_step(
    ed: &mut EditorSession,
    coord: &crate::coordinator::CaptureCoordinator,
) {
    let Some(request) = ed
        .compose_job
        .take_if(|j| !matches!(j.kind, ComposeKind::GapPreview(_)))
    else {
        return;
    };
    let fresh = ed.model.history_generation() == request.generation
        && ed.model.snapshot() == request.present;
    if !fresh {
        ed.set_status("Resize cancelled — the image changed. Try again.");
        if let ComposeKind::Align(_) = request.kind {
            ed.set_status("Alignment cancelled — the image changed. Try again.");
        }
        return;
    }
    let status = match request.kind {
        ComposeKind::Resize { width, height } => {
            format!("Resized to {width} × {height} px. ⌘Z to undo.")
        }
        ComposeKind::Align(align) => {
            format!("Aligned to {}. ⌘Z to undo.", align.label().to_lowercase())
        }
        ComposeKind::GapPreview(_) => return,
    };
    if apply_step(ed, compose(request.next)) {
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
