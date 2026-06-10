//! Capture history persistence — the on-disk model from spec §3.10.
//!
//! When the user opts in via [`HistoryRetention`], every saved capture
//! produces three things:
//!
//! 1. A PNG file at `<root>/YYYY/MM/<uuid>.png`.
//! 2. A sidecar JSON file at `<root>/YYYY/MM/<uuid>.json` holding the
//!    [`CaptureRecord`] (annotations, OCR text, metadata).
//! 3. An entry appended to the master `<root>/history.index.json`.
//!
//! The trio is one logical record. A torn write (PNG written, JSON
//! missing) is recoverable: [`HistoryStore::list`] silently skips records
//! whose JSON can't be read, and [`HistoryStore::apply_retention`] gladly
//! deletes orphans.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Datelike, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::annotation::Annotation;
use crate::error::HistoryError;
use crate::fs_atomic::write_atomic;
use crate::preferences::HistoryRetention;

pub const HISTORY_SCHEMA_VERSION: u32 = 1;
pub const HISTORY_INDEX_FILENAME: &str = "history.index.json";
const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 200;

/// One captured screenshot with metadata. The PNG bytes themselves live
/// alongside the sidecar JSON, not inside the record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaptureRecord {
    pub id: Uuid,
    pub captured_at: DateTime<Utc>,
    pub width_px: u32,
    pub height_px: u32,
    pub display_id: String,
    pub ocr_text: Option<String>,
    pub annotation_model: Vec<Annotation>,
}

impl CaptureRecord {
    /// Convenience for tests and call sites that need a fresh record but
    /// don't care about its uuid or timestamp.
    pub fn new(
        captured_at: DateTime<Utc>,
        width_px: u32,
        height_px: u32,
        display_id: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            captured_at,
            width_px,
            height_px,
            display_id: display_id.into(),
            ocr_text: None,
            annotation_model: Vec::new(),
        }
    }
}

/// Master index of every capture in the history directory.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HistoryIndex {
    pub schema_version: u32,
    pub updated_at: Option<DateTime<Utc>>,
    pub records: Vec<HistoryIndexEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryIndexEntry {
    pub id: Uuid,
    pub captured_at: DateTime<Utc>,
    /// Path to the PNG, relative to the history root.
    pub png_path: PathBuf,
    /// Path to the JSON sidecar, relative to the history root.
    pub json_path: PathBuf,
}

/// Trait for capture-history backends. The single production
/// implementation is [`FsHistoryStore`]; tests use a fake.
pub trait HistoryStore: Send + Sync {
    fn save(&self, record: &CaptureRecord, png: &[u8]) -> Result<(), HistoryError>;
    fn list(&self) -> Result<Vec<CaptureRecord>, HistoryError>;
    fn apply_retention(
        &self,
        policy: HistoryRetention,
        now: DateTime<Utc>,
    ) -> Result<(), HistoryError>;
    fn clear_all(&self) -> Result<(), HistoryError>;
    /// Rewrite an existing record's sidecar JSON. The PNG is left
    /// alone — `update` is for filling in OCR text or annotations
    /// after the original capture has already landed.
    ///
    /// `ocr_text: None` means "leave the stored text unchanged", not
    /// "clear it": callers that never ran OCR must not erase text a
    /// concurrent OCR pass already persisted.
    ///
    /// Best-effort by default: a record that's been retention-pruned
    /// before `update` runs is silently ignored. Implementations that
    /// can do better should override.
    fn update(&self, record: &CaptureRecord) -> Result<(), HistoryError> {
        let _ = record;
        Ok(())
    }
    /// Hard-delete a single record by id (PNG + sidecar JSON + index
    /// entry). Best-effort: deleting a record that's already gone is
    /// not an error.
    fn delete(&self, id: Uuid) -> Result<(), HistoryError> {
        let _ = id;
        Ok(())
    }
}

/// File-system backed history store. All filesystem state is rooted at
/// `root`.
pub struct FsHistoryStore {
    root: PathBuf,
    index_lock: Mutex<()>,
}

