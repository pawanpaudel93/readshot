//! Opening a History record while earlier writes to it are still in
//! flight: every open waits for them, then reads the record from disk.

use super::*;

/// An `FsHistoryStore` whose image writes can be held (a stand-in for
/// the encode and write of a very large document) and whose rewrite
/// commits can be made to fail.
struct GatedStore {
    inner: FsHistoryStore,
    held: std::sync::Mutex<bool>,
    released: std::sync::Condvar,
    fail_commits: std::sync::atomic::AtomicUsize,
}

impl GatedStore {
    fn new(root: std::path::PathBuf) -> Self {
        Self {
            inner: FsHistoryStore::new(root),
            held: std::sync::Mutex::new(false),
            released: std::sync::Condvar::new(),
            fail_commits: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn hold(&self) {
        *self.held.lock().unwrap() = true;
    }

    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.released.notify_all();
    }

    fn fail_next_commits(&self, n: usize) {
        self.fail_commits
            .store(n, std::sync::atomic::Ordering::SeqCst);
    }

    fn stored(&self, id: readshot_core::Uuid) -> readshot_core::CaptureRecord {
        self.inner
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
    }
}

impl HistoryStore for GatedStore {
    fn save(
        &self,
        r: &readshot_core::CaptureRecord,
        png: &[u8],
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.save(r, png)
    }
    fn list(&self) -> Result<Vec<readshot_core::CaptureRecord>, readshot_core::HistoryError> {
        self.inner.list()
    }
    fn apply_retention(
        &self,
        p: readshot_core::HistoryRetention,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.apply_retention(p, now)
    }
    fn clear_all(&self) -> Result<(), readshot_core::HistoryError> {
        self.inner.clear_all()
    }
    fn update(&self, r: &readshot_core::CaptureRecord) -> Result<(), readshot_core::HistoryError> {
        self.inner.update(r)
    }
    fn stage_image(
        &self,
        r: &readshot_core::CaptureRecord,
        png: &[u8],
        thumb: Option<&[u8]>,
    ) -> Result<readshot_core::StagedImage, readshot_core::HistoryError> {
        let mut held = self.held.lock().unwrap();
        while *held {
            held = self.released.wait(held).unwrap();
        }
        drop(held);
        self.inner.stage_image(r, png, thumb)
    }
    fn commit_rewrite(
        &self,
        r: &readshot_core::CaptureRecord,
        staged: readshot_core::StagedImage,
    ) -> Result<bool, readshot_core::HistoryError> {
        let failing = self
            .fail_commits
            .try_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |v| v.checked_sub(1),
            )
            .is_ok();
        if failing {
            return Err(readshot_core::HistoryError::Io("disk full".into()));
        }
        self.inner.commit_rewrite(r, staged)
    }
    fn commit_save(
        &self,
        r: &readshot_core::CaptureRecord,
        staged: readshot_core::StagedImage,
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.commit_save(r, staged)
    }
    fn sweep_staging(&self) {
        self.inner.sweep_staging()
    }
    fn stored_redacted(
        &self,
        r: &readshot_core::CaptureRecord,
    ) -> Result<bool, readshot_core::HistoryError> {
        self.inner.stored_redacted(r)
    }
    fn begin_scrub(
        &self,
        r: &readshot_core::CaptureRecord,
    ) -> Result<bool, readshot_core::HistoryError> {
        self.inner.begin_scrub(r)
    }
    fn record_recognized_text(
        &self,
        r: &readshot_core::CaptureRecord,
        doc: &readshot_core::text_lines::TextDocument,
        fill: bool,
    ) -> Result<(), readshot_core::HistoryError> {
        self.inner.record_recognized_text(r, doc, fill)
    }
    fn contains(&self, r: &readshot_core::CaptureRecord) -> bool {
        self.inner.contains(r)
    }
    fn delete(&self, id: readshot_core::Uuid) -> Result<(), readshot_core::HistoryError> {
        self.inner.delete(id)
    }
}

