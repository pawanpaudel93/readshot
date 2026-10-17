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
//! state, a gap preview's slider baseline, a flatten's base) and a stale
//! step or preview result is dropped. A stale flatten is still shown
//! when it is over the document on screen (it is newer than what is
//! there), the newest rendering next; only a render of the present enters
//! the model, which outputs and History use, so they never use a display.
//! Edits not in the image on screen yet are drawn over it by the canvas
//! ([`EditorSession::pending_overlay`]), redactions opaque.
//!
//! A step never lands under an open interaction: every editor message
//! that could start a drag or draft, set `busy`, output or move history
//! first settles pending steps on the UI thread ([`settle_before`]; an
//! allow-list of passive messages, key presses themselves (the message a
//! key maps to settles on its own) and the chained steps above may
//! wait). Settling waits for a compose already running rather than
//! composing the same document twice. A held gap drag is not settled: a
//! value of it landing under an interaction stays queued, and its commit
//! shows the newest value first. Superseded workers are abandoned (never
//! started, or free their own result), and big images let go of are
//! freed on one long-lived dropper thread.

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
    let drawn = next.doc.geometry.cover_redactions(&next.annotations);
    let flattened = readshot_core::render(&base, &drawn);
    ComposedDoc {
        next,
        base,
        flattened,
    }
}

/// Below this many bytes an image is freed where it is let go.
const FREE_OFF_THREAD_MIN_BYTES: usize = 4 << 20;

type Garbage = Box<dyn Send>;

