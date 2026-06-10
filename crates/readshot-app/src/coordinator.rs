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
    /// Rendezvous: acknowledged once everything queued before it has
    /// been written. Lets shutdown (and tests) wait for durability.
    Flush(std::sync::mpsc::SyncSender<()>),
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
                    while let Ok(msg) = rx.recv() {
                        match msg {
                            HistoryUpdate::Write(record) => {
                                if let Err(e) = store.update(&record) {
                                    tracing::warn!(
                                        target: "readshot::history",
                                        "background history update failed: {e}"
                                    );
                                }
                            }
                            HistoryUpdate::Flush(ack) => {
                                let _ = ack.send(());
                            }
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

    pub async fn recognise(&self, req: OCRRequest) -> Result<String, OCRError> {
        let result = self.ocr.recognise(req).await?;
        // Layout-aware reconstruction when the engine exposes per-line
        // bounding boxes (Apple Vision today); fall back to the
        // engine's flat newline-joined text otherwise. Then run the
        // result through the cleanup pass so trailing whitespace,
        // line endings, and blank-line runs are normalised.
        //
        // This is the single chokepoint — every consumer (overlay /
        // history sidecar / editor Copy Text) gets the same string.
        let text = if !result.lines.is_empty() {
            readshot_core::ocr_layout::reconstruct(&result.lines)
        } else {
            result.text
        };
        Ok(readshot_core::ocr_text::clean(&text))
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
                    HistoryUpdate::Flush(_) => return,
                },
            },
            None => record,
        };
        if let Err(e) = self.update_history(&record) {
            tracing::warn!(target: "readshot::history", "history update failed: {e}");
        }
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
}
