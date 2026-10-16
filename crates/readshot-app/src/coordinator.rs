//! Capture coordinator — service-layer orchestration that the iced
//! App's `update` fn drives.
//!
//! The coordinator is *not* the iced state machine itself (that's
//! `crate::app::App`); it's the bag of async methods the state
//! machine calls when transitioning between user-visible states.
//! Splitting service from view keeps every backend interaction
//! testable with fakes.
//!
//! Capture flow (per spec §3.5 and §5.1):
//!
//! 1. Permissions check — `pre_capture_gate()`.
//! 2. List displays — `list_displays()`.
//! 3. (User confirms a region in the overlay; the App receives a
//!    `Message::OverlaySelected` carrying the display id, logical rect,
//!    and scale.)
//! 4. `capture_region(req)` — produces an `RgbaImage`.
//! 5. (User edits in the editor; the App tracks `EditorState`.)
//! 6. On editor outcome:
//!    * `CopyImage` → write image to clipboard (App calls
//!      `Exporter`/`ClipboardWriter`; coordinator helps assemble
//!      bytes).
//!    * `CopyText` → `recognise(image)` → write text.
//!    * `Save` → write file.
//!    * `Pin` → spawn pin window.
//!    * `Discard` → no-op.
//! 7. If preferences allow history, call
//!    `record_history(record, png_bytes)`.

use std::sync::Arc;

use image::RgbaImage;
use readshot_capture::{CaptureRequest, Capturer, DisplayInfo};
use readshot_core::error::{CaptureError, OCRError};
use readshot_core::{CaptureRecord, FsHistoryStore, HistoryRetention, HistoryStore};
use readshot_ocr::{OCREngine, OCRRequest};

use crate::permissions::{PermissionStatus, PermissionsProvider};

/// Bag of services the coordinator binds against. Owned by the App
/// state and shared via `Arc` so the capture coordinator can spawn
/// background tasks that outlive a single iced `update` call.
///
/// `Clone` is `Arc`-cheap (every field is already an `Arc`) — it's
/// derived so iced `Task`s can capture an owned coordinator without
/// borrowing across `await` points.
#[derive(Clone)]
pub struct CaptureCoordinator {
    capturer: Arc<dyn Capturer>,
    ocr: Arc<dyn OCREngine>,
    permissions: Arc<dyn PermissionsProvider>,
    history: Option<Arc<dyn HistoryStore>>,
    /// Ordered background writer for sidecar updates. The editor syncs
    /// history on every commit/undo/redo; `HistoryStore::update` fsyncs,
    /// so doing it synchronously inside iced's `update` janks the UI
    /// thread. A single worker thread consuming an mpsc channel keeps
    /// writes off the UI thread *and* in dispatch order (concurrent
    /// fire-and-forget tasks could land out of order and persist stale
    /// annotations). The thread exits when the last coordinator clone
    /// drops its sender.
    history_update_tx: Option<std::sync::mpsc::Sender<HistoryUpdate>>,
    /// See [`Scrubs`].
    scrubs: Arc<Scrubs>,
}

/// Message for the background history writer thread.
enum HistoryUpdate {
    Write(CaptureRecord),
    /// Replace a record's image and sidecar (the editor's History scrub
    /// and its restore). See [`HistoryRewrite`].
    Rewrite(Box<HistoryRewrite>),
    /// Rendezvous: acknowledged once everything queued before it has
    /// been written. Lets shutdown (and tests) wait for durability.
    Flush(std::sync::mpsc::SyncSender<()>),
}

/// Outcome of a queued [`CaptureCoordinator::rewrite_history_async`]:
/// `Err` carries a user-presentable reason. A rewrite superseded by a
/// newer one for the same record never runs; its receiver is cancelled.
pub type HistoryRewriteResult = Result<(), String>;

/// One full record replacement: the PNG is `bake` rendered onto `base`
/// (or `base` itself when `bake` is empty).
#[derive(Clone)]
struct RewriteJob {
    /// Request order per record; only the newest may land.
    seq: u64,
    record: CaptureRecord,
    base: Arc<RgbaImage>,
    bake: Vec<readshot_core::Annotation>,
}

/// A queued rewrite. Rendering and encoding run on the writer thread,
/// never the UI thread; a failed write is retried with backoff.
struct HistoryRewrite {
    job: RewriteJob,
    done: Option<iced::futures::channel::oneshot::Sender<HistoryRewriteResult>>,
    /// Failed attempts so far (drives the retry backoff).
    attempt: u32,
}

/// Coordinator-owned History scrub state, shared with the writer
/// thread, so a scrub never depends on the editor staying open.
///
/// Two locks, always taken in this order, neither ever held across an
/// image render, encode or big write (those are staged first):
/// - `book` is the commit gate. Held for "is this rewrite still the
///   newest?" plus the short commit (renames + sidecar), so a stale
///   restore can't land after a newer scrub's phase one, and for the
///   parked-save hand-off.
/// - `state` is request bookkeeping, held only for map updates. The UI
///   thread takes just this one on an edit whose redactions are already
///   secured.
#[derive(Default)]
struct Scrubs {
    book: std::sync::Mutex<ScrubBook>,
    state: std::sync::Mutex<ScrubState>,
}

#[derive(Default)]
struct ScrubBook {
    /// Scrubs of records not on disk yet (the capture's save hasn't
    /// landed). `record_history` saves these in place of the raw
    /// capture, so unredacted content never reaches disk.
    parked: std::collections::HashMap<readshot_core::Uuid, RewriteJob>,
    /// Scrubs whose phase one hasn't succeeded yet; retried by every
    /// rewrite attempt and by [`CaptureCoordinator::flush_history_updates`].
    unsecured: std::collections::HashMap<readshot_core::Uuid, (u64, CaptureRecord)>,
}

#[derive(Default)]
struct ScrubState {
    /// Newest rewrite request per record.
    latest: std::collections::HashMap<readshot_core::Uuid, u64>,
    /// Redactions whose phase one (masked sidecar, unredacted image
    /// removed) is on disk, per record, with the document frame they are
    /// in: the same redactions in another frame cover other pixels.
    secured: std::collections::HashMap<readshot_core::Uuid, SecuredScrub>,
}

/// A secured scrub's document frame (`CaptureRecord::geometry_id`) and
/// redactions.
type SecuredScrub = (Option<String>, Vec<readshot_core::Annotation>);

fn secured_key(record: &CaptureRecord) -> SecuredScrub {
    (record.geometry_id.clone(), record.redactions.clone())
}

fn next_rewrite_seq() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Backoff before retry `attempt` (1-based) of a failed rewrite.
fn retry_delay(attempt: u32) -> std::time::Duration {
    let base = if cfg!(test) {
        std::time::Duration::from_millis(10)
    } else {
        std::time::Duration::from_secs(1)
    };
    let max = std::time::Duration::from_secs(60);
    base.saturating_mul(1u32 << attempt.saturating_sub(1).min(16))
        .min(max)
}

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Scrubs {
    /// A newer request than `seq` exists for record `id`.
    fn is_stale(&self, id: readshot_core::Uuid, seq: u64) -> bool {
        lock(&self.state)
            .latest
            .get(&id)
            .is_some_and(|&latest| latest > seq)
    }

    /// Record that phase one for `record`'s redactions (in its document
    /// frame) is on disk — only while
    /// request `seq` is still the newest for `id`. `secured` is a cache
    /// that lets an edit skip phase one, so it may only ever be missing
    /// (phase one runs again: fail closed), never wrong: a job finishing
    /// after a newer request (a restore) must not put back the entry the
    /// newer request cleared.
    fn mark_secured(&self, id: readshot_core::Uuid, seq: u64, record: &CaptureRecord) {
        let mut st = lock(&self.state);
        if st.latest.get(&id) == Some(&seq) {
            st.secured.insert(id, secured_key(record));
        }
    }
}