/// The one long-lived thread that frees big images (fed by a channel).
fn dropper() -> Option<&'static std::sync::mpsc::Sender<Garbage>> {
    static DROPPER: std::sync::OnceLock<Option<std::sync::mpsc::Sender<Garbage>>> =
        std::sync::OnceLock::new();
    DROPPER
        .get_or_init(|| {
            let (tx, rx) = std::sync::mpsc::channel::<Garbage>();
            std::thread::Builder::new()
                .name("readshot-free".into())
                .spawn(move || {
                    for garbage in rx {
                        drop(garbage);
                    }
                })
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}

#[cfg(test)]
thread_local! {
    static SENT_TO_DROPPER: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times this thread sent something to the dropper thread.
#[cfg(test)]
pub(crate) fn sent_to_dropper() -> u64 {
    SENT_TO_DROPPER.with(std::cell::Cell::get)
}

/// Free `t` on the dropper thread: releasing a document-sized image takes
/// the UI thread ~2 ms per 100 MP (its pages are unmapped). Without the
/// thread, `t` is dropped here.
pub(crate) fn free_off_thread<T: Send + 'static>(t: T) {
    #[cfg(test)]
    SENT_TO_DROPPER.with(|c| c.set(c.get() + 1));
    match dropper() {
        Some(tx) => {
            let _ = tx.send(Box::new(t));
        }
        None => drop(t),
    }
}

/// A finished compose nobody will use, freed off the UI thread.
fn discard(doc: Option<ComposedDoc>) {
    if doc.is_some() {
        free_off_thread(doc);
    }
}

/// The images the editor shows or keeps now, held across a change so
/// those it lets go are freed off the UI thread ([`Held::release`]).
pub(crate) struct Held {
    bases: Vec<Arc<image::RgbaImage>>,
    flats: Vec<readshot_ui::editor::Flattened>,
    handles: Vec<iced::widget::image::Handle>,
}

/// Every composed base `ed` holds (model, display and kept display).
fn held_bases(ed: &EditorSession) -> Vec<Arc<image::RgbaImage>> {
    let (mut bases, _) = ed.model.held_images();
    bases.push(Arc::clone(&ed.shown.base));
    if let Some(kept) = &ed.kept_display {
        bases.push(Arc::clone(&kept.shown.base));
    }
    bases
}

pub(crate) fn hold(ed: &EditorSession) -> Held {
    let (_, flats) = ed.model.held_images();
    let bases = held_bases(ed);
    let mut handles = vec![ed.image_handle.clone()];
    if let Some(kept) = &ed.kept_display {
        handles.push(kept.handle.clone());
    }
    Held {
        bases,
        flats,
        handles,
    }
}

fn handle_bytes(h: &iced::widget::image::Handle) -> usize {
    match h {
        iced::widget::image::Handle::Rgba { pixels, .. } => pixels.len(),
        _ => 0,
    }
}

/// Whether `held` has a big base that `live` (what the editor still
/// holds) does not: dropping `held` may then free it, so it goes to the
/// dropper thread. Never decided by reference counts (see [`Held::release`]).
fn lets_go_of_big_base(held: &[Arc<image::RgbaImage>], live: &[Arc<image::RgbaImage>]) -> bool {
    held.iter().any(|b| {
        b.as_raw().len() >= FREE_OFF_THREAD_MIN_BYTES && !live.iter().any(|l| Arc::ptr_eq(l, b))
    })
}

impl Held {
    /// Let go of what was held: when that frees a big image (nothing in
    /// `ed` holds it any more), on the dropper thread; else here.
    ///
    /// A base counts as let go when `ed` no longer holds it, whatever else
    /// may: a worker holding a copy can drop it at any moment, so counting
    /// references would race and could free the image here.
    pub(crate) fn release(self, ed: Option<&EditorSession>) {
        let live_bases: Vec<Arc<image::RgbaImage>> = ed.map(held_bases).unwrap_or_default();
        let live: Vec<_> = ed
            .map(|ed| {
                let (_, flats) = ed.model.held_images();
                std::iter::once(ed.image_handle.id())
                    .chain(ed.kept_display.iter().map(|k| k.handle.id()))
                    .chain(flats.iter().map(|f| f.handle().id()))
                    .collect()
            })
            .unwrap_or_default();
        let big = |bytes: usize| bytes >= FREE_OFF_THREAD_MIN_BYTES;
        let frees_base = lets_go_of_big_base(&self.bases, &live_bases);
        let frees_pixels = self
            .flats
            .iter()
            .map(|f| f.handle())
            .chain(self.handles.iter().cloned())
            .any(|h| !live.contains(&h.id()) && big(handle_bytes(&h)));
        if frees_base || frees_pixels {
            free_off_thread(self);
        }
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
    register_settling_steps(ed, kind, next, drag_baseline, steps, base);
}

/// [`register`] without its debug check: should pending steps reach it
/// anyway (a message that bypassed `settle_before`), they are settled
/// first, never abandoned, and the new request keeps the tags of the
/// state it was planned from, so it lands as stale.
fn register_settling_steps(
    ed: &mut EditorSession,
    kind: ComposeKind,
    next: Snapshot,
    drag_baseline: Option<Snapshot>,
    steps: Vec<QueuedStep>,
    base: Option<Arc<image::RgbaImage>>,
) {
    let generation = ed.model.history_generation();
    let present = ed.model.snapshot();
    settle_displaced_steps(ed);
    abandon(ed.compose_job.take());
    ed.compose_seq += 1;
    let composed = steps.len();
    ed.compose_job = Some(ComposeRequest {
        seq: ed.compose_seq,
        kind,
        generation,
        present,
        drag_baseline,
        next,
        steps,
        composed,
        base,
        output: ComposedOutput::queued(),
        launched: false,
    });
}

/// Pending steps about to be displaced by another request: applied now
/// (the running compose's result waited for), never lost. `true` when
/// there were some.
fn settle_displaced_steps(ed: &mut EditorSession) -> bool {
    if !steps_pending(ed) {
        return false;
    }
    settle_steps(ed, None);
    true
}

/// A replaced or dropped request: its worker never runs, or frees its own
/// result ([`ComposedOutput::abandon`]).
pub(crate) fn abandon(job: Option<ComposeRequest>) {
    if let Some(job) = job {
        discard(job.output.abandon());
    }
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

/// Whether undo and redo are possible once the queued steps land (what
/// the toolbar shows; ⌘Z and ⇧⌘Z plan against the same state).
pub(crate) fn can_undo_redo(ed: &EditorSession) -> (bool, bool) {
    match ed.compose_job.as_ref().filter(|j| j.kind.is_step()) {
        Some(job) => ed
            .model
            .planned_undo_redo(&ops_of(&job.steps))
            .unwrap_or((ed.model.can_undo(), ed.model.can_redo())),
        None => (ed.model.can_undo(), ed.model.can_redo()),
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
            abandon(ed.compose_job.take());
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
        discard(output.take());
        return Task::none();
    };
    let doc = output.take();
    let held = hold(ed);
    let task = match request.kind {
        ComposeKind::GapPreview(gap) => {
            if interaction_open(ed) {
                // A held drag isn't settled before other messages: never
                // install a frame under an open interaction; keep the
                // value for the drag's commit (or its cancel) instead.
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
                    } = doc;
                    if ed.model.preview_precomposed(next, base, flattened).is_ok() {
                        let handle = ed.model.flattened_handle();
                        ed.install_image(handle);
                        ed.layout_gap_previewed_at = Some(std::time::Instant::now());
                    }
                }
                None if fresh && same_drag => {
                    // The worker failed: show the value on the UI thread.
                    ed.gap_queued.get_or_insert(gap);
                    super::combine::flush_gap_preview(ed);
                }
                stale => discard(stale),
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
                keep_steps_while_busy(ed, request, doc);
                return Task::none();
            }
            land_steps(ed, Some(&coord), request, doc)
        }
    };
    held.release(state.editor.as_ref());
    task
}

/// Steps whose compose landed while an output runs: kept pending, with
/// their result, so the next settle applies them without composing
/// again; never dropped.
fn keep_steps_while_busy(
    ed: &mut EditorSession,
    mut request: ComposeRequest,
    doc: Option<ComposedDoc>,
) {
    request.output = ComposedOutput::new(doc);
    ed.compose_job = Some(request);
}

/// A flatten landed. It is shown whenever it is over the document on
/// screen, even if the edits moved on meanwhile: it is newer than what is
/// shown (one flatten runs at a time), so continuous input (a slider,
/// held arrow keys) keeps the screen moving. If it isn't what the display
/// wants, the display stays dirty and [`after_update`] renders the newest.
/// Only a render of the present enters the model's cache, which outputs
/// use; a display never does.
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
    let same_document = Arc::ptr_eq(&doc.base, &base) && Arc::ptr_eq(&ed.shown.base, &base);
    if !same_document || ed.shown.shows(&base, &wanted) {
        // Another document, or the display is current already (a kept
        // display put back): nothing to show.
        discard(Some(doc));
        return;
    }
    debug_assert!(request
        .base
        .as_ref()
        .is_some_and(|b| Arc::ptr_eq(b, &doc.base)));
    let ComposedDoc {
        next, flattened, ..
    } = doc;
    // `base` is the model's (checked above): a render of the present is
    // installed, its pixels shared with the display.
    let handle = if next.annotations == ed.model.annotations() {
        ed.model
            .install_flattened(&base, &next.annotations, flattened);
        ed.model.flattened_handle()
    } else {
        iced::widget::image::Handle::from_rgba(
            flattened.width(),
            flattened.height(),
            flattened.into_raw(),
        )
    };
    let current = next.annotations == wanted;
    ed.set_display(handle, ShownImage::of(base, next.annotations));
    ed.display_dirty = !current;
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
    let handle = if wanted == ed.model.annotations() {
        ed.model.flattened_handle()
    } else {
        let flat = readshot_core::render(&base, &ed.model.geometry().cover_redactions(&wanted));
        iced::widget::image::Handle::from_rgba(flat.width(), flat.height(), flat.into_raw())
    };
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
        // A key press settles nothing itself: the message it maps to is
        // dispatched (and settles) on its own; one that maps to nothing,
        // like a bare modifier, only updates held-key state.
        Message::EditorKeyPressed { .. }
        | Message::EditorComposed { .. }
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
    matches!(
        message,
        Message::EditorRotate { .. }
            | Message::EditorFlip(..)
            | Message::EditorToolbar(
                readshot_ui::ToolbarMessage::Undo | readshot_ui::ToolbarMessage::Redo
            )
    )
}

