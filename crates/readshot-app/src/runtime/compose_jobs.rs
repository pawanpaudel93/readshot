//! Off-thread document work (#80 / #100, D1). Near the 256 MP budget a
//! geometry change composes the document and flattens the annotations
//! over it, ~1 GB each; done on the UI thread that freezes the editor.
//! On documents of at least [`crate::editor::ASYNC_COMPOSE_MIN_PIXELS`]
//! output pixels ([`EditorSession::is_large`]) that work runs on a
//! blocking worker instead; smaller ones stay synchronous.
//!
//! One request runs at a time ([`EditorSession::compose_job`]), of three
//! kinds:
//!
//! - **History steps** (resize, alignment, rotate, flip, an added part, a
//!   released gap drag, undo, redo). They queue in order and are never
//!   dropped: the editor keeps showing (and editing) the state before them
//!   until the composed result lands, then applies them as their undo
//!   steps in one go. Rotate, flip, undo and redo arriving meanwhile queue
//!   behind them ([`chains_onto_steps`]) instead of settling them; the
//!   compose in flight covers the steps queued when it started, the rest
//!   compose next. A step whose end state's base was composed before (the
//!   state one undo or redo away, a preview's baseline) only flattens.
//! - **Gap previews** (the combine gap slider): coalesced, the newest
//!   value wins; release commits.
//! - **Flattens**: after an annotation edit only the flattened image is
//!   stale (same document, same frame). The model moves at once; the
//!   previous render stays on screen, positioned in the present frame by
//!   the view, until the new one lands ([`after_update`]).
//!
//! Every request is tagged (`seq`, the history generation and present
//! state, a gap preview's slider baseline, a flatten's base and
//! annotations) and its result is dropped when stale.
//!
//! A step or preview never lands under an open interaction: every editor
//! message that could start a drag or draft, set `busy`, output or move
//! history first settles the pending work on the UI thread
//! ([`settle_before`]; only an allow-list of passive messages may wait,
//! and the chained steps above). Settling waits for a compose already
//! running rather than composing the same document twice.

use super::*;

use std::sync::Arc;

use iced::Task;

use crate::app::{App, Message};
use crate::editor::{
    ComposeKind, ComposeRequest, ComposedDoc, ComposedOutput, EditorSession, QueuedStep, ShownImage,
};
use readshot_ui::editor::{DocOp, EditorState, Snapshot};

/// Whether moving to `next` composes off the UI thread.
pub(crate) fn is_large(ed: &EditorSession, next: &Snapshot) -> bool {
    let (w, h) = next.doc.geometry.size();
    u64::from(w) * u64::from(h) >= ed.async_compose_min_pixels
}

