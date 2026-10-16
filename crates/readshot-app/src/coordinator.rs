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

/// A full record replacement for the background writer. The PNG is
/// `bake` rendered onto `base` (or `base` itself when `bake` is empty),
/// rendered and encoded on the writer thread, never the UI thread.
struct HistoryRewrite {
    record: CaptureRecord,
    base: Arc<RgbaImage>,
    bake: Vec<readshot_core::Annotation>,
    done: Option<iced::futures::channel::oneshot::Sender<HistoryRewriteResult>>,
}

impl HistoryUpdate {
    fn record_id(&self) -> Option<readshot_core::Uuid> {
        match self {
            Self::Write(r) => Some(r.id),
            Self::Rewrite(rw) => Some(rw.record.id),
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

/// Run one rewrite: render, encode, write, report. Called on the
/// writer thread (or the fallback path).
fn run_rewrite(store: &dyn HistoryStore, rewrite: HistoryRewrite) {
    let HistoryRewrite {
        mut record,
        base,
        bake,
        done,
    } = rewrite;
    let baked = (!bake.is_empty()).then(|| readshot_core::render(&base, &bake));
    let img = baked.as_ref().unwrap_or(&base);
    record.width_px = img.width();
    record.height_px = img.height();
    let result = readshot_core::encode_png(img)
        .map_err(|e| e.to_string())
        .and_then(|png| store.rewrite(&record, &png).map_err(|e| e.to_string()));
    if let Err(e) = &result {
        tracing::warn!(target: "readshot::history", "history rewrite failed: {e}");
    }
    if let Some(done) = done {
        let _ = done.send(result);
    }
}

fn run_update(store: &dyn HistoryStore, msg: HistoryUpdate) {
    match msg {
        HistoryUpdate::Write(record) => {
            if let Err(e) = store.update(&record) {
                tracing::warn!(
                    target: "readshot::history",
                    "background history update failed: {e}"
                );
            }
        }
        HistoryUpdate::Rewrite(rewrite) => run_rewrite(store, *rewrite),
        HistoryUpdate::Flush(ack) => {
            let _ = ack.send(());
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
        let history_update_tx = history.as_ref().and_then(|store| {
            let store = Arc::clone(store);
            let (tx, rx) = std::sync::mpsc::channel::<HistoryUpdate>();
            std::thread::Builder::new()
                .name("history-sync".into())
                .spawn(move || {
                    // Drain whatever queued up while the last write ran
                    // and coalesce it: only the newest rewrite of a
                    // record pays for a render + PNG encode.
                    while let Ok(first) = rx.recv() {
                        let mut batch = vec![first];
                        batch.extend(rx.try_iter());
                        for msg in coalesce(batch) {
                            run_update(store.as_ref(), msg);
                        }
                    }
                })
                .ok()
                .map(|_| tx)
        });
        Self {
            capturer,
            ocr,
            permissions,
            history,
            history_update_tx,
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

    /// Queue a full replacement of `record` on the ordered background
    /// writer: its PNG becomes `bake` rendered onto `base` (`base` as is
    /// when `bake` is empty), its thumbnail is regenerated and its
    /// sidecar written exactly as given (`ocr_text: None` clears the
    /// text). Rendering and encoding run on the writer thread, and a
    /// newer rewrite of the same record queued meanwhile supersedes this
    /// one (its receiver is then cancelled). The receiver resolves with
    /// the outcome; no history store → cancelled.
    pub fn rewrite_history_async(
        &self,
        record: CaptureRecord,
        base: Arc<RgbaImage>,
        bake: Vec<readshot_core::Annotation>,
    ) -> iced::futures::channel::oneshot::Receiver<HistoryRewriteResult> {
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        let Some(store) = &self.history else {
            return rx;
        };
        let msg = HistoryUpdate::Rewrite(Box::new(HistoryRewrite {
            record,
            base,
            bake,
            done: Some(tx),
        }));
        let msg = match &self.history_update_tx {
            Some(sender) => match sender.send(msg) {
                Ok(()) => return rx,
                Err(std::sync::mpsc::SendError(msg)) => msg,
            },
            None => msg,
        };
        // No writer thread: write inline rather than drop the scrub.
        run_update(store.as_ref(), msg);
        rx
    }

    /// Block until every history update queued before this call has
    /// been written. Bounded wait so a wedged disk can't hang the
    /// caller. Used on shutdown so a quit straight after an annotation
    /// edit can't lose the trailing write; tests use it to make the
    /// async writer deterministic.
    pub fn flush_history_updates(&self) {
        let Some(tx) = &self.history_update_tx else {
            return;
        };
        let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
        if tx.send(HistoryUpdate::Flush(ack_tx)).is_ok() {
            let _ = ack_rx.recv_timeout(std::time::Duration::from_secs(5));
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
        let join = tokio::task::spawn_blocking(move || {
            if let Err(e) = history.save(&record, &png_bytes) {
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
            record: tagged(id, tag),
            base: Arc::new(image::RgbaImage::new(1, 1)),
            bake: Vec::new(),
            done: None,
        }))
    }

    fn tags(batch: &[HistoryUpdate]) -> Vec<String> {
        batch
            .iter()
            .map(|m| match m {
                HistoryUpdate::Write(r) => format!("W:{}", r.ocr_text.as_deref().unwrap()),
                HistoryUpdate::Rewrite(rw) => {
                    format!("R:{}", rw.record.ocr_text.as_deref().unwrap())
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
}