/// Run before an editor message is handled: unless `message` may wait,
/// pending steps are applied now (waiting for their compose if it is
/// running, composing here if it hasn't started) and a released gap drag
/// is committed (as a queued step when its compose is still running). A
/// held gap drag is left to the handler. The async path stays for the common case —
/// apply, then wait — and nothing ever lands under, or is lost to, a
/// later interaction.
///
/// Settling may change the document frame (a step or gap value lands):
/// the dispatcher then maps a canvas position in `message`, computed
/// against the frame the last view showed, into the present one
/// ([`points_into_the_shown_image`]).
pub(crate) fn settle_before(state: &mut App, message: &Message) {
    let (mut steps_may_wait, gap_may_wait) = may_wait(message);
    let coord = state.coordinator.clone();
    let Some(ed) = state.editor.as_mut() else {
        return;
    };
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
    if !gap_may_wait {
        settle_gap(ed, &coord);
    }
}

/// A canvas press (or a click tool's commit) whose position was mapped
/// against the image on screen when it happened. If settling just
/// changed the frame ([`settle_before`]), or a change handled earlier
/// in the same batch did (the frame the last view showed is not the
/// present one: `EditorSession::shown_frame`), that point means something
/// else now — it could select or move another annotation, a redaction
/// included — so it is mapped into the new frame when that is exact
/// ([`remap_stale_press`]), else the message is dropped
/// ([`drop_stale_press`]) and the user presses again on the new image.
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