impl FsHistoryStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            index_lock: Mutex::new(()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn index_path(&self) -> PathBuf {
        self.root.join(HISTORY_INDEX_FILENAME)
    }

    fn read_index(&self) -> Result<HistoryIndex, HistoryError> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(HistoryIndex {
                schema_version: HISTORY_SCHEMA_VERSION,
                updated_at: None,
                records: Vec::new(),
            });
        }
        let content = fs::read_to_string(&path)?;
        let mut index: HistoryIndex = serde_json::from_str(&content)?;
        if index.schema_version < HISTORY_SCHEMA_VERSION {
            index.schema_version = HISTORY_SCHEMA_VERSION;
        }
        Ok(index)
    }

    fn write_index(&self, index: &HistoryIndex) -> Result<(), HistoryError> {
        fs::create_dir_all(&self.root)?;
        let content = serde_json::to_string_pretty(index)?;
        // Stage to a sibling `.tmp` and rename so a crash mid-write
        // cannot truncate or corrupt the index file. POSIX rename is
        // atomic within a filesystem.
        write_atomic(&self.index_path(), content.as_bytes())?;
        Ok(())
    }

    fn record_paths(record: &CaptureRecord) -> (PathBuf, PathBuf) {
        let year = record.captured_at.year();
        let month = record.captured_at.month();
        let dir = PathBuf::from(format!("{year:04}")).join(format!("{month:02}"));
        let png = dir.join(format!("{}.png", record.id));
        let json = dir.join(format!("{}.json", record.id));
        (png, json)
    }

    pub fn png_path(record: &CaptureRecord) -> PathBuf {
        Self::record_paths(record).0
    }

    pub fn json_path(record: &CaptureRecord) -> PathBuf {
        Self::record_paths(record).1
    }

    pub fn thumbnail_path(record: &CaptureRecord) -> PathBuf {
        let year = record.captured_at.year();
        let month = record.captured_at.month();
        PathBuf::from(format!("{year:04}"))
            .join(format!("{month:02}"))
            .join(format!("{}.thumb.png", record.id))
    }

    fn write_thumbnail_from_bytes(&self, record: &CaptureRecord, png: &[u8]) {
        let Ok(img) = image::load_from_memory(png) else {
            return;
        };
        self.write_thumbnail_image(record, img);
    }

    fn backfill_thumbnail_from_png_path(&self, record: &CaptureRecord, png_path: &Path) {
        let thumb_path = self.root.join(Self::thumbnail_path(record));
        if thumb_path.exists() {
            return;
        }
        let Ok(img) = image::open(png_path) else {
            return;
        };
        self.write_thumbnail_image(record, img);
    }

    fn write_thumbnail_image(&self, record: &CaptureRecord, img: image::DynamicImage) {
        let thumb = img.thumbnail(THUMBNAIL_MAX_WIDTH, THUMBNAIL_MAX_HEIGHT);
        let abs_thumb = self.root.join(Self::thumbnail_path(record));
        if let Some(parent) = abs_thumb.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = crate::save_png(&thumb.to_rgba8(), &abs_thumb);
    }

    fn lock_index(&self) -> Result<std::sync::MutexGuard<'_, ()>, HistoryError> {
        self.index_lock
            .lock()
            .map_err(|_| HistoryError::Io("history index lock poisoned".into()))
    }
}

impl HistoryStore for FsHistoryStore {
    fn save(&self, record: &CaptureRecord, png: &[u8]) -> Result<(), HistoryError> {
        let _index_guard = self.lock_index()?;
        let (rel_png, rel_json) = Self::record_paths(record);
        let abs_png = self.root.join(&rel_png);
        let abs_json = self.root.join(&rel_json);
        if let Some(parent) = abs_png.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write the PNG atomically (temp file + rename + parent fsync), the
        // same way the JSON sidecar and index are written below. A plain
        // create+write could leave a torn PNG that `list()` cannot detect
        // (the JSON sidecar would still be intact and the record would load
        // pointing at a corrupt image).
        write_atomic(&abs_png, png)?;
        self.write_thumbnail_from_bytes(record, png);

        let json_content = serde_json::to_string_pretty(record)?;
        write_atomic(&abs_json, json_content.as_bytes())?;

        let mut index = self.read_index()?;
        index.records.retain(|entry| entry.id != record.id);
        index.records.push(HistoryIndexEntry {
            id: record.id,
            captured_at: record.captured_at,
            png_path: rel_png,
            json_path: rel_json,
        });
        index.updated_at = Some(Utc::now());
        self.write_index(&index)?;
        Ok(())
    }