#[cfg(test)]
thread_local! {
    static COMPOSES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many documents [`compose_with`] composed on this thread (a cached
/// base composes nothing).
#[cfg(test)]
pub(crate) fn composes_on_this_thread() -> u64 {
    COMPOSES.with(std::cell::Cell::get)
}

/// Compose and flatten `next` (blocking; runs on a worker), reusing
/// `base` when it is `next`'s composed base. The geometry was validated
/// when the request was made.
pub(crate) fn compose_with(next: Snapshot, base: Option<Arc<image::RgbaImage>>) -> ComposedDoc {
    let base = base.unwrap_or_else(|| {
        #[cfg(test)]
        COMPOSES.with(|c| c.set(c.get() + 1));
        next.doc.geometry.compose()
    });
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

/// Free `t` on a helper thread: releasing a document-sized image takes the
/// UI thread ~2 ms per 100 MP (the pages are unmapped).
pub(crate) fn free_off_thread<T: Send + 'static>(t: T) {
    // If no thread can be made, `t` is dropped here instead.
    let _ = std::thread::Builder::new()
        .name("readshot-free".into())
        .spawn(move || drop(t));
}

/// The images the editor shows or keeps now, held across a change so
/// those it lets go are freed off the UI thread ([`free_off_thread`]).
pub(crate) struct Held {
    _model: Vec<Arc<image::RgbaImage>>,
    _handle: iced::widget::image::Handle,
    _shown: ShownImage,
    _kept: Option<crate::editor::KeptDisplay>,
}

pub(crate) fn hold(ed: &EditorSession) -> Held {
    Held {
        _model: ed.model.held_images(),
        _handle: ed.image_handle.clone(),
        _shown: ed.shown.clone(),
        _kept: ed.kept_display.clone(),
    }
}

/// Register a request (replacing a gap preview or flatten still running:
/// its result will be dropped). Its worker starts with [`launch`].
fn register(
    ed: &mut EditorSession,
    kind: ComposeKind,
    next: Snapshot,
    drag_baseline: Option<Snapshot>,
    steps: Vec<QueuedStep>,
    base: Option<Arc<image::RgbaImage>>,
) {
    // Every message that can start a compose settles a pending step
    // first (`settle_before`), or queues behind it: only a gap preview or
    // a flatten is replaced.
    debug_assert!(
        ed.compose_job.as_ref().is_none_or(|j| !j.kind.is_step()),
        "a pending step would be lost"
    );
    ed.compose_seq += 1;
    let composed = steps.len();
    ed.compose_job = Some(ComposeRequest {
        seq: ed.compose_seq,
        kind,
        generation: ed.model.history_generation(),
        present: ed.model.snapshot(),
        drag_baseline,
        next,
        steps,
        composed,
        base,
        output: ComposedOutput::queued(),
        launched: false,
    });
}

/// Start the worker of the registered request, if it hasn't started.
pub(crate) fn launch(ed: &mut EditorSession) -> Task<Message> {
    let session = ed.session_id;
    let Some(job) = ed.compose_job.as_mut().filter(|j| !j.launched) else {
        return Task::none();
    };
    job.launched = true;
    let seq = job.seq;
    let next = job.next.clone();
    let base = job.base.clone();
    let output = job.output.clone();
    let worker = output.clone();
    let work = async move {
        if let Err(e) = tokio::task::spawn_blocking(move || {
            worker.run(|| compose_with(next, base));
        })
        .await
        {
            tracing::warn!(target: "readshot::editor", "compose worker: {e}");
        }
    };
    Task::perform(work, move |()| Message::EditorComposed {
        session,
        seq,
        output: output.clone(),
    })
}

/// Start composing `next` for `kind` (a gap preview) off the UI thread.
/// It replaces any gap preview or flatten still running.
pub(crate) fn spawn(
    ed: &mut EditorSession,
    kind: ComposeKind,
    next: Snapshot,
    drag_baseline: Option<Snapshot>,
) -> Task<Message> {
    register(ed, kind, next, drag_baseline, Vec::new(), None);
    launch(ed)
}

/// Whether history steps are queued (composing, or waiting to).
pub(crate) fn steps_pending(ed: &EditorSession) -> bool {
    ed.compose_job.as_ref().is_some_and(|j| j.kind.is_step())
}

/// The state the editor will be in once the queued steps land: where a
/// step chained behind them starts from.
pub(crate) fn planned_present(ed: &EditorSession) -> Option<Snapshot> {
    match ed.compose_job.as_ref().filter(|j| j.kind.is_step()) {
        Some(job) => ed.model.plan_ops(&ops_of(&job.steps)),
        None => Some(ed.model.snapshot()),
    }
}

fn ops_of(steps: &[QueuedStep]) -> Vec<DocOp> {
    steps.iter().map(|s| s.op.clone()).collect()
}

/// Queue `step` off the UI thread: behind the steps already queued (it is
/// composed when the one in flight lands), or as a new request.
pub(crate) fn queue_step(ed: &mut EditorSession, step: QueuedStep) -> Task<Message> {
    if let Some(job) = ed.compose_job.as_mut().filter(|j| j.kind.is_step()) {
        job.kind = step.kind;
        job.steps.push(step);
        return Task::none();
    }
    start_steps(ed, vec![step])
}

/// Register and launch a request composing the end state of `steps`.
fn start_steps(ed: &mut EditorSession, steps: Vec<QueuedStep>) -> Task<Message> {
    let Some(kind) = steps.last().map(|s| s.kind) else {
        return Task::none();
    };
    let Some(next) = ed.model.plan_ops(&ops_of(&steps)) else {
        // Unreachable: every step was planned when it was queued. Redo
        // them here rather than lose them.
        debug_assert!(false, "queued steps no longer apply");
        replay_steps(ed, None, steps);
        return Task::none();
    };
    let base = ed.model.cached_base(&next.doc.geometry);
    register(ed, kind, next, None, steps, base);
    launch(ed)
}

/// A geometry step (rotate, flip): queued off the UI thread when the
/// document is large or steps are already queued (it must follow them);
/// `None` when it runs synchronously (small, or a no-op or refused
/// change the caller reports).
pub(crate) fn queue_geometry_step(
    ed: &mut EditorSession,
    kind: ComposeKind,
    action: &str,
    next_geometry: impl FnOnce(
        &readshot_core::document::DocGeometry,
    ) -> readshot_core::document::DocGeometry,
) -> Option<Task<Message>> {
    let pending = steps_pending(ed);
    let from = planned_present(ed)?;
    match EditorState::step_from(&from, next_geometry(&from.doc.geometry)) {
        Ok(Some(next)) if pending || ed.is_large() || is_large(ed, &next) => {
            ed.set_status(in_progress_status(kind));
            Some(queue_step(
                ed,
                QueuedStep {
                    kind,
                    op: DocOp::Push(next),
                    part: None,
                    bake: None,
                },
            ))
        }
        Ok(None) if pending => Some(Task::none()),
        Err(e) if pending => {
            let cropped = from
                .annotations
                .iter()
                .any(|a| matches!(a, readshot_core::Annotation::Crop { .. }));
            ed.set_status(geometry_error_status(action, cropped, e));
            Some(Task::none())
        }
        _ => None,
    }
}

/// Undo or redo (`kind`) off the UI thread when the document is large or
/// steps are queued; `None` when it runs synchronously.
pub(crate) fn queue_history_move(
    ed: &mut EditorSession,
    kind: ComposeKind,
) -> Option<Task<Message>> {
    let op = match kind {
        ComposeKind::Undo => DocOp::Undo,
        ComposeKind::Redo => DocOp::Redo,
        _ => return None,
    };
    let pending = steps_pending(ed);
    let mut ops = ed
        .compose_job
        .as_ref()
        .filter(|j| j.kind.is_step())
        .map(|j| ops_of(&j.steps))
        .unwrap_or_default();
    ops.push(op.clone());
    let Some(target) = ed.model.plan_ops(&ops) else {
        // Nothing to undo (or redo) once the queued steps land; with no
        // queue, the synchronous path decides (it also closes previews).
        return pending.then(Task::none);
    };
    if !(pending || ed.is_large() || is_large(ed, &target)) {
        return None;
    }
    ed.set_status(in_progress_status(kind));
    Some(queue_step(
        ed,
        QueuedStep {
            kind,
            op,
            part: None,
            bake: None,
        },
    ))
}

/// Add `image` as a part joined `join` off the UI thread when the result
/// is large or steps are queued; `None` when it runs synchronously.
pub(crate) fn queue_add_part(
    ed: &mut EditorSession,
    image: &Arc<image::RgbaImage>,
    join: readshot_core::document::Join,
    what: &'static str,
) -> Option<Task<Message>> {
    let pending = steps_pending(ed);
    let (w, h) = ed.model.geometry().size();
    let added = u64::from(w) * u64::from(h) + u64::from(image.width()) * u64::from(image.height());
    if !(pending || ed.is_large() || added >= ed.async_compose_min_pixels) {
        return None;
    }
    let from = planned_present(ed)?;
    let kind = ComposeKind::AddPart { join, what };
    match ed.model.add_part_step(&from, Arc::clone(image), join) {
        Ok((next, _)) => {
            let bake = (!from.doc.geometry.transform.is_identity())
                .then(|| next.doc.geometry.parts.first().map(|p| p.id))
                .flatten()
                .map(|id| (id, Arc::clone(&from.doc.geometry)));
            ed.set_status(in_progress_status(kind));
            Some(queue_step(
                ed,
                QueuedStep {
                    kind,
                    op: DocOp::Push(next),
                    part: Some(Arc::clone(image)),
                    bake,
                },
            ))
        }
        // The synchronous path reports a refused part (same validation).
        Err(_) if !pending => None,
        Err(e) => {
            let cropped = from
                .annotations
                .iter()
                .any(|a| matches!(a, readshot_core::Annotation::Crop { .. }));
            ed.set_status(geometry_error_status(
                &format!("add the {what}"),
                cropped,
                e,
            ));
            Some(Task::none())
        }
    }
}

/// A resize or alignment as a queued step (`next` from
/// `EditorState::geometry_step` on the present).
pub(crate) fn queue_push(
    ed: &mut EditorSession,
    kind: ComposeKind,
    next: Snapshot,
) -> Task<Message> {
    queue_step(
        ed,
        QueuedStep {
            kind,
            op: DocOp::Push(next),
            part: None,
            bake: None,
        },
    )
}

fn in_progress_status(kind: ComposeKind) -> &'static str {
    match kind {
        ComposeKind::Resize { .. } => "Resizing…",
        ComposeKind::Align(_) => "Aligning…",
        ComposeKind::Rotate { .. } => "Rotating…",
        ComposeKind::Flip(_) => "Flipping…",
        ComposeKind::AddPart { .. } => "Adding…",
        ComposeKind::Undo => "Undoing…",
        ComposeKind::Redo => "Redoing…",
        ComposeKind::Gap(_) | ComposeKind::GapPreview(_) | ComposeKind::Flatten => "Updating…",
    }
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
        free_off_thread(output.take());
        return Task::none();
    };
    let doc = output.take();
    let held = hold(ed);
    let task = match request.kind {
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
                stale => free_off_thread(stale),
            }
            after_gap_landed(ed, &coord)
        }
        ComposeKind::Flatten => {
            land_flatten(ed, request, doc);
            Task::none()
        }
        _ => {
            // Every message that sets `busy` settles a pending step first
            // (`settle_before`), so this can't happen; if it did, keep the
            // steps pending rather than change the document under an
            // output. They are settled with the next interaction.
            if ed.busy {
                debug_assert!(false, "a compose landed while busy");
                ed.compose_job = Some(request);
                free_off_thread(doc);
                return Task::none();
            }
            land_steps(ed, Some(&coord), request, doc)
        }
    };
    free_off_thread(held);
    task
}

