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
    fn finish_pending_scrub(
        &self,
        id: readshot_core::Uuid,
    ) -> Result<bool, readshot_core::HistoryError> {
        self.inner.finish_pending_scrub(id)
    }
    fn finish_pending_scrubs(&self) -> usize {
        self.inner.finish_pending_scrubs()
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

/// xorshift64*: a small seeded generator (no new dependency).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
    }
}

/// One History record edited, closed and reopened in random order while
/// its writes fail or are slow. What the runtime would run as tasks is
/// run here: a History open's load on its own thread, delivered later.
struct Interleaving {
    _dir: tempfile::TempDir,
    store: Arc<GatedStore>,
    app: App,
    id: readshot_core::Uuid,
    seed: u64,
    /// Loads started by opens, delivered in order.
    loads: std::collections::VecDeque<std::sync::mpsc::Receiver<Message>>,
    /// An editor opened the record redacted: History must keep it so.
    shown_redacted: bool,
    log: Vec<&'static str>,
}

impl Interleaving {
    fn new() -> Self {
        let (dir, store, app, record) = history_app();
        Self {
            _dir: dir,
            store,
            app,
            id: record.id,
            seed: 0,
            loads: std::collections::VecDeque::new(),
            shown_redacted: false,
            log: Vec::new(),
        }
    }

    /// The next run, on a record of its own (the app is reused: building
    /// one per run would dominate the test's time).
    fn next_run(&mut self, seed: u64) {
        if let Some(ed) = self.app.editor.take() {
            if let Some(window) = ed.window_id {
                self.app.windows.forget(window);
            }
            super::super::compose_jobs::drop_editor(ed);
        }
        // History stays one record long, so reading it stays cheap.
        self.store.delete(self.id).unwrap();
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
        self.store
            .save(&record, &readshot_core::encode_png(&ramp(40, 20)).unwrap())
            .unwrap();
        self.app.history_records = self.store.list().unwrap();
        self.app.history_status = None;
        self.id = record.id;
        self.seed = seed;
        self.shown_redacted = false;
        self.log.clear();
    }

    fn editor_of_record(&mut self) -> Option<&mut crate::editor::EditorSession> {
        let id = self.id;
        self.app
            .editor
            .as_mut()
            .filter(|ed| ed.source_record.as_ref().is_some_and(|r| r.id == id))
    }

    /// `update`, starting the load an open begins (the runtime runs it
    /// as a task).
    fn send(&mut self, message: Message) {
        let loading =
            |app: &App, id| app.awaiting_reopen == Some(id) && app.pending_history_open.is_none();
        let before = loading(&self.app, self.id);
        let _ = update(&mut self.app, message);
        if loading(&self.app, self.id) && !before {
            self.loads.push_back(start_load(&self.app, self.id));
        }
    }

    fn sync_after(&mut self, edit: impl FnOnce(&mut crate::editor::EditorSession)) {
        let coord = self.app.coordinator.clone();
        if let Some(ed) = self.editor_of_record() {
            edit(ed);
            super::super::editor_ops::sync_editor_history(ed, &coord);
        }
    }

    /// The oldest load's answer, when it is ready (`wait`: block for it).
    fn deliver(&mut self, wait: bool) {
        let Some(rx) = self.loads.front() else {
            return;
        };
        let msg = if wait {
            rx.recv_timeout(std::time::Duration::from_secs(30))
                .expect("an open never finished waiting")
        } else {
            match rx.try_recv() {
                Ok(msg) => msg,
                Err(_) => return,
            }
        };
        self.loads.pop_front();
        self.send(msg);
        // The window the runtime opened for it.
        let window = self
            .app
            .windows
            .iter()
            .find(|(_, kind)| **kind == WindowKind::Editor)
            .map(|(id, _)| *id);
        if let Some(ed) = self.editor_of_record() {
            if ed.window_id.is_none() {
                ed.window_id = window;
            }
            if ed.source_record.as_ref().is_some_and(|r| r.redacted) {
                self.shown_redacted = true;
            }
        }
    }