    fn list(&self) -> Result<Vec<CaptureRecord>, HistoryError> {
        let index = self.read_index()?;
        let mut entries = index.records;
        // Newest first.
        entries.sort_by_key(|e| std::cmp::Reverse(e.captured_at));

        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let path = self.root.join(&entry.json_path);
            // Skip orphaned index entries — the file was deleted by the
            // user or a torn write left the index ahead of the data.
            if !path.exists() {
                continue;
            }
            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            if let Ok(record) = serde_json::from_str::<CaptureRecord>(&content) {
                self.backfill_thumbnail_from_png_path(&record, &self.root.join(&entry.png_path));
                out.push(record);
            }
        }
        Ok(out)
    }

    fn apply_retention(
        &self,
        policy: HistoryRetention,
        now: DateTime<Utc>,
    ) -> Result<(), HistoryError> {
        let _index_guard = self.lock_index()?;
        let mut index = self.read_index()?;
        // Sort newest first so `take(N)` keeps the freshest captures.
        index
            .records
            .sort_by_key(|e| std::cmp::Reverse(e.captured_at));

        let kept: Vec<HistoryIndexEntry> = match policy {
            HistoryRetention::Off | HistoryRetention::Unlimited => {
                // `Off` deliberately keeps existing records (the user
                // toggled writes off; their old captures aren't deleted).
                // `Unlimited` keeps everything.
                index.records.clone()
            }
            HistoryRetention::Last50 => index.records.iter().take(50).cloned().collect(),
            HistoryRetention::Last30Days => {
                let cutoff = now - Duration::days(30);
                index
                    .records
                    .iter()
                    .filter(|e| e.captured_at >= cutoff)
                    .cloned()
                    .collect()
            }
        };

        // Anything not in `kept` is removed from disk. ID-based filtering
        // tolerates duplicate `captured_at` values.
        let kept_ids: std::collections::HashSet<Uuid> = kept.iter().map(|e| e.id).collect();
        for entry in &index.records {
            if !kept_ids.contains(&entry.id) {
                let _ = fs::remove_file(self.root.join(&entry.png_path));
                let _ = fs::remove_file(self.root.join(&entry.json_path));
                let record_for_path = CaptureRecord {
                    id: entry.id,
                    captured_at: entry.captured_at,
                    width_px: 0,
                    height_px: 0,
                    display_id: String::new(),
                    ocr_text: None,
                    annotation_model: Vec::new(),
                };
                let _ = fs::remove_file(self.root.join(Self::thumbnail_path(&record_for_path)));
            }
        }

        index.records = kept;
        index.updated_at = Some(now);
        self.write_index(&index)?;
        Ok(())
    }

    fn clear_all(&self) -> Result<(), HistoryError> {
        let _index_guard = self.lock_index()?;
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        fs::create_dir_all(&self.root)?;
        let cleared = HistoryIndex {
            schema_version: HISTORY_SCHEMA_VERSION,
            updated_at: Some(Utc::now()),
            records: Vec::new(),
        };
        self.write_index(&cleared)?;
        Ok(())
    }

    fn update(&self, record: &CaptureRecord) -> Result<(), HistoryError> {
        let _index_guard = self.lock_index()?;
        let (_rel_png, rel_json) = Self::record_paths(record);
        let abs_json = self.root.join(&rel_json);
        // Best-effort: a record that's been retention-pruned between
        // save and update is not an error, just a no-op.
        if !abs_json.exists() {
            return Ok(());
        }
        // `ocr_text: None` means "unchanged", not "clear": callers that
        // never ran OCR (e.g. the editor's annotation sync, whose record
        // predates the background OCR pass) must not wipe text another
        // writer already persisted. Merging here, under the index lock,
        // keeps the two writers race-free in-process.
        let mut merged = record.clone();
        if merged.ocr_text.is_none() {
            if let Some(prev) = fs::read_to_string(&abs_json)
                .ok()
                .and_then(|s| serde_json::from_str::<CaptureRecord>(&s).ok())
            {
                merged.ocr_text = prev.ocr_text;
            }
        }
        let json_content = serde_json::to_string_pretty(&merged)?;
        write_atomic(&abs_json, json_content.as_bytes())?;
        // The index is deliberately not touched: `update` changes only
        // the sidecar, nothing reads the index's `updated_at`, and the
        // editor calls this on every annotation edit — a full index
        // read-rewrite (two more fsyncs) per keystroke is pure waste.
        Ok(())
    }

    fn delete(&self, id: Uuid) -> Result<(), HistoryError> {
        let _index_guard = self.lock_index()?;
        let mut index = self.read_index()?;
        let Some(pos) = index.records.iter().position(|e| e.id == id) else {
            // Already gone — not an error.
            return Ok(());
        };
        let entry = index.records.remove(pos);
        let _ = fs::remove_file(self.root.join(&entry.png_path));
        let _ = fs::remove_file(self.root.join(&entry.json_path));
        let record_for_path = CaptureRecord {
            id,
            captured_at: entry.captured_at,
            width_px: 0,
            height_px: 0,
            display_id: String::new(),
            ocr_text: None,
            annotation_model: Vec::new(),
        };
        let _ = fs::remove_file(self.root.join(Self::thumbnail_path(&record_for_path)));
        index.updated_at = Some(Utc::now());
        self.write_index(&index)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn store() -> (TempDir, FsHistoryStore) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("history");
        let s = FsHistoryStore::new(&root);
        (dir, s)
    }

    fn record_at(t: DateTime<Utc>) -> CaptureRecord {
        CaptureRecord::new(t, 64, 64, "display-0")
    }

    /// 10 fake PNG bytes — the store writes them verbatim.
    fn fake_png() -> Vec<u8> {
        b"\x89PNG\r\n\x1a\nXX".to_vec()
    }

    fn valid_png(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba([40, 80, 120, 255]);
        }
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn save_then_list_roundtrip() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &fake_png()).unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, r.id);
    }

    #[test]
    fn saving_same_record_twice_replaces_index_entry() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());

        s.save(&r, &fake_png()).unwrap();
        s.save(&r, &fake_png()).unwrap();

        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, r.id);
    }

    #[test]
    fn concurrent_saves_keep_every_index_entry() {
        let (_dir, s) = store();
        let s = std::sync::Arc::new(s);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(12));
        let mut handles = Vec::new();

        for i in 0..12 {
            let s = s.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let record = record_at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, i).unwrap());
                barrier.wait();
                s.save(&record, &fake_png()).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(s.list().unwrap().len(), 12);
    }

    #[test]
    fn list_orders_newest_first() {
        let (_dir, s) = store();
        let older = record_at(Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap());
        let newer = record_at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        s.save(&older, &fake_png()).unwrap();
        s.save(&newer, &fake_png()).unwrap();
        let list = s.list().unwrap();
        assert_eq!(list[0].id, newer.id);
        assert_eq!(list[1].id, older.id);
    }

    #[test]
    fn update_rewrites_sidecar_with_new_ocr_text() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        s.save(&r, &fake_png()).unwrap();
        // List comes back without OCR text by default.
        assert!(s.list().unwrap()[0].ocr_text.is_none());
        // Now mutate and update.
        r.ocr_text = Some("hello world".into());
        s.update(&r).unwrap();
        let list = s.list().unwrap();
        assert_eq!(list[0].ocr_text.as_deref(), Some("hello world"));
    }

    #[test]
    fn corrupt_index_surfaces_error_for_list_and_save() {
        // A torn or corrupted index file is the worst history failure
        // mode: read_index propagates the parse error, which blocks
        // both reads *and* all future saves. Pin that behavior so any
        // future recovery branch is a deliberate change.
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &fake_png()).unwrap();

        fs::write(s.root().join(HISTORY_INDEX_FILENAME), b"{not json").unwrap();

        assert!(s.list().is_err(), "list should surface the parse error");
        let newer = record_at(Utc::now());
        assert!(
            s.save(&newer, &fake_png()).is_err(),
            "save should surface the parse error rather than clobber the index"
        );
    }

    #[test]
    fn list_skips_record_with_corrupt_sidecar_content() {
        // Sidecar corruption (invalid JSON, truncated write) must skip
        // only the damaged record — distinct from the deleted-file skip
        // branch, which is covered separately.
        let (_dir, s) = store();
        let damaged = record_at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        let survivor = record_at(Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap());
        s.save(&damaged, &fake_png()).unwrap();
        s.save(&survivor, &fake_png()).unwrap();

        let (_png, rel_json) = FsHistoryStore::record_paths(&damaged);
        fs::write(s.root().join(&rel_json), b"{truncated").unwrap();

        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, survivor.id);
    }

    #[test]
    fn update_without_ocr_text_preserves_stored_text() {
        // The editor's annotation sync carries a record whose
        // `ocr_text` is still `None` (it predates the background OCR
        // pass). Its update must not wipe the text OCR already wrote.
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        s.save(&r, &fake_png()).unwrap();

        r.ocr_text = Some("recognised text".into());
        s.update(&r).unwrap();

        let mut annotated = r.clone();
        annotated.ocr_text = None;
        annotated.annotation_model = vec![crate::Annotation::Arrow {
            a: crate::PointLike::new(1.0, 2.0),
            b: crate::PointLike::new(3.0, 4.0),
            color: crate::Rgba::new(1.0, 0.0, 0.0, 1.0),
            line_width: 2.0,
        }];
        s.update(&annotated).unwrap();

        let list = s.list().unwrap();
        assert_eq!(list[0].ocr_text.as_deref(), Some("recognised text"));
        assert_eq!(list[0].annotation_model.len(), 1);
    }

    #[test]
    fn update_is_a_noop_when_record_was_pruned() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        // Never saved — `update` must not fail.
        let res = s.update(&r);
        assert!(res.is_ok());
    }

    #[test]
    fn delete_removes_png_json_and_index_entry() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &fake_png()).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        s.delete(r.id).unwrap();
        assert!(s.list().unwrap().is_empty());
        let (rel_png, rel_json) = FsHistoryStore::record_paths(&r);
        assert!(!s.root().join(&rel_png).exists());
        assert!(!s.root().join(&rel_json).exists());
    }

    #[test]
    fn delete_unknown_id_is_a_noop() {
        let (_dir, s) = store();
        // Empty store — delete must not fail.
        let res = s.delete(Uuid::new_v4());
        assert!(res.is_ok());
    }

    #[test]
    fn list_skips_records_whose_sidecar_was_deleted() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &fake_png()).unwrap();
        // Manually delete the sidecar JSON to simulate a torn write or
        // user mistake. The index still lists the entry; `list()` must
        // skip it rather than fail.
        let (_png, rel_json) = FsHistoryStore::record_paths(&r);
        std::fs::remove_file(s.root().join(&rel_json)).unwrap();
        let list = s.list().unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn retention_last50_drops_excess_oldest() {
        let (_dir, s) = store();
        for i in 0..55 {
            let t = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, i as u32).unwrap();
            s.save(&record_at(t), &fake_png()).unwrap();
        }
        s.apply_retention(HistoryRetention::Last50, Utc::now())
            .unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 50);
        // Newest 50 are kept — oldest captured_at in the kept set must be
        // strictly later than the dropped 5.
        let oldest_kept = list.iter().map(|r| r.captured_at).min().unwrap();
        let expected_threshold = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 5).unwrap();
        assert!(oldest_kept >= expected_threshold);
    }

    #[test]
    fn retention_last30days_drops_old_keeps_recent() {
        let (_dir, s) = store();
        let now = Utc::now();
        let old = now - Duration::days(60);
        let recent = now - Duration::days(5);
        s.save(&record_at(old), &fake_png()).unwrap();
        s.save(&record_at(recent), &fake_png()).unwrap();
        s.apply_retention(HistoryRetention::Last30Days, now)
            .unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].captured_at, recent);
    }

    #[test]
    fn retention_off_does_not_delete_existing_records() {
        let (_dir, s) = store();
        for i in 0..3 {
            let t = Utc::now() + Duration::seconds(i);
            s.save(&record_at(t), &fake_png()).unwrap();
        }
        s.apply_retention(HistoryRetention::Off, Utc::now())
            .unwrap();
        let list = s.list().unwrap();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn clear_all_empties_directory_and_index() {
        let (_dir, s) = store();
        s.save(&record_at(Utc::now()), &fake_png()).unwrap();
        s.save(&record_at(Utc::now()), &fake_png()).unwrap();
        s.clear_all().unwrap();
        let list = s.list().unwrap();
        assert!(list.is_empty());
        // Index file still exists but contains no records.
        let index_content = std::fs::read_to_string(s.index_path()).unwrap();
        assert!(index_content.contains("\"records\": []"));
    }

    #[test]
    fn save_creates_year_month_directories() {
        let (_dir, s) = store();
        let t = Utc.with_ymd_and_hms(2026, 4, 25, 14, 30, 0).unwrap();
        let r = record_at(t);
        s.save(&r, &fake_png()).unwrap();
        let png_path = s
            .root()
            .join("2026")
            .join("04")
            .join(format!("{}.png", r.id));
        assert!(png_path.exists());
    }

    #[test]
    fn save_writes_pre_rendered_thumbnail_for_valid_png() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(800, 600)).unwrap();

        let thumb_path = s.root().join(FsHistoryStore::thumbnail_path(&r));
        assert!(thumb_path.exists());
        let thumb = image::open(&thumb_path).unwrap();
        assert!(thumb.width() <= 320);
        assert!(thumb.height() <= 200);
    }

    #[test]
    fn list_backfills_missing_thumbnail_for_old_record() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(640, 480)).unwrap();
        let thumb_path = s.root().join(FsHistoryStore::thumbnail_path(&r));
        std::fs::remove_file(&thumb_path).unwrap();
        assert!(!thumb_path.exists());

        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert!(thumb_path.exists());
    }
}