/// A flatten landed: shown when it still is what the display wants
/// (the same annotations over the same base); a stale one is dropped and
/// [`after_update`] starts the newest.
fn land_flatten(ed: &mut EditorSession, request: ComposeRequest, doc: Option<ComposedDoc>) {
    let base = ed.model.shared_base();
    let Some(doc) = doc else {
        // The worker failed: render on the UI thread.
        if ed.display_dirty {
            render_display_now(ed);
        }
        return;
    };
    let wanted = ed.wanted_display();
    if !Arc::ptr_eq(&doc.base, &base) || doc.next.annotations != wanted {
        free_off_thread(doc);
        return;
    }
    debug_assert!(request
        .base
        .as_ref()
        .is_some_and(|b| Arc::ptr_eq(b, &doc.base)));
    let ComposedDoc {
        next,
        flattened,
        handle,
        ..
    } = doc;
    if next.annotations == ed.model.annotations() {
        ed.model
            .install_flattened(&base, &next.annotations, flattened);
    }
    ed.set_display(handle, ShownImage::of(base, next.annotations));
}

/// Before an output: a render of the present already running is waited
/// for (and shown) instead of rendering the same image again here.
pub(crate) fn settle_flatten(ed: &mut EditorSession) {
    let base = ed.model.shared_base();
    let present = ed.model.annotations().to_vec();
    let Some(request) = ed.compose_job.take_if(|j| {
        j.kind == ComposeKind::Flatten
            && j.next.annotations == present
            && j.base.as_ref().is_some_and(|b| Arc::ptr_eq(b, &base))
    }) else {
        return;
    };
    let doc = request.output.claim();
    land_flatten(ed, request, doc);
}