    fn step(&mut self, rng: &mut Rng) {
        match rng.below(12) {
            0 | 1 => {
                self.log.push("edit");
                let at = rng.below(20) as f32;
                self.sync_after(|ed| {
                    ed.model.commit_annotation(Annotation::Rectangle {
                        rect: RectLike::new(at, 1.0, 3.0, 3.0),
                        color: Rgba::OPAQUE_BLACK,
                        line_width: 1.0,
                        style: Default::default(),
                    })
                });
            }
            2 => {
                self.log.push("rotate");
                if self.editor_of_record().is_some() {
                    self.send(Message::EditorRotate { clockwise: true });
                }
            }
            3 => {
                self.log.push("redact");
                self.sync_after(|ed| {
                    ed.model.commit_annotation(Annotation::Blur {
                        rect: RectLike::new(2.0, 2.0, 6.0, 6.0),
                        radius: 4.0,
                    })
                });
            }
            4 => {
                self.log.push("undo");
                if self.editor_of_record().is_some() {
                    self.send(Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo));
                }
            }
            5 => {
                self.log.push("close");
                if let Some(window) = self.editor_of_record().and_then(|ed| ed.window_id) {
                    self.send(Message::WindowClosed(window));
                }
            }
            6 => {
                self.log.push("open");
                self.send(Message::HistoryOpenInEditor(self.id));
            }
            7 => {
                self.log.push("second-open");
                self.send(Message::HistoryOpenInEditor(self.id));
                self.send(Message::HistoryOpenInEditor(self.id));
            }
            8 => {
                self.log.push("write-failure");
                self.store.fail_next_commits(1 + rng.below(2) as usize);
            }
            9 => {
                self.log.push("write-slow");
                self.store.hold();
            }
            10 => {
                self.log.push("write-done");
                self.store.release();
            }
            _ => {
                self.log.push("load-lands");
                // It can't finish while a write it waits for is held.
                let held = *self.store.held.lock().unwrap()
                    && self.app.coordinator.record_writes_outstanding(self.id);
                self.deliver(!held);
            }
        }
        self.check_privacy();
    }

    /// (b) A record an editor opened redacted is never stored unredacted.
    fn check_privacy(&self) {
        if self.shown_redacted {
            assert!(
                self.store.stored(self.id).redacted,
                "a record shown redacted is stored unredacted after seed {}: {:?}",
                self.seed,
                self.log
            );
        }
    }

    /// Let every write and open finish.
    fn settle(&mut self) {
        self.store.fail_next_commits(0);
        self.store.release();
        while !self.loads.is_empty() {
            self.deliver(true);
        }
        self.app.coordinator.drain_history_writes();
    }

    /// Once writes are quiet: a withheld record (redaction pending) has
    /// nothing at its image path for any reader to serve, and no text.
    /// (Mid-commit, the new image is renamed in just before the sidecar
    /// stops saying pending: a transient this can't check.)
    fn check_withheld(&self) {
        let stored = self.store.stored(self.id);
        if stored.scrub_pending {
            let png = FsHistoryStore::abs_png_path(self.store.inner.root(), &stored);
            assert!(
                !png.exists() && stored.ocr_text.is_none(),
                "a withheld record is served after seed {}: {:?}",
                self.seed,
                self.log
            );
        }
    }

    /// After every write drained, an open always opens the record, and
    /// its image is on disk: nothing is ever lost.
    fn check_final_open(&mut self) {
        let log = format!("seed {}: {:?}", self.seed, self.log);
        let stored = self.store.stored(self.id);
        let png = FsHistoryStore::abs_png_path(self.store.inner.root(), &stored);
        assert!(png.exists(), "the image is gone after {log}");
        assert!(
            self.editor_of_record().is_some(),
            "the final open opened nothing ({:?}) after {log}",
            self.app.history_status
        );
    }

    /// (a) The open editor of the record shows what History stores.
    fn check_editor_matches_store(&mut self) {
        let log = format!("seed {}: {:?}", self.seed, self.log);
        let stored = self.store.stored(self.id);
        let Some(ed) = self.editor_of_record() else {
            return;
        };
        let annotations = ed.model.annotations().to_vec();
        let redacted = readshot_core::scrub::has_redaction(&annotations)
            || ed.source_record.as_ref().is_some_and(|r| r.redacted);
        let expected_annotations = if readshot_core::scrub::has_redaction(&annotations) {
            Vec::new()
        } else {
            annotations
        };
        let base = (ed.model.base().width(), ed.model.base().height());
        let frame = super::super::editor_ops::history_geometry_id(ed);
        assert_eq!(
            (stored.width_px, stored.height_px),
            base,
            "size differs after {log}"
        );
        assert_eq!(stored.redacted, redacted, "redaction differs after {log}");
        assert_eq!(stored.geometry_id, frame, "frame differs after {log}");
        assert_eq!(
            stored.annotation_model, expected_annotations,
            "annotations differ after {log}"
        );
    }
}