/// A press the frame moved under ([`points_into_the_shown_image`]), from
/// the frame `old` that was on screen: mapped into the present frame when
/// the change is an affine of `old` at its point
/// ([`readshot_core::document::map_point`]), else dropped (`None`).
///
/// A Select press, a text click, an eyedropper pick and a click tool's
/// commit (pin, stamp) carry their point and are mapped. A drawing drag
/// is always dropped: the canvas keeps its anchor in screen space and
/// maps it with whatever frame is on screen at release, so its points
/// could not all be mapped the same way (its commit is refused through
/// `CanvasDragFrame::Dropped`). A redaction is thus only ever committed
/// in the frame its points belong to.
pub(crate) fn remap_stale_press(
    ed: &mut EditorSession,
    old: &Arc<readshot_core::document::DocGeometry>,
    message: Message,
) -> Option<Message> {
    use readshot_core::document::{frame_map_at, map_annotation, map_point};
    use readshot_ui::CanvasMessage;
    let new = Arc::clone(ed.model.geometry());
    let remapped = match &message {
        Message::EditorCanvas(CanvasMessage::SelectPressed(p)) => {
            frame_map_at(old, &new, *p).map(|m| {
                let len = m.length_scale();
                let shown_scale = ed.render_scale.get();
                ed.press_remap = Some(crate::editor::PressRemap {
                    old: Arc::clone(old),
                    new: Arc::clone(&new),
                    map: m,
                    scale: if len.is_finite() && len > 0.0 {
                        shown_scale / len
                    } else {
                        shown_scale
                    },
                    view_seq: ed.canvas_view_seq.get(),
                });
                Message::EditorCanvas(CanvasMessage::SelectPressed(m.apply(*p)))
            })
        }
        Message::EditorCanvas(CanvasMessage::RequestText(p)) => {
            map_point(old, &new, *p).map(|q| Message::EditorCanvas(CanvasMessage::RequestText(q)))
        }
        Message::EditorCanvas(CanvasMessage::EyedropperPick(p)) => map_point(old, &new, *p)
            .map(|q| Message::EditorCanvas(CanvasMessage::EyedropperPick(q))),
        Message::EditorCanvas(CanvasMessage::CommitAnnotation(
            a @ (readshot_core::Annotation::NumberedPin { .. }
            | readshot_core::Annotation::Stamp { .. }),
        )) => frame_map_at(old, &new, readshot_core::document::annotation_anchor(a))
            .map(|m| Message::EditorCanvas(CanvasMessage::CommitAnnotation(map_annotation(a, &m)))),
        _ => None,
    };
    if remapped.is_none() {
        drop_stale_press(ed, &message);
    }
    remapped
}