/// An app with History (its window open) holding one 40×20 record.
fn history_app() -> (
    tempfile::TempDir,
    Arc<GatedStore>,
    App,
    readshot_core::CaptureRecord,
) {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("h");
    let store = Arc::new(GatedStore::new(root.clone()));
    let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
    app.history_root = Some(root);
    app.history_window_id = Some(iced::window::Id::unique());
    let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
    store
        .save(&record, &readshot_core::encode_png(&ramp(40, 20)).unwrap())
        .unwrap();
    app.history_records = store.list().unwrap();
    (dir, store, app, record)
}

/// Make an editor of `record` (as History has it) current.
fn edit_record(app: &mut App, record: &readshot_core::CaptureRecord) -> iced::window::Id {
    let mut ed = crate::editor::EditorSession::from_history(ramp(40, 20), record.clone());
    let id = iced::window::Id::unique();
    ed.window_id = Some(id);
    app.windows.register(id, WindowKind::Editor);
    app.editor = Some(ed);
    id
}

/// Run what the deferred open's task runs, off this thread.
fn start_load(app: &App, id: readshot_core::Uuid) -> std::sync::mpsc::Receiver<Message> {
    let (tx, rx) = std::sync::mpsc::channel();
    let coord = app.coordinator.clone();
    let root = app.history_root.clone();
    std::thread::spawn(move || {
        let (result, unsaved) = super::super::history_ops::load_after_writes(&coord, root, id);
        let _ = tx.send(Message::HistoryReopenedAfterSync {
            record_id: id,
            result,
            unsaved,
        });
    });
    rx
}

fn annotate_and_sync(app: &mut App) {
    let coord = app.coordinator.clone();
    let ed = app.editor.as_mut().unwrap();
    ed.model.commit_annotation(Annotation::Rectangle {
        rect: RectLike::new(2.0, 2.0, 5.0, 5.0),
        color: Rgba::OPAQUE_BLACK,
        line_width: 1.0,
        style: Default::default(),
    });
    super::super::editor_ops::sync_editor_history(ed, &coord);
}

/// Fix-3 review I-2: rotate, close the editor, then Open from History
/// while the rotate is still being written. The open must wait for that
/// write (and read the record from disk), not open the stale copy.
#[test]
fn opening_a_closed_editors_record_waits_for_its_writes() {
    let (_dir, store, mut app, record) = history_app();
    let a = edit_record(&mut app, &record);
    store.hold();
    let _ = update(&mut app, Message::EditorRotate { clockwise: true });
    let _ = update(&mut app, Message::WindowClosed(a));
    assert!(app.editor.is_none() && app.retiring_editors.is_empty());

    let _ = update(&mut app, Message::HistoryOpenInEditor(record.id));
    assert_eq!(
        app.awaiting_reopen,
        Some(record.id),
        "the open didn't wait for the closed editor's write"
    );
    let rx = start_load(&app, record.id);
    assert!(
        rx.recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "opened before the rotate was written"
    );
    store.release();
    let msg = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    let _ = update(&mut app, msg);
    assert_eq!(
        app.editor.as_ref().unwrap().image_size(),
        (20, 40),
        "B opens on the rotated record"
    );
    annotate_and_sync(&mut app);
    app.coordinator.flush_history_updates();
    let stored = store.stored(record.id);
    assert_eq!((stored.width_px, stored.height_px), (20, 40));
    assert_eq!(stored.annotation_model.len(), 1, "B's edit is stored");
}