/// Random interleavings of edit, rotate, redact, undo, close, open,
/// second open, failing and slow writes for one record. After all writes
/// drain, the open editor shows exactly what History stores, and a record
/// an editor showed redacted is never stored unredacted. Then it is
/// opened once more: that copy matches History too, and nothing written
/// before the open lands after it.
#[test]
fn reopens_under_random_slow_and_failing_writes_keep_editor_and_history_in_step() {
    // Thousands of History writes; durability across a crash isn't what
    // this tests.
    readshot_core::skip_fsync_in_this_process();
    let mut run = Interleaving::new();
    for seed in 0..300u64 {
        let mut rng = Rng::new(seed);
        run.next_run(seed);
        run.send(Message::HistoryOpenInEditor(run.id));
        for _ in 0..24 {
            run.step(&mut rng);
        }
        run.settle();
        run.check_privacy();
        run.check_withheld();
        run.check_editor_matches_store();
        run.log.push("final-open");
        run.send(Message::HistoryOpenInEditor(run.id));
        run.settle();
        run.check_privacy();
        run.check_final_open();
        run.check_editor_matches_store();
    }
}

/// A redaction whose image write failed is cancelled by an open like
/// any other write, and nothing is lost: phase one only set the image
/// aside, so the open finishes the redaction from it and the capture
/// opens redacted, image and all. The cancelled write never lands after.
#[test]
fn an_open_during_a_failed_redaction_opens_it_redacted_with_its_image() {
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
    let b = app.editor.as_ref().expect("the capture opens");
    assert!(b.source_record.as_ref().unwrap().redacted);
    let status = b.status_text().unwrap_or_default();
    assert!(status.contains("disk full"), "{status}");
    app.coordinator.drain_history_writes();
    let stored = store.stored(record.id);
    assert!(stored.redacted && !stored.scrub_pending);
    assert!(FsHistoryStore::abs_png_path(store.inner.root(), &stored).exists());
}

/// Fix-4 review N-1: the redaction's image write fails, the user undoes
/// it, and that restore fails too; then the capture is opened from
/// History. The capture keeps its image: it opens with the redaction
/// drawn in (History never served it unredacted, and still doesn't).
#[test]
fn an_undone_redaction_whose_writes_failed_still_opens_with_its_image() {
    let (_dir, store, mut app, record) = history_app();
    edit_record(&mut app, &record);
    store.fail_next_commits(1000);
    let coord = app.coordinator.clone();
    let a = app.editor.as_mut().unwrap();
    a.model.commit_annotation(Annotation::Blur {
        rect: RectLike::new(2.0, 2.0, 6.0, 6.0),
        radius: 4.0,
    });
    super::super::editor_ops::sync_editor_history(a, &coord);
    let _ = update(
        &mut app,
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    let _ = update(&mut app, Message::HistoryOpenInEditor(record.id));
    let msg = start_load(&app, record.id)
        .recv_timeout(std::time::Duration::from_secs(20))
        .unwrap();
    store.fail_next_commits(0);
    let _ = update(&mut app, msg);
    let b = app.editor.as_ref().unwrap_or_else(|| {
        panic!(
            "the capture didn't open: {:?}",
            app.history_status.as_deref()
        )
    });
    assert!(b.source_record.as_ref().unwrap().redacted);
    app.coordinator.drain_history_writes();
    let stored = store.stored(record.id);
    assert!(stored.redacted && !stored.scrub_pending);
    assert!(FsHistoryStore::abs_png_path(store.inner.root(), &stored).exists());
}

/// A record withheld while its redaction is saved hands nothing out:
/// Copy Image, Pin and Reveal say so instead of reading its image.
#[test]
fn a_withheld_record_hands_out_no_image() {
    let (_dir, _store, mut app, record) = history_app();
    app.history_records[0].scrub_pending = true;
    for message in [
        Message::HistoryCopyImage(record.id),
        Message::HistoryPin(record.id),
        Message::HistoryReveal(record.id),
    ] {
        app.history_status = None;
        let task = update(&mut app, message);
        assert_eq!(task.units(), 0, "it read the image");
        assert_eq!(
            app.history_status.as_deref(),
            Some(super::super::history_ops::actions::WITHHELD_NOTE)
        );
    }
}