impl HistoryUpdate {
    fn record_id(&self) -> Option<readshot_core::Uuid> {
        match self {
            Self::Write(r) => Some(r.id),
            Self::Rewrite(rw) => Some(rw.job.record.id),
            Self::Flush(_) => None,
        }
    }
}

/// Drop the queued updates a later one makes pointless, keeping order.
/// Per record: a rewrite supersedes every earlier write and rewrite (it
/// carries the whole state, image included), and a plain write
/// supersedes earlier plain writes queued after the last rewrite. So a
/// burst of edits costs one render + encode, not one per edit. Flushes
/// always stay.
fn coalesce(batch: Vec<HistoryUpdate>) -> Vec<HistoryUpdate> {
    use std::collections::HashSet;
    let mut seen_rewrite: HashSet<readshot_core::Uuid> = HashSet::new();
    let mut seen_write: HashSet<readshot_core::Uuid> = HashSet::new();
    let mut kept: Vec<HistoryUpdate> = Vec::with_capacity(batch.len());
    for msg in batch.into_iter().rev() {
        let keep = match (&msg, msg.record_id()) {
            (HistoryUpdate::Rewrite(_), Some(id)) => seen_rewrite.insert(id),
            (HistoryUpdate::Write(_), Some(id)) => {
                !seen_rewrite.contains(&id) && seen_write.insert(id)
            }
            _ => true,
        };
        if keep {
            kept.push(msg);
        }
    }
    kept.reverse();
    kept
}

/// The record, PNG and thumbnail bytes a job writes. Pure CPU (render,
/// encode; the thumbnail comes from the in-memory image, not a decode).
/// What a job writes: the record (dimensions updated), PNG and thumbnail.
type Rendered = (CaptureRecord, Vec<u8>, Option<Vec<u8>>);

fn render_job(job: &RewriteJob) -> Result<Rendered, String> {
    let baked = (!job.bake.is_empty()).then(|| readshot_core::render(&job.base, &job.bake));
    let img = baked.as_ref().unwrap_or(&job.base);
    let mut record = job.record.clone();
    record.width_px = img.width();
    record.height_px = img.height();
    let png = readshot_core::encode_png_fast(img).map_err(|e| e.to_string())?;
    let thumb = readshot_core::history::thumbnail_png(img);
    Ok((record, png, thumb))
}

enum Outcome {
    Done,
    /// The record isn't on disk; a scrub was parked for its save.
    Missing,
    /// A newer request for the record exists; nothing was written.
    Stale,
    Failed(String),
}

/// Write one rendered job: stage the image (slow; no lock held), then,
/// under the commit gate, check it is still the newest request and
/// commit (renames + sidecar). Keeps the scrub bookkeeping in step.
fn apply_rewrite(
    store: &dyn HistoryStore,
    scrubs: &Scrubs,
    job: &RewriteJob,
    record: &CaptureRecord,
    png: &[u8],
    thumb: Option<&[u8]>,
) -> Outcome {
    let id = record.id;
    let staged = match store.stage_image(record, png, thumb) {
        Ok(staged) => staged,
        Err(e) => {
            fail_closed(store, scrubs, job);
            return Outcome::Failed(e.to_string());
        }
    };
    let mut b = lock(&scrubs.book);
    if scrubs.is_stale(id, job.seq) {
        return Outcome::Stale; // `staged` drops: its files are removed
    }
    match store.commit_rewrite(record, staged) {
        Ok(true) => {
            if record.redacted {
                scrubs.mark_secured(id, job.seq, record);
            }
            b.unsecured.remove(&id);
            Outcome::Done
        }
        Ok(false) => {
            // A frame rewrite also replaces the raw capture when the
            // save lands (History stores the transformed document).
            if record.redacted || record.geometry_id.is_some() {
                b.parked.insert(id, job.clone());
            }
            Outcome::Missing
        }
        Err(e) => {
            if record.redacted {
                // On-disk state unknown: phase one is redone next time.
                lock(&scrubs.state).secured.remove(&id);
                b.unsecured.insert(id, (job.seq, record.clone()));
            }
            Outcome::Failed(e.to_string())
        }
    }
}

/// A scrub's image couldn't be produced or staged: still make sure the
/// unredacted content goes (phase one) — but only if the scrub is still
/// the newest request, so a stale one can't undo a later restore.
fn fail_closed(store: &dyn HistoryStore, scrubs: &Scrubs, job: &RewriteJob) {
    if !job.record.redacted {
        return;
    }
    let id = job.record.id;
    let mut b = lock(&scrubs.book);
    if scrubs.is_stale(id, job.seq) {
        return;
    }
    match store.begin_scrub(&job.record) {
        Ok(true) => {
            b.unsecured.remove(&id);
        }
        Ok(false) => {
            b.parked.insert(id, job.clone());
        }
        Err(_) => {
            lock(&scrubs.state).secured.remove(&id);
            b.unsecured.insert(id, (job.seq, job.record.clone()));
        }
    }
}

/// Run one rewrite: render, encode, stage, commit, report. Returns the
/// retry to schedule when it failed. Called on the writer thread (or
/// the fallback path).
fn run_rewrite(
    store: &dyn HistoryStore,
    scrubs: &Scrubs,
    mut rewrite: Box<HistoryRewrite>,
) -> Option<(std::time::Instant, Box<HistoryRewrite>)> {
    if scrubs.is_stale(rewrite.job.record.id, rewrite.job.seq) {
        return None;
    }
    let outcome = match render_job(&rewrite.job) {
        Ok((record, png, thumb)) => {
            apply_rewrite(store, scrubs, &rewrite.job, &record, &png, thumb.as_deref())
        }
        Err(e) => {
            fail_closed(store, scrubs, &rewrite.job);
            Outcome::Failed(e)
        }
    };
    match outcome {
        Outcome::Done => {
            if let Some(done) = rewrite.done.take() {
                let _ = done.send(Ok(()));
            }
            None
        }
        Outcome::Missing | Outcome::Stale => None,
        Outcome::Failed(e) => {
            tracing::warn!(target: "readshot::history", "history rewrite failed: {e}");
            if let Some(done) = rewrite.done.take() {
                let _ = done.send(Err(e));
            }
            rewrite.attempt += 1;
            let due = std::time::Instant::now() + retry_delay(rewrite.attempt);
            Some((due, rewrite))
        }
    }
}

fn run_update(
    store: &dyn HistoryStore,
    scrubs: &Scrubs,
    msg: HistoryUpdate,
) -> Option<(std::time::Instant, Box<HistoryRewrite>)> {
    match msg {
        HistoryUpdate::Write(record) => {
            if let Err(e) = store.update(&record) {
                tracing::warn!(
                    target: "readshot::history",
                    "background history update failed: {e}"
                );
            }
            None
        }
        HistoryUpdate::Rewrite(rewrite) => run_rewrite(store, scrubs, rewrite),
        HistoryUpdate::Flush(ack) => {
            let _ = ack.send(());
            None
        }
    }
}

