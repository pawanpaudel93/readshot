//! Capture coordinator — service-layer orchestration that the iced
//! App's `update` fn drives.
//!
//! The coordinator is *not* the iced state machine itself (that's
//! `crate::app::App`); it's the bag of async methods the state
//! machine calls when transitioning between user-visible states.
//! Splitting service from view keeps every backend interaction
//! testable with fakes.
//!
//! Capture flow:
//!
//! 1. Permissions check — `pre_capture_gate()`.
//! 2. List displays — `list_displays()`.
//! 3. (User confirms a region in the overlay; the App receives a
//!    `CaptureMsg::OverlaySelected` carrying the display id, logical rect,
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
    /// A plain sidecar update, and its place among the record's
    /// outstanding writes ([`WriteTracker`]).
    Write(Box<CaptureRecord>, Option<PendingWrite>),
    /// Replace a record's image and sidecar (the editor's History scrub
    /// and its restore). See [`HistoryRewrite`].
    Rewrite(Box<HistoryRewrite>),
    /// Rendezvous: acknowledged once everything queued before it has
    /// been written. Lets shutdown (and tests) wait for durability. The
    /// answer is how many failed rewrites still wait for a retry.
    Flush(std::sync::mpsc::SyncSender<usize>),
}

/// Why a queued History rewrite didn't land.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HistoryRewriteError {
    /// The write failed (it is retried); the reason, user-presentable.
    #[error("{0}")]
    Failed(String),
    /// An unredacted rewrite of a record that is redacted in History by
    /// someone else (another editor, or an earlier run): refused for
    /// good, History keeps the redacted record.
    #[error("the capture is redacted in History by another window")]
    RedactedElsewhere,
    /// An unredacted rewrite of a record whose History sidecar couldn't
    /// be read: its state is unknown, so it is treated as redacted and
    /// not changed. Not final: a later write tries again.
    #[error("couldn’t read this capture’s History record")]
    Unreadable,
}

/// Outcome of a queued [`CaptureCoordinator::rewrite_history_async`].
/// A rewrite superseded by a newer one for the same record never runs;
/// its receiver is cancelled.
pub type HistoryRewriteResult = Result<(), HistoryRewriteError>;

/// One full record replacement: the PNG is `bake` rendered onto `base`
/// (or `base` itself when `bake` is empty).
#[derive(Clone)]
struct RewriteJob {
    /// Request order per record; only the newest may land.
    seq: u64,
    record: CaptureRecord,
    base: Arc<RgbaImage>,
    bake: Vec<readshot_core::Annotation>,
    /// A restore whose requester owns the record's newest scrub (the
    /// editor session undoing its own redaction): it may replace a
    /// redacted record. Any other restore never does ([`apply_rewrite`]).
    may_unredact: bool,
}

/// A queued rewrite. Rendering and encoding run on the writer thread,
/// never the UI thread; a failed write is retried with backoff.
struct HistoryRewrite {
    job: RewriteJob,
    done: Option<iced::futures::channel::oneshot::Sender<HistoryRewriteResult>>,
    /// Failed attempts so far (drives the retry backoff).
    attempt: u32,
    /// Outstanding until it lands, goes stale, is refused or first fails;
    /// see [`WriteTracker`]. Its retry is not waited for: the wait that
    /// sees the failure cancels it
    /// ([`CaptureCoordinator::settle_record_writes`]).
    pending: Option<PendingWrite>,
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
    /// Outstanding History writes per record ([`WriteTracker`]).
    writes: Arc<WriteTracker>,
}

/// The History writes still outstanding for each record, so a caller can
/// wait until every write queued for a record is done (landed, stale,
/// refused or failed) with no timeout, and learn which failed. Each
/// queued write holds a [`PendingWrite`]; dropping it marks it done,
/// whichever way the message goes (written, coalesced away, stale). A
/// failed write reports its reason through [`PendingWrite::fail`], which
/// records it *before* the write stops being outstanding, so a waiter
/// woken by it always sees why.
#[derive(Default)]
struct WriteTracker {
    state: std::sync::Mutex<TrackedWrites>,
    done: std::sync::Condvar,
}

#[derive(Default)]
struct TrackedWrites {
    outstanding: std::collections::HashMap<readshot_core::Uuid, usize>,
    /// Why writes of a record failed or were refused since its last
    /// successful one.
    failures: std::collections::HashMap<readshot_core::Uuid, Vec<String>>,
}

/// One outstanding History write of `id`; done when dropped.
struct PendingWrite {
    id: readshot_core::Uuid,
    tracker: Arc<WriteTracker>,
}

impl Drop for PendingWrite {
    fn drop(&mut self) {
        let mut st = lock(&self.tracker.state);
        if let Some(n) = st.outstanding.get_mut(&self.id) {
            *n -= 1;
            if *n == 0 {
                st.outstanding.remove(&self.id);
            }
        }
        drop(st);
        self.tracker.done.notify_all();
    }
}

impl PendingWrite {
    /// Done, and failed for `why`: recorded first, then released.
    fn fail(self, why: String) {
        self.tracker.failed(self.id, why);
        // `self` drops here: no longer outstanding.
    }
}

impl WriteTracker {
    fn begin(self: &Arc<Self>, id: readshot_core::Uuid) -> PendingWrite {
        *lock(&self.state).outstanding.entry(id).or_insert(0) += 1;
        PendingWrite {
            id,
            tracker: Arc::clone(self),
        }
    }

    fn failed(&self, id: readshot_core::Uuid, why: String) {
        lock(&self.state).failures.entry(id).or_default().push(why);
    }

    fn succeeded(&self, id: readshot_core::Uuid) {
        lock(&self.state).failures.remove(&id);
    }