/// A Select drag point the canvas sent before a view was built after a
/// remapped press ([`EditorSession::press_remap`]) is in the old frame:
/// mapped with the press's own map, so the move's delta is in one frame
/// (even over another part, or a gutter). Once a view is built, points
/// are in the present frame and pass as they are.
pub(crate) fn remap_same_batch_drag(ed: &mut EditorSession, message: Message) -> Option<Message> {
    use readshot_ui::CanvasMessage;
    let Some(r) = ed.press_remap.as_ref() else {
        return Some(message);
    };
    if r.view_seq != ed.canvas_view_seq.get() || !Arc::ptr_eq(&r.new, ed.model.geometry()) {
        ed.press_remap = None;
        return Some(message);
    }
    match message {
        Message::EditorCanvas(CanvasMessage::SelectDragged(p)) => Some(Message::EditorCanvas(
            CanvasMessage::SelectDragged(r.map.apply(p)),
        )),
        other => Some(other),
    }
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

/// Commit a gap drag released while it composed (when it couldn't be
/// queued as a step). A drag still held is left alone: whatever the
/// message does next either cancels it (its value is never composed) or
/// keeps it through `commit_snapshot_drag`, which shows the newest value
/// first; a value landing meanwhile never lands under an interaction
/// (`handle_composed` keeps it queued instead).
fn settle_gap(ed: &mut EditorSession, coord: &crate::coordinator::CaptureCoordinator) {
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
                discard(other);
                compose_with(request.next, request.base)
            }
        };
        let ComposedDoc {
            base, flattened, ..
        } = doc;
        match ed
            .model
            .apply_ops_precomposed(&ops_of(&landed), base, flattened)
        {
            Ok(true) => {
                let handle = ed.model.flattened_handle();
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
            format!("Resized to {w} × {h} px. {UNDO_HINT}")
        }
        ComposeKind::Align(align) => {
            format!("Aligned to {}. {UNDO_HINT}", align.label().to_lowercase())
        }
        ComposeKind::Gap(gap) => format!("Gap set to {gap} px. {UNDO_HINT}"),
        ComposeKind::Rotate { clockwise } => {
            let dir = if clockwise { "right" } else { "left" };
            format!("Rotated {dir}. {UNDO_HINT}")
        }
        ComposeKind::Flip(axis) => {
            let what = match axis {
                FlipAxis::Horizontal => "horizontally",
                FlipAxis::Vertical => "vertically",
            };
            format!("Flipped {what}. {UNDO_HINT}")
        }
        ComposeKind::AddPart { join, what } => {
            let side = match join {
                Join::Below => "below",
                Join::Beside => "beside",
            };
            format!("Added {what} {side}. {UNDO_HINT}")
        }
        ComposeKind::Undo => format!("Undid edit. {REDO_HINT}"),
        ComposeKind::Redo => format!("Redid edit. {UNDO_HINT}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_base_let_go_of_goes_to_the_dropper_even_while_a_worker_holds_it() {
        let big = Arc::new(image::RgbaImage::new(1_100, 1_000));
        // A worker's copy: counting references would see it and decide the
        // base is not freed here, then the worker drops its copy and ours
        // frees the image on the UI thread.
        let worker = Arc::clone(&big);
        assert!(lets_go_of_big_base(std::slice::from_ref(&big), &[]));
        // Still shown by the editor: dropping our copy frees nothing.
        assert!(!lets_go_of_big_base(
            std::slice::from_ref(&big),
            std::slice::from_ref(&worker)
        ));
        let small = Arc::new(image::RgbaImage::new(40, 20));
        assert!(!lets_go_of_big_base(&[small], &[]));
    }

    /// An async editor with one annotation and an undo of it queued (its
    /// worker not started).
    fn editor_with_a_queued_undo() -> EditorSession {
        let mut ed = EditorSession::new(image::RgbaImage::new(40, 20));
        ed.async_compose_min_pixels = 0;
        ed.model
            .commit_annotation(readshot_core::Annotation::Pixelate {
                rect: readshot_core::RectLike::new(2.0, 2.0, 10.0, 8.0),
                block_size: 4.0,
            });
        ed.refresh_image();
        let _ = queue_history_move(&mut ed, ComposeKind::Undo).expect("queued");
        assert!(steps_pending(&ed));
        assert_eq!(ed.model.annotations().len(), 1, "not applied yet");
        ed
    }

    #[test]
    fn a_request_registered_over_pending_steps_settles_them_never_drops_them() {
        let mut ed = editor_with_a_queued_undo();
        let generation = ed.model.history_generation();
        let planned_from = ed.model.snapshot();
        // As if a message bypassed `settle_before` and asked for a flatten.
        register_settling_steps(
            &mut ed,
            ComposeKind::Flatten,
            planned_from.clone(),
            None,
            Vec::new(),
            None,
        );
        assert!(ed.model.annotations().is_empty(), "the undo was applied");
        assert!(ed.history_sync_owed);
        let job = ed.compose_job.as_ref().expect("the flatten is registered");
        assert_eq!(job.kind, ComposeKind::Flatten);
        // Tagged with the state it was planned from: it lands as stale.
        assert_eq!(job.generation, generation);
        assert_eq!(job.present, planned_from);
        assert_ne!(ed.model.history_generation(), generation);
    }

    #[test]
    fn steps_landing_while_busy_stay_pending_with_their_result() {
        let mut ed = EditorSession::new(image::RgbaImage::new(40, 20));
        ed.async_compose_min_pixels = 0;
        ed.refresh_image();
        // A rotate: its turned base was never composed (no cached base).
        let _ = queue_geometry_step(
            &mut ed,
            ComposeKind::Rotate { clockwise: true },
            "rotate",
            |g| EditorState::rotated_geometry(g, true),
        )
        .expect("queued");
        let request = ed.compose_job.take().expect("queued");
        assert!(request.base.is_none());
        let doc = compose_with(request.next.clone(), None);
        ed.busy = true;
        keep_steps_while_busy(&mut ed, request, Some(doc));
        assert!(steps_pending(&ed), "never dropped");
        assert_eq!(ed.image_size(), (40, 20), "not applied under busy");
        ed.busy = false;
        let composes = composes_on_this_thread();
        settle_steps(&mut ed, None);
        assert_eq!(ed.image_size(), (20, 40), "applied at the settle");
        assert_eq!(
            composes_on_this_thread(),
            composes,
            "with the kept result: no second compose"
        );
    }
}