type Retry = (std::time::Instant, Box<HistoryRewrite>);

/// A waiting retry is replaced only by a newer **rewrite** of its record
/// (which carries the whole state). A plain sidecar write doesn't carry
/// the image, so it must not cancel a pending scrub's retry.
fn drop_superseded_retries(retries: &mut Vec<Retry>, batch: &[HistoryUpdate]) {
    let fresh: std::collections::HashSet<_> = batch
        .iter()
        .filter_map(|m| match m {
            HistoryUpdate::Rewrite(rw) => Some(rw.job.record.id),
            _ => None,
        })
        .collect();
    retries.retain(|(_, rw)| !fresh.contains(&rw.job.record.id));
}

/// The writer thread: drain what queued up while the last write ran,
/// coalesce it (only the newest rewrite of a record pays for a render +
/// PNG encode), run it, and retry failed rewrites with backoff until
/// they land or a newer rewrite of the record replaces them.
fn writer_loop(
    store: &dyn HistoryStore,
    scrubs: &Scrubs,
    rx: &std::sync::mpsc::Receiver<HistoryUpdate>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    store.sweep_staging();
    let mut retries: Vec<Retry> = Vec::new();
    loop {
        let next_due = retries.iter().map(|(due, _)| *due).min();
        let first = match next_due {
            None => match rx.recv() {
                Ok(msg) => Some(msg),
                Err(_) => return,
            },
            Some(due) => {
                match rx.recv_timeout(due.saturating_duration_since(std::time::Instant::now())) {
                    Ok(msg) => Some(msg),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        };
        let now = std::time::Instant::now();
        let (due, waiting): (Vec<_>, Vec<_>) = retries.into_iter().partition(|(t, _)| *t <= now);
        retries = waiting;
        // Due retries are older than anything just received, so a newer
        // rewrite of the same record supersedes them in `coalesce`.
        let mut batch: Vec<HistoryUpdate> = due
            .into_iter()
            .map(|(_, rw)| HistoryUpdate::Rewrite(rw))
            .collect();
        if let Some(msg) = first {
            batch.push(msg);
            batch.extend(rx.try_iter());
        }
        drop_superseded_retries(&mut retries, &batch);
        for msg in coalesce(batch) {
            if let Some(retry) = run_update(store, scrubs, msg) {
                retries.push(retry);
            }
        }
    }
}

impl CaptureCoordinator {
    pub fn new(
        capturer: Arc<dyn Capturer>,
        ocr: Arc<dyn OCREngine>,
        permissions: Arc<dyn PermissionsProvider>,
        history: Option<Arc<dyn HistoryStore>>,
    ) -> Self {
        let scrubs = Arc::new(Scrubs::default());
        let history_update_tx = history.as_ref().and_then(|store| {
            let store = Arc::clone(store);
            let scrubs_for_writer = Arc::clone(&scrubs);
            let (tx, rx) = std::sync::mpsc::channel::<HistoryUpdate>();
            std::thread::Builder::new()
                .name("history-sync".into())
                .spawn(move || writer_loop(store.as_ref(), &scrubs_for_writer, &rx))
                .ok()
                .map(|_| tx)
        });
        Self {
            capturer,
            ocr,
            permissions,
            history,
            history_update_tx,
            scrubs,
        }
    }

    /// Permission gate. Returns the current status without prompting;
    /// the App's `update` fn calls `permissions.request()` directly
    /// when it wants to show the prompt.
    pub fn pre_capture_gate(&self) -> PermissionStatus {
        self.permissions.status()
    }

    pub async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        self.capturer.list_displays().await
    }

    pub async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
        self.capturer.capture_region(req).await
    }

    /// OCR for the editor's Text panel: lines in reading order, with
    /// base-pixel boxes when the engine reports positions (Apple Vision
    /// today), plus the layout-aware text Copy Text has always produced.
    pub async fn recognise_document(
        &self,
        req: OCRRequest,
    ) -> Result<readshot_core::text_lines::TextDocument, OCRError> {
        use readshot_core::text_lines::TextDocument;
        let (w, h) = (req.image.width(), req.image.height());
        let result = self.ocr.recognise(req).await?;
        Ok(if result.lines.is_empty() {
            TextDocument::from_plain(&result.text)
        } else {
            TextDocument::from_recognized(&result.lines, w, h)
        })
    }

    /// Whether the engine reports line positions (Apple Vision). Without
    /// them, OCR of a History capture could only reproduce its stored
    /// `ocr_text`.
    pub fn engine_reports_positions(&self) -> bool {
        self.ocr.reports_positions()
    }

    pub async fn recognise(&self, req: OCRRequest) -> Result<String, OCRError> {
        // Layout-aware reconstruction when the engine exposes per-line
        // bounding boxes, the engine's flat text otherwise, then the
        // cleanup pass. This is the single chokepoint — every consumer
        // (overlay / history sidecar / editor Copy Text) gets the same
        // string.
        Ok(self.recognise_document(req).await?.layout_text)
    }

    pub fn supported_languages(&self) -> Vec<String> {
        self.ocr.supported_languages()
    }

    /// List every persisted capture, newest first. `Ok(vec![])` when
    /// no history store is wired.
    pub fn history_list(&self) -> Result<Vec<CaptureRecord>, readshot_core::HistoryError> {
        match &self.history {
            Some(h) => h.list(),
            None => Ok(Vec::new()),
        }
    }

    /// Rewrite an existing record's sidecar — typically after a
    /// background OCR or post-capture annotation pass. No-op when no
    /// history store is wired.
    pub fn update_history(
        &self,
        record: &CaptureRecord,
    ) -> Result<(), readshot_core::HistoryError> {
        match &self.history {
            Some(h) => h.update(record),
            None => Ok(()),
        }
    }

    /// Queue a sidecar update on the ordered background writer.
    /// Returns immediately; the write lands in dispatch order and
    /// failures are logged by the worker. Falls back to a synchronous
    /// write if the worker is unavailable. No-op when no history store
    /// is wired.
    pub fn update_history_async(&self, record: CaptureRecord) {
        let record = match &self.history_update_tx {
            Some(tx) => match tx.send(HistoryUpdate::Write(record)) {
                Ok(()) => return,
                Err(std::sync::mpsc::SendError(msg)) => match msg {
                    HistoryUpdate::Write(record) => record,
                    HistoryUpdate::Rewrite(_) | HistoryUpdate::Flush(_) => return,
                },
            },
            None => record,
        };
        if let Err(e) = self.update_history(&record) {
            tracing::warn!(target: "readshot::history", "history update failed: {e}");
        }
    }

    /// Replace `record` in History: its PNG becomes `bake` rendered onto
    /// `base` (`base` as is when `bake` is empty), its thumbnail is
    /// regenerated and its sidecar written as given (`ocr_text: None`
    /// clears the text, except a scrub keeps text already masked for the
    /// same redactions). The receiver resolves with the outcome; it is
    /// cancelled when a newer request supersedes this one, when the
    /// record isn't saved yet, or with no history store.
    ///
    /// A scrub (`record.redacted`) is fail-closed and owned here, not by
    /// the editor:
    /// - **Phase one** runs right now, unless these same redactions are
    ///   already secured: masked sidecar, unredacted PNG and thumbnail
    ///   removed. It's a small sidecar write, no image encode, so from
    ///   this call on History and MCP serve nothing unredacted.
    /// - **Phase two** (render, encode, write the redacted image) runs on
    ///   the writer thread and is retried with backoff until it lands.
    /// - A record not saved yet is parked; [`Self::record_history`] saves
    ///   the scrubbed version in place of the raw capture. So is a frame
    ///   rewrite (`record.geometry_id` set) of a record not saved yet.
    ///
    /// A restore (`redacted: false`) cancels any parked or pending scrub.
    pub fn rewrite_history_async(
        &self,
        mut record: CaptureRecord,
        base: Arc<RgbaImage>,
        bake: Vec<readshot_core::Annotation>,
    ) -> iced::futures::channel::oneshot::Receiver<HistoryRewriteResult> {
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        let Some(store) = &self.history else {
            return rx;
        };
        let id = record.id;
        if record.redacted {
            record.redactions = bake
                .iter()
                .filter(|a| readshot_core::scrub::is_redaction(a))
                .cloned()
                .collect();
        }
        if !record.redacted {
            // A restore (rare). Under the commit gate, then the state
            // lock (the documented order), so it can't interleave with a
            // commit: every trace of a scrub — the secured cache, parked
            // and unsecured entries — goes in one step, after any commit
            // already holding the gate has finished.
            let mut b = lock(&self.scrubs.book);
            let mut st = lock(&self.scrubs.state);
            let seq = next_rewrite_seq();
            st.latest.insert(id, seq);
            st.secured.remove(&id);
            drop(st);
            b.parked.remove(&id);
            b.unsecured.remove(&id);
            drop(b);
            let job = RewriteJob {
                seq,
                record,
                base,
                bake,
            };
            return self.queue_rewrite(job, tx, rx);
        }
        // A scrub. Bookkeeping only (no I/O): the UI thread never waits
        // on a write here unless phase one is actually needed.
        let (seq, phase_one) = {
            let mut st = lock(&self.scrubs.state);
            let seq = next_rewrite_seq();
            st.latest.insert(id, seq);
            let phase_one = st.secured.get(&id) != Some(&secured_key(&record));
            (seq, phase_one)
        };
        let job = RewriteJob {
            seq,
            record: record.clone(),
            base,
            bake,
        };
        if phase_one {
            // Phase one: a small sidecar write plus two removals. The
            // commit gate is only ever held for short commits (images are
            // staged outside it), so this doesn't wait on an encode or a
            // big image write.
            let mut b = lock(&self.scrubs.book);
            match store.begin_scrub(&record) {
                Ok(true) => {
                    self.scrubs.mark_secured(id, seq, &record);
                    b.unsecured.remove(&id);
                }
                Ok(false) => {
                    b.parked.insert(id, job);
                    return rx;
                }
                Err(e) => {
                    tracing::warn!(
                        target: "readshot::history",
                        "history scrub (phase one) failed: {e}"
                    );
                    b.unsecured.insert(id, (seq, record));
                }
            }
        }
        self.queue_rewrite(job, tx, rx)
    }

    /// Hand a rewrite to the writer thread (or write it inline when there
    /// is none) and return its receiver.
    fn queue_rewrite(
        &self,
        job: RewriteJob,
        tx: iced::futures::channel::oneshot::Sender<HistoryRewriteResult>,
        rx: iced::futures::channel::oneshot::Receiver<HistoryRewriteResult>,
    ) -> iced::futures::channel::oneshot::Receiver<HistoryRewriteResult> {
        let Some(store) = &self.history else {
            return rx;
        };
        let msg = HistoryUpdate::Rewrite(Box::new(HistoryRewrite {
            job,
            done: Some(tx),
            attempt: 0,
        }));
        let msg = match &self.history_update_tx {
            Some(sender) => match sender.send(msg) {
                Ok(()) => return rx,
                Err(std::sync::mpsc::SendError(msg)) => msg,
            },
            None => msg,
        };
        // No writer thread: write inline (no retry) rather than drop it.
        let _ = run_update(store.as_ref(), &self.scrubs, msg);
        rx
    }

    /// Store recognized text for `record` (see
    /// [`HistoryStore::record_recognized_text`]): masked with the stored
    /// redactions when the record is scrubbed; for an unredacted record
    /// only when `fill_unredacted`. No-op without a history store.
    pub fn record_recognized_text(
        &self,
        record: &CaptureRecord,
        doc: &readshot_core::text_lines::TextDocument,
        fill_unredacted: bool,
    ) -> Result<(), readshot_core::HistoryError> {
        match &self.history {
            Some(h) => h.record_recognized_text(record, doc, fill_unredacted),
            None => Ok(()),
        }
    }

    /// Block until every history update queued before this call has
    /// been written. Bounded wait so a wedged disk can't hang the
    /// caller. Used on shutdown so a quit straight after an annotation
    /// edit can't lose the trailing write; tests use it to make the
    /// async writer deterministic.
    ///
    /// Then, whether or not the wait timed out (a slow encode on quit),
    /// phase one is retried for every scrub that doesn't have it yet, so
    /// no redacted record is left serving unredacted content.
    pub fn flush_history_updates(&self) {
        if let Some(tx) = &self.history_update_tx {
            let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
            if tx.send(HistoryUpdate::Flush(ack_tx)).is_ok() {
                let _ = ack_rx.recv_timeout(std::time::Duration::from_secs(5));
            }
        }
        self.secure_pending_scrubs();
    }

    fn secure_pending_scrubs(&self) {
        let Some(store) = &self.history else {
            return;
        };
        let mut b = lock(&self.scrubs.book);
        let pending: Vec<(u64, CaptureRecord)> = b.unsecured.values().cloned().collect();
        for (seq, record) in pending {
            // A newer request (its own job handles the record) makes this
            // one moot; scrubbing anyway could undo a later restore.
            if self.scrubs.is_stale(record.id, seq) {
                b.unsecured.remove(&record.id);
                continue;
            }
            match store.begin_scrub(&record) {
                Ok(found) => {
                    b.unsecured.remove(&record.id);
                    if found {
                        self.scrubs.mark_secured(record.id, seq, &record);
                    }
                }
                Err(e) => tracing::warn!(
                    target: "readshot::history",
                    "history scrub (phase one) failed again: {e}"
                ),
            }
        }
    }

    /// Delete a single record (PNG + JSON + index entry). No-op when
    /// no history store is wired.
    pub fn delete_history(
        &self,
        id: readshot_core::Uuid,
    ) -> Result<(), readshot_core::HistoryError> {
        match &self.history {
            Some(h) => h.delete(id),
            None => Ok(()),
        }
    }

    /// Clear every persisted capture. No-op when no history store is wired.
    pub fn clear_history(&self) -> Result<(), readshot_core::HistoryError> {
        match &self.history {
            Some(h) => h.clear_all(),
            None => Ok(()),
        }
    }

    /// Append a capture to history, respecting the retention policy.
    /// Off → no-op. Other policies → save and apply retention.
    pub async fn record_history(
        &self,
        record: CaptureRecord,
        png_bytes: Vec<u8>,
        policy: HistoryRetention,
        now: chrono::DateTime<chrono::Utc>,
    ) {
        if matches!(policy, HistoryRetention::Off) {
            return;
        }
        let Some(history) = &self.history else {
            return;
        };
        // `save` writes the PNG, decodes it again to render a
        // thumbnail, and fsyncs — synchronous fs/CPU work that would
        // otherwise stall a tokio worker for the duration.
        let history = Arc::clone(history);
        let scrubs = Arc::clone(&self.scrubs);
        let join = tokio::task::spawn_blocking(move || {
            if let Err(e) = save_capture(history.as_ref(), &scrubs, &record, &png_bytes) {
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "history save failed: {e}");
                return;
            }
            if let Err(e) = history.apply_retention(policy, now) {
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "retention apply failed: {e}");
            }
        })
        .await;
        if let Err(e) = join {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "history save worker failed: {e}");
        }
    }
}