/// Fix-3 review I-3: a second Open click while "Finishing…" shows joins
/// the open already waiting; it never starts a second, stale one or
/// cancels the right one.
#[test]
fn a_second_open_click_joins_the_waiting_open() {
    let (_dir, store, mut app, record) = history_app();
    edit_record(&mut app, &record);
    store.hold();
    let _ = update(&mut app, Message::EditorRotate { clockwise: true });
    let _ = update(&mut app, Message::HistoryOpenInEditor(record.id));
    assert_eq!(app.awaiting_reopen, Some(record.id));
    let rx = start_load(&app, record.id);

    let second = update(&mut app, Message::HistoryOpenInEditor(record.id));
    assert_eq!(
        app.awaiting_reopen,
        Some(record.id),
        "the second click cancelled the waiting open"
    );
    assert_eq!(second.units(), 0, "the second click started another open");
    assert!(app.editor.is_none(), "nothing opened yet");
    assert_eq!(
        app.history_status.as_deref(),
        Some("Finishing your previous edits to this capture…")
    );
    store.release();
    let msg = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    let _ = update(&mut app, msg);
    assert_eq!(app.editor.as_ref().unwrap().image_size(), (20, 40));
    annotate_and_sync(&mut app);
    app.coordinator.flush_history_updates();
    assert_eq!(store.stored(record.id).annotation_model.len(), 1);
}

/// Fix-3 review m-2: a new capture opening in the editor while a History
/// open waits cancels that open and says so. The capture (clean, but
/// with a crop drawn and not applied) is never replaced when the wait
/// ends.
#[test]
fn a_new_capture_during_the_wait_cancels_the_open() {
    let (_dir, store, mut app, record) = history_app();
    edit_record(&mut app, &record);
    store.hold();
    let _ = update(&mut app, Message::EditorRotate { clockwise: true });
    let _ = update(&mut app, Message::HistoryOpenInEditor(record.id));
    assert_eq!(app.awaiting_reopen, Some(record.id));
    let rx = start_load(&app, record.id);

    // A new capture opens in the editor.
    let mut capture = crate::editor::EditorSession::new(solid(30, 30));
    capture.crop_draft = Some(RectLike::new(2.0, 2.0, 10.0, 10.0));
    let _ = super::super::window_settings::open_editor_window_replacing(&mut app, capture, None);
    let c = app.editor.as_ref().unwrap().session_id;
    assert_eq!(app.awaiting_reopen, None, "the open is cancelled");
    let said = app.history_status.clone().unwrap_or_default();
    assert!(said.contains("Didn’t open"), "{said:?}");

    store.release();
    let msg = rx.recv_timeout(std::time::Duration::from_secs(20)).unwrap();
    let _ = update(&mut app, msg);
    let ed = app.editor.as_ref().unwrap();
    assert_eq!(ed.session_id, c, "the capture was replaced");
    assert!(ed.crop_draft.is_some(), "its crop draft is kept");
}

/// A redaction whose image write failed (its first phase already took
/// the unredacted image away) is not cancelled by an open: cancelling it
/// would leave the capture with no image for good. The open is refused
/// instead, saying History is still saving the redaction; once the retry
/// lands, the capture opens redacted.
#[test]
fn an_open_never_cancels_a_redaction_still_being_written() {
    let (_dir, store, mut app, record) = history_app();
    edit_record(&mut app, &record);
    store.fail_next_commits(1);
    let coord = app.coordinator.clone();
    let a = app.editor.as_mut().unwrap();
    a.model.commit_annotation(Annotation::Blur {
        rect: RectLike::new(2.0, 2.0, 6.0, 6.0),
        radius: 4.0,
    });
    super::super::editor_ops::sync_editor_history(a, &coord);
    let _ = update(&mut app, Message::HistoryOpenInEditor(record.id));
    let msg = start_load(&app, record.id)
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    let _ = update(&mut app, msg);
    assert!(app.editor.is_none(), "opened a capture with no image");
    let said = app.history_status.clone().unwrap();
    assert!(said.contains("still saving its redaction"), "{said}");
    assert!(said.contains("disk full"), "{said}");

    // The retry lands; opening again shows the redacted capture.
    app.coordinator.drain_history_writes();
    assert!(store.stored(record.id).redacted);
    let _ = update(&mut app, Message::HistoryOpenInEditor(record.id));
    let msg = start_load(&app, record.id)
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    let _ = update(&mut app, msg);
    let b = app
        .editor
        .as_ref()
        .expect("opens once the redaction landed");
    assert!(b.source_record.as_ref().unwrap().redacted);
}