    /// Block until no write of `id` is outstanding.
    fn wait_idle(&self, id: readshot_core::Uuid) {
        let mut st = lock(&self.state);
        while st.outstanding.contains_key(&id) {
            st = self
                .done
                .wait(st)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn is_outstanding(&self, id: readshot_core::Uuid) -> bool {
        lock(&self.state).outstanding.contains_key(&id)
    }
}

/// What [`CaptureCoordinator::settle_record_writes`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettledWrites {
    /// Why writes of the record failed or were refused since its last
    /// successful write. Each failed write is cancelled: it never lands.
    pub failures: Vec<String>,
    /// The record's newest request is a redaction whose first phase
    /// (hiding the unredacted image) still couldn't be written: History
    /// may still hold the unredacted image, so it must not be opened.
    pub exposed: bool,
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
    /// The editor session whose scrub made each record redacted (`None`:
    /// a caller without one). Only that session may write it unredacted
    /// again: its undo, and its later changes, until one of them lands
    /// (the record is then unredacted on disk and needs no owner). A
    /// restore request alone doesn't end it: a frame change queued behind
    /// that restore is the same session's, and must not be refused.
    scrub_owners: std::collections::HashMap<readshot_core::Uuid, Option<u64>>,
}

/// A secured scrub's document frame (`CaptureRecord::geometry_id`) and
/// redactions, and, while only its phase one is on disk, what that phase
/// one recorded to draw (`None` once its image landed).
type SecuredScrub = (
    Option<String>,
    Vec<readshot_core::Annotation>,
    Option<Vec<readshot_core::Annotation>>,
);

/// `record`'s phase one as recorded (its image not landed yet).
fn secured_key(record: &CaptureRecord) -> SecuredScrub {
    (
        record.geometry_id.clone(),
        record.redactions.clone(),
        Some(
            record
                .pending_image
                .as_ref()
                .map(|p| p.bake.clone())
                .unwrap_or_default(),
        ),
    )
}

/// Whether phase one for `record` is already on disk as it would be
/// written now: the same redactions in the same frame, and either their
/// image landed or the pending drawing is the same. Otherwise phase one
/// runs again (so the pending record always holds the newest request).
fn already_secured(secured: Option<&SecuredScrub>, record: &CaptureRecord) -> bool {
    let want = secured_key(record);
    secured.is_some_and(|(frame, redactions, bake)| {
        *frame == want.0 && *redactions == want.1 && (bake.is_none() || *bake == want.2)
    })
}

/// The frame id of a scrub's stored image when it is `crop` of the
/// document in frame `frame`: deterministic (the same crop of the same
/// frame is the same frame, so the secured cache and kept masked text
/// still match), never `None` (that is the capture as taken) and never an
/// id the editor produces (those are a stored id or `<uuid>:<geometry>`).
/// Fixed length, so reopening and cropping again never grows it.
/// Persisted, so hashed with FNV-1a (stable across builds), not std's
/// `DefaultHasher` (whose output may change with the toolchain, and an
/// id that stops matching drops the record's text). The version tag
/// changes when the text a cropped record may keep changes (v3: only
/// words whose boxes lie wholly inside the crop), so text stored under an older
/// rule is never kept by a later scrub (`HistoryStore::begin_scrub`).
/// The rule is also named by the id's prefix
/// ([`readshot_core::history::CROP_FRAME_ID_PREFIX`]), so a record of an
/// older rule is recognised and migrated when History loads
/// ([`readshot_core::history::migrate_legacy_record`]).
pub(crate) fn cropped_frame_id(
    frame: Option<&str>,
    crop: readshot_core::render::OutputCrop,
) -> String {
    let mut h = readshot_core::document::Fnv1a64::new();
    h.write(b"crop-v3\0");
    match frame {
        None => h.write(b"original"),
        Some(f) => {
            h.write(b"frame\0");
            h.write(f.as_bytes());
        }
    }
    h.write(b"\0");
    for v in [crop.x.to_bits(), crop.y.to_bits(), crop.width, crop.height] {
        h.write(&v.to_le_bytes());
    }
    format!(
        "{}{:016x}",
        readshot_core::history::CROP_FRAME_ID_PREFIX,
        h.finish()
    )
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

    /// [`Self::mark_secured`] for a scrub whose image landed.
    fn mark_landed(&self, id: readshot_core::Uuid, seq: u64, record: &CaptureRecord) {
        let mut st = lock(&self.state);
        if st.latest.get(&id) == Some(&seq) {
            let (frame, redactions, _) = secured_key(record);
            st.secured.insert(id, (frame, redactions, None));
        }
    }

    /// Unredacted request `seq` of record `id` landed: if it is still the
    /// newest, the record is unredacted on disk and nobody owns a scrub
    /// of it any more.
    fn unredacted_landed(&self, id: readshot_core::Uuid, seq: u64) {
        let mut st = lock(&self.state);
        if st.latest.get(&id) == Some(&seq) {
            st.scrub_owners.remove(&id);
        }
    }
}

impl HistoryUpdate {
    fn record_id(&self) -> Option<readshot_core::Uuid> {
        match self {
            Self::Write(r, _) => Some(r.id),
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
            (HistoryUpdate::Write(..), Some(id)) => {
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
    /// A restore over a record that is (or may be) redacted; nothing was
    /// written, and it is not retried.
    Refused(HistoryRewriteError),
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
    // A restore nobody here may make over a redacted record (its scrub
    // came from an earlier run, or the sidecar can't be read): dropped.
    if !record.redacted && !job.may_unredact {
        let refused = match store.stored_redacted(record) {
            Ok(false) => None,
            Ok(true) => Some(HistoryRewriteError::RedactedElsewhere),
            Err(_) => Some(HistoryRewriteError::Unreadable),
        };
        if let Some(why) = refused {
            tracing::warn!(
                target: readshot_core::log::cat::HISTORY,
                "dropped a restore of record {id}: {why}"
            );
            return Outcome::Refused(why);
        }
    }
    match store.commit_rewrite(record, staged) {
        Ok(true) => {
            if record.redacted {
                scrubs.mark_landed(id, job.seq, record);
            } else {
                scrubs.unredacted_landed(id, job.seq);
            }
            b.unsecured.remove(&id);
            // Under the commit gate, so a settle (which takes the gate)
            // never reports a failure this write has since made good.
            scrubs.writes.succeeded(id);
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
            // On-disk state unknown: phase one is redone next time, for
            // the newest request only (the gate is held; a newer one has
            // its own): redoing an older one would put back what the user
            // has since changed.
            if record.redacted && !scrubs.is_stale(id, job.seq) {
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
    let id = rewrite.job.record.id;
    match outcome {
        Outcome::Done => {
            if let Some(done) = rewrite.done.take() {
                let _ = done.send(Ok(()));
            }
            None
        }
        Outcome::Missing | Outcome::Stale => None,
        Outcome::Refused(e) => {
            match rewrite.pending.take() {
                Some(pending) => pending.fail(e.to_string()),
                None => scrubs.writes.failed(id, e.to_string()),
            }
            if let Some(done) = rewrite.done.take() {
                let _ = done.send(Err(e));
            }
            None
        }
        Outcome::Failed(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "history rewrite failed: {e}");
            // Its first failure ends the wait (the reason is recorded
            // before it does); the retry goes on until it lands, a newer
            // rewrite replaces it, or a settle cancels it.
            if let Some(pending) = rewrite.pending.take() {
                pending.fail(e.clone());
            }
            if let Some(done) = rewrite.done.take() {
                let _ = done.send(Err(HistoryRewriteError::Failed(e)));
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
        HistoryUpdate::Write(record, pending) => {
            if let Err(e) = store.update(&record) {
                tracing::warn!(
                    target: readshot_core::log::cat::HISTORY,
                    "background history update failed: {e}"
                );
                match pending {
                    Some(pending) => pending.fail(e.to_string()),
                    None => scrubs.writes.failed(record.id, e.to_string()),
                }
            }
            // Written (or failed): no longer outstanding.
            None
        }
        HistoryUpdate::Rewrite(rewrite) => run_rewrite(store, scrubs, rewrite),
        HistoryUpdate::Flush(ack) => {
            let _ = ack.send(0);
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

/// A plain sidecar sync of a record whose rewrite (in the same document
/// frame) waits for a retry: that retry carries the record's whole state
/// and lands after it, so it takes the sync's annotations, rather than
/// putting older ones back. (While the rewrite hasn't landed, the sync
/// itself may be dropped by the store's frame guard.)
fn fold_into_retries(retries: &mut [Retry], record: &CaptureRecord) {
    for (_, rw) in retries.iter_mut() {
        let job = &mut rw.job.record;
        if job.id == record.id && !job.redacted && job.geometry_id == record.geometry_id {
            job.annotation_model = record.annotation_model.clone();
        }
    }
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
    // Redactions a crash or quit left half-written (phase one only):
    // finished before anything else is written, so no record stays
    // withheld, and none ever lost its image.
    store.finish_pending_scrubs();
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
            match msg {
                HistoryUpdate::Flush(ack) => {
                    let _ = ack.send(retries.len());
                    continue;
                }
                HistoryUpdate::Write(ref record, _) => fold_into_retries(&mut retries, record),
                HistoryUpdate::Rewrite(_) => {}
            }
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
            Some(tx) => match tx.send(HistoryUpdate::Write(
                Box::new(record.clone()),
                Some(self.scrubs.writes.begin(record.id)),
            )) {
                Ok(()) => return,
                Err(std::sync::mpsc::SendError(msg)) => match msg {
                    HistoryUpdate::Write(record, _) => *record,
                    HistoryUpdate::Rewrite(_) | HistoryUpdate::Flush(_) => return,
                },
            },
            None => record,
        };
        if let Err(e) = self.update_history(&record) {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "history update failed: {e}");
        }
    }

    /// Replace `record` in History: its PNG becomes `bake` rendered onto
    /// `base` (`base` as is when `bake` is empty). A scrub's stored
    /// redactions are `record.redactions` (those already in `base`) plus
    /// the redactions in `bake`; its thumbnail is
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
        record: CaptureRecord,
        base: Arc<RgbaImage>,
        bake: Vec<readshot_core::Annotation>,
    ) -> iced::futures::channel::oneshot::Receiver<HistoryRewriteResult> {
        self.rewrite_history_as(None, record, base, bake)
    }

    /// [`Self::rewrite_history_async`] for the editor session `owner`.
    /// A restore (`redacted: false`) only ever replaces a redacted
    /// record when `owner` requested that record's newest scrub (it is
    /// undoing its own redaction). Any other restore of a record with a
    /// scrub on record is dropped here, unwritten; one of a record nobody
    /// here scrubbed is dropped at commit if the record is redacted on
    /// disk. A late writer (an editor closed mid-compose, a second editor
    /// of the same record) thus never un-redacts.
    pub fn rewrite_history_as(
        &self,
        owner: Option<u64>,
        mut record: CaptureRecord,
        base: Arc<RgbaImage>,
        bake: Vec<readshot_core::Annotation>,
    ) -> iced::futures::channel::oneshot::Receiver<HistoryRewriteResult> {
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        let Some(store) = &self.history else {
            return rx;
        };
        let id = record.id;
        // Outstanding from before its sequence number is taken, so a
        // settle never makes a request it hasn't seen stale
        // ([`Self::settle_record_writes`]). Dropped (done) on every early
        // return below.
        let pending = self.scrubs.writes.begin(id);
        if record.redacted {
            // Redactions the caller says are already in `base` (a
            // reopened scrubbed record's, mapped into this frame), then
            // the ones baked now, then a cover over every magnifier bubble
            // baked now that shows any of them: the stored image has that
            // magnified copy of weakly hidden text in it, and recognition
            // of a reopened record would read it.
            // Redactions already in `base`: the image holds them baked.
            let already_redacted = !record.redactions.is_empty();
            // Where the caller's editor says these redactions are in the
            // other frames it knows (kept for phase one).
            let other_frames = record
                .pending_image
                .take()
                .map(|p| p.other_frames)
                .unwrap_or_default();
            let shown: Vec<_> = record.redactions.iter().chain(&bake).cloned().collect();
            let magnified = readshot_core::scrub::magnified_redactions(&shown);
            record
                .redactions
                .extend(bake.iter().filter(|a| a.is_redaction()).cloned());
            record.redactions.extend(magnified);
            // What this scrub draws (its redactions and every other
            // annotation baked with them), in the document frame before
            // any crop: should its image never land, phase one's record of
            // what to draw onto the stored image (`HistoryStore::begin_scrub`).
            record.pending_image = Some(Box::new(readshot_core::PendingImage {
                geometry_id: record.geometry_id.clone(),
                width: base.width(),
                height: base.height(),
                bake: bake
                    .iter()
                    .filter(|a| !matches!(a, readshot_core::Annotation::Crop { .. }))
                    .cloned()
                    .collect(),
                already_redacted,
                other_frames,
                ..Default::default()
            }));
        }
        // The stored image is `bake` rendered onto `base`: a baked Crop
        // makes it the crop. The record then describes that image, not the
        // document it was cut from: its size, its redactions in its own px
        // (OCR of the stored image is masked with them), and a frame id of
        // its own, so OCR of the uncropped document (the capture's, or the
        // open editor's) never lands on it masked with boxes in another
        // frame.
        let crop = readshot_core::render::output_crop(base.width(), base.height(), &bake);
        match crop {
            Some(crop) => {
                record.width_px = crop.width;
                record.height_px = crop.height;
                if record.redacted {
                    record.redactions =
                        readshot_core::scrub::redactions_in_crop(&record.redactions, crop);
                    record.geometry_id =
                        Some(cropped_frame_id(record.geometry_id.as_deref(), crop));
                }
            }
            None => {
                record.width_px = base.width();
                record.height_px = base.height();
                if record.redacted {
                    // Never a stored redaction outside the stored image.
                    record.redactions = readshot_core::scrub::redactions_in_image(
                        &record.redactions,
                        base.width(),
                        base.height(),
                    );
                }
            }
        }
        if !record.redacted {
            // A restore (rare). Under the commit gate, then the state
            // lock (the documented order), so it can't interleave with a
            // commit: every trace of a scrub — the secured cache, parked
            // and unsecured entries — goes in one step, after any commit
            // already holding the gate has finished.
            let mut b = lock(&self.scrubs.book);
            let mut st = lock(&self.scrubs.state);
            let may_unredact = match st.scrub_owners.get(&id) {
                Some(scrubber) if *scrubber != owner => {
                    tracing::warn!(
                        target: readshot_core::log::cat::HISTORY,
                        "dropped a restore of record {id}: another editor redacted it"
                    );
                    let _ = tx.send(Err(HistoryRewriteError::RedactedElsewhere));
                    return rx;
                }
                Some(_) => true,
                None => false,
            };
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
                may_unredact,
            };
            return self.queue_rewrite(job, pending, tx, rx);
        }
        // A scrub. Bookkeeping only (no I/O): the UI thread never waits
        // on a write here unless phase one is actually needed.
        let (seq, phase_one) = {
            let mut st = lock(&self.scrubs.state);
            let seq = next_rewrite_seq();
            st.latest.insert(id, seq);
            st.scrub_owners.insert(id, owner);
            let phase_one = !already_secured(st.secured.get(&id), &record);
            (seq, phase_one)
        };
        let job = RewriteJob {
            seq,
            record: record.clone(),
            base,
            bake,
            may_unredact: false,
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
                        target: readshot_core::log::cat::HISTORY,
                        "history scrub (phase one) failed: {e}"
                    );
                    // On-disk state unknown: phase one is redone next time.
                    lock(&self.scrubs.state).secured.remove(&id);
                    b.unsecured.insert(id, (seq, record));
                }
            }
        }
        self.queue_rewrite(job, pending, tx, rx)
    }

    /// A capture whose History save failed: it will never be on disk, so
    /// whatever was parked for its save is dropped (each holds a document
    /// image), and any rewrite still queued for it is made stale, so it
    /// can't park again.
    pub fn forget_unsaved_record(&self, id: readshot_core::Uuid) {
        let mut b = lock(&self.scrubs.book);
        let mut st = lock(&self.scrubs.state);
        st.latest.insert(id, next_rewrite_seq());
        st.secured.remove(&id);
        st.scrub_owners.remove(&id);
        drop(st);
        b.parked.remove(&id);
        b.unsecured.remove(&id);
    }

    /// How many rewrites wait for a record's save (tests).
    #[cfg(test)]
    pub(crate) fn parked_rewrites(&self) -> usize {
        lock(&self.scrubs.book).parked.len()
    }

    /// The barrier every editor open of History record `id` passes
    /// before it reads the record (`history_ops::load_after_writes`).
    /// Blocks, with no timeout, until every History write queued for `id`
    /// so far is done: landed, stale, refused, or failed. Call it off the
    /// UI thread; with nothing outstanding it returns at once.
    ///
    /// **Invariant: a write of `id` queued before this call never lands
    /// after it returns.** Each such write has either landed already, or
    /// is cancelled here for good. This is the one place that enforces
    /// it: under the commit gate, with nothing outstanding, the record's
    /// newest request sequence is bumped, so a failed rewrite still
    /// waiting for its retry (its reason is in `failures`) is stale and
    /// its retry exits without writing, and one mid-commit has already
    /// finished (commits hold the gate). So an editor opened on what this
    /// returns is never overwritten by an earlier editor's late write,
    /// and a restore that failed never un-redacts a record opened
    /// redacted afterwards.
    ///
    /// Nothing is lost by a cancel: no write ever deletes an image before
    /// its replacement is in place, so a cancelled redaction leaves the
    /// record withheld with its image set aside, and the open finishes it
    /// from there ([`Self::finish_pending_scrub`]). Who owned the record's
    /// scrub is forgotten too: from here on only what is on disk counts.
    ///
    /// A redaction whose first phase (hiding the unredacted image) is
    /// still pending is retried here, at once; if it still can't be
    /// written, `exposed` says the record must not be opened.
    pub fn settle_record_writes(&self, id: readshot_core::Uuid) -> SettledWrites {
        loop {
            self.scrubs.writes.wait_idle(id);
            // Lock order: the commit gate, request state, then the tracker.
            let mut b = lock(&self.scrubs.book);
            let mut st = lock(&self.scrubs.state);
            let mut tracked = lock(&self.scrubs.writes.state);
            if tracked.outstanding.contains_key(&id) {
                continue; // queued meanwhile: wait for it too
            }
            let failures = tracked.failures.remove(&id).unwrap_or_default();
            drop(tracked);
            // Every earlier request is stale from here on.
            let seq = next_rewrite_seq();
            st.latest.insert(id, seq);
            st.scrub_owners.remove(&id);
            // Fail closed: phase one runs again for the next scrub.
            st.secured.remove(&id);
            drop(st);
            let mut exposed = false;
            if let Some(entry) = b.unsecured.get_mut(&id) {
                // Still the record's newest request: kept retrying.
                entry.0 = seq;
                let record = entry.1.clone();
                let secured = self
                    .history
                    .as_ref()
                    .map(|store| store.begin_scrub(&record));
                match secured {
                    Some(Ok(found)) => {
                        b.unsecured.remove(&id);
                        if found {
                            self.scrubs.mark_secured(id, seq, &record);
                        }
                    }
                    _ => exposed = true,
                }
            }
            return SettledWrites { failures, exposed };
        }
    }

    /// Finish record `id`'s redaction whose own image never landed (see
    /// [`HistoryStore::finish_pending_scrub`]): its pending redactions
    /// are drawn onto the image set aside, which is then swapped in.
    /// Blocking (a decode, render and encode): call it off the UI thread,
    /// after [`Self::settle_record_writes`]. Whether it finished one.
    pub fn finish_pending_scrub(&self, id: readshot_core::Uuid) -> bool {
        let Some(store) = &self.history else {
            return false;
        };
        store.finish_pending_scrub(id).unwrap_or_else(|e| {
            tracing::warn!(
                target: readshot_core::log::cat::HISTORY,
                "couldn't finish the pending redaction of record {id}: {e}"
            );
            false
        })
    }

    /// Whether any History write of record `id` is queued or being
    /// written right now (never blocks).
    pub fn record_writes_outstanding(&self, id: readshot_core::Uuid) -> bool {
        self.scrubs.writes.is_outstanding(id)
    }

    /// Flush the writer, then wait until no failed rewrite waits for a
    /// retry (tests: every write has drained).
    #[cfg(test)]
    pub(crate) fn drain_history_writes(&self) {
        let Some(tx) = &self.history_update_tx else {
            return;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
            if tx.send(HistoryUpdate::Flush(ack_tx)).is_err() {
                return;
            }
            if ack_rx.recv().unwrap_or(0) == 0 {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "History writes never drained"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Hand a rewrite to the writer thread (or write it inline when there
    /// is none) and return its receiver.
    fn queue_rewrite(
        &self,
        job: RewriteJob,
        pending: PendingWrite,
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
            pending: Some(pending),
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
                    target: readshot_core::log::cat::HISTORY,
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
    /// Off (or no store) → `Ok`, nothing written. Other policies → save
    /// and apply retention. A failed save is the error (the capture is
    /// not in History); a failed retention pass after a good save is
    /// only logged.
    pub async fn record_history(
        &self,
        record: CaptureRecord,
        png_bytes: Vec<u8>,
        policy: HistoryRetention,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), readshot_core::HistoryError> {
        if matches!(policy, HistoryRetention::Off) {
            return Ok(());
        }
        let Some(history) = &self.history else {
            return Ok(());
        };
        // `save` writes the PNG, decodes it again to render a
        // thumbnail, and fsyncs — synchronous fs/CPU work that would
        // otherwise stall a tokio worker for the duration.
        let history = Arc::clone(history);
        let scrubs = Arc::clone(&self.scrubs);
        let join = tokio::task::spawn_blocking(move || {
            if let Err(e) = save_capture(history.as_ref(), &scrubs, &record, &png_bytes) {
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "history save failed: {e}");
                return Err(e);
            }
            if let Err(e) = history.apply_retention(policy, now) {
                tracing::warn!(target: readshot_core::log::cat::HISTORY, "retention apply failed: {e}");
            }
            Ok(())
        })
        .await;
        join.unwrap_or_else(|e| {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "history save worker failed: {e}");
            Err(readshot_core::HistoryError::Io(format!(
                "the history save worker failed: {e}"
            )))
        })
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
                Ok(()) if scrubbed.redacted => scrubs.mark_landed(record.id, job.seq, &scrubbed),
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

    #[test]
    fn a_scrub_stores_its_redactions_clipped_to_the_stored_image() {
        use readshot_core::{Annotation, RectLike};
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let fs_store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let coord = history_coord(fs_store.clone());
        let blur = |rect| Annotation::Blur { rect, radius: 2.0 };
        let mut s = scrubbed(&record);
        s.ocr_text = None;
        drop(coord.rewrite_history_async(
            s,
            Arc::new(white(8, 8)),
            vec![
                // Straddles the right edge: clipped.
                blur(RectLike::new(6.0, 1.0, 5.0, 2.0)),
                // Wholly outside: dropped.
                blur(RectLike::new(20.0, 20.0, 4.0, 4.0)),
            ],
        ));
        coord.flush_history_updates();
        let stored = sidecar(&fs_store, &record);
        assert_eq!(
            stored.redactions,
            vec![blur(RectLike::new(6.0, 1.0, 2.0, 2.0))]
        );
        // So the legacy misplaced-redaction check never fires on it.
        let mut again = stored.clone();
        assert!(!readshot_core::history::migrate_legacy_record(&mut again));
    }

    #[test]
    fn cropped_frame_ids_are_stable_known_values() {
        let crop = readshot_core::render::OutputCrop {
            x: 20.0,
            y: 0.0,
            width: 20,
            height: 20,
        };
        // Persisted ids: these must never change between builds.
        assert_eq!(cropped_frame_id(None, crop), "crop3:3268992da8e9215b");
        assert_eq!(
            cropped_frame_id(Some("frame-a"), crop),
            "crop3:0289caee4b31ebee"
        );
        assert_ne!(
            cropped_frame_id(Some("frame-a"), crop),
            cropped_frame_id(Some("frame-b"), crop)
        );
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
                may_unredact: false,
            },
            done: None,
            attempt: 0,
            pending: None,
        }))
    }

    fn tags(batch: &[HistoryUpdate]) -> Vec<String> {
        batch
            .iter()
            .map(|m| match m {
                HistoryUpdate::Write(r, _) => format!("W:{}", r.ocr_text.as_deref().unwrap()),
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
            HistoryUpdate::Write(Box::new(tagged(a, "w1")), None),
            rewrite(a, "r1"),
            HistoryUpdate::Write(Box::new(tagged(b, "b1")), None),
            HistoryUpdate::Write(Box::new(tagged(a, "w2")), None),
            HistoryUpdate::Write(Box::new(tagged(a, "w3")), None),
            HistoryUpdate::Flush(ack),
            HistoryUpdate::Write(Box::new(tagged(b, "b2")), None),
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
        scrubbed.redactions_complete = true;
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
        n.try_update(
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
        fn sweep_staging(&self) {
            self.inner.sweep_staging()
        }
        fn finish_pending_scrub(
            &self,
            id: readshot_core::Uuid,
        ) -> Result<bool, readshot_core::HistoryError> {
            self.inner.finish_pending_scrub(id)
        }
        fn finish_pending_scrubs(&self) -> usize {
            self.inner.finish_pending_scrubs()
        }
        fn stored_redacted(&self, r: &CaptureRecord) -> Result<bool, readshot_core::HistoryError> {
            self.inner.stored_redacted(r)
        }
        fn record_recognized_text(
            &self,
            r: &CaptureRecord,
            doc: &readshot_core::text_lines::TextDocument,
            fill_unredacted: bool,
        ) -> Result<(), readshot_core::HistoryError> {
            self.inner.record_recognized_text(r, doc, fill_unredacted)
        }
        fn contains(&self, r: &CaptureRecord) -> bool {
            self.inner.contains(r)
        }
        fn delete(&self, id: readshot_core::Uuid) -> Result<(), readshot_core::HistoryError> {
            self.inner.delete(id)
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

    /// `r` scrubbed by the editor from a never-scrubbed capture: its
    /// redaction list is complete, so it holds masked text.
    fn scrubbed(r: &CaptureRecord) -> CaptureRecord {
        let mut s = r.clone();
        s.redactions_complete = true;
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

    /// A capture whose save failed: what was parked for it goes.
    #[test]
    fn forgetting_an_unsaved_record_drops_what_waits_for_its_save() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let coord = history_coord(store);
        let record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::new(white(8, 8)), pixelate()));
        coord.flush_history_updates();
        assert_eq!(coord.parked_rewrites(), 1);
        coord.forget_unsaved_record(record.id);
        assert_eq!(coord.parked_rewrites(), 0);
    }

    /// A restore (unredacted rewrite) never replaces a redacted record
    /// unless it comes from the editor session whose scrub it undoes:
    /// another session's late restore is dropped, and so is one with no
    /// known owner over a record that is redacted on disk.
    #[test]
    fn only_the_scrubbing_session_may_restore_a_redacted_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let coord = history_coord(store.clone());
        let base = Arc::new(white(8, 8));
        let record = CaptureRecord::new(chrono::Utc::now(), 8, 8, "d");
        store
            .save(&record, &readshot_core::encode_png(&base).unwrap())
            .unwrap();
        let (a, b) = (Some(1), Some(2));
        drop(coord.rewrite_history_as(b, scrubbed(&record), Arc::clone(&base), pixelate()));
        coord.flush_history_updates();
        assert!(sidecar(&store, &record).redacted);
        // Session A (not the scrub's owner) restores: refused.
        let mut refused =
            coord.rewrite_history_as(a, record.clone(), Arc::clone(&base), Vec::new());
        coord.flush_history_updates();
        assert!(
            sidecar(&store, &record).redacted,
            "another session un-redacted it"
        );
        assert_eq!(
            refused.try_recv(),
            Ok(Some(Err(HistoryRewriteError::RedactedElsewhere)))
        );
        // A restart: nobody owns the scrub on disk; a restore is refused.
        let fresh = history_coord(store.clone());
        let mut refused =
            fresh.rewrite_history_as(b, record.clone(), Arc::clone(&base), Vec::new());
        fresh.flush_history_updates();
        assert!(
            sidecar(&store, &record).redacted,
            "an ownerless restore un-redacted it"
        );
        assert_eq!(
            refused.try_recv(),
            Ok(Some(Err(HistoryRewriteError::RedactedElsewhere)))
        );
        // B, whose scrub it is, undoes it: allowed.
        drop(coord.rewrite_history_as(b, record.clone(), Arc::clone(&base), Vec::new()));
        coord.flush_history_updates();
        assert!(
            !sidecar(&store, &record).redacted,
            "the owner's undo restores"
        );
        // Unredacted on disk now: any session may rewrite its frame.
        drop(coord.rewrite_history_as(a, record.clone(), Arc::clone(&base), Vec::new()));
        coord.flush_history_updates();
        assert!(!sidecar(&store, &record).redacted);
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
            .await
            .unwrap();
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
            .await
            .unwrap();
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
            .await
            .unwrap();
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
        // Synchronous phase one: withheld (no text served, no image at
        // its path).
        let stored = sidecar(&fs_store, &record);
        assert_eq!(stored.ocr_text.as_deref(), None, "withheld: no text served");
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
        assert_eq!(stored.ocr_text.as_deref(), None, "withheld: no text served");
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
            may_unredact: false,
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
            may_unredact: false,
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
        assert_eq!(
            sidecar(&store, &record).ocr_text.as_deref(),
            None,
            "withheld: no text served"
        );
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
        drop_superseded_retries(
            &mut retries,
            &[HistoryUpdate::Write(Box::new(tagged(id, "w")), None)],
        );
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
        fn update(&self, r: &CaptureRecord) -> Result<(), readshot_core::HistoryError> {
            self.inner.update(r)
        }
        fn commit_save(
            &self,
            r: &CaptureRecord,
            staged: readshot_core::StagedImage,
        ) -> Result<(), readshot_core::HistoryError> {
            self.inner.commit_save(r, staged)
        }
        fn sweep_staging(&self) {
            self.inner.sweep_staging()
        }
        fn finish_pending_scrub(
            &self,
            id: readshot_core::Uuid,
        ) -> Result<bool, readshot_core::HistoryError> {
            self.inner.finish_pending_scrub(id)
        }
        fn finish_pending_scrubs(&self) -> usize {
            self.inner.finish_pending_scrubs()
        }
        fn stored_redacted(&self, r: &CaptureRecord) -> Result<bool, readshot_core::HistoryError> {
            self.inner.stored_redacted(r)
        }
        fn record_recognized_text(
            &self,
            r: &CaptureRecord,
            doc: &readshot_core::text_lines::TextDocument,
            fill_unredacted: bool,
        ) -> Result<(), readshot_core::HistoryError> {
            self.inner.record_recognized_text(r, doc, fill_unredacted)
        }
        fn contains(&self, r: &CaptureRecord) -> bool {
            self.inner.contains(r)
        }
        fn delete(&self, id: readshot_core::Uuid) -> Result<(), readshot_core::HistoryError> {
            self.inner.delete(id)
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
        assert_eq!(stored.ocr_text.as_deref(), None, "withheld: no text served");
        assert!(stored.redacted);
        coord.flush_history_updates();
    }

    #[test]
    fn the_same_redactions_in_another_frame_run_phase_one_again() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let fs_store = readshot_core::FsHistoryStore::new(dir.path().join("h"));
        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let store = Arc::new(BlockingStore {
            inner: readshot_core::FsHistoryStore::new(dir.path().join("h")),
            block_stage: std::sync::atomic::AtomicBool::new(false),
            block_commit: std::sync::atomic::AtomicBool::new(false),
            entered: std::sync::Mutex::new(entered_tx),
            release: std::sync::Mutex::new(release_rx),
        });
        let coord = history_coord(store.clone());
        let base = Arc::new(white(8, 8));
        // Frame A: secured and landed.
        drop(coord.rewrite_history_async(scrubbed(&record), Arc::clone(&base), pixelate()));
        coord.flush_history_updates();
        assert_eq!(sidecar(&fs_store, &record).geometry_id, None);
        // Frame B, the same redaction values: phase two is held up, so
        // only phase one can change what History serves.
        store
            .block_stage
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut framed = scrubbed(&record);
        framed.geometry_id = Some("n:rot=2".into());
        drop(coord.rewrite_history_async(framed, Arc::clone(&base), pixelate()));
        entered
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(
            sidecar(&fs_store, &record).geometry_id.as_deref(),
            Some("n:rot=2"),
            "phase one ran for the new frame"
        );
        let _ = release.send(());
        coord.flush_history_updates();
    }

    fn flaky(dir: &std::path::Path, fail_rewrites: usize) -> Arc<FlakyStore> {
        Arc::new(FlakyStore {
            inner: readshot_core::FsHistoryStore::new(dir.join("h")),
            fail_rewrites: std::sync::atomic::AtomicUsize::new(fail_rewrites),
            fail_begins: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Longer than any retry backoff a test sees (10 ms, then 20 ms…).
    fn past_the_retry() {
        std::thread::sleep(std::time::Duration::from_millis(250));
    }

    /// Fix-3 review I-1: a frame rewrite fails once, a reopen waits and
    /// is told so, and the new editor writes to what History has. The
    /// failed write's retry must never land after that: it would put the
    /// old editor's state back over the new editor's edits (and every
    /// later sync of the new editor would be dropped by the frame guard).
    #[test]
    fn a_failed_write_a_reopen_was_told_about_never_lands_after_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = flaky(dir.path(), 1);
        let coord = history_coord(store.clone());
        // Editor A rotates: its frame rewrite fails once.
        let mut rotated = record.clone();
        rotated.geometry_id = Some("rotated".into());
        drop(coord.rewrite_history_as(Some(1), rotated, Arc::new(white(6, 4)), Vec::new()));
        let failures = coord.settle_record_writes(record.id).failures;
        assert!(!failures.is_empty(), "the reopen is told the write failed");
        // Editor B opens what History has, and annotates it.
        let mut b = sidecar(&store.inner, &record);
        assert_eq!(
            (b.width_px, b.height_px, b.geometry_id.clone()),
            (8, 8, None)
        );
        b.annotation_model = pixelate();
        coord.update_history_async(b);
        past_the_retry();
        coord.flush_history_updates();
        let now = sidecar(&store.inner, &record);
        assert_eq!(
            (now.width_px, now.height_px, now.geometry_id.clone()),
            (8, 8, None),
            "A's retry landed after the reopen"
        );
        assert_eq!(now.annotation_model.len(), 1, "B's edit was overwritten");
    }

    /// Fix-3 review I-1, privacy: editor A redacts, then undoes it; the
    /// restore fails once. The reopen opens the redacted copy (and says
    /// the last edits weren't saved). A's retried restore must never
    /// un-redact History after that.
    #[test]
    fn a_failed_restore_a_reopen_was_told_about_never_unredacts_after_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = flaky(dir.path(), 0);
        let coord = history_coord(store.clone());
        let base = Arc::new(white(8, 8));
        let a = Some(1);
        drop(coord.rewrite_history_as(a, scrubbed(&record), Arc::clone(&base), pixelate()));
        coord.flush_history_updates();
        assert!(sidecar(&store.inner, &record).redacted);
        // A undoes its redaction; the restore fails once.
        store
            .fail_rewrites
            .store(1, std::sync::atomic::Ordering::SeqCst);
        drop(coord.rewrite_history_as(a, record.clone(), Arc::clone(&base), Vec::new()));
        let failures = coord.settle_record_writes(record.id).failures;
        assert!(!failures.is_empty());
        // B opens the redacted copy.
        assert!(sidecar(&store.inner, &record).redacted);
        past_the_retry();
        coord.flush_history_updates();
        assert!(
            sidecar(&store.inner, &record).redacted,
            "A's retried restore un-redacted a record B opened redacted"
        );
    }

    /// A redaction whose image write failed is cancelled by a settle like
    /// any other write, and nothing is lost: phase one only set the image
    /// aside (the record is withheld meanwhile), so the open finishes the
    /// redaction from it.
    #[test]
    fn a_settle_cancels_a_failed_redaction_and_its_image_is_kept() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = flaky(dir.path(), 1);
        let coord = history_coord(store.clone());
        drop(coord.rewrite_history_as(
            Some(1),
            scrubbed(&record),
            Arc::new(white(8, 8)),
            pixelate(),
        ));
        let settled = coord.settle_record_writes(record.id);
        assert!(!settled.exposed);
        assert_eq!(settled.failures, ["history io: disk full"]);
        assert!(png_on_disk(&store.inner, &record).is_none(), "withheld");
        coord.drain_history_writes();
        assert!(sidecar(&store.inner, &record).scrub_pending, "cancelled");
        assert!(coord.finish_pending_scrub(record.id));
        let now = sidecar(&store.inner, &record);
        assert!(now.redacted && !now.scrub_pending);
        let png = png_on_disk(&store.inner, &record).expect("the image is kept");
        assert_eq!(png.get_pixel(1, 1).0, [0, 0, 0, 255], "redacted");
    }

    /// An editor rotates (the frame rewrite fails once and waits for its
    /// retry), then annotates: that sync is for the new frame, which isn't
    /// stored yet, so the store drops it. The retry must carry the
    /// annotation, not put the record back without it.
    #[test]
    fn a_retried_rewrite_keeps_the_syncs_made_after_it() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = flaky(dir.path(), 1);
        let coord = history_coord(store.clone());
        let mut rotated = record.clone();
        rotated.geometry_id = Some("rotated".into());
        drop(coord.rewrite_history_as(Some(1), rotated.clone(), Arc::new(white(6, 4)), Vec::new()));
        let mut annotated = rotated;
        annotated.width_px = 6;
        annotated.height_px = 4;
        annotated.annotation_model = pixelate();
        coord.update_history_async(annotated);
        coord.drain_history_writes();
        let now = sidecar(&store.inner, &record);
        assert_eq!(now.geometry_id.as_deref(), Some("rotated"));
        assert_eq!(
            now.annotation_model.len(),
            1,
            "the retry dropped the later sync"
        );
    }

    /// Fix-4 review N-2: the same editor redacts, undoes it, then rotates
    /// before the restore lands. The rotate is that editor's own change to
    /// a record only it redacted: it lands (unredacted, rotated), and the
    /// capture never loses its image.
    #[test]
    fn a_frame_change_after_an_undone_redaction_lands_with_its_image() {
        for i in 0..20 {
            let dir = tempfile::TempDir::new().unwrap();
            let record = saved_record(dir.path());
            let store = flaky(dir.path(), 0);
            let coord = history_coord(store.clone());
            let base = Arc::new(white(8, 8));
            let a = Some(1);
            drop(coord.rewrite_history_as(a, scrubbed(&record), Arc::clone(&base), pixelate()));
            drop(coord.rewrite_history_as(a, record.clone(), Arc::clone(&base), Vec::new()));
            let mut rotated = record.clone();
            rotated.geometry_id = Some("rotated".into());
            drop(coord.rewrite_history_as(a, rotated, Arc::new(white(6, 4)), Vec::new()));
            coord.flush_history_updates();
            let now = sidecar(&store.inner, &record);
            assert!(
                png_on_disk(&store.inner, &record).is_some(),
                "run {i}: the capture lost its image"
            );
            assert_eq!(
                (
                    now.geometry_id.as_deref(),
                    now.width_px,
                    now.height_px,
                    now.redacted
                ),
                (Some("rotated"), 6, 4, false),
                "run {i}: the rotate didn't land"
            );
        }
    }

    /// A redaction left half-written (phase one done, its image never
    /// written: a crash or quit) is finished at the next launch: the
    /// pending redactions are drawn onto the capture's image and swapped
    /// in. No image is lost and no unredacted pixel is served.
    #[test]
    fn a_launch_finishes_a_redaction_left_half_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let fs_store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut half = scrubbed(&record);
        half.redactions = pixelate();
        assert!(fs_store.begin_scrub(&half).unwrap());
        assert!(sidecar(&fs_store, &record).scrub_pending);
        // The next launch.
        let coord = history_coord(fs_store.clone());
        coord.drain_history_writes();
        let now = sidecar(&fs_store, &record);
        assert!(now.redacted && !now.scrub_pending, "{now:?}");
        let png = png_on_disk(&fs_store, &record).expect("the capture lost its image");
        assert_eq!(png.dimensions(), (8, 8));
        assert_eq!(
            png.get_pixel(1, 1).0,
            [0, 0, 0, 255],
            "the redaction is drawn"
        );
        assert_eq!(
            png.get_pixel(6, 6).0,
            [255, 255, 255, 255],
            "only the redaction"
        );
        assert!(
            readshot_core::FsHistoryStore::abs_thumbnail_path(fs_store.root(), &record).exists()
        );
    }

    /// Fix-5 review I-1: a capture already redacted in History is rotated
    /// by its editor (a scrub in the new frame, nothing new redacted); the
    /// write fails, and the capture is reopened. Its image was already
    /// safe: it is put back as it was, never blacked out.
    #[test]
    fn a_failed_frame_change_of_a_redacted_capture_keeps_its_image() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = flaky(dir.path(), 0);
        let coord = history_coord(store.clone());
        drop(coord.rewrite_history_as(
            Some(1),
            scrubbed(&record),
            Arc::new(white(8, 8)),
            pixelate(),
        ));
        coord.flush_history_updates();
        let landed = png_on_disk(&store.inner, &record).unwrap();
        assert_eq!(landed.get_pixel(6, 6).0, [255, 255, 255, 255]);
        store
            .fail_rewrites
            .store(1000, std::sync::atomic::Ordering::SeqCst);
        let mut rotated = sidecar(&store.inner, &record);
        rotated.geometry_id = Some("rotated".into());
        drop(coord.rewrite_history_as(Some(2), rotated, Arc::new(white(8, 8)), Vec::new()));
        coord.settle_record_writes(record.id);
        assert!(coord.finish_pending_scrub(record.id));
        let now = sidecar(&store.inner, &record);
        assert!(now.redacted && !now.scrub_pending);
        let png = png_on_disk(&store.inner, &record).unwrap();
        assert_eq!(png, landed, "the capture's image changed");
    }

    /// Fix-5 review M-4: a redaction finished from its set-aside image
    /// bakes what the scrub itself would have (the editor's other
    /// annotations too, as every scrub does), so no edit is dropped.
    #[test]
    fn a_finished_redaction_keeps_the_editors_other_annotations() {
        let dir = tempfile::TempDir::new().unwrap();
        let record = saved_record(dir.path());
        let store = flaky(dir.path(), 1000);
        let coord = history_coord(store.clone());
        let mut bake = pixelate();
        // An outline: not a redaction.
        let outline = readshot_core::Annotation::Rectangle {
            rect: readshot_core::RectLike::new(4.0, 4.0, 3.0, 3.0),
            color: readshot_core::Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: readshot_core::ShapeStyle::default(),
        };
        assert!(!outline.is_redaction());
        bake.push(outline);
        drop(coord.rewrite_history_as(Some(1), scrubbed(&record), Arc::new(white(8, 8)), bake));
        coord.settle_record_writes(record.id);
        assert!(coord.finish_pending_scrub(record.id));
        let png = png_on_disk(&store.inner, &record).unwrap();
        assert_eq!(png.get_pixel(1, 1).0, [0, 0, 0, 255], "the redaction");
        assert_ne!(
            png.get_pixel(4, 5).0,
            [255, 255, 255, 255],
            "the other annotation"
        );
        assert_eq!(png.get_pixel(7, 0).0, [255, 255, 255, 255]);
    }

    /// Lands a redaction (`bake`) of a fresh 8×8 white record, then makes
    /// every later rewrite fail.
    fn landed_then_failing(
        dir: &std::path::Path,
        bake: Vec<readshot_core::Annotation>,
    ) -> (CaptureRecord, Arc<FlakyStore>, CaptureCoordinator) {
        let record = saved_record(dir);
        let store = flaky(dir, 0);
        let coord = history_coord(store.clone());
        drop(coord.rewrite_history_as(Some(1), scrubbed(&record), Arc::new(white(8, 8)), bake));
        coord.flush_history_updates();
        store
            .fail_rewrites
            .store(1000, std::sync::atomic::Ordering::SeqCst);
        (record, store, coord)
    }

    fn solid(x: f32, y: f32, w: f32, h: f32) -> readshot_core::Annotation {
        readshot_core::Annotation::Rectangle {
            rect: readshot_core::RectLike::new(x, y, w, h),
            color: readshot_core::Rgba::OPAQUE_BLACK,
            line_width: 0.0,
            style: readshot_core::ShapeStyle {
                fill: readshot_core::FillMode::Filled,
                ..readshot_core::ShapeStyle::default()
            },
        }
    }

    fn black_pixels(img: &image::RgbaImage) -> usize {
        img.pixels().filter(|p| p.0 == [0, 0, 0, 255]).count()
    }

    /// Fix-6 review P2: a redaction and a crop landed; a later edit by the
    /// same session (an outline) fails, and the capture is reopened. Its
    /// redaction is already in the stored (cropped) image: it is put back
    /// as it was, never blacked out.
    #[test]
    fn a_failed_edit_after_a_landed_crop_keeps_the_image() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut bake = pixelate();
        bake.push(readshot_core::Annotation::Crop {
            rect: readshot_core::RectLike::new(0.0, 0.0, 7.0, 7.0),
        });
        let (record, store, coord) = landed_then_failing(dir.path(), bake.clone());
        let landed = png_on_disk(&store.inner, &record).unwrap();
        bake.push(readshot_core::Annotation::Rectangle {
            rect: readshot_core::RectLike::new(5.0, 5.0, 1.0, 1.0),
            color: readshot_core::Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: readshot_core::ShapeStyle::default(),
        });
        drop(coord.rewrite_history_as(Some(1), scrubbed(&record), Arc::new(white(8, 8)), bake));
        coord.settle_record_writes(record.id);
        assert!(coord.finish_pending_scrub(record.id));
        let png = png_on_disk(&store.inner, &record).unwrap();
        assert_eq!(png, landed, "{} of 49 pixels black", black_pixels(&png));
    }