/// Save a fresh capture, or — when the editor already scrubbed it while
/// the save was in flight (a parked scrub) — its scrubbed version
/// instead, so the raw capture never reaches disk. A parked frame
/// rewrite (rotated, resized or combined document, no redaction) is
/// saved in place of the raw capture the same way, but is not a scrub. Rendering, encoding
/// and staging happen without any lock; the commit gate is held only to
/// re-check that the parked entry didn't change meanwhile (else
/// prepare again) and for the short commit.
fn save_capture(
    history: &dyn HistoryStore,
    scrubs: &Scrubs,
    record: &CaptureRecord,
    png: &[u8],
) -> Result<(), readshot_core::HistoryError> {
    let raw_thumb = readshot_core::history::thumbnail_png_from_bytes(png);
    loop {
        let parked = lock(&scrubs.book).parked.get(&record.id).cloned();
        // Everything slow happens here, without a lock: render and stage
        // the parked scrub, and stage the raw capture whenever it may be
        // needed (no scrub, or the scrub couldn't be produced).
        let rendered = parked.as_ref().and_then(|job| render_job(job).ok());
        let staged_scrub = rendered
            .as_ref()
            .map(|(scrubbed, bytes, thumb)| history.stage_image(scrubbed, bytes, thumb.as_deref()));
        let need_raw = !matches!(staged_scrub, Some(Ok(_)));
        let staged_raw = need_raw.then(|| history.stage_image(record, png, raw_thumb.as_deref()));
        let mut b = lock(&scrubs.book);
        if b.parked.get(&record.id).map(|j| j.seq) != parked.as_ref().map(|j| j.seq) {
            continue; // changed meanwhile; staged files drop
        }
        let Some(job) = parked else {
            return match staged_raw {
                Some(Ok(staged)) => history.commit_save(record, staged),
                Some(Err(e)) => Err(e),
                None => Ok(()),
            };
        };
        b.parked.remove(&record.id);
        if let (Some((scrubbed, _, _)), Some(Ok(staged))) = (rendered, staged_scrub) {
            let saved = history.commit_save(&scrubbed, staged);
            match &saved {
                // A parked frame rewrite is not a scrub: nothing to mark.
                Ok(()) if scrubbed.redacted => scrubs.mark_secured(record.id, job.seq, &scrubbed),
                Ok(()) => {}
                Err(_) => {
                    b.parked.insert(record.id, job);
                }
            }
            return saved;
        }
        // Couldn't render or stage the scrubbed image (rare): commit the
        // raw capture staged above (outside the lock), then run phase one
        // on it at once, still under the gate.
        let committed = match staged_raw {
            Some(Ok(staged)) => history.commit_save(record, staged),
            Some(Err(e)) => Err(e),
            None => Ok(()),
        };
        if let Err(e) = committed {
            b.parked.insert(record.id, job);
            return Err(e);
        }
        if job.record.redacted && history.begin_scrub(&job.record).is_err() {
            b.unsecured.insert(record.id, (job.seq, job.record.clone()));
        }
        return Ok(());
    }
}