/// Render the display on the UI thread (a small document, a failed
/// worker, or a document change that didn't land composed).
fn render_display_now(ed: &mut EditorSession) {
    let base = ed.model.shared_base();
    let wanted = ed.wanted_display();
    let flat = if wanted == ed.model.annotations() {
        ed.model.flatten()
    } else {
        readshot_core::render(&base, &wanted)
    };
    let handle =
        iced::widget::image::Handle::from_rgba(flat.width(), flat.height(), flat.into_raw());
    ed.set_display(handle, ShownImage::of(base, wanted));
}

/// Run after every message: start a registered worker, re-render a stale
/// display of a large document off the UI thread (one flatten in flight;
/// a newer edit waits for it, then the newest is rendered), and request
/// recognition of a part added by a landed step.
pub(crate) fn after_update(state: &mut App) -> Task<Message> {
    let mut tasks = Vec::new();
    let panel_open = state.preferences.editor_panel_open;
    let recognize = state.editor.as_mut().is_some_and(|ed| {
        std::mem::take(&mut ed.recognition_owed)
            && super::combine::part_recognition_wanted(ed, panel_open)
    });
    if recognize {
        tasks.push(super::text_panel::request_text_recognition(state));
    }
    let Some(ed) = state.editor.as_mut() else {
        return Task::batch(tasks);
    };
    tasks.push(launch(ed));
    if ed.display_dirty && ed.compose_job.is_none() {
        let base = ed.model.shared_base();
        let wanted = ed.wanted_display();
        if ed.shown.shows(&base, &wanted) {
            ed.display_dirty = false;
        } else if !ed.is_large() || !Arc::ptr_eq(&ed.shown.base, &base) {
            render_display_now(ed);
        } else {
            let next = Snapshot {
                annotations: wanted,
                doc: ed.model.snapshot().doc,
            };
            register(ed, ComposeKind::Flatten, next, None, Vec::new(), Some(base));
            tasks.push(launch(ed));
        }
    }
    Task::batch(tasks)
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
/// as `(steps, gap)`. Everything else settles the pending work first
/// ([`settle_before`]), so a landing never meets an open drag, draft,
/// output or file picker.
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

/// Rotate, flip, undo and redo (by button or key) queue behind pending
/// history steps instead of settling them: they only need the state the
/// steps lead to, never the pixels or the frame on screen.
fn chains_onto_steps(message: &Message) -> bool {
    let chains = |m: &Message| {
        matches!(
            m,
            Message::EditorRotate { .. }
                | Message::EditorFlip(..)
                | Message::EditorToolbar(
                    readshot_ui::ToolbarMessage::Undo | readshot_ui::ToolbarMessage::Redo
                )
        )
    };
    match message {
        Message::EditorKeyPressed {
            key,
            modifiers,
            status_ignored,
            ..
        } => super::view::editor_key_message(key.clone(), *modifiers, *status_ignored)
            .is_some_and(|m| chains(&m)),
        m => chains(m),
    }
}

/// Run before an editor message is handled: unless `message` may wait,
/// pending steps are applied now (waiting for their compose if it is
/// running, composing here if it hasn't started) and a gap compose is
/// shown now (a released drag is committed, as a queued step when its
/// compose is still running). The async path stays for the common case —
/// apply, then wait — and nothing ever lands under, or is lost to, a
/// later interaction.
///
/// Returns whether settling changed the document frame (a step or gap
/// value landed): a canvas position in `message` was then computed
/// against the frame shown before ([`points_into_the_shown_image`]).
pub(crate) fn settle_before(state: &mut App, message: &Message) -> bool {
    let (mut steps_may_wait, gap_may_wait) = may_wait(message);
    let coord = state.coordinator.clone();
    let Some(ed) = state.editor.as_mut() else {
        return false;
    };
    let before = Arc::clone(ed.model.geometry());
    if !gap_may_wait {
        queue_released_gap(ed);
    }
    let chained = chains_onto_steps(message);
    if chained && steps_pending(ed) {
        steps_may_wait = true;
    }
    if !steps_may_wait {
        settle_pending_step(ed, &coord);
    }
    // A held gap drag is cancelled by a chained message's handler (back
    // to its baseline): showing its newest value first would be wasted.
    if !gap_may_wait && !chained {
        settle_gap(ed, &coord);
    }
    !Arc::ptr_eq(&before, ed.model.geometry())
}

/// A canvas press (or a click tool's commit) whose position was mapped
/// against the image on screen when it happened. If settling just
/// changed the frame ([`settle_before`]), that point means something
/// else now — it could select or move another annotation, a redaction
/// included — so the message is dropped ([`drop_stale_press`]) and the
/// user presses again on the new image.
pub(crate) fn points_into_the_shown_image(message: &Message) -> bool {
    use readshot_ui::CanvasMessage;
    matches!(
        message,
        Message::EditorCanvas(
            CanvasMessage::SelectPressed(_)
                | CanvasMessage::DragStarted
                | CanvasMessage::CommitAnnotation(_)
                | CanvasMessage::RequestText(_)
                | CanvasMessage::EyedropperPick(_)
        )
    )
}

/// Drop a press the frame moved under ([`points_into_the_shown_image`]).
/// The canvas keeps its own drag state, so a drag it started is cancelled
/// through the cancel sequence (its next event, or its release, drops it)
/// and marked dropped, so a commit of it that still arrives is refused
/// ([`EditorSession::drag_started_in`]). A Select press opens no move
/// drag, so its drags and release do nothing.
pub(crate) fn drop_stale_press(ed: &mut EditorSession, message: &Message) {
    if let Message::EditorCanvas(readshot_ui::CanvasMessage::DragStarted) = message {
        ed.canvas_drag_active = false;
        ed.drag_cancel_seq = ed.drag_cancel_seq.wrapping_add(1);
        ed.drag_started_in = Some(crate::editor::CanvasDragFrame::Dropped);
    }
    ed.move_drag = None;
    ed.canvas_cache.clear();
}

/// Settle everything off-thread compose holds back: the pending steps,
/// then the gap preview (committing a released drag).
pub(crate) fn settle_all(ed: &mut EditorSession, coord: &crate::coordinator::CaptureCoordinator) {
    queue_released_gap(ed);
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

/// A gap drag was released while its compose ran: rather than compose
/// the released value on the UI thread, the preview is cancelled (its
/// baseline's display and base were kept: no render) and the value is
/// queued as one history step over the baseline. The compose already
/// running is reused when it is for that value.
pub(crate) fn queue_released_gap(ed: &mut EditorSession) {
    if !ed.gap_commit_on_land || steps_pending(ed) {
        return;
    }
    let Some(baseline) = ed
        .snapshot_drag_baseline
        .clone()
        .filter(|_| !ed.snapshot_drag_cancelled)
    else {
        return;
    };
    let running = ed
        .compose_job
        .take_if(|j| matches!(j.kind, ComposeKind::GapPreview(_)));
    let running_gap = running.as_ref().and_then(|j| match j.kind {
        ComposeKind::GapPreview(gap) => Some(gap),
        _ => None,
    });
    let Some(gap) = ed
        .layout_gap_pending
        .take()
        .or(ed.gap_queued.take())
        .or(running_gap)
    else {
        ed.compose_job = ed.compose_job.take().or(running);
        return;
    };
    let align = baseline.doc.geometry.align;
    let step = EditorState::step_from(&baseline, baseline.doc.geometry.with_layout(gap, align));
    ed.gap_commit_on_land = false;
    ed.snapshot_drag_baseline = None;
    let next = match step {
        Ok(Some(next)) => next,
        Ok(None) => {
            // Back at the baseline's value: nothing to keep.
            ed.model.cancel_preview_snapshot(baseline);
            ed.refresh_image();
            return;
        }
        Err(e) => {
            // The newest value is refused (it was never shown): keep the
            // value on screen, as a commit after a refused tick does.
            let cropped = ed.last_crop().is_some();
            ed.set_status(geometry_error_status("change the gap", cropped, e));
            if ed.model.commit_preview_snapshot(baseline) {
                ed.clear_discard_confirmation();
                ed.refresh_image();
                ed.history_sync_owed = true;
            }
            return;
        }
    };
    ed.model.cancel_preview_snapshot(baseline);
    ed.refresh_image();
    let step = QueuedStep {
        kind: ComposeKind::Gap(gap),
        op: DocOp::Push(next.clone()),
        part: None,
        bake: None,
    };
    match running {
        Some(mut job) if running_gap == Some(gap) && job.next == next => {
            // Its compose is this step's: keep it running, as the step.
            job.kind = step.kind;
            job.generation = ed.model.history_generation();
            job.present = ed.model.snapshot();
            job.drag_baseline = None;
            job.steps = vec![step];
            job.composed = 1;
            ed.compose_job = Some(job);
        }
        _ => {
            // Launched by `after_update` (a settle right after composes
            // it here instead).
            let base = ed.model.cached_base(&next.doc.geometry);
            register(ed, step.kind, next, None, vec![step], base);
        }
    }
}

/// Apply the pending steps now, on the UI thread: with the result of a
/// compose already running (waited for), else composed here.
pub(crate) fn settle_pending_step(
    ed: &mut EditorSession,
    coord: &crate::coordinator::CaptureCoordinator,
) {
    settle_steps(ed, Some(coord));
}

/// [`settle_pending_step`]; without `coord` the History sync is owed
/// ([`EditorSession::history_sync_owed`]).
fn settle_steps(ed: &mut EditorSession, coord: Option<&crate::coordinator::CaptureCoordinator>) {
    while let Some(request) = ed.compose_job.take_if(|j| j.kind.is_step()) {
        let doc = request.output.claim();
        let _ = land_steps(ed, coord, request, doc);
        // Steps queued behind the compose were registered as a new
        // request: settled on the next turn (its worker never started).
    }
}

/// A gap drag released while its compose ran, settled where no History
/// sync can run (`cancel_editor_previews`): kept as its step, with the
/// running compose's result (waited for) rather than a second compose.
pub(crate) fn settle_released_gap(ed: &mut EditorSession) {
    if !ed.gap_commit_on_land {
        return;
    }
    queue_released_gap(ed);
    settle_steps(ed, None);
    if std::mem::take(&mut ed.gap_commit_on_land) {
        // Not queueable (unreachable: steps never wait beside a gap
        // drag): show the newest value and keep it, here.
        super::combine::flush_gap_preview(ed);
        if let Some(baseline) = ed.snapshot_drag_baseline.take() {
            if ed.model.commit_preview_snapshot(baseline) {
                ed.clear_discard_confirmation();
                ed.refresh_image();
                ed.history_sync_owed = true;
            }
        }
    }
}

/// Apply landed steps, each one undo step (or one undo / redo); never
/// dropped. `doc` is the composed end state of the first
/// `request.composed` steps, if the worker returned one. An open
/// interaction is settled first, in order (the steps came first, so they
/// are never installed under one). When the document moved since the
/// request, the same changes are made to the document as it is now.
/// Steps queued behind the compose start composing (the returned task).
fn land_steps(
    ed: &mut EditorSession,
    coord: Option<&crate::coordinator::CaptureCoordinator>,
    mut request: ComposeRequest,
    doc: Option<ComposedDoc>,
) -> Task<Message> {
    if let Some(coord) = coord.filter(|_| interaction_open(ed)) {
        cancel_editor_previews(ed);
        commit_pending_editor_text(ed, coord);
        apply_crop_draft(ed, coord);
    }
    let rest = request
        .steps
        .split_off(request.composed.min(request.steps.len()));
    let landed = request.steps;
    let fresh = ed.model.history_generation() == request.generation
        && ed.model.snapshot() == request.present;
    if fresh {
        let doc = match doc {
            Some(doc) if doc.next == request.next => doc,
            other => {
                free_off_thread(other);
                compose_with(request.next, request.base)
            }
        };
        let ComposedDoc {
            base,
            flattened,
            handle,
            ..
        } = doc;
        match ed
            .model
            .apply_ops_precomposed(&ops_of(&landed), base, flattened)
        {
            Ok(true) => {
                ed.install_image(handle);
                after_steps_landed(ed, coord, &landed);
            }
            Ok(false) => replay_steps(ed, coord, landed),
            Err(e) => {
                tracing::warn!(target: "readshot::editor", "landed compose refused: {e}");
                replay_steps(ed, coord, landed);
            }
        }
    } else {
        replay_steps(ed, coord, landed);
    }
    if rest.is_empty() {
        Task::none()
    } else {
        start_steps(ed, rest)
    }
}

/// Redo `steps` on the document as it is now, synchronously (the history
/// moved since they were queued, or their landing was refused).
fn replay_steps(
    ed: &mut EditorSession,
    coord: Option<&crate::coordinator::CaptureCoordinator>,
    steps: Vec<QueuedStep>,
) {
    let mut applied: Vec<QueuedStep> = Vec::new();
    for step in steps {
        let cropped = ed.last_crop().is_some();
        let old = Arc::clone(ed.model.geometry());
        let result = match step.kind {
            ComposeKind::Resize { sx, sy, .. } => ed.model.scale_by(sx, sy),
            ComposeKind::Align(align) => {
                let gap = ed.model.geometry().gap;
                ed.model.set_layout(gap, align)
            }
            ComposeKind::Gap(gap) => {
                let align = ed.model.geometry().align;
                ed.model.set_layout(gap, align)
            }
            ComposeKind::Rotate { clockwise } => ed.model.rotate(clockwise),
            ComposeKind::Flip(axis) => ed.model.flip(axis),
            ComposeKind::AddPart { join, .. } => match step.part.clone() {
                Some(image) => ed
                    .model
                    .add_part(Arc::unwrap_or_clone(image), join)
                    .map(|_| true),
                None => Ok(false),
            },
            ComposeKind::Undo => Ok(ed.model.undo()),
            ComposeKind::Redo => Ok(ed.model.redo()),
            ComposeKind::GapPreview(_) | ComposeKind::Flatten => Ok(false),
        };
        match result {
            Ok(true) => {
                let mut step = step;
                // A bake made now is of the document as it is now.
                step.bake = (matches!(step.kind, ComposeKind::AddPart { .. })
                    && !old.transform.is_identity())
                .then(|| ed.model.geometry().parts.first().map(|p| p.id))
                .flatten()
                .map(|id| (id, old));
                applied.push(step);
            }
            Ok(false) => {}
            Err(e) => {
                let action = match step.kind {
                    ComposeKind::Resize { .. } => "resize".to_string(),
                    ComposeKind::Align(_) => "align".to_string(),
                    ComposeKind::Gap(_) => "change the gap".to_string(),
                    ComposeKind::Rotate { .. } => "rotate".to_string(),
                    ComposeKind::Flip(_) => "flip".to_string(),
                    ComposeKind::AddPart { what, .. } => format!("add the {what}"),
                    _ => "change the image".to_string(),
                };
                ed.set_status(geometry_error_status(&action, cropped, e));
            }
        }
    }
    if applied.is_empty() {
        // Nothing to report: drop a "…ing" status the queue set.
        if ed.status_is_in_progress() {
            ed.status = None;
            ed.status_set_at = None;
        }
        return;
    }
    ed.refresh_image();
    after_steps_landed(ed, coord, &applied);
}

/// What a synchronous step does after the image refresh: the Text
/// panel's bake sources for an added part, the status, the History sync.
fn after_steps_landed(
    ed: &mut EditorSession,
    coord: Option<&crate::coordinator::CaptureCoordinator>,
    landed: &[QueuedStep],
) {
    note_parts_added(ed, landed);
    let Some(last) = landed.last() else {
        return;
    };
    ed.clear_eyedropper_loupe();
    ed.clear_discard_confirmation();
    let status = landed_status(ed, last.kind);
    ed.set_status(status);
    match coord {
        Some(coord) => sync_editor_history(ed, coord),
        None => ed.history_sync_owed = true,
    }
}

/// An added part's bookkeeping: a bake's source geometry for the Text
/// panel, and recognition of the new part ([`after_update`]).
fn note_parts_added(ed: &mut EditorSession, landed: &[QueuedStep]) {
    let mut added = false;
    for step in landed {
        if !matches!(step.kind, ComposeKind::AddPart { .. }) {
            continue;
        }
        added = true;
        if let Some((id, old)) = step.bake.clone() {
            ed.text_panel.bake_sources.insert(id, old);
        }
    }
    if added {
        let _ = super::text_panel::resolve_baked_docs(&mut ed.text_panel);
        ed.recognition_owed = true;
    }
}

fn landed_status(ed: &EditorSession, kind: ComposeKind) -> String {
    use readshot_core::document::{FlipAxis, Join};
    match kind {
        ComposeKind::Resize { .. } => {
            let (w, h) = ed.effective_image_size();
            format!("Resized to {w} × {h} px. ⌘Z to undo.")
        }
        ComposeKind::Align(align) => {
            format!("Aligned to {}. ⌘Z to undo.", align.label().to_lowercase())
        }
        ComposeKind::Gap(gap) => format!("Gap set to {gap} px. ⌘Z to undo."),
        ComposeKind::Rotate { clockwise } => {
            let dir = if clockwise { "right" } else { "left" };
            format!("Rotated {dir}. ⌘Z to undo.")
        }
        ComposeKind::Flip(axis) => {
            let what = match axis {
                FlipAxis::Horizontal => "horizontally",
                FlipAxis::Vertical => "vertically",
            };
            format!("Flipped {what}. ⌘Z to undo.")
        }
        ComposeKind::AddPart { join, what } => {
            let side = match join {
                Join::Below => "below",
                Join::Beside => "beside",
            };
            format!("Added {what} {side}. ⌘Z to undo.")
        }
        ComposeKind::Undo => "Undid edit. ⇧⌘Z to redo.".to_string(),
        ComposeKind::Redo => "Redid edit. ⌘Z to undo.".to_string(),
        ComposeKind::GapPreview(_) | ComposeKind::Flatten => String::new(),
    }
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