    /// A genuinely new redaction next to one already baked is still
    /// enforced when its write fails: drawn where it is, the rest kept.
    #[test]
    fn a_new_redaction_next_to_a_baked_one_is_still_drawn() {
        let dir = tempfile::TempDir::new().unwrap();
        let (record, store, coord) = landed_then_failing(dir.path(), pixelate());
        let mut bake = pixelate();
        bake.push(solid(5.0, 5.0, 3.0, 3.0));
        drop(coord.rewrite_history_as(Some(1), scrubbed(&record), Arc::new(white(8, 8)), bake));
        coord.settle_record_writes(record.id);
        assert!(coord.finish_pending_scrub(record.id));
        let png = png_on_disk(&store.inner, &record).unwrap();
        assert_eq!(png.get_pixel(1, 1).0, [0, 0, 0, 255], "the baked one");
        assert_eq!(png.get_pixel(6, 6).0, [0, 0, 0, 255], "the new one");
        assert_eq!(png.get_pixel(4, 6).0, [255, 255, 255, 255], "nothing else");
    }

    /// Never stores anything: every image fails to stage.
    struct FailingStore;

    impl HistoryStore for FailingStore {
        fn save(&self, _: &CaptureRecord, _: &[u8]) -> Result<(), readshot_core::HistoryError> {
            Ok(())
        }
        fn list(&self) -> Result<Vec<CaptureRecord>, readshot_core::HistoryError> {
            Ok(Vec::new())
        }
        fn apply_retention(
            &self,
            _: HistoryRetention,
            _: chrono::DateTime<chrono::Utc>,
        ) -> Result<(), readshot_core::HistoryError> {
            Ok(())
        }
        fn clear_all(&self) -> Result<(), readshot_core::HistoryError> {
            Ok(())
        }
        fn update(&self, _: &CaptureRecord) -> Result<(), readshot_core::HistoryError> {
            Ok(())
        }
        fn stage_image(
            &self,
            _: &CaptureRecord,
            _: &[u8],
            _: Option<&[u8]>,
        ) -> Result<readshot_core::StagedImage, readshot_core::HistoryError> {
            Err(readshot_core::HistoryError::Io("disk full".into()))
        }
        fn commit_rewrite(
            &self,
            _: &CaptureRecord,
            _: readshot_core::StagedImage,
        ) -> Result<bool, readshot_core::HistoryError> {
            Ok(false)
        }
        fn commit_save(
            &self,
            _: &CaptureRecord,
            _: readshot_core::StagedImage,
        ) -> Result<(), readshot_core::HistoryError> {
            Ok(())
        }
        fn begin_scrub(&self, _: &CaptureRecord) -> Result<bool, readshot_core::HistoryError> {
            Ok(false)
        }
        fn sweep_staging(&self) {}
        fn stored_redacted(&self, _: &CaptureRecord) -> Result<bool, readshot_core::HistoryError> {
            Ok(false)
        }
        fn record_recognized_text(
            &self,
            _: &CaptureRecord,
            _: &readshot_core::text_lines::TextDocument,
            _: bool,
        ) -> Result<(), readshot_core::HistoryError> {
            Ok(())
        }
        fn contains(&self, _: &CaptureRecord) -> bool {
            false
        }
        fn delete(&self, _: readshot_core::Uuid) -> Result<(), readshot_core::HistoryError> {
            Ok(())
        }
    }

    /// Fix-3 review m-1: a write's failure is recorded before it stops
    /// being outstanding, so a wait that ends on it always sees it. This
    /// watches the tracker the way a waiter does, from the first moment
    /// the write is no longer outstanding (the race the review found
    /// showed in about 1% of real waits).
    #[test]
    fn a_failure_is_recorded_before_the_write_stops_being_outstanding() {
        let coord = history_coord(Arc::new(FailingStore));
        let base = Arc::new(white(1, 1));
        for i in 0..200 {
            let record = CaptureRecord::new(chrono::Utc::now(), 1, 1, "d");
            drop(coord.rewrite_history_as(None, record.clone(), Arc::clone(&base), Vec::new()));
            let seen = loop {
                let st = lock(&coord.scrubs.writes.state);
                if !st.outstanding.contains_key(&record.id) {
                    break st.failures.contains_key(&record.id);
                }
            };
            assert!(
                seen,
                "write {i} stopped being outstanding before its failure was recorded"
            );
        }
    }
}