/// Convenience constructor used by [`crate::app::App::new`] — wires
/// the per-OS defaults via the trait factories.
pub fn default_coordinator(
    capturer: Arc<dyn Capturer>,
    ocr: Arc<dyn OCREngine>,
    permissions: Arc<dyn PermissionsProvider>,
    history: Option<Arc<FsHistoryStore>>,
) -> CaptureCoordinator {
    let history: Option<Arc<dyn HistoryStore>> = history.map(|h| h as Arc<dyn HistoryStore>);
    CaptureCoordinator::new(capturer, ocr, permissions, history)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::fake::FakePermissions;
    use readshot_capture::fake::FakeCapturer;
    use readshot_capture::DisplayId;
    use readshot_core::geom::Rect;
    use readshot_ocr::fake::FakeOcrEngine;

    fn coordinator(perms: Arc<dyn PermissionsProvider>) -> CaptureCoordinator {
        CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello")),
            perms,
            None,
        )
    }

    fn capture_request() -> CaptureRequest {
        CaptureRequest {
            display_id: "fake-0".to_string() as DisplayId,
            rect: Rect::from_xywh(0.0, 0.0, 256.0, 256.0).unwrap(),
            scale: 1.0,
            hide_cursor: true,
        }
    }

    #[tokio::test]
    async fn list_displays_delegates_to_capturer() {
        let coord = coordinator(Arc::new(FakePermissions::granted()));
        let displays = coord.list_displays().await.unwrap();
        assert_eq!(displays.len(), 1);
    }

    #[tokio::test]
    async fn capture_region_returns_image_via_fake_capturer() {
        let coord = coordinator(Arc::new(FakePermissions::granted()));
        let img = coord.capture_region(capture_request()).await.unwrap();
        assert_eq!(img.width(), 256);
    }

    #[tokio::test]
    async fn recognise_returns_fake_text() {
        let coord = coordinator(Arc::new(FakePermissions::granted()));
        let img = image::RgbaImage::new(8, 8);
        let req = OCRRequest {
            image: img,
            languages: vec![],
            use_language_correction: false,
        };
        assert_eq!(coord.recognise(req).await.unwrap(), "hello");
    }

    #[test]
    fn pre_capture_gate_reflects_permissions_state() {
        let granted = Arc::new(FakePermissions::granted());
        let coord = coordinator(granted.clone());
        assert_eq!(coord.pre_capture_gate(), PermissionStatus::Granted);

        let denied = Arc::new(FakePermissions::denied());
        let coord = coordinator(denied);
        assert_eq!(coord.pre_capture_gate(), PermissionStatus::Denied);
    }

    fn request(w: u32, h: u32) -> OCRRequest {
        OCRRequest {
            image: image::RgbaImage::new(w, h),
            languages: vec![],
            use_language_correction: false,
        }
    }

    #[tokio::test]
    async fn recognise_document_keeps_positions_from_positioned_engines() {
        use readshot_core::geom::RectLike;
        use readshot_core::ocr_layout::RecognizedLine;
        let line = RecognizedLine {
            text: "hello".into(),
            x: 0.25,
            y: 0.5,
            w: 0.5,
            h: 0.25,
            words: Vec::new(),
        };
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_lines(vec![line])),
            Arc::new(FakePermissions::granted()),
            None,
        );
        let doc = coord.recognise_document(request(8, 4)).await.unwrap();
        assert!(doc.positioned);
        assert_eq!(doc.lines[0].rect, Some(RectLike::new(2.0, 2.0, 4.0, 1.0)));
        assert_eq!(coord.recognise(request(8, 4)).await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn recognise_document_without_positions_lists_plain_lines() {
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("one\n\ntwo")),
            Arc::new(FakePermissions::granted()),
            None,
        );
        let doc = coord.recognise_document(request(8, 8)).await.unwrap();
        assert!(!doc.positioned);
        let texts: Vec<&str> = doc.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(texts, ["one", "two"]);
        assert_eq!(doc.layout_text, "one\n\ntwo");
    }

    fn tagged(id: readshot_core::Uuid, tag: &str) -> CaptureRecord {
        let mut r = CaptureRecord::new(chrono::Utc::now(), 4, 4, "d");
        r.id = id;
        r.ocr_text = Some(tag.to_string());
        r
    }

    fn rewrite(id: readshot_core::Uuid, tag: &str) -> HistoryUpdate {
        HistoryUpdate::Rewrite(Box::new(HistoryRewrite {
            job: RewriteJob {
                seq: 0,
                record: tagged(id, tag),
                base: Arc::new(image::RgbaImage::new(1, 1)),
                bake: Vec::new(),
            },
            done: None,
            attempt: 0,
        }))
    }

    fn tags(batch: &[HistoryUpdate]) -> Vec<String> {
        batch
            .iter()
            .map(|m| match m {
                HistoryUpdate::Write(r) => format!("W:{}", r.ocr_text.as_deref().unwrap()),
                HistoryUpdate::Rewrite(rw) => {
                    format!("R:{}", rw.job.record.ocr_text.as_deref().unwrap())
                }
                HistoryUpdate::Flush(_) => "F".to_string(),
            })
            .collect()
    }

    #[test]
    fn rapid_rewrites_of_one_record_coalesce_to_the_latest() {
        let id = readshot_core::Uuid::new_v4();
        let batch = vec![rewrite(id, "1"), rewrite(id, "2"), rewrite(id, "3")];
        assert_eq!(tags(&coalesce(batch)), ["R:3"]);
    }

    #[test]
    fn coalescing_keeps_order_flushes_and_other_records() {
        let a = readshot_core::Uuid::new_v4();
        let b = readshot_core::Uuid::new_v4();
        let (ack, _rx) = std::sync::mpsc::sync_channel(1);
        let batch = vec![
            HistoryUpdate::Write(tagged(a, "w1")),
            rewrite(a, "r1"),
            HistoryUpdate::Write(tagged(b, "b1")),
            HistoryUpdate::Write(tagged(a, "w2")),
            HistoryUpdate::Write(tagged(a, "w3")),
            HistoryUpdate::Flush(ack),
            HistoryUpdate::Write(tagged(b, "b2")),
        ];
        // A rewrite supersedes every earlier write of its record; a
        // plain write only earlier plain writes after the last rewrite
        // (the rewrite carries the image a later write doesn't).
        assert_eq!(tags(&coalesce(batch)), ["R:r1", "W:w3", "F", "W:b2"]);
    }

    #[test]
    fn rewrite_history_async_bakes_annotations_off_thread_and_reports_success() {
        use readshot_core::{Annotation, RectLike};
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello")),
            Arc::new(FakePermissions::granted()),
            Some(store.clone() as Arc<dyn HistoryStore>),
        );
        let mut base = image::RgbaImage::new(8, 8);
        for px in base.pixels_mut() {
            *px = image::Rgba([255, 255, 255, 255]);
        }
        let mut record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        record.ocr_text = Some("original".into());
        store
            .save(&record, &readshot_core::encode_png(&base).unwrap())
            .unwrap();
        let bake = vec![Annotation::Pixelate {
            rect: RectLike::new(0.0, 0.0, 4.0, 4.0),
            block_size: 2.0,
        }];
        let mut scrubbed = record.clone();
        scrubbed.ocr_text = Some("masked".into());
        scrubbed.redacted = true;
        let base = Arc::new(base);
        let rx = coord.rewrite_history_async(scrubbed, Arc::clone(&base), bake.clone());
        coord.flush_history_updates();
        assert_eq!(
            iced::futures::executor::block_on(rx).unwrap(),
            Ok(()),
            "the write reports success"
        );
        let listed = store.list().unwrap();
        assert_eq!(listed[0].ocr_text.as_deref(), Some("masked"));
        let png = std::fs::read(readshot_core::FsHistoryStore::abs_png_path(
            store.root(),
            &listed[0],
        ))
        .unwrap();
        let on_disk = image::load_from_memory(&png).unwrap().to_rgba8();
        assert_eq!(on_disk, readshot_core::render(&base, &bake));
    }

    /// Delegates to an `FsHistoryStore`, failing the first
    /// `fail_rewrites` rewrites and `fail_begins` phase-one calls.
    struct FlakyStore {
        inner: readshot_core::FsHistoryStore,
        fail_rewrites: std::sync::atomic::AtomicUsize,
        fail_begins: std::sync::atomic::AtomicUsize,
    }

    fn take_failure(n: &std::sync::atomic::AtomicUsize) -> bool {
        n.fetch_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |v| v.checked_sub(1),
        )
        .is_ok()
    }

    impl HistoryStore for FlakyStore {
        fn save(&self, r: &CaptureRecord, png: &[u8]) -> Result<(), readshot_core::HistoryError> {
            self.inner.save(r, png)
        }
        fn list(&self) -> Result<Vec<CaptureRecord>, readshot_core::HistoryError> {
            self.inner.list()
        }
        fn apply_retention(
            &self,
            p: HistoryRetention,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), readshot_core::HistoryError> {
            self.inner.apply_retention(p, now)
        }
        fn clear_all(&self) -> Result<(), readshot_core::HistoryError> {
            self.inner.clear_all()
        }
        fn update(&self, r: &CaptureRecord) -> Result<(), readshot_core::HistoryError> {
            self.inner.update(r)
        }
        fn stage_image(
            &self,
            r: &CaptureRecord,
            png: &[u8],
            thumb: Option<&[u8]>,
        ) -> Result<readshot_core::StagedImage, readshot_core::HistoryError> {
            self.inner.stage_image(r, png, thumb)
        }
        fn commit_rewrite(
            &self,
            r: &CaptureRecord,
            staged: readshot_core::StagedImage,
        ) -> Result<bool, readshot_core::HistoryError> {
            if take_failure(&self.fail_rewrites) {
                return Err(readshot_core::HistoryError::Io("disk full".into()));
            }
            self.inner.commit_rewrite(r, staged)
        }
        fn commit_save(
            &self,
            r: &CaptureRecord,
            staged: readshot_core::StagedImage,
        ) -> Result<(), readshot_core::HistoryError> {
            self.inner.commit_save(r, staged)
        }
        fn begin_scrub(&self, r: &CaptureRecord) -> Result<bool, readshot_core::HistoryError> {
            if take_failure(&self.fail_begins) {
                return Err(readshot_core::HistoryError::Io("disk full".into()));
            }
            self.inner.begin_scrub(r)
        }
    }

    fn history_coord(store: Arc<dyn HistoryStore>) -> CaptureCoordinator {
        CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello")),
            Arc::new(FakePermissions::granted()),
            Some(store),
        )
    }

    fn white(w: u32, h: u32) -> image::RgbaImage {
        let mut img = image::RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 255, 255, 255]);
        }
        img
    }

    fn pixelate() -> Vec<readshot_core::Annotation> {
        vec![readshot_core::Annotation::Rectangle {
            rect: readshot_core::RectLike::new(0.0, 0.0, 4.0, 4.0),
            color: readshot_core::Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: readshot_core::ShapeStyle {
                fill: readshot_core::FillMode::Filled,
                ..readshot_core::ShapeStyle::default()
            },
        }]
    }

    fn scrubbed(r: &CaptureRecord) -> CaptureRecord {
        let mut s = r.clone();
        s.ocr_text = Some("masked".into());
        s.redacted = true;
        s
    }

    fn png_on_disk(
        store: &readshot_core::FsHistoryStore,
        r: &CaptureRecord,
    ) -> Option<image::RgbaImage> {
        let bytes =
            std::fs::read(readshot_core::FsHistoryStore::abs_png_path(store.root(), r)).ok()?;
        Some(image::load_from_memory(&bytes).unwrap().to_rgba8())
    }

    fn sidecar(store: &readshot_core::FsHistoryStore, r: &CaptureRecord) -> CaptureRecord {
        store
            .list()
            .unwrap()
            .into_iter()
            .find(|x| x.id == r.id)
            .unwrap()
    }

    #[tokio::test]
    async fn a_scrub_of_a_record_not_saved_yet_is_applied_when_the_save_lands() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let coord = history_coord(store.clone());
        let base = Arc::new(white(8, 8));
        let mut record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        record.ocr_text = Some("secret".into());
        // The editor redacts and then goes away before the save lands.
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate()));
        coord.flush_history_updates();
        assert!(store.list().unwrap().is_empty());
        let raw = readshot_core::encode_png(&base).unwrap();
        coord
            .record_history(
                record.clone(),
                raw,
                HistoryRetention::Unlimited,
                chrono::Utc::now(),
            )
            .await;
        let stored = sidecar(&store, &record);
        assert!(stored.redacted);
        assert_eq!(stored.ocr_text.as_deref(), Some("masked"));
        assert_eq!(
            png_on_disk(&store, &record).unwrap(),
            readshot_core::render(&base, &pixelate())
        );
    }

    #[tokio::test]
    async fn a_frame_rewrite_of_a_record_not_saved_yet_is_saved_in_place_of_the_raw_capture() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let coord = history_coord(store.clone());
        let raw_img = white(8, 4);
        let rotated = Arc::new(image::imageops::rotate90(&raw_img));
        let record = CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        let mut framed = record.clone();
        framed.geometry_id = Some("n:rot=1".into());
        drop(coord.rewrite_history_async(framed, Arc::clone(&rotated), Vec::new()));
        coord.flush_history_updates();
        let raw = readshot_core::encode_png(&raw_img).unwrap();
        coord
            .record_history(
                record.clone(),
                raw,
                HistoryRetention::Unlimited,
                chrono::Utc::now(),
            )
            .await;
        let stored = sidecar(&store, &record);
        assert_eq!(stored.geometry_id.as_deref(), Some("n:rot=1"));
        assert!(!stored.redacted);
        assert_eq!(png_on_disk(&store, &record).unwrap(), *rotated);
    }

    #[tokio::test]
    async fn a_restore_cancels_a_parked_scrub() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let coord = history_coord(store.clone());
        let base = Arc::new(white(8, 8));
        let mut record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        record.ocr_text = Some("original".into());
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate()));
        drop(coord.rewrite_history_async(record.clone(), Arc::clone(&base), Vec::new()));
        coord.flush_history_updates();
        let raw = readshot_core::encode_png(&base).unwrap();
        coord
            .record_history(
                record.clone(),
                raw,
                HistoryRetention::Unlimited,
                chrono::Utc::now(),
            )
            .await;
        let stored = sidecar(&store, &record);
        assert!(!stored.redacted);
        assert_eq!(stored.ocr_text.as_deref(), Some("original"));
        assert_eq!(png_on_disk(&store, &record).unwrap(), *base);
    }

    #[test]
    fn phase_one_hides_the_unredacted_content_before_the_image_is_encoded() {
        let dir = tempfile::TempDir::new().unwrap();
        let fs_store = readshot_core::FsHistoryStore::new(dir.path().join("h"));
        let mut record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        record.ocr_text = Some("secret".into());
        fs_store
            .save(&record, &readshot_core::encode_png(&white(8, 8)).unwrap())
            .unwrap();
        let store = Arc::new(FlakyStore {
            inner: readshot_core::FsHistoryStore::new(dir.path().join("h")),
            // The image write keeps failing for a while.
            fail_rewrites: std::sync::atomic::AtomicUsize::new(2),
            fail_begins: std::sync::atomic::AtomicUsize::new(0),
        });
        let coord = history_coord(store.clone());
        let base = Arc::new(white(8, 8));
        let rx = coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate());
        // Synchronous phase one: masked text, no unredacted image.
        let stored = sidecar(&fs_store, &record);
        assert_eq!(stored.ocr_text.as_deref(), Some("masked"));
        assert!(stored.scrub_pending);
        assert!(png_on_disk(&fs_store, &record).is_none());
        // The first write fails and is reported…
        assert!(matches!(iced::futures::executor::block_on(rx), Ok(Err(_))));
        // …and retried by the coordinator, no editor involved.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while sidecar(&fs_store, &record).scrub_pending {
            assert!(std::time::Instant::now() < deadline, "never retried");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            png_on_disk(&fs_store, &record).unwrap(),
            readshot_core::render(&base, &pixelate())
        );
    }

    #[test]
    fn flushing_secures_a_scrub_whose_phase_one_failed() {
        let dir = tempfile::TempDir::new().unwrap();
        let fs_store = readshot_core::FsHistoryStore::new(dir.path().join("h"));
        let mut record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        record.ocr_text = Some("secret".into());
        fs_store
            .save(&record, &readshot_core::encode_png(&white(8, 8)).unwrap())
            .unwrap();
        let store = Arc::new(FlakyStore {
            inner: readshot_core::FsHistoryStore::new(dir.path().join("h")),
            fail_rewrites: std::sync::atomic::AtomicUsize::new(usize::MAX),
            fail_begins: std::sync::atomic::AtomicUsize::new(1),
        });
        let coord = history_coord(store);
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::new(white(8, 8)), pixelate()));
        assert_eq!(
            sidecar(&fs_store, &record).ocr_text.as_deref(),
            Some("secret"),
            "phase one failed"
        );
        // Quit: the flush secures what's still unsecured.
        coord.flush_history_updates();
        let stored = sidecar(&fs_store, &record);
        assert_eq!(stored.ocr_text.as_deref(), Some("masked"));
        assert!(png_on_disk(&fs_store, &record).is_none());
    }

    fn saved_record(dir: &std::path::Path) -> CaptureRecord {
        let fs_store = readshot_core::FsHistoryStore::new(dir.join("h"));
        let mut record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        record.ocr_text = Some("secret".into());
        fs_store
            .save(&record, &readshot_core::encode_png(&white(8, 8)).unwrap())
            .unwrap();
        record
    }

    #[test]
    fn a_stale_rewrite_never_lands_after_a_newer_request() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = readshot_core::FsHistoryStore::new(dir.path().join("h"));
        let scrubs = Scrubs::default();
        lock(&scrubs.state).latest.insert(record.id, 2);
        let mut stale = record.clone();
        stale.ocr_text = Some("stale".into());
        let job = RewriteJob {
            seq: 1,
            record: stale.clone(),
            base: Arc::new(white(8, 8)),
            bake: Vec::new(),
        };
        let png = readshot_core::encode_png(&white(8, 8)).unwrap();
        let outcome = apply_rewrite(&store, &scrubs, &job, &stale, &png, None);
        assert!(matches!(outcome, Outcome::Stale));
        assert_eq!(sidecar(&store, &record).ocr_text.as_deref(), Some("secret"));
        // The staged files of the stale write were removed.
        let dir_of = readshot_core::FsHistoryStore::abs_png_path(store.root(), &record);
        let leftovers = std::fs::read_dir(dir_of.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".stage"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn a_stale_scrub_that_fails_to_render_never_removes_a_restored_image() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = readshot_core::FsHistoryStore::new(dir.path().join("h"));
        let scrubs = Scrubs::default();
        // A restore (seq 2) came after this scrub (seq 1).
        lock(&scrubs.state).latest.insert(record.id, 2);
        let job = RewriteJob {
            seq: 1,
            record: scrubbed(&record),
            base: Arc::new(white(8, 8)),
            bake: pixelate(),
        };
        fail_closed(&store, &scrubs, &job);
        assert!(
            png_on_disk(&store, &record).is_some(),
            "restored image kept"
        );
        assert_eq!(sidecar(&store, &record).ocr_text.as_deref(), Some("secret"));
        // The newest scrub does run phase one.
        lock(&scrubs.state).latest.insert(record.id, 1);
        fail_closed(&store, &scrubs, &job);
        assert!(png_on_disk(&store, &record).is_none());
        assert_eq!(sidecar(&store, &record).ocr_text.as_deref(), Some("masked"));
    }

    #[test]
    fn a_plain_write_never_cancels_a_pending_scrub_retry() {
        let id = readshot_core::Uuid::new_v4();
        let other = readshot_core::Uuid::new_v4();
        let retry = |id| {
            let HistoryUpdate::Rewrite(rw) = rewrite(id, "retry") else {
                unreachable!()
            };
            (std::time::Instant::now(), rw)
        };
        let mut retries = vec![retry(id), retry(other)];
        drop_superseded_retries(&mut retries, &[HistoryUpdate::Write(tagged(id, "w"))]);
        assert_eq!(retries.len(), 2, "a plain write carries no image");
        drop_superseded_retries(&mut retries, &[rewrite(id, "newer")]);
        assert_eq!(retries.len(), 1);
        assert_eq!(retries[0].1.job.record.id, other);
    }

    /// Delegates to an `FsHistoryStore`, but `stage_image` and/or
    /// `commit_rewrite` block until released, signalling when entered.
    struct BlockingStore {
        inner: readshot_core::FsHistoryStore,
        block_stage: std::sync::atomic::AtomicBool,
        block_commit: std::sync::atomic::AtomicBool,
        entered: std::sync::Mutex<std::sync::mpsc::Sender<()>>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl BlockingStore {
        fn wait_if(&self, flag: &std::sync::atomic::AtomicBool) {
            if flag.swap(false, std::sync::atomic::Ordering::SeqCst) {
                let _ = self.entered.lock().unwrap().send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10));
            }
        }
    }

    impl HistoryStore for BlockingStore {
        fn save(&self, r: &CaptureRecord, png: &[u8]) -> Result<(), readshot_core::HistoryError> {
            self.inner.save(r, png)
        }
        fn list(&self) -> Result<Vec<CaptureRecord>, readshot_core::HistoryError> {
            self.inner.list()
        }
        fn apply_retention(
            &self,
            p: HistoryRetention,
            now: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), readshot_core::HistoryError> {
            self.inner.apply_retention(p, now)
        }
        fn clear_all(&self) -> Result<(), readshot_core::HistoryError> {
            self.inner.clear_all()
        }
        fn begin_scrub(&self, r: &CaptureRecord) -> Result<bool, readshot_core::HistoryError> {
            self.inner.begin_scrub(r)
        }
        fn stage_image(
            &self,
            r: &CaptureRecord,
            png: &[u8],
            thumb: Option<&[u8]>,
        ) -> Result<readshot_core::StagedImage, readshot_core::HistoryError> {
            self.wait_if(&self.block_stage);
            self.inner.stage_image(r, png, thumb)
        }
        fn commit_rewrite(
            &self,
            r: &CaptureRecord,
            staged: readshot_core::StagedImage,
        ) -> Result<bool, readshot_core::HistoryError> {
            self.wait_if(&self.block_commit);
            self.inner.commit_rewrite(r, staged)
        }
    }

    /// Runs `f` on another thread; true if it finished within 2 s.
    fn finishes_promptly(f: impl FnOnce() + Send + 'static) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok()
    }

    type Gate = (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>);

    fn blocking_coord(
        dir: &std::path::Path,
        block_stage: bool,
        block_commit: bool,
    ) -> (CaptureCoordinator, Gate) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let store = Arc::new(BlockingStore {
            inner: readshot_core::FsHistoryStore::new(dir.join("h")),
            block_stage: std::sync::atomic::AtomicBool::new(block_stage),
            block_commit: std::sync::atomic::AtomicBool::new(block_commit),
            entered: std::sync::Mutex::new(entered_tx),
            release: std::sync::Mutex::new(release_rx),
        });
        (history_coord(store), (entered_rx, release_tx))
    }

    #[test]
    fn a_new_redaction_does_not_wait_on_an_image_being_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let (coord, (entered, release)) = blocking_coord(dir.path(), true, false);
        let base = Arc::new(white(8, 8));
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate()));
        // The writer is now stuck writing the image.
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // A different redaction needs phase one (the commit gate)…
        let mut more = pixelate();
        more.push(readshot_core::Annotation::Blur {
            rect: readshot_core::RectLike::new(4.0, 4.0, 2.0, 2.0),
            radius: 2.0,
        });
        let c = coord.clone();
        let (r, b) = (scrubbed(&record), Arc::clone(&base));
        // …and still doesn't wait on the image write.
        assert!(finishes_promptly(move || drop(
            c.rewrite_history_async(r, b, more)
        )));
        let _ = release.send(());
        coord.flush_history_updates();
    }

    #[test]
    fn an_edit_with_the_same_redactions_does_not_wait_on_a_commit() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let (coord, (entered, release)) = blocking_coord(dir.path(), false, true);
        let base = Arc::new(white(8, 8));
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate()));
        // The writer holds the commit gate.
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let c = coord.clone();
        let (r, b) = (scrubbed(&record), Arc::clone(&base));
        assert!(finishes_promptly(move || drop(c.rewrite_history_async(
            r,
            b,
            pixelate()
        ))));
        let _ = release.send(());
        coord.flush_history_updates();
    }

    #[test]
    fn a_redo_after_a_restore_raced_with_a_commit_still_runs_phase_one() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let fs_store = readshot_core::FsHistoryStore::new(dir.path().join("h"));
        let (coord, (entered, release)) = blocking_coord(dir.path(), false, true);
        let base = Arc::new(white(8, 8));
        // S1 (redactions R) is mid-commit, holding the commit gate.
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate()));
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        // ⌘Z: undo all → restore Q, requested while S1 commits.
        let c = coord.clone();
        let (r, b) = (record.clone(), Arc::clone(&base));
        let restore = std::thread::spawn(move || {
            drop(c.rewrite_history_async(r, b, Vec::new()));
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let _ = release.send(());
        restore.join().unwrap();
        coord.flush_history_updates();
        assert!(png_on_disk(&fs_store, &record).is_some(), "restore landed");
        assert_eq!(
            sidecar(&fs_store, &record).ocr_text.as_deref(),
            Some("secret")
        );
        // Redo with the same redactions: phase one must run right away.
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate()));
        assert!(
            png_on_disk(&fs_store, &record).is_none(),
            "unredacted image still served"
        );
        let stored = sidecar(&fs_store, &record);
        assert_eq!(stored.ocr_text.as_deref(), Some("masked"));
        assert!(stored.redacted);
        coord.flush_history_updates();
    }
}
