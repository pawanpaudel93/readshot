//! Capture history persistence — the on-disk model.
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
use crate::text_lines::TextDocument;

pub const HISTORY_SCHEMA_VERSION: u32 = 1;
pub const HISTORY_INDEX_FILENAME: &str = "history.index.json";
/// Sidecar lock file guarding cross-process index read-modify-writes.
const HISTORY_LOCK_FILENAME: &str = "history.index.lock";
const THUMBNAIL_MAX_WIDTH: u32 = 320;
const THUMBNAIL_MAX_HEIGHT: u32 = 200;

/// Join `rel` onto `root` only when `rel` is a safe *relative* path:
/// no absolute paths, and every component must be `Normal` (rejecting
/// `..`, a root, or a Windows prefix). Returns `None` for anything that
/// could escape `root`, so a tampered or corrupt `history.index.json`
/// cannot steer `list`/thumbnail I/O at arbitrary files on disk.
fn safe_relative_join(root: &Path, rel: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if rel.as_os_str().is_empty() || rel.is_absolute() {
        return None;
    }
    for component in rel.components() {
        if !matches!(component, Component::Normal(_)) {
            return None;
        }
    }
    Some(root.join(rel))
}

/// Longest `CaptureRecord::geometry_id` kept as is on load (bytes).
pub const MAX_GEOMETRY_ID_LEN: usize = 4096;

/// What an over-long `geometry_id` loads as: a frame id no editor
/// geometry ever has, so recognized text of any frame is dropped for it.
pub const INVALID_GEOMETRY_ID: &str = "";

fn deserialize_geometry_id<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let id: Option<String> = Deserialize::deserialize(d)?;
    Ok(id.map(|id| {
        if id.len() > MAX_GEOMETRY_ID_LEN {
            INVALID_GEOMETRY_ID.to_owned()
        } else {
            id
        }
    }))
}

/// Prefix of the frame id of a scrub's stored image when that image is a
/// crop of the document (`readshot-app`'s `cropped_frame_id`), under the
/// current rule for the text a cropped record may keep (v3: only words
/// whose boxes lie wholly inside the crop). The prefix names the rule, so
/// a record written under an older one is recognised on load and
/// migrated ([`migrate_legacy_record`]).
pub const CROP_FRAME_ID_PREFIX: &str = "crop3:";

/// Prefix of the crop frame ids written before [`CROP_FRAME_ID_PREFIX`]
/// (std `DefaultHasher`, then FNV-1a under rules v1–v3). Their hashes
/// can't tell the rule apart, so every one of them is migrated.
const LEGACY_CROP_FRAME_ID_PREFIX: &str = "crop:";

/// Whether `id` is a crop frame id of the current rule.
pub fn is_crop_frame_id(id: &str) -> bool {
    id.starts_with(CROP_FRAME_ID_PREFIX)
}

/// Bring a record written by an older build up to the current rules, in
/// place. `true` when anything changed (the sidecar must be rewritten).
/// Idempotent: a migrated record is never changed again.
///
/// - **A crop frame id of an older rule** (`crop:…`). Its text may hold
///   words from the cut-away area, or partial tokens cut by the edge: the
///   text is dropped (fail closed) and the id moves to
///   [`CROP_FRAME_ID_PREFIX`] (same hash, so it is still this record's own
///   frame). No text is recovered from the stored image on load: records
///   of that era may have been scrubbed over an earlier scrub whose
///   redactions were never stored, so their stored redactions can't be
///   known to be complete, and recognition of the image could read a word
///   under an unrecorded weak blur. The text stays absent (fail closed).
/// - **A scrub cropped before crop frame ids existed.** Such a record kept
///   the original frame's id (usually none), its redactions in the
///   uncropped document's px and the text of the whole document, while
///   its PNG and size are the crop's. When a stored redaction lies wholly
///   outside the stored image, the redactions can't be in its px (an
///   editor redaction always touches the image): the text is dropped and
///   so are the misplaced redactions, so nothing is ever masked with them
///   (text then stays absent: fail closed). A redaction "touches" the
///   image when it covers some of its area ([`crate::scrub::touches_image`]:
///   one only along an edge, or with no area, does not). A record whose
///   redactions all touch its image can't be told apart from an uncropped
///   scrub — no stored field keeps the uncropped size or the crop's
///   origin, and the sidecar size is the PNG's — so it is left as is.
///   Checked for every record that may be of that era: no current crop
///   id and a list not known complete. That includes records the current
///   build wrote with an unknown list (a scrub over an unknown one), which
///   can't be told apart from legacy ones; the check never fires for
///   them, because current writers store only redactions that touch the
///   stored image ([`crate::scrub::redactions_in_image`],
///   [`crate::scrub::redactions_in_crop`]: clipped, the zero-area and
///   edge-only ones dropped) and their text is already absent (unknown
///   list).
/// - **A redacted record whose redaction list isn't known complete**
///   ([`CaptureRecord::redactions_complete`]; every record written before
///   the flag): its text is dropped ([`text_allowed`]).
pub fn migrate_legacy_record(record: &mut CaptureRecord) -> bool {
    let mut changed = false;
    // May have been written before crop ids named their rule and before
    // redaction lists were known complete: the era a crop could leave
    // redactions in another frame's px. A current write with an unknown
    // list is included (indistinguishable) but never misplaced: current
    // writers keep only redactions that touch the stored image.
    let legacy_era =
        !record.redactions_complete && !record.geometry_id.as_deref().is_some_and(is_crop_frame_id);
    let legacy_crop = record
        .geometry_id
        .as_deref()
        .and_then(|id| id.strip_prefix(LEGACY_CROP_FRAME_ID_PREFIX))
        .map(str::to_owned);
    if let Some(hash) = legacy_crop {
        record.geometry_id = Some(format!("{CROP_FRAME_ID_PREFIX}{hash}"));
        record.ocr_text = None;
        changed = true;
    }
    let (w, h) = (record.width_px as f32, record.height_px as f32);
    let misplaced = legacy_era
        && record.redacted
        && record.redactions.iter().any(|a| match a {
            Annotation::Rectangle { rect, .. }
            | Annotation::Blur { rect, .. }
            | Annotation::Pixelate { rect, .. } => {
                // The test current writers clip to: never true of a
                // redaction they stored. A NaN never touches.
                !crate::scrub::touches_image(*rect, w, h)
            }
            _ => false,
        });
    if misplaced {
        record.redactions.clear();
        record.ocr_text = None;
        changed = true;
    }
    changed |= drop_unknown_text(record);
    changed
}

/// Whether `record` may hold text: unredacted, or redacted with a
/// complete redaction list ([`CaptureRecord::redactions_complete`]).
pub fn text_allowed(record: &CaptureRecord) -> bool {
    !record.redacted || record.redactions_complete
}

/// Whether `a` survives a sidecar round trip. serde_json writes a
/// non-finite `f32` (NaN, ∞) as `null`, which then fails to load and would
/// make the whole record vanish from History and MCP.
fn loads_back(a: &Annotation) -> bool {
    serde_json::to_value(a)
        .ok()
        .and_then(|v| serde_json::from_value::<Annotation>(v).ok())
        .is_some()
}

/// Drop annotations that wouldn't load back ([`loads_back`]). A
/// redaction list that loses one no longer lists every redaction, so it
/// stops being complete (and its text goes: fail closed).
fn drop_unloadable_annotations(record: &mut CaptureRecord) {
    let (anns, redactions) = (record.annotation_model.len(), record.redactions.len());
    record.annotation_model.retain(loads_back);
    record.redactions.retain(loads_back);
    if record.redactions.len() != redactions {
        record.redactions_complete = false;
    }
    if record.annotation_model.len() != anns || record.redactions.len() != redactions {
        tracing::warn!(
            target: crate::log::cat::HISTORY,
            "history record {}: dropped annotations with non-finite values",
            record.id
        );
    }
}

/// Drop the text of a record that may not hold any ([`text_allowed`]).
/// `true` when something was dropped.
fn drop_unknown_text(record: &mut CaptureRecord) -> bool {
    if text_allowed(record) || record.ocr_text.is_none() {
        return false;
    }
    record.ocr_text = None;
    true
}

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
    /// The stored PNG is a redacted, flattened editor output and
    /// `ocr_text` is masked to match (see [`HistoryStore::rewrite`]).
    /// The PNG already shows the redaction annotations, so they are not
    /// in `annotation_model` (reopening would apply them twice). Absent
    /// in records written before this field existed, and never written
    /// while false, so unredacted records keep the original schema.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redacted: bool,
    /// The redactions (base-image px) of the scrub that produced a
    /// `redacted` record, kept so text recognized later can be masked the
    /// same way ([`HistoryStore::record_recognized_text`]). Not edited and
    /// not shown: reopening ignores them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<Annotation>,
    /// `redactions` provably lists every redaction baked into the stored
    /// image, in its px: set only by a scrub of a never-scrubbed image, or
    /// of one whose own list was complete (plus the redactions baked now).
    /// Only then may text be masked with them. A `redacted` record without
    /// it is **unknown**: an unrecorded weak blur may hide a word that
    /// recognition can still read and nothing can mask, so it never holds
    /// text ([`text_allowed`]: dropped on load and on every write) and no
    /// recognized text is stored for it. Absent — unknown — in records
    /// written before it existed; never written while false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub redactions_complete: bool,
    /// A scrub has started but its redacted image hasn't landed: the
    /// record is **withheld**. Its unredacted image was moved aside (never
    /// deleted: [`FsHistoryStore::withheld_path`]) and its thumbnail
    /// removed, so nothing is served at its image path, and readers serve
    /// no text for it ([`HistoryStore::list`]). The redacted image then
    /// replaces it atomically; if it never does (a crash, a quit, a
    /// cancelled write), [`HistoryStore::finish_pending_scrub`] draws
    /// [`Self::pending_image`]'s redactions onto the withheld image and
    /// swaps that in instead.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scrub_pending: bool,
    /// While `scrub_pending`: the image set aside (its document frame and
    /// size, whether it is already redacted) and what to draw onto it, in
    /// its own px, should the scrub's own image never land. On a scrub
    /// request: what the scrub bakes (before any crop) and the frame it is
    /// in, which phase one maps onto the stored image
    /// ([`PendingImage`]). Never stored on a record that isn't pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_image: Option<PendingImage>,
    /// Which document frame the stored image (and `redactions`,
    /// `annotation_model`) is in: `None` for the capture as taken, else an
    /// opaque id of an editor geometry (rotated, resized, combined).
    /// Recognized text is only stored — and masked — for a record of the
    /// same frame ([`HistoryStore::record_recognized_text`]), so OCR of
    /// one frame never masks, or replaces the text of, another. Absent in
    /// older records and never written while `None`. An over-long id
    /// loads as [`INVALID_GEOMETRY_ID`]: never `None` (that would match
    /// the original frame and fail open), and never an id the editor
    /// produces for any frame.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_geometry_id"
    )]
    pub geometry_id: Option<String>,
}

/// An image, by document frame and size, and what to draw onto it (see
/// [`CaptureRecord::pending_image`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingImage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry_id: Option<String>,
    pub width: u32,
    pub height: u32,
    /// What to draw: as a scrub bakes it, the redactions and every other
    /// annotation drawn with them (any redaction flattens the output), in
    /// this image's px. Never a crop.
    #[serde(default, alias = "redactions")]
    pub bake: Vec<Annotation>,
    /// Stored: the image itself is already redacted (a redaction that
    /// landed), so it is safe to serve as it is once `bake` is drawn,
    /// even when `bake` is empty. On a request: the request's image
    /// already holds redactions baked in (a reopened redacted capture).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub already_redacted: bool,
}

/// A solid cover over the whole of a `width` × `height` image: what a
/// pending scrub draws when its redactions can't be placed on it (fail
/// closed: never an unredacted pixel, never a lost image).
fn whole_image_cover(width: u32, height: u32) -> Annotation {
    Annotation::Rectangle {
        rect: crate::geom::RectLike::new(0.0, 0.0, width as f32, height as f32),
        color: crate::annotation::Rgba::OPAQUE_BLACK,
        line_width: 0.0,
        style: crate::annotation::ShapeStyle {
            fill: crate::annotation::FillMode::Filled,
            ..crate::annotation::ShapeStyle::default()
        },
    }
}

/// What phase one records to draw, should the scrub's own image never
/// land: what is already pending on the image on disk (`on_disk`: its
/// frame, size, and whether it is already redacted), plus what this
/// request bakes, placed on it:
///
/// - Nothing new is redacted and the image on disk is already redacted:
///   it is safe as it is. The request's other annotations are added in
///   the same frame; in another frame nothing is (a failed rotate of a
///   redacted capture puts the capture back unchanged, never blacked out).
/// - New redactions in the same frame and size: drawn as baked.
/// - New redactions that can't be placed (another frame or size), or an
///   unredacted image with nothing to hide it, or a request whose image
///   holds redactions the image on disk lacks: a whole-image cover (fail
///   closed: never an unredacted pixel, never a lost image).
///
/// An unknown image on disk (`None`) gets the cover at whatever size it
/// turns out to be.
fn pending_on(on_disk: Option<PendingImage>, record: &CaptureRecord) -> PendingImage {
    let request = record
        .pending_image
        .clone()
        .unwrap_or_else(|| PendingImage {
            geometry_id: record.geometry_id.clone(),
            width: record.width_px,
            height: record.height_px,
            bake: record.redactions.clone(),
            already_redacted: false,
        });
    let Some(mut disk) = on_disk else {
        return PendingImage {
            geometry_id: None,
            width: 0,
            height: 0,
            bake: Vec::new(),
            already_redacted: false,
        };
    };
    let same = request.geometry_id == disk.geometry_id
        && (request.width, request.height) == (disk.width, disk.height);
    let bake: Vec<Annotation> = request
        .bake
        .into_iter()
        .filter(|a| !matches!(a, Annotation::Crop { .. }))
        .collect();
    let redacts = crate::scrub::has_redaction(&bake);
    let cover = || vec![whole_image_cover(disk.width, disk.height)];
    let add = match (disk.already_redacted, redacts, same) {
        (true, false, true) => bake,
        (true, false, false) => Vec::new(),
        (true, true, true) => bake,
        (true, true, false) => cover(),
        (false, true, true) if !request.already_redacted => bake,
        (false, _, _) => cover(),
    };
    disk.bake.extend(add);
    disk
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
            redacted: false,
            redactions: Vec::new(),
            redactions_complete: false,
            scrub_pending: false,
            pending_image: None,
            geometry_id: None,
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
    /// An update with `redacted: false` onto a stored record with
    /// `redacted: true` is ignored: only a writer that knows the record
    /// was scrubbed may change it (see [`Self::rewrite`]).
    ///
    /// Best-effort: a record that's been retention-pruned before
    /// `update` runs is silently ignored.
    fn update(&self, record: &CaptureRecord) -> Result<(), HistoryError>;
    /// Replace an existing record wholesale: its PNG (atomically), its
    /// thumbnail (regenerated from `png`) and its sidecar, written
    /// exactly as given — unlike [`Self::update`], `ocr_text: None`
    /// clears the stored text. Used by the editor to scrub a record
    /// after a redaction and to restore it when the redactions are
    /// undone.
    ///
    /// Never delete-before-replace: every image change renames a fully
    /// written (fsynced) file over the old one, so the record always has
    /// an image. A scrub (`record.redacted`) fails closed: it first does
    /// [`Self::begin_scrub`] (the record withheld, its unredacted image
    /// set aside), then renames the redacted image and thumbnail into
    /// place, then clears `scrub_pending`, then removes the set-aside
    /// image. A failure at any point leaves at worst a withheld record
    /// whose pending redactions are finished later
    /// ([`Self::finish_pending_scrub`]), never the unredacted image or
    /// text served. A restore (`redacted: false`) puts the image in place
    /// first, then the sidecar.
    ///
    /// `Ok(false)` when the record doesn't exist (not saved yet, or
    /// pruned): nothing is written and the caller must keep the request.
    ///
    /// Convenience for [`Self::stage_image`] + [`Self::commit_rewrite`]
    /// (thumbnail decoded from `png`). Callers that care about lock hold
    /// times stage first, outside their own locks.
    fn rewrite(&self, record: &CaptureRecord, png: &[u8]) -> Result<bool, HistoryError> {
        let thumb = thumbnail_png_from_bytes(png);
        let staged = self.stage_image(record, png, thumb.as_deref())?;
        self.commit_rewrite(record, staged)
    }
    /// Write `png` (and `thumb`, when given) to staging files next to
    /// `record`'s final files: the slow part of a write (big file,
    /// fsync), done without the index lock. Nothing is visible to
    /// readers until a commit renames them into place; an uncommitted
    /// [`StagedImage`] removes its files when dropped, and
    /// [`Self::sweep_staging`] removes any a crash left behind.
    fn stage_image(
        &self,
        record: &CaptureRecord,
        png: &[u8],
        thumb: Option<&[u8]>,
    ) -> Result<StagedImage, HistoryError>;
    /// The quick part of [`Self::rewrite`]: under the index lock, rename
    /// `staged` into place and write the sidecar (see `rewrite` for the
    /// fail-closed order of a scrub). `Ok(false)` when the record doesn't
    /// exist; the staged files are then discarded.
    fn commit_rewrite(
        &self,
        record: &CaptureRecord,
        staged: StagedImage,
    ) -> Result<bool, HistoryError>;
    /// The quick part of [`Self::save`]: under the index lock, rename
    /// `staged` into place and write the sidecar and index entry. An
    /// unredacted save onto a record stored as redacted only registers
    /// the index entry.
    fn commit_save(&self, record: &CaptureRecord, staged: StagedImage) -> Result<(), HistoryError>;
    /// Remove staging files a crash left behind (older than a few
    /// minutes, so another process's in-flight write is left alone).
    fn sweep_staging(&self);
    /// Whether `record` is redacted as stored (`Ok(false)` when it isn't
    /// stored). An error means its state is unknown: callers treat it as
    /// redacted (fail closed).
    fn stored_redacted(&self, record: &CaptureRecord) -> Result<bool, HistoryError>;
    /// Phase one of a scrub, cheap (no image encode): mark first — write
    /// `record`'s sidecar (masked text, `redacted`) with `scrub_pending`
    /// and the redactions to draw should its image never land
    /// ([`CaptureRecord::pending_image`]) — then set the unredacted PNG
    /// aside (a rename, never a delete) and remove its thumbnail. From
    /// then on History and MCP serve no unredacted content for this
    /// record. `Ok(false)` when the record doesn't exist.
    fn begin_scrub(&self, record: &CaptureRecord) -> Result<bool, HistoryError>;
    /// Finish record `id`'s pending scrub whose own image never landed:
    /// draw its pending redactions onto the image set aside (the core
    /// render, as the editor draws them) and swap that in like any other
    /// image write; the record is then redacted and served again.
    /// `Ok(false)` when there is nothing to finish (not pending, no image
    /// to draw on, or the record changed meanwhile).
    fn finish_pending_scrub(&self, _id: Uuid) -> Result<bool, HistoryError> {
        Ok(false)
    }
    /// [`Self::finish_pending_scrub`] for every pending record (at
    /// launch: a crash or quit mid-scrub). Returns how many it finished.
    fn finish_pending_scrubs(&self) -> usize {
        0
    }
    /// Store recognized text for `record`. A scrubbed record gets `doc`
    /// masked with its stored redactions (only when `doc` has positions or
    /// no text is stored yet, and never without stored redactions — the
    /// text stays as is rather than risk leaking). An unredacted record
    /// gets `doc.layout_text` only when `fill_unredacted`. Annotations and
    /// everything else stored are kept. No-op for a missing record, and
    /// no-op when `record.geometry_id` differs from the stored one (`doc`
    /// was recognized in another document frame than the stored image
    /// and redactions).
    fn record_recognized_text(
        &self,
        record: &CaptureRecord,
        doc: &TextDocument,
        fill_unredacted: bool,
    ) -> Result<(), HistoryError>;
    /// Whether `record` exists (its sidecar is on disk).
    fn contains(&self, record: &CaptureRecord) -> bool;
    /// Hard-delete a single record by id (PNG + sidecar JSON + index
    /// entry). Best-effort: deleting a record that's already gone is
    /// not an error.
    fn delete(&self, id: Uuid) -> Result<(), HistoryError>;
}

/// Image files written by [`HistoryStore::stage_image`], waiting to be
/// renamed into place by a commit. Removes whatever is still at its
/// paths when dropped, so a staged file never outlives a failed, stale or
/// abandoned write (a crash is covered by [`HistoryStore::sweep_staging`]).
#[derive(Debug)]
pub struct StagedImage {
    png: PathBuf,
    thumb: Option<PathBuf>,
}

impl Drop for StagedImage {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.png);
        if let Some(thumb) = &self.thumb {
            let _ = fs::remove_file(thumb);
        }
    }
}

/// The History thumbnail of `img`, PNG-encoded: scaled down to fit the
/// thumbnail box (aspect kept), without copying the full image. `None`
/// if encoding fails.
pub fn thumbnail_png(img: &image::RgbaImage) -> Option<Vec<u8>> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    // Never upscaled: a capture smaller than the box is its own
    // thumbnail (upscaling also smears alpha to 254 at block edges).
    let ratio = f64::min(
        f64::from(THUMBNAIL_MAX_WIDTH) / f64::from(w),
        f64::from(THUMBNAIL_MAX_HEIGHT) / f64::from(h),
    )
    .min(1.0);
    let tw = ((f64::from(w) * ratio).round() as u32).max(1);
    let th = ((f64::from(h) * ratio).round() as u32).max(1);
    let thumb = image::imageops::thumbnail(img, tw, th);
    crate::encode_png(&thumb).ok()
}

/// [`thumbnail_png`] of an encoded PNG (decodes it first).
pub fn thumbnail_png_from_bytes(png: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(png).ok()?.to_rgba8();
    thumbnail_png(&img)
}

/// Staging files older than this are leftovers of a crash.
const STALE_STAGING_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

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
        // A corrupt index used to permanently wedge every future save
        // (the parse error propagated out of `read_index`, which every
        // read-modify-write calls first). Self-heal instead: quarantine
        // the bad file and rebuild the index from the sidecar JSON files
        // that are the real source of truth.
        let mut index: HistoryIndex = match serde_json::from_str(&content) {
            Ok(index) => index,
            Err(_) => return Ok(self.recover_corrupt_index(&path)),
        };
        if index.schema_version < HISTORY_SCHEMA_VERSION {
            index.schema_version = HISTORY_SCHEMA_VERSION;
        }
        Ok(index)
    }

    /// Move a corrupt `history.index.json` aside (renamed with a
    /// timestamp) and rebuild a fresh index from the on-disk sidecars.
    /// Best-effort: rename/write failures degrade to an in-memory
    /// rebuild that is still correct for the current call.
    fn recover_corrupt_index(&self, path: &Path) -> HistoryIndex {
        let ts = Utc::now().format("%Y%m%dT%H%M%S%.6f");
        let quarantine = self
            .root
            .join(format!("{HISTORY_INDEX_FILENAME}.corrupt-{ts}"));
        let _ = fs::rename(path, &quarantine);
        let rebuilt = self.rebuild_index_from_disk();
        // Persist so the next read starts clean; the in-memory rebuild
        // is still returned even if this write fails.
        let _ = self.write_index(&rebuilt);
        // Redaction policy (see `log.rs`): log the count and the fact,
        // never the $HOME-rooted path or any record content.
        tracing::warn!(
            target: crate::log::cat::HISTORY,
            "history index was corrupt; quarantined it and rebuilt {} record(s) from sidecars",
            rebuilt.records.len()
        );
        rebuilt
    }

    /// Reconstruct the index by scanning `<root>/YYYY/MM/*.json`
    /// sidecars. Each sidecar is the source of truth for one record, and
    /// the derived png/json paths are always the canonical, safe
    /// relative layout.
    fn rebuild_index_from_disk(&self) -> HistoryIndex {
        let mut records = Vec::new();
        if let Ok(year_iter) = fs::read_dir(&self.root) {
            for year in year_iter.flatten() {
                let ypath = year.path();
                if !ypath.is_dir() {
                    continue;
                }
                let Ok(month_iter) = fs::read_dir(&ypath) else {
                    continue;
                };
                for month in month_iter.flatten() {
                    let mpath = month.path();
                    if !mpath.is_dir() {
                        continue;
                    }
                    let Ok(file_iter) = fs::read_dir(&mpath) else {
                        continue;
                    };
                    for file in file_iter.flatten() {
                        let fpath = file.path();
                        if fpath.extension().and_then(|e| e.to_str()) != Some("json") {
                            continue;
                        }
                        let Ok(content) = fs::read_to_string(&fpath) else {
                            continue;
                        };
                        let Ok(record) = serde_json::from_str::<CaptureRecord>(&content) else {
                            continue;
                        };
                        let (rel_png, rel_json) = Self::record_paths(&record);
                        records.push(HistoryIndexEntry {
                            id: record.id,
                            captured_at: record.captured_at,
                            png_path: rel_png,
                            json_path: rel_json,
                        });
                    }
                }
            }
        }
        HistoryIndex {
            schema_version: HISTORY_SCHEMA_VERSION,
            updated_at: Some(Utc::now()),
            records,
        }
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

    /// Where a withheld record's unredacted image waits (same folder, so
    /// setting it aside and putting it back are atomic renames) until its
    /// redacted image replaces it. Never served.
    pub fn withheld_path(record: &CaptureRecord) -> PathBuf {
        let (png, _) = Self::record_paths(record);
        png.with_file_name(format!("{}.withheld.png", record.id))
    }

    pub fn thumbnail_path(record: &CaptureRecord) -> PathBuf {
        let year = record.captured_at.year();
        let month = record.captured_at.month();
        PathBuf::from(format!("{year:04}"))
            .join(format!("{month:02}"))
            .join(format!("{}.thumb.png", record.id))
    }

    /// Remove every staging file of `record` (`<id>.*.stage*.png`): a
    /// crash can leave one holding unredacted bytes, and a deleted or
    /// pruned record must not keep any of its image data on disk.
    fn remove_staging_for(&self, record: &CaptureRecord) {
        let abs_png = self.root.join(Self::png_path(record));
        let Some(dir) = abs_png.parent() else {
            return;
        };
        let Ok(files) = fs::read_dir(dir) else {
            return;
        };
        let prefix = format!("{}.", record.id);
        for file in files.flatten() {
            let name = file.file_name();
            let name = name.to_string_lossy();
            let staging = name.ends_with(".stage.png") || name.ends_with(".stage-thumb.png");
            if staging && name.starts_with(&prefix) {
                let _ = fs::remove_file(file.path());
            }
        }
    }

    /// Rename a staged PNG (and thumbnail) into place. The staged PNG was
    /// fully written and fsynced by `stage_image`, so the rename can't
    /// leave a torn image. A missing staged thumbnail removes the old one
    /// (never left showing a previous image); `list` backfills it.
    fn place_staged(
        &self,
        record: &CaptureRecord,
        staged: &StagedImage,
    ) -> Result<(), HistoryError> {
        let abs_png = self.root.join(Self::png_path(record));
        fs::rename(&staged.png, &abs_png)?;
        let abs_thumb = self.root.join(Self::thumbnail_path(record));
        let thumb_placed = staged
            .thumb
            .as_ref()
            .is_some_and(|t| fs::rename(t, &abs_thumb).is_ok());
        if !thumb_placed {
            let _ = fs::remove_file(&abs_thumb);
        }
        crate::fs_atomic::sync_parent_dir(&abs_png);
        Ok(())
    }

    /// `record`'s stored sidecar: `Ok(None)` when there is none, `Err`
    /// when it exists but can't be read or parsed (logged). Guards must
    /// fail closed on `Err` — treat the record as scrubbed and refuse to
    /// write text or pixels over it — never take it for "no record".
    fn read_sidecar(&self, record: &CaptureRecord) -> Result<Option<CaptureRecord>, HistoryError> {
        let unreadable = |e: HistoryError| {
            tracing::warn!(
                target: crate::log::cat::HISTORY,
                "history record {} has an unreadable sidecar; treating it as redacted: {e}",
                record.id
            );
            e
        };
        let text = match fs::read_to_string(self.root.join(Self::json_path(record))) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(unreadable(e.into())),
        };
        serde_json::from_str::<CaptureRecord>(&text)
            .map(Some)
            .map_err(|e| unreadable(e.into()))
    }

    /// Write `record`'s sidecar — without text when it may hold none
    /// ([`text_allowed`]).
    fn write_sidecar(&self, record: &CaptureRecord) -> Result<(), HistoryError> {
        let mut record = record.clone();
        if !record.scrub_pending {
            record.pending_image = None;
        }
        drop_unloadable_annotations(&mut record);
        drop_unknown_text(&mut record);
        let json_content = serde_json::to_string_pretty(&record)?;
        write_atomic(
            &self.root.join(Self::json_path(&record)),
            json_content.as_bytes(),
        )?;
        Ok(())
    }

    /// The text a scrub of `record` stores. `ocr_text: None` on a scrub
    /// means "not known yet" (the precise text lands later through
    /// `record_recognized_text`): keep text already stored for a scrub
    /// with the same redactions in the same document frame, since it is
    /// masked exactly as needed. Anything else — no stored scrub, other
    /// redactions, another frame — is dropped.
    fn scrub_text(&self, record: &CaptureRecord) -> Option<String> {
        if record.ocr_text.is_some() {
            return record.ocr_text.clone();
        }
        // An unreadable sidecar keeps no text (fail closed).
        self.read_sidecar(record)
            .ok()
            .flatten()
            .filter(|stored| {
                stored.redacted
                    && stored.redactions_complete
                    && record.redactions_complete
                    && stored.redactions == record.redactions
                    && stored.geometry_id == record.geometry_id
            })
            .and_then(|stored| stored.ocr_text)
    }

    /// Persist [`migrate_legacy_record`] for `record`: under the index
    /// lock, the sidecar is read again (a writer may have changed it since
    /// it was listed), migrated and written back. Returns the record to
    /// serve, `None` when it is gone. If the lock or the write fails, the
    /// migrated record is still returned (it is migrated again next time):
    /// the old text is never served.
    fn migrate_on_disk(&self, migrated: &CaptureRecord) -> Option<CaptureRecord> {
        let Ok(_guard) = self.lock_index() else {
            return Some(migrated.clone());
        };
        if !self.root.join(Self::json_path(migrated)).exists() {
            return None;
        }
        let mut stored = match self.read_sidecar(migrated) {
            Ok(Some(stored)) => stored,
            Ok(None) => return None,
            // Unreadable now: serve the record already migrated in memory.
            Err(_) => return Some(migrated.clone()),
        };
        if migrate_legacy_record(&mut stored) {
            if let Err(e) = self.write_sidecar(&stored) {
                tracing::warn!(
                    target: crate::log::cat::HISTORY,
                    "couldn't persist the migration of history record {}: {e}",
                    stored.id
                );
            }
        }
        Some(stored)
    }

    /// [`HistoryStore::begin_scrub`] with the index lock held.
    fn begin_scrub_locked(&self, record: &CaptureRecord) -> Result<bool, HistoryError> {
        if !self.root.join(Self::json_path(record)).exists() {
            return Ok(false);
        }
        // An unreadable sidecar says nothing about the image on disk: its
        // pending redactions become a whole-image cover (fail closed).
        let stored = self.read_sidecar(record).ok().flatten();
        let on_disk = match &stored {
            Some(s) if s.scrub_pending && s.pending_image.is_some() => s.pending_image.clone(),
            Some(s) => Some(PendingImage {
                geometry_id: s.geometry_id.clone(),
                width: s.width_px,
                height: s.height_px,
                bake: Vec::new(),
                // Only a redaction that landed: a record marked pending
                // without a pending image (an older build, or a crash) may
                // still hold the unredacted image.
                already_redacted: s.redacted && !s.scrub_pending,
            }),
            None => None,
        };
        let mut pending = record.clone();
        pending.redacted = true;
        pending.scrub_pending = true;
        pending.ocr_text = self.scrub_text(record);
        pending.pending_image = Some(pending_on(on_disk, record));
        // The thumbnail first (derived; rebuilt from the redacted image),
        // so none of the unredacted image outlives the mark, whatever
        // happens after it.
        remove_if_exists(&self.root.join(Self::thumbnail_path(record)))?;
        // Mark: from this write on, nothing of it is served.
        self.write_sidecar(&pending)?;
        // Then set the unredacted image aside (it replaces an older one
        // set aside, which is older still).
        let png = self.root.join(Self::png_path(record));
        let set_aside = match fs::rename(&png, self.root.join(Self::withheld_path(record))) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(HistoryError::from(e)),
        };
        crate::fs_atomic::sync_parent_dir(&png);
        set_aside?;
        Ok(true)
    }

    /// Remove every set-aside image whose record is gone (no sidecar): a
    /// crash in a delete or retention pass, or an older build's, must not
    /// leave an unredacted copy on disk for good.
    fn remove_orphaned_withheld(&self) {
        let Ok(_index_guard) = self.lock_index() else {
            return;
        };
        let Ok(years) = fs::read_dir(&self.root) else {
            return;
        };
        for year in years.flatten().filter(|e| e.path().is_dir()) {
            let Ok(months) = fs::read_dir(year.path()) else {
                continue;
            };
            for month in months.flatten().filter(|e| e.path().is_dir()) {
                let Ok(files) = fs::read_dir(month.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let name = file.file_name();
                    let Some(id) = name
                        .to_string_lossy()
                        .strip_suffix(".withheld.png")
                        .map(String::from)
                    else {
                        continue;
                    };
                    if !month.path().join(format!("{id}.json")).exists() {
                        let _ = fs::remove_file(file.path());
                    }
                }
            }
        }
    }

    /// Remove a set-aside image once a newer image is in place.
    fn drop_withheld(&self, record: &CaptureRecord) {
        let _ = fs::remove_file(self.root.join(Self::withheld_path(record)));
    }

    /// [`HistoryStore::finish_pending_scrub`] for an index entry.
    fn finish_pending_entry(&self, entry: &HistoryIndexEntry) -> Result<bool, HistoryError> {
        let paths = Self::record_for_paths(entry);
        let json = self.root.join(Self::json_path(&paths));
        let Ok(before) = fs::read(&json) else {
            return Ok(false);
        };
        let stored: CaptureRecord = serde_json::from_slice(&before)?;
        let withheld = self.root.join(Self::withheld_path(&paths));
        if !stored.scrub_pending {
            // A crash after the replacement landed but before the
            // set-aside image went: no unredacted copy stays on disk.
            if withheld.exists() {
                let _index_guard = self.lock_index()?;
                if fs::read(&json).ok() == Some(before) {
                    self.drop_withheld(&stored);
                }
            }
            return Ok(false);
        }
        // No thumbnail of the image it hides, even if this can't finish.
        let _ = fs::remove_file(self.root.join(Self::thumbnail_path(&paths)));
        // Marked but not yet set aside (a crash between the two): the
        // image at its own path is the one to draw on.
        let source = if withheld.exists() {
            withheld
        } else {
            self.root.join(Self::png_path(&paths))
        };
        let Some(stamp) = FileStamp::of(&source) else {
            return Ok(false);
        };
        let img = image::load_from_memory(&fs::read(&source)?)
            .map_err(|e| HistoryError::Io(format!("couldn't read the image to redact: {e}")))?
            .to_rgba8();
        let pending = stored.pending_image.clone();
        // What phase one recorded for this very image; anything that
        // doesn't describe it (another size), or would leave an image not
        // already redacted with nothing hiding it, is a whole-image cover.
        let bake = match pending
            .as_ref()
            .filter(|p| (p.width, p.height) == img.dimensions())
        {
            Some(p) if p.already_redacted || crate::scrub::has_redaction(&p.bake) => p
                .bake
                .iter()
                .filter(|a| !matches!(a, Annotation::Crop { .. }))
                .cloned()
                .collect(),
            _ => vec![whole_image_cover(img.width(), img.height())],
        };
        let baked = crate::render::render(&img, &bake);
        let png = crate::png::encode_fast(&baked)
            .map_err(|e| HistoryError::Io(format!("couldn't encode the redacted image: {e}")))?;
        let thumb = thumbnail_png(&baked);
        let staged = self.stage_image(&stored, &png, thumb.as_deref())?;
        let _index_guard = self.lock_index()?;
        // Changed meanwhile (a newer scrub or write): theirs stands.
        if fs::read(&json).ok() != Some(before) || FileStamp::of(&source) != Some(stamp) {
            return Ok(false);
        }
        let mut done = stored.clone();
        done.scrub_pending = false;
        done.pending_image = None;
        done.redacted = true;
        done.geometry_id = pending.map_or(stored.geometry_id.clone(), |p| p.geometry_id);
        done.width_px = baked.width();
        done.height_px = baked.height();
        done.redactions = bake.into_iter().filter(Annotation::is_redaction).collect();
        // Not provably every redaction in the image: no text is kept.
        done.redactions_complete = false;
        done.ocr_text = None;
        done.annotation_model.clear();
        self.place_staged(&stored, &staged)?;
        self.write_sidecar(&done)?;
        self.drop_withheld(&stored);
        tracing::info!(
            target: crate::log::cat::HISTORY,
            "finished the pending redaction of history record {}",
            stored.id
        );
        Ok(true)
    }

    /// Remove an index entry's PNG and sidecar, only where their stored
    /// paths stay inside the root: lexically ([`safe_relative_join`]) and
    /// on disk ([`Self::resolves_inside_root`], so a symlinked directory
    /// inside the root is never followed out of it). A tampered index
    /// must not be able to delete arbitrary files. A symlink at the leaf
    /// is only unlinked (`remove_file` never follows it).
    fn remove_entry_files(&self, entry: &HistoryIndexEntry) {
        for rel in [&entry.png_path, &entry.json_path] {
            match safe_relative_join(&self.root, rel).filter(|p| self.resolves_inside_root(p)) {
                Some(path) => {
                    let _ = fs::remove_file(path);
                }
                None => tracing::warn!(
                    target: crate::log::cat::HISTORY,
                    "not removing {} of history record {}: its path escapes the history root",
                    rel.display(),
                    entry.id
                ),
            }
        }
    }

    /// Why an entry's files can't be removed where they are, though its
    /// index paths are well formed: its folder (or the thumbnail's)
    /// resolves outside the history root — a symlinked `YYYY/MM`, or a
    /// root that can't be resolved. `None` when they can be removed (or
    /// a path is malformed: a tampered entry, dropped with a log).
    fn removal_refused(&self, entry: &HistoryIndexEntry) -> Option<HistoryError> {
        let thumb = self
            .root
            .join(Self::thumbnail_path(&Self::record_for_paths(entry)));
        let path = [&entry.png_path, &entry.json_path]
            .into_iter()
            .filter_map(|rel| safe_relative_join(&self.root, rel))
            .chain(std::iter::once(thumb))
            .find(|p| !self.resolves_inside_root(p))?;
        // Say which it is: a folder that can't be read is not a link.
        if let Err(e) = fs::canonicalize(&self.root) {
            return Some(HistoryError::Io(format!(
                "the history folder {} can't be resolved: {e}",
                self.root.display()
            )));
        }
        if let Some(Err(e)) = path.parent().map(fs::canonicalize) {
            return Some(HistoryError::Io(format!(
                "the folder of {} can't be resolved: {e}",
                path.display()
            )));
        }
        Some(HistoryError::OutsideRoot(format!(
            "the folder of {} resolves outside the history root (a symlink)",
            path.display()
        )))
    }

    /// The fields of a record its file paths are derived from.
    fn record_for_paths(entry: &HistoryIndexEntry) -> CaptureRecord {
        let mut r = CaptureRecord::new(entry.captured_at, 0, 0, String::new());
        r.id = entry.id;
        r
    }

    /// Remove a deleted record's thumbnail and staging files, unless their
    /// directory resolves outside the root (a symlinked `YYYY/MM`).
    fn remove_derived_files(&self, record: &CaptureRecord) {
        let thumb = self.root.join(Self::thumbnail_path(record));
        if !self.resolves_inside_root(&thumb) {
            tracing::warn!(
                target: crate::log::cat::HISTORY,
                "not removing the thumbnail or staging files of history record {}: their folder resolves outside the history root",
                record.id
            );
            return;
        }
        let _ = fs::remove_file(thumb);
        self.drop_withheld(record);
        self.remove_staging_for(record);
    }

    /// Whether `path`'s directory, with every symlink resolved, is still
    /// inside the (resolved) root. A directory that doesn't exist holds
    /// nothing to remove: `true` (the removal is then a no-op).
    fn resolves_inside_root(&self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        match (fs::canonicalize(parent), fs::canonicalize(&self.root)) {
            (Ok(dir), Ok(root)) => dir.starts_with(root),
            (Err(e), _) if e.kind() == std::io::ErrorKind::NotFound => true,
            _ => false,
        }
    }

    /// Absolute path of a record's PNG under `root`. Shared by the
    /// history browser and the MCP server so the layout is defined in
    /// exactly one place.
    pub fn abs_png_path(root: &Path, record: &CaptureRecord) -> PathBuf {
        root.join(Self::png_path(record))
    }

    /// Absolute path of a record's thumbnail under `root`.
    pub fn abs_thumbnail_path(root: &Path, record: &CaptureRecord) -> PathBuf {
        root.join(Self::thumbnail_path(record))
    }

    fn backfill_thumbnail_from_png_path(&self, record: &CaptureRecord, png_path: &Path) {
        let thumb_path = self.root.join(Self::thumbnail_path(record));
        if thumb_path.exists() {
            return;
        }
        if let Some(read) = self.read_for_backfill(record, png_path) {
            self.place_backfill(record, png_path, read);
        }
    }

    /// Step one of a thumbnail backfill: snapshot the record's sidecar and
    /// the PNG's [`FileStamp`], then read and decode the PNG — the slow
    /// part, without the index lock. `None` when the record is unreadable
    /// or mid-scrub, or the PNG changed while it was read.
    fn read_for_backfill(&self, record: &CaptureRecord, png_path: &Path) -> Option<Backfill> {
        let sidecar = fs::read(self.root.join(Self::json_path(record))).ok()?;
        let stored: CaptureRecord = serde_json::from_slice(&sidecar).ok()?;
        if stored.scrub_pending {
            return None;
        }
        let before = FileStamp::of(png_path)?;
        let png = fs::read(png_path).ok()?;
        if FileStamp::of(png_path)? != before {
            return None;
        }
        let img = image::load_from_memory(&png).ok()?;
        Some(Backfill {
            sidecar,
            png: before,
            thumb: img
                .thumbnail(THUMBNAIL_MAX_WIDTH, THUMBNAIL_MAX_HEIGHT)
                .to_rgba8(),
        })
    }

    /// Step two of [`Self::backfill_thumbnail_from_png_path`]: write the
    /// thumbnail decoded without the lock — only if it is still current.
    /// Under the index lock (the one every scrub phase holds) it re-checks,
    /// cheaply, that there is still no thumbnail, that the sidecar is
    /// byte for byte the one read before the decode (every scrub phase
    /// rewrites it), and that the PNG's [`FileStamp`] is unchanged (a
    /// scrub's rename always changes it). Anything else makes the result
    /// stale: it is discarded, never written from pre-redaction pixels.
    fn place_backfill(&self, record: &CaptureRecord, png_path: &Path, read: Backfill) {
        let Ok(_guard) = self.lock_index() else {
            return;
        };
        let abs_thumb = self.root.join(Self::thumbnail_path(record));
        if abs_thumb.exists() {
            return;
        }
        if fs::read(self.root.join(Self::json_path(record))).ok() != Some(read.sidecar) {
            return;
        }
        if FileStamp::of(png_path) != Some(read.png) {
            return;
        }
        let Some(bytes) = thumbnail_bytes(&read.thumb) else {
            return;
        };
        if let Some(parent) = abs_thumb.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = write_atomic(&abs_thumb, &bytes);
    }

    /// Acquire the in-process mutex only. Used by `clear_all`, which
    /// wipes the whole root (including the on-disk lock file) and so
    /// can't meaningfully hold a file lock on it.
    fn lock_mutex(&self) -> Result<std::sync::MutexGuard<'_, ()>, HistoryError> {
        self.index_lock
            .lock()
            .map_err(|_| HistoryError::Io("history index lock poisoned".into()))
    }

    /// Acquire the full index guard for one read-modify-write cycle: the
    /// in-process mutex *and* a cross-process advisory lock on
    /// `<root>/history.index.lock`.
    ///
    /// The mutex is taken first so at most one thread in this process
    /// contends for the file lock; the file lock then serialises
    /// separate processes that share the same history root (e.g. the GUI
    /// app and a `readshot capture` CLI invocation running at once).
    /// Without it, two processes could interleave the read → mutate →
    /// write of `history.index.json` and lose entries.
    fn lock_index(&self) -> Result<IndexGuard<'_>, HistoryError> {
        let mutex = self.lock_mutex()?;
        fs::create_dir_all(&self.root)?;
        let lock_path = self.root.join(HISTORY_LOCK_FILENAME);
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        // Blocks until any other process holding the lock releases it.
        file.lock()
            .map_err(|e| HistoryError::Io(format!("history index file lock: {e}")))?;
        Ok(IndexGuard {
            _file: file,
            _mutex: mutex,
        })
    }
}

/// Guard for one history read-modify-write cycle. Field order matters:
/// `_file` is dropped first, releasing the OS advisory lock (by closing
/// the handle) before `_mutex` releases the in-process lock.
struct IndexGuard<'a> {
    _file: fs::File,
    _mutex: std::sync::MutexGuard<'a, ()>,
}

impl HistoryStore for FsHistoryStore {
    fn save(&self, record: &CaptureRecord, png: &[u8]) -> Result<(), HistoryError> {
        let thumb = thumbnail_png_from_bytes(png);
        let staged = self.stage_image(record, png, thumb.as_deref())?;
        self.commit_save(record, staged)
    }

    fn stage_image(
        &self,
        record: &CaptureRecord,
        png: &[u8],
        thumb: Option<&[u8]>,
    ) -> Result<StagedImage, HistoryError> {
        static STAGE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let abs_png = self.root.join(Self::png_path(record));
        let dir = abs_png.parent().unwrap_or(&self.root).to_path_buf();
        fs::create_dir_all(&dir)?;
        let n = STAGE_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stem = format!("{}.{}-{n}", record.id, std::process::id());
        let mut staged = StagedImage {
            png: dir.join(format!("{stem}.stage.png")),
            thumb: None,
        };
        write_atomic(&staged.png, png)?;
        if let Some(bytes) = thumb {
            let path = dir.join(format!("{stem}.stage-thumb.png"));
            if write_atomic(&path, bytes).is_ok() {
                staged.thumb = Some(path);
            }
        }
        Ok(staged)
    }

    fn stored_redacted(&self, record: &CaptureRecord) -> Result<bool, HistoryError> {
        let _index_guard = self.lock_index()?;
        Ok(self
            .read_sidecar(record)?
            .is_some_and(|stored| stored.redacted))
    }

    fn commit_save(&self, record: &CaptureRecord, staged: StagedImage) -> Result<(), HistoryError> {
        let _index_guard = self.lock_index()?;
        let (rel_png, rel_json) = Self::record_paths(record);
        // Never put unredacted content back over a scrubbed record: an
        // unredacted save onto one only (re)registers the index entry
        // (the staged files are dropped).
        // An unreadable sidecar may be a scrubbed record: refuse (fail
        // closed).
        let keep_scrubbed = !record.redacted
            && self
                .read_sidecar(record)?
                .is_some_and(|stored| stored.redacted);
        let mut index = self.read_index()?;
        // A new record whose save fails part-way leaves none of its files:
        // the caller reports it as not saved, and an index rebuild must
        // not bring it back.
        let is_new = !index.records.iter().any(|entry| entry.id == record.id);
        let undo_placed = |placed: bool| {
            if placed && is_new {
                for rel in [&rel_png, &rel_json, &Self::thumbnail_path(record)] {
                    let _ = fs::remove_file(self.root.join(rel));
                }
            }
        };
        if !keep_scrubbed {
            let placed = self
                .place_staged(record, &staged)
                .and_then(|()| self.write_sidecar(record));
            if let Err(e) = placed {
                undo_placed(true);
                return Err(e);
            }
        }
        drop(staged);

        index.records.retain(|entry| entry.id != record.id);
        index.records.push(HistoryIndexEntry {
            id: record.id,
            captured_at: record.captured_at,
            png_path: rel_png.clone(),
            json_path: rel_json.clone(),
        });
        index.updated_at = Some(Utc::now());
        if let Err(e) = self.write_index(&index) {
            undo_placed(!keep_scrubbed);
            return Err(e);
        }
        Ok(())
    }

    fn sweep_staging(&self) {
        let cutoff = std::time::SystemTime::now().checked_sub(STALE_STAGING_AGE);
        let Some(cutoff) = cutoff else {
            return;
        };
        let Ok(years) = fs::read_dir(&self.root) else {
            return;
        };
        for year in years.flatten().filter(|e| e.path().is_dir()) {
            let Ok(months) = fs::read_dir(year.path()) else {
                continue;
            };
            for month in months.flatten().filter(|e| e.path().is_dir()) {
                let Ok(files) = fs::read_dir(month.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let name = file.file_name();
                    let name = name.to_string_lossy();
                    let staging =
                        name.ends_with(".stage.png") || name.ends_with(".stage-thumb.png");
                    let stale = file
                        .metadata()
                        .and_then(|m| m.modified())
                        .is_ok_and(|t| t < cutoff);
                    if staging && stale {
                        let _ = fs::remove_file(file.path());
                    }
                }
            }
        }
    }

    fn list(&self) -> Result<Vec<CaptureRecord>, HistoryError> {
        let index = self.read_index()?;
        let mut entries = index.records;
        // Newest first.
        entries.sort_by_key(|e| std::cmp::Reverse(e.captured_at));

        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            // Validate the stored relative paths before joining them to
            // the root: reject absolute paths and any `..`/root/prefix
            // component so a tampered or corrupt index can't make list /
            // thumbnail I/O touch files outside the history directory.
            let (Some(json_abs), Some(png_abs)) = (
                safe_relative_join(&self.root, &entry.json_path),
                safe_relative_join(&self.root, &entry.png_path),
            ) else {
                tracing::warn!(
                    target: crate::log::cat::HISTORY,
                    "skipping history index entry {} whose stored path escapes the history root",
                    entry.id
                );
                continue;
            };
            // Skip orphaned index entries — the file was deleted by the
            // user or a torn write left the index ahead of the data.
            if !json_abs.exists() {
                continue;
            }
            let content = match fs::read_to_string(&json_abs) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let parsed = serde_json::from_str::<CaptureRecord>(&content);
            if let Err(e) = &parsed {
                // Not served (it may be a scrubbed record): skipped, logged.
                tracing::warn!(
                    target: crate::log::cat::HISTORY,
                    "skipping history record {} whose sidecar can't be parsed: {e}",
                    entry.id
                );
            }
            if let Ok(mut record) = parsed {
                // Written by an older build: migrated before it is
                // served, so its old text never leaves this function.
                if migrate_legacy_record(&mut record) {
                    match self.migrate_on_disk(&record) {
                        Some(stored) => record = stored,
                        None => continue, // deleted meanwhile
                    }
                }
                // A withheld record (scrub pending) is served without its
                // text (and has no image at its path, so nothing to
                // backfill) until its redacted image lands.
                if record.scrub_pending {
                    record.ocr_text = None;
                }
                self.backfill_thumbnail_from_png_path(&record, &png_abs);
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
                if let Some(why) = self.removal_refused(entry) {
                    // Retention still drops the entry (it is past the
                    // policy), but says what stays on disk and why.
                    tracing::warn!(
                        target: crate::log::cat::HISTORY,
                        "retention dropped history record {} but left its files: {why}",
                        entry.id
                    );
                }
                let record_for_path = CaptureRecord {
                    id: entry.id,
                    captured_at: entry.captured_at,
                    width_px: 0,
                    height_px: 0,
                    display_id: String::new(),
                    ocr_text: None,
                    annotation_model: Vec::new(),
                    redacted: false,
                    redactions: Vec::new(),
                    redactions_complete: false,
                    scrub_pending: false,
                    pending_image: None,
                    geometry_id: None,
                };
                // Image files (a set-aside unredacted copy among them)
                // before the sidecar: a crash between leaves none behind.
                self.remove_derived_files(&record_for_path);
                self.remove_entry_files(entry);
            }
        }

        index.records = kept;
        index.updated_at = Some(now);
        self.write_index(&index)?;
        Ok(())
    }

    fn clear_all(&self) -> Result<(), HistoryError> {
        let _index_guard = self.lock_mutex()?;
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
        // An unreadable sidecar may be a scrubbed record, and nothing
        // tells which frame it holds: refuse every write (fail closed).
        let prev = self.read_sidecar(record)?;
        // A scrubbed record is only ever changed by a writer that knows
        // it is scrubbed (`rewrite`, or an update carrying
        // `redacted: true`). A late writer that doesn't — the capture's
        // background OCR landing after the editor redacted — would put
        // the unmasked text and the pre-flatten annotations back.
        if prev.as_ref().is_some_and(|p| p.redacted) && !record.redacted {
            return Ok(());
        }
        // A sidecar sync made for another document frame than the stored
        // image (a rewrite to a new frame landed meanwhile, or failed) must
        // not relabel it: its annotations and frame id belong elsewhere.
        if prev
            .as_ref()
            .is_some_and(|p| p.geometry_id != record.geometry_id)
        {
            return Ok(());
        }
        let mut merged = record.clone();
        if let Some(prev) = prev {
            if merged.ocr_text.is_none() {
                merged.ocr_text = prev.ocr_text;
            }
            // A sync knows nothing of a pending scrub: only an image swap
            // (a commit, or finishing it) may end one. Clearing it here
            // would leave a record with its image set aside and nothing to
            // finish it.
            if prev.scrub_pending {
                merged.scrub_pending = true;
                merged.pending_image = prev.pending_image;
            }
        }
        self.write_sidecar(&merged)?;
        // The index is deliberately not touched: `update` changes only
        // the sidecar, nothing reads the index's `updated_at`, and the
        // editor calls this on every annotation edit — a full index
        // read-rewrite (two more fsyncs) per keystroke is pure waste.
        Ok(())
    }

    fn commit_rewrite(
        &self,
        record: &CaptureRecord,
        staged: StagedImage,
    ) -> Result<bool, HistoryError> {
        let _index_guard = self.lock_index()?;
        let mut done = record.clone();
        done.scrub_pending = false;
        done.pending_image = None;
        if record.redacted {
            done.ocr_text = self.scrub_text(record);
            // Fail closed: nothing unredacted is served past here,
            // whatever happens to the image write below.
            if !self.begin_scrub_locked(record)? {
                return Ok(false);
            }
        } else if self.read_sidecar(record)?.is_none() {
            // Gone, or (`Err`) unreadable and so maybe scrubbed: an
            // unredacted image never goes over it.
            return Ok(false);
        }
        // The new image replaces the old one in one rename; only then does
        // the sidecar say so, and only then does a set-aside image go.
        self.place_staged(record, &staged)?;
        self.write_sidecar(&done)?;
        self.drop_withheld(record);
        Ok(true)
    }

    fn begin_scrub(&self, record: &CaptureRecord) -> Result<bool, HistoryError> {
        let _index_guard = self.lock_index()?;
        self.begin_scrub_locked(record)
    }

    fn finish_pending_scrub(&self, id: Uuid) -> Result<bool, HistoryError> {
        let index = self.read_index()?;
        match index.records.iter().find(|e| e.id == id) {
            Some(entry) => self.finish_pending_entry(entry),
            None => Ok(false),
        }
    }

    fn finish_pending_scrubs(&self) -> usize {
        self.remove_orphaned_withheld();
        let Ok(index) = self.read_index() else {
            return 0;
        };
        let mut finished = 0;
        for entry in &index.records {
            match self.finish_pending_entry(entry) {
                Ok(true) => finished += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(
                    target: crate::log::cat::HISTORY,
                    "couldn't finish the pending redaction of history record {}: {e}",
                    entry.id
                ),
            }
        }
        finished
    }

    fn record_recognized_text(
        &self,
        record: &CaptureRecord,
        doc: &TextDocument,
        fill_unredacted: bool,
    ) -> Result<(), HistoryError> {
        use crate::scrub::{has_redaction, masked_text};
        let _index_guard = self.lock_index()?;
        let Some(mut stored) = self.read_sidecar(record)? else {
            return Ok(());
        };
        // Text recognized for one document frame must never be masked
        // with — or stored over — a record holding another frame.
        if stored.geometry_id != record.geometry_id {
            return Ok(());
        }
        if stored.redacted {
            // Unknown redactions: nothing recognized may be stored.
            if !stored.redactions_complete {
                return Ok(());
            }
            if !has_redaction(&stored.redactions) {
                return Ok(());
            }
            // Only positioned text can be masked: without positions
            // nothing can say which words a redaction hides, so nothing
            // is stored (`masked_text` is `None`).
            let Some(text) = masked_text(doc, &stored.redactions) else {
                return Ok(());
            };
            stored.ocr_text = Some(text);
        } else if fill_unredacted {
            stored.ocr_text = Some(doc.layout_text.clone());
        } else {
            return Ok(());
        }
        self.write_sidecar(&stored)
    }

    fn contains(&self, record: &CaptureRecord) -> bool {
        self.root.join(Self::json_path(record)).exists()
    }

    fn delete(&self, id: Uuid) -> Result<(), HistoryError> {
        let _index_guard = self.lock_index()?;
        let mut index = self.read_index()?;
        let Some(pos) = index.records.iter().position(|e| e.id == id) else {
            // Already gone — not an error.
            return Ok(());
        };
        // Files that can't be removed where they are keep their entry, so
        // the record stays listed and the delete can be retried.
        if let Some(why) = self.removal_refused(&index.records[pos]) {
            tracing::warn!(
                target: crate::log::cat::HISTORY,
                "not deleting history record {id}: {why}"
            );
            return Err(why);
        }
        let entry = index.records.remove(pos);
        let record_for_path = CaptureRecord {
            id,
            captured_at: entry.captured_at,
            width_px: 0,
            height_px: 0,
            display_id: String::new(),
            ocr_text: None,
            annotation_model: Vec::new(),
            redacted: false,
            redactions: Vec::new(),
            redactions_complete: false,
            scrub_pending: false,
            pending_image: None,
            geometry_id: None,
        };
        // Image files (a set-aside unredacted copy among them) before the
        // sidecar: a crash between leaves none behind.
        self.remove_derived_files(&record_for_path);
        self.remove_entry_files(&entry);
        index.updated_at = Some(Utc::now());
        self.write_index(&index)?;
        Ok(())
    }
}

/// A thumbnail decoded for a backfill, with what it was decoded from:
/// the record's sidecar bytes and the PNG's stamp at the time.
struct Backfill {
    sidecar: Vec<u8>,
    png: FileStamp,
    thumb: image::RgbaImage,
}

/// What identifies a file's present contents without reading it: size,
/// modification time and file identity (the inode on Unix, the creation
/// time elsewhere). A rename of another file over it changes the
/// identity; every History image write is such a rename.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    identity: FileIdentity,
}

#[cfg(unix)]
type FileIdentity = (u64, u64);

#[cfg(not(unix))]
type FileIdentity = Option<std::time::SystemTime>;

impl FileStamp {
    fn of(path: &Path) -> Option<Self> {
        let m = fs::metadata(path).ok()?;
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (m.dev(), m.ino())
        };
        #[cfg(not(unix))]
        let identity = m.created().ok();
        Some(Self {
            len: m.len(),
            modified: m.modified().ok(),
            identity,
        })
    }
}

/// `thumb` encoded as PNG.
fn thumbnail_bytes(thumb: &image::RgbaImage) -> Option<Vec<u8>> {
    crate::encode_png(thumb).ok()
}

fn remove_if_exists(path: &Path) -> Result<(), HistoryError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// True when `record` matches the lowercase search query `q`. Tries
/// the OCR text, the display id, the pixel dimensions, and the
/// timestamp formatted in the local zone — the latter lets users
/// search "2026-04" or "14:32" naturally.
///
/// Single source of truth for history search: both the in-app history
/// browser and the MCP `search_captures` tool call this, after their
/// previously separate copies drifted apart (the MCP copy matched only
/// OCR text and a UTC-formatted timestamp).
pub fn record_matches_query(record: &CaptureRecord, q: &str) -> bool {
    if let Some(ocr) = record.ocr_text.as_deref() {
        if ocr.to_lowercase().contains(q) {
            return true;
        }
    }
    if record.display_id.to_lowercase().contains(q) {
        return true;
    }
    let dims = format!(
        "{}x{} {} {}",
        record.width_px, record.height_px, record.width_px, record.height_px
    );
    if dims.contains(q) {
        return true;
    }
    let stamp = record
        .captured_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
        .to_lowercase();
    stamp.contains(q)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record whose folder can't be read keeps its entry on delete, and
    /// the error says the folder can't be resolved, not that it is a link
    /// outside History.
    #[cfg(unix)]
    #[test]
    fn a_delete_in_an_unreadable_folder_is_not_called_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("h");
        let store = FsHistoryStore::new(root.clone());
        let png = crate::encode_png(&image::RgbaImage::new(4, 4)).unwrap();
        let record = CaptureRecord::new(Utc::now(), 4, 4, "d");
        store.save(&record, &png).unwrap();
        let year = root.join(FsHistoryStore::png_path(&record).iter().next().unwrap());
        std::fs::set_permissions(&year, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = store.delete(record.id);
        std::fs::set_permissions(&year, std::fs::Permissions::from_mode(0o755)).unwrap();
        match result {
            Err(HistoryError::Io(msg)) => assert!(msg.contains("can't be resolved"), "{msg}"),
            other => panic!("expected an io error, got {other:?}"),
        }
        assert_eq!(store.list().unwrap().len(), 1, "the record is kept");
    }

    /// A save that fails at the index write leaves none of its files
    /// behind: the user is told the capture isn't in History, and an
    /// index rebuild must not bring an (unredacted) image back.
    #[cfg(unix)]
    #[test]
    fn a_save_whose_index_write_fails_leaves_no_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("h");
        let store = FsHistoryStore::new(root.clone());
        let png = crate::encode_png(&image::RgbaImage::new(4, 4)).unwrap();
        let first = CaptureRecord::new(Utc::now(), 4, 4, "d");
        store.save(&first, &png).unwrap();
        let second = CaptureRecord::new(Utc::now(), 4, 4, "d");
        // The root (where the index lives) refuses new files; the record's
        // own folder still takes them.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = store.save(&second, &png);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err(), "the index write must fail in this setup");
        for rel in [
            FsHistoryStore::png_path(&second),
            FsHistoryStore::json_path(&second),
            FsHistoryStore::thumbnail_path(&second),
        ] {
            assert!(!root.join(&rel).exists(), "{} left behind", rel.display());
        }
        assert!(root.join(FsHistoryStore::png_path(&first)).exists());
        assert_eq!(store.list().unwrap().len(), 1);
    }
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
    fn corrupt_index_self_heals_by_rebuilding_from_sidecars() {
        // A torn or corrupted index file used to permanently wedge the
        // store (read_index propagated the parse error, blocking both
        // reads and all future saves). Now it self-heals: the corrupt
        // file is quarantined and the index is rebuilt from the sidecar
        // JSON files that are the real source of truth.
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &fake_png()).unwrap();

        fs::write(s.root().join(HISTORY_INDEX_FILENAME), b"{not json").unwrap();

        // list recovers the record from its sidecar.
        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, r.id);

        // The corrupt file was moved aside, not left in place.
        let quarantined = fs::read_dir(s.root()).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(&format!("{HISTORY_INDEX_FILENAME}.corrupt-"))
        });
        assert!(quarantined, "corrupt index should be quarantined aside");

        // save works again rather than surfacing a parse error.
        let newer = record_at(Utc::now());
        s.save(&newer, &fake_png()).unwrap();
        assert_eq!(s.list().unwrap().len(), 2);
    }

    #[test]
    fn cross_instance_saves_keep_every_index_entry() {
        // Separate FsHistoryStore instances on the same root approximate
        // separate processes: they don't share the in-process mutex, so
        // only the cross-process advisory file lock serialises their
        // read-modify-write cycles. Without it, concurrent saves would
        // clobber each other's index entries.
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("history");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut handles = Vec::new();
        for i in 0..8 {
            let root = root.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                let store = FsHistoryStore::new(&root);
                let record = record_at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, i).unwrap());
                barrier.wait();
                store.save(&record, &fake_png()).unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(FsHistoryStore::new(&root).list().unwrap().len(), 8);
    }

    #[test]
    fn list_skips_entries_with_unsafe_paths() {
        // A tampered or corrupt index must not be able to steer file I/O
        // outside the history root: entries with absolute paths or `..`
        // traversal are skipped, and the well-formed entry survives.
        let (_dir, s) = store();
        let good = record_at(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        s.save(&good, &fake_png()).unwrap();

        let mut index = s.read_index().unwrap();
        index.records.push(HistoryIndexEntry {
            id: Uuid::new_v4(),
            captured_at: Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap(),
            png_path: PathBuf::from("/etc/passwd"),
            json_path: PathBuf::from("/etc/passwd"),
        });
        index.records.push(HistoryIndexEntry {
            id: Uuid::new_v4(),
            captured_at: Utc.with_ymd_and_hms(2026, 1, 3, 0, 0, 0).unwrap(),
            png_path: PathBuf::from("../../../etc/shadow"),
            json_path: PathBuf::from("../../../etc/shadow"),
        });
        s.write_index(&index).unwrap();

        let list = s.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, good.id);
    }

    #[test]
    fn safe_relative_join_rejects_escapes_and_accepts_normal_paths() {
        let root = Path::new("/history/root");
        assert!(safe_relative_join(root, Path::new("2026/01/x.json")).is_some());
        assert!(safe_relative_join(root, Path::new("/etc/passwd")).is_none());
        assert!(safe_relative_join(root, Path::new("../escape")).is_none());
        assert!(safe_relative_join(root, Path::new("2026/../../escape")).is_none());
        assert!(safe_relative_join(root, Path::new("")).is_none());
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
            style: Default::default(),
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
    fn a_thumbnail_decoded_before_a_scrub_never_lands() {
        // `list()` (the History window, or the MCP process) decodes the
        // unredacted PNG of a record without a thumbnail while the editor
        // scrubs it; its write must not put pre-redaction pixels back.
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(8, 8)).unwrap();
        let png_path = s.root().join(FsHistoryStore::png_path(&r));
        let thumb_path = s.root().join(FsHistoryStore::thumbnail_path(&r));
        fs::remove_file(&thumb_path).unwrap();
        let unredacted = s.read_for_backfill(&r, &png_path).expect("decoded");
        let unredacted_thumb = unredacted.thumb.clone();
        // The scrub lands (its own thumbnail failed to encode).
        let (_, black) = black_png(8, 8);
        s.begin_scrub(&scrubbed_of(&r)).unwrap();
        let mid = s.read_for_backfill(&r, &png_path);
        assert!(mid.is_none(), "no image on disk mid-scrub");
        s.rewrite(&scrubbed_of(&r), &black).unwrap();
        let _ = fs::remove_file(&thumb_path);
        s.place_backfill(&r, &png_path, unredacted);
        if thumb_path.exists() {
            let landed = decode(&fs::read(&thumb_path).unwrap());
            assert!(
                landed != unredacted_thumb,
                "a pre-redaction thumbnail landed"
            );
        }
        // The next backfill uses the redacted pixels.
        s.list().unwrap();
        let landed = decode(&fs::read(&thumb_path).unwrap());
        assert!(landed.pixels().all(|p| p.0 == [0, 0, 0, 255]));
    }

    #[test]
    fn a_backfill_decoded_before_any_scrub_phase_is_discarded() {
        // A scrub that leaves the very same PNG bytes in place (its
        // sidecar still changed) also makes the decoded result stale.
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        let png = valid_png(8, 8);
        s.save(&r, &png).unwrap();
        let png_path = s.root().join(FsHistoryStore::png_path(&r));
        let thumb_path = s.root().join(FsHistoryStore::thumbnail_path(&r));
        fs::remove_file(&thumb_path).unwrap();
        let read = s.read_for_backfill(&r, &png_path).expect("decoded");
        let mut scrubbed = scrubbed_of(&r);
        scrubbed.ocr_text = Some("other".into());
        s.rewrite(&scrubbed, &png).unwrap();
        let _ = fs::remove_file(&thumb_path);
        s.place_backfill(&r, &png_path, read);
        assert!(!thumb_path.exists());
        // Unchanged in between: it lands.
        let read = s.read_for_backfill(&r, &png_path).expect("decoded");
        s.place_backfill(&r, &png_path, read);
        assert!(thumb_path.exists());
    }

    #[test]
    fn a_backfill_never_lands_on_a_pending_scrub() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(8, 8)).unwrap();
        let png_path = s.root().join(FsHistoryStore::png_path(&r));
        let thumb_path = s.root().join(FsHistoryStore::thumbnail_path(&r));
        fs::remove_file(&thumb_path).unwrap();
        let unredacted = s.read_for_backfill(&r, &png_path).expect("decoded");
        s.begin_scrub(&scrubbed_of(&r)).unwrap();
        s.place_backfill(&r, &png_path, unredacted);
        assert!(!thumb_path.exists());
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

    fn decode(png: &[u8]) -> image::RgbaImage {
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    fn png_of(img: &image::RgbaImage) -> Vec<u8> {
        crate::encode_png(img).unwrap()
    }

    #[test]
    fn records_without_the_redacted_field_load_and_plain_records_serialise_without_it() {
        // An old sidecar (no `redacted` key) loads unchanged.
        let old = r#"{
            "id": "6f1c1d2e-0000-4000-8000-000000000001",
            "captured_at": "2026-01-01T00:00:00Z",
            "width_px": 4,
            "height_px": 4,
            "display_id": "d",
            "ocr_text": "hello",
            "annotation_model": []
        }"#;
        let r: CaptureRecord = serde_json::from_str(old).unwrap();
        assert!(!r.redacted);
        assert_eq!(r.ocr_text.as_deref(), Some("hello"));
        // An unredacted record writes exactly the old schema.
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("redacted"), "{json}");
        let mut scrubbed = r.clone();
        scrubbed.redacted = true;
        let json = serde_json::to_string(&scrubbed).unwrap();
        assert!(json.contains("\"redacted\":true"), "{json}");
    }

    #[test]
    fn rewrite_replaces_png_thumbnail_text_and_annotations() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.ocr_text = Some("token ABC123 here".into());
        r.annotation_model = vec![Annotation::Blur {
            rect: crate::RectLike::new(0.0, 0.0, 2.0, 2.0),
            radius: 2.0,
        }];
        s.save(&r, &valid_png(8, 8)).unwrap();

        let mut scrubbed_img = image::RgbaImage::new(8, 8);
        for px in scrubbed_img.pixels_mut() {
            *px = image::Rgba([0, 0, 0, 255]);
        }
        let mut scrubbed = r.clone();
        scrubbed.ocr_text = Some("token ██████ here".into());
        scrubbed.annotation_model = Vec::new();
        scrubbed.redacted = true;
        scrubbed.redactions_complete = true;
        s.rewrite(&scrubbed, &png_of(&scrubbed_img)).unwrap();

        let listed = s.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], scrubbed);
        assert!(!listed[0].ocr_text.as_deref().unwrap().contains("ABC123"));
        // What the MCP server reads: the PNG at `abs_png_path`.
        let on_disk = fs::read(FsHistoryStore::abs_png_path(s.root(), &listed[0])).unwrap();
        assert_eq!(decode(&on_disk), scrubbed_img);
        let thumb = fs::read(FsHistoryStore::abs_thumbnail_path(s.root(), &listed[0])).unwrap();
        assert!(decode(&thumb)
            .pixels()
            .all(|p| *p == image::Rgba([0, 0, 0, 255])));
    }

    #[test]
    fn rewrite_with_no_text_clears_the_stored_text() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.ocr_text = Some("secret".into());
        s.save(&r, &valid_png(4, 4)).unwrap();
        r.ocr_text = None;
        s.rewrite(&r, &valid_png(4, 4)).unwrap();
        assert_eq!(s.list().unwrap()[0].ocr_text, None);
    }

    #[test]
    fn rewrite_is_a_noop_when_record_was_pruned() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.rewrite(&r, &valid_png(4, 4)).unwrap();
        assert!(s.list().unwrap().is_empty());
        assert!(!s.root().join(FsHistoryStore::png_path(&r)).exists());
    }

    fn scrubbed_of(r: &CaptureRecord) -> CaptureRecord {
        let mut scrubbed = r.clone();
        scrubbed.ocr_text = Some("masked".into());
        scrubbed.redacted = true;
        scrubbed.redactions_complete = true;
        scrubbed
    }

    fn black_png(w: u32, h: u32) -> (image::RgbaImage, Vec<u8>) {
        let mut img = image::RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba([0, 0, 0, 255]);
        }
        let png = png_of(&img);
        (img, png)
    }

    fn with_redaction(mut r: CaptureRecord) -> CaptureRecord {
        // Over "4821" in `positioned_doc`.
        r.redactions = vec![Annotation::Blur {
            rect: crate::RectLike::new(4.0, 0.0, 4.0, 2.0),
            radius: 2.0,
        }];
        r
    }

    fn staging_files(s: &FsHistoryStore, r: &CaptureRecord) -> Vec<PathBuf> {
        let dir = s
            .root()
            .join(FsHistoryStore::png_path(r))
            .parent()
            .unwrap()
            .to_path_buf();
        fs::read_dir(dir)
            .map(|it| {
                it.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.to_string_lossy().contains(".stage"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_staged_image_is_invisible_and_removed_when_not_committed() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let (_, png) = black_png(4, 4);
        let staged = s.stage_image(&r, &png, Some(&png)).unwrap();
        assert_eq!(staging_files(&s, &r).len(), 2);
        // Readers still see the old image.
        let on_disk = fs::read(s.root().join(FsHistoryStore::png_path(&r))).unwrap();
        assert_eq!(on_disk, valid_png(4, 4));
        drop(staged);
        assert!(staging_files(&s, &r).is_empty());
        // Committing consumes the files.
        let staged = s.stage_image(&r, &png, Some(&png)).unwrap();
        assert!(s.commit_rewrite(&scrubbed_of(&r), staged).unwrap());
        assert!(staging_files(&s, &r).is_empty());
        // A commit for a missing record discards them too.
        let gone = record_at(Utc::now());
        let staged = s.stage_image(&gone, &png, None).unwrap();
        assert!(!s.commit_rewrite(&scrubbed_of(&gone), staged).unwrap());
        assert!(staging_files(&s, &gone).is_empty());
    }

    #[test]
    fn delete_and_retention_remove_a_records_staging_files() {
        let (_dir, s) = store();
        let keep = record_at(Utc::now());
        let pruned = record_at(Utc::now() - Duration::days(60));
        let deleted = record_at(Utc::now());
        for r in [&keep, &pruned, &deleted] {
            s.save(r, &valid_png(4, 4)).unwrap();
        }
        let stage = |r: &CaptureRecord| {
            let dir = s
                .root()
                .join(FsHistoryStore::png_path(r))
                .parent()
                .unwrap()
                .to_path_buf();
            let png = dir.join(format!("{}.9-0.stage.png", r.id));
            let thumb = dir.join(format!("{}.9-0.stage-thumb.png", r.id));
            fs::write(&png, b"raw").unwrap();
            fs::write(&thumb, b"raw").unwrap();
            [png, thumb]
        };
        let (k, p, d) = (stage(&keep), stage(&pruned), stage(&deleted));
        s.delete(deleted.id).unwrap();
        assert!(d.iter().all(|f| !f.exists()));
        s.apply_retention(HistoryRetention::Last30Days, Utc::now())
            .unwrap();
        assert!(p.iter().all(|f| !f.exists()));
        assert!(
            k.iter().all(|f| f.exists()),
            "other records' stages untouched"
        );
    }

    #[test]
    fn sweep_removes_only_stale_staging_files() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let dir = s
            .root()
            .join(FsHistoryStore::png_path(&r))
            .parent()
            .unwrap()
            .to_path_buf();
        let old = dir.join(format!("{}.1-0.stage.png", r.id));
        let fresh = dir.join(format!("{}.1-1.stage-thumb.png", r.id));
        fs::write(&old, b"x").unwrap();
        fs::write(&fresh, b"x").unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        s.sweep_staging();
        assert!(!old.exists());
        assert!(fresh.exists(), "may be another process's in-flight write");
        assert!(s.root().join(FsHistoryStore::png_path(&r)).exists());
    }

    #[test]
    fn thumbnails_from_memory_match_the_decoded_path() {
        let (img, png) = black_png(640, 100);
        assert_eq!(thumbnail_png(&img), thumbnail_png_from_bytes(&png));
        let thumb = image::load_from_memory(&thumbnail_png(&img).unwrap()).unwrap();
        assert_eq!((thumb.width(), thumb.height()), (320, 50));
    }

    #[test]
    fn begin_scrub_withholds_the_record_and_sets_its_image_aside() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.ocr_text = Some("secret".into());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let png = s.root().join(FsHistoryStore::png_path(&r));
        let thumb = s.root().join(FsHistoryStore::thumbnail_path(&r));
        let withheld = s.root().join(FsHistoryStore::withheld_path(&r));
        assert!(png.exists() && thumb.exists());
        assert!(s.begin_scrub(&scrubbed_of(&r)).unwrap());
        // Withheld: nothing at its image path, no thumbnail, no text served.
        let listed = s.list().unwrap();
        assert!(listed[0].redacted && listed[0].scrub_pending);
        assert_eq!(listed[0].ocr_text, None);
        assert!(!png.exists() && !thumb.exists());
        // Never deleted: set aside until the redacted image replaces it.
        assert_eq!(fs::read(&withheld).unwrap(), valid_png(4, 4));
        let stored = s.read_sidecar(&r).unwrap().unwrap();
        assert_eq!(stored.ocr_text.as_deref(), Some("masked"));
        let pending = stored.pending_image.unwrap();
        assert_eq!((pending.width, pending.height), (r.width_px, r.height_px));
        assert!(crate::scrub::has_redaction(&pending.bake));
    }

    #[test]
    fn a_scrub_whose_image_write_fails_leaves_the_record_withheld_with_its_image() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.ocr_text = Some("secret".into());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let thumb = s.root().join(FsHistoryStore::thumbnail_path(&r));
        let scrubbed = scrubbed_of(&r);
        let (_, black) = black_png(4, 4);
        let staged = s.stage_image(&scrubbed, &black, None).unwrap();
        // The staged image vanishes before the commit renames it.
        fs::remove_file(&staged.png).unwrap();
        assert!(s.commit_rewrite(&scrubbed, staged).is_err());
        let stored = s.read_sidecar(&r).unwrap().unwrap();
        assert_eq!(stored.ocr_text.as_deref(), Some("masked"));
        assert!(stored.redacted && stored.scrub_pending);
        assert!(!thumb.exists());
        assert!(!s.root().join(FsHistoryStore::png_path(&r)).exists());
        assert!(s.root().join(FsHistoryStore::withheld_path(&r)).exists());
    }

    /// A pending scrub whose image never landed (a crash or quit after
    /// phase one) is finished from its set-aside image: the pending
    /// redactions are drawn on it with the core render and it is swapped
    /// in. The record is redacted, keeps its image, and keeps no text.
    #[test]
    fn a_pending_scrub_is_finished_from_its_set_aside_image() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.width_px = 8;
        r.height_px = 8;
        r.ocr_text = Some("secret".into());
        let white = image::RgbaImage::from_pixel(8, 8, image::Rgba([255, 255, 255, 255]));
        s.save(&r, &crate::png::encode(&white).unwrap()).unwrap();
        let mut half = scrubbed_of(&r);
        half.redactions = vec![Annotation::Rectangle {
            rect: crate::geom::RectLike::new(0.0, 0.0, 4.0, 4.0),
            color: crate::annotation::Rgba::OPAQUE_BLACK,
            line_width: 0.0,
            style: crate::annotation::ShapeStyle {
                fill: crate::annotation::FillMode::Filled,
                ..crate::annotation::ShapeStyle::default()
            },
        }];
        assert!(s.begin_scrub(&half).unwrap());
        assert_eq!(s.finish_pending_scrubs(), 1);
        let listed = &s.list().unwrap()[0];
        assert!(listed.redacted && !listed.scrub_pending);
        assert_eq!(listed.ocr_text, None);
        assert_eq!(listed.pending_image, None);
        let on_disk = decode(&fs::read(s.root().join(FsHistoryStore::png_path(&r))).unwrap());
        assert_eq!(on_disk.get_pixel(1, 1).0, [0, 0, 0, 255]);
        assert_eq!(on_disk.get_pixel(6, 6).0, [255, 255, 255, 255]);
        assert!(s.root().join(FsHistoryStore::thumbnail_path(&r)).exists());
        assert!(!s.root().join(FsHistoryStore::withheld_path(&r)).exists());
        // Nothing left to finish.
        assert_eq!(s.finish_pending_scrubs(), 0);
    }

    /// Fail closed: a plain sidecar sync (which knows nothing of a
    /// pending scrub) never clears `scrub_pending` or the pending
    /// redactions. Only an image swap may.
    #[test]
    fn a_sidecar_sync_never_clears_a_pending_scrub() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let scrubbed = scrubbed_of(&r);
        assert!(s.begin_scrub(&scrubbed).unwrap());
        // An editor of the redacted record syncs its annotations.
        let mut sync = scrubbed.clone();
        sync.scrub_pending = false;
        sync.pending_image = None;
        sync.ocr_text = None;
        s.update(&sync).unwrap();
        let stored = s.read_sidecar(&r).unwrap().unwrap();
        assert!(stored.scrub_pending, "a sync cleared the pending scrub");
        assert!(stored.pending_image.is_some());
        assert!(stored.redacted);
        // Still finished from its set-aside image.
        assert_eq!(s.finish_pending_scrubs(), 1);
        assert!(s.root().join(FsHistoryStore::png_path(&r)).exists());
    }

    /// Fail closed: a crash after a redacted image landed but before the
    /// set-aside unredacted image was removed leaves no unredacted copy
    /// for good: the next launch removes it.
    #[test]
    fn a_set_aside_image_left_behind_is_removed_at_launch() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let scrubbed = scrubbed_of(&r);
        let (_, black) = black_png(4, 4);
        assert!(s.rewrite(&scrubbed, &black).unwrap());
        let withheld = s.root().join(FsHistoryStore::withheld_path(&r));
        fs::write(&withheld, valid_png(4, 4)).unwrap();
        s.finish_pending_scrubs();
        assert!(!withheld.exists(), "an unredacted copy stayed on disk");
        assert!(s.root().join(FsHistoryStore::png_path(&r)).exists());
    }

    /// Fail closed: a pending scrub whose set-aside image can't be
    /// decoded stays withheld (logged): no image is placed at its path
    /// and none of it is served; it is kept for a later try.
    #[test]
    fn a_pending_scrub_that_cant_be_drawn_stays_withheld() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        assert!(s.begin_scrub(&scrubbed_of(&r)).unwrap());
        let withheld = s.root().join(FsHistoryStore::withheld_path(&r));
        fs::write(&withheld, b"not a png").unwrap();
        assert_eq!(s.finish_pending_scrubs(), 0);
        let listed = &s.list().unwrap()[0];
        assert!(listed.scrub_pending && listed.redacted);
        assert_eq!(listed.ocr_text, None);
        assert!(!s.root().join(FsHistoryStore::png_path(&r)).exists());
        assert!(withheld.exists(), "kept for a later try");
    }

    /// Fail closed: a pending record whose sidecar is garbled (its
    /// pending redactions can't be decoded) is never served, and its
    /// recovery places nothing, even when its unredacted image is still
    /// at its own path (a crash between marking and setting it aside).
    #[test]
    fn a_garbled_pending_sidecar_is_never_served_or_drawn_from() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let json = s.root().join(FsHistoryStore::json_path(&r));
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&json).unwrap()).unwrap();
        raw["redacted"] = true.into();
        raw["scrub_pending"] = true.into();
        raw["pending_image"] = "garbage".into();
        fs::write(&json, raw.to_string()).unwrap();
        let png = s.root().join(FsHistoryStore::png_path(&r));
        let before = fs::read(&png).unwrap();
        assert!(s.list().unwrap().is_empty(), "a garbled record was served");
        assert_eq!(s.finish_pending_scrubs(), 0);
        assert_eq!(fs::read(&png).unwrap(), before, "recovery wrote over it");
        assert!(s.stored_redacted(&r).is_err(), "unknown counts as redacted");
    }

    /// Fix-5 review M-1: a set-aside image whose record is gone (a crash
    /// in a delete or retention pass after its sidecar went) is removed
    /// at the next launch: no unredacted copy outlives its record.
    #[test]
    fn a_set_aside_image_without_a_record_is_removed_at_launch() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        assert!(s.begin_scrub(&scrubbed_of(&r)).unwrap());
        let withheld = s.root().join(FsHistoryStore::withheld_path(&r));
        assert!(withheld.exists());
        fs::remove_file(s.root().join(FsHistoryStore::json_path(&r))).unwrap();
        s.finish_pending_scrubs();
        assert!(!withheld.exists(), "an unredacted copy outlived its record");
    }

    /// Fix-5 review M-3: a record marked pending whose image is still at
    /// its path (a crash between marking and setting it aside) and can't
    /// be drawn on keeps no thumbnail of the unredacted image.
    #[test]
    fn a_pending_scrub_that_cant_be_drawn_keeps_no_thumbnail() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let thumb = s.root().join(FsHistoryStore::thumbnail_path(&r));
        assert!(thumb.exists());
        let json = s.root().join(FsHistoryStore::json_path(&r));
        let mut raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&json).unwrap()).unwrap();
        raw["redacted"] = true.into();
        raw["scrub_pending"] = true.into();
        fs::write(&json, raw.to_string()).unwrap();
        fs::write(s.root().join(FsHistoryStore::png_path(&r)), b"not a png").unwrap();
        assert_eq!(s.finish_pending_scrubs(), 0);
        assert!(!thumb.exists(), "the unredacted thumbnail stayed");
    }

    /// Redactions that can't be placed on the set-aside image (another
    /// frame or size) become a whole-image cover: never an unredacted
    /// pixel, never a lost image.
    #[test]
    fn a_pending_scrub_in_another_frame_is_finished_as_a_whole_image_cover() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.width_px = 4;
        r.height_px = 4;
        s.save(&r, &valid_png(4, 4)).unwrap();
        let mut rotated = scrubbed_of(&r);
        rotated.geometry_id = Some("rotated".into());
        assert!(s.begin_scrub(&rotated).unwrap());
        assert_eq!(s.finish_pending_scrubs(), 1);
        let listed = &s.list().unwrap()[0];
        assert!(listed.redacted && !listed.scrub_pending);
        assert_eq!(listed.geometry_id, None, "the image's own frame");
        let on_disk = decode(&fs::read(s.root().join(FsHistoryStore::png_path(&r))).unwrap());
        assert!(on_disk.pixels().all(|p| p.0 == [0, 0, 0, 255]));
    }

    #[test]
    fn a_later_rewrite_completes_a_pending_scrub() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let scrubbed = scrubbed_of(&r);
        s.begin_scrub(&scrubbed).unwrap();
        let (img, png) = black_png(4, 4);
        assert!(s.rewrite(&scrubbed, &png).unwrap());
        let listed = &s.list().unwrap()[0];
        assert!(!listed.scrub_pending);
        let on_disk = fs::read(s.root().join(FsHistoryStore::png_path(&r))).unwrap();
        assert_eq!(decode(&on_disk), img);
    }

    #[test]
    fn an_unredacted_save_never_overwrites_a_scrubbed_record() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.ocr_text = Some("secret".into());
        let (img, png) = black_png(4, 4);
        s.save(&r, &valid_png(4, 4)).unwrap();
        let scrubbed = scrubbed_of(&r);
        s.rewrite(&scrubbed, &png).unwrap();
        s.save(&r, &valid_png(4, 4)).unwrap();
        let listed = s.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], scrubbed);
        let on_disk = fs::read(s.root().join(FsHistoryStore::png_path(&r))).unwrap();
        assert_eq!(decode(&on_disk), img);
        // A scrubbed save (the parked scrub applied at save time) writes.
        let r2 = scrubbed_of(&record_at(Utc::now()));
        s.save(&r2, &png).unwrap();
        assert!(s.contains(&r2));
    }

    fn positioned_doc() -> TextDocument {
        use crate::text_lines::{TextLine, WordRect};
        TextDocument {
            lines: vec![TextLine {
                text: "pin 4821 ok".into(),
                rect: Some(crate::RectLike::new(0.0, 0.0, 11.0, 2.0)),
                words: [(0, 3, 0.0, 3.0), (4, 8, 4.0, 4.0), (9, 11, 9.0, 2.0)]
                    .into_iter()
                    .map(|(start, end, x, w)| WordRect {
                        start,
                        end,
                        rect: crate::RectLike::new(x, 0.0, w, 2.0),
                    })
                    .collect(),
            }],
            positioned: true,
            layout_text: "pin 4821 ok".into(),
        }
    }

    #[test]
    fn recognized_text_is_masked_with_the_stored_redactions() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let mut scrubbed = with_redaction(scrubbed_of(&r));
        scrubbed.ocr_text = None;
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        s.record_recognized_text(&r, &positioned_doc(), true)
            .unwrap();
        assert_eq!(
            s.list().unwrap()[0].ocr_text.as_deref(),
            Some("pin ████ ok")
        );
    }

    #[test]
    fn recognized_text_never_lands_unmasked_on_a_scrubbed_record() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        // No stored redactions to mask with: keep what's there.
        let mut scrubbed = scrubbed_of(&r);
        scrubbed.ocr_text = None;
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        s.record_recognized_text(&r, &positioned_doc(), true)
            .unwrap();
        assert_eq!(s.list().unwrap()[0].ocr_text, None);
        // Text without positions doesn't replace text already stored.
        let mut scrubbed = with_redaction(scrubbed_of(&r));
        scrubbed.ocr_text = Some("pin ████ ok".into());
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        let plain = TextDocument::from_plain("pin 4821 ok");
        s.record_recognized_text(&r, &plain, true).unwrap();
        assert_eq!(
            s.list().unwrap()[0].ocr_text.as_deref(),
            Some("pin ████ ok")
        );
    }

    /// A scrubbed record whose sidecar then can't be parsed (a transient
    /// read error, or a newer build's enum value): returns the store, the
    /// record, and the sidecar and PNG bytes on disk.
    fn scrubbed_with_unreadable_sidecar(
    ) -> (TempDir, FsHistoryStore, CaptureRecord, Vec<u8>, Vec<u8>) {
        let (dir, s) = store();
        let mut r = record_at(Utc::now());
        r.ocr_text = Some("secret 4821".into());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let (_, redacted_png) = black_png(4, 4);
        s.rewrite(&scrubbed_of(&r), &redacted_png).unwrap();
        let json = s.root().join(FsHistoryStore::json_path(&r));
        fs::write(&json, b"{\"id\": \"not a record\"").unwrap();
        let png = fs::read(s.root().join(FsHistoryStore::png_path(&r))).unwrap();
        (dir, s, r, fs::read(&json).unwrap(), png)
    }

    #[test]
    fn an_unreadable_sidecar_counts_as_scrubbed_for_every_guard() {
        let (_dir, s, r, sidecar, png) = scrubbed_with_unreadable_sidecar();
        let json = s.root().join(FsHistoryStore::json_path(&r));
        let png_path = s.root().join(FsHistoryStore::png_path(&r));
        let unchanged = |what: &str| {
            assert_eq!(fs::read(&json).unwrap(), sidecar, "{what}: sidecar");
            assert_eq!(fs::read(&png_path).unwrap(), png, "{what}: png");
        };
        // The capture's late background OCR (an unredacted writer).
        assert!(s.update(&r).is_err());
        unchanged("update");
        // An unredacted save of the same record.
        assert!(s.save(&r, &valid_png(4, 4)).is_err());
        unchanged("save");
        // Recognized text, even when asked to fill an unredacted record.
        let doc = TextDocument::from_plain("secret 4821");
        assert!(s.record_recognized_text(&r, &doc, true).is_err());
        unchanged("recognized text");
        // An unredacted rewrite (a restore).
        assert!(s.rewrite(&r, &valid_png(4, 4)).is_err());
        unchanged("rewrite");
    }

    #[test]
    fn retention_and_delete_never_touch_paths_outside_the_root() {
        let (dir, s) = store();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, b"keep me").unwrap();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let mut index = s.read_index().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        for id in [a, b] {
            index.records.push(HistoryIndexEntry {
                id,
                captured_at: old,
                png_path: PathBuf::from("../outside.txt"),
                json_path: outside.clone(),
            });
        }
        s.write_index(&index).unwrap();
        s.delete(a).unwrap();
        assert!(outside.exists(), "delete escaped the root");
        s.apply_retention(HistoryRetention::Last30Days, Utc::now())
            .unwrap();
        assert!(outside.exists(), "retention escaped the root");
        assert!(s.read_index().unwrap().records.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn retention_and_delete_never_follow_a_symlinked_directory_out_of_the_root() {
        let (dir, s) = store();
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let victim = elsewhere.join("file.png");
        fs::write(&victim, b"keep me").unwrap();
        fs::create_dir_all(s.root()).unwrap();
        std::os::unix::fs::symlink(&elsewhere, s.root().join("link")).unwrap();
        let old = Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap();
        let mut index = s.read_index().unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        for id in [a, b] {
            index.records.push(HistoryIndexEntry {
                id,
                captured_at: old,
                png_path: PathBuf::from("link/file.png"),
                json_path: PathBuf::from("link/file.png"),
            });
        }
        s.write_index(&index).unwrap();
        // The delete removes nothing, so it fails and keeps the entry
        // (the user can retry once the folder is fixed).
        assert!(s.delete(a).is_err());
        assert!(victim.exists(), "delete followed the symlink");
        assert_eq!(s.read_index().unwrap().records.len(), 2);
        // Retention drops entries past the policy, files left in place.
        s.apply_retention(HistoryRetention::Last30Days, Utc::now())
            .unwrap();
        assert!(victim.exists(), "retention followed the symlink");
        assert!(s.read_index().unwrap().records.is_empty());
        // A symlink at the leaf itself is only unlinked, never followed.
        let leaf = s.root().join("leaf.png");
        std::os::unix::fs::symlink(&victim, &leaf).unwrap();
        let c = Uuid::new_v4();
        let mut index = s.read_index().unwrap();
        index.records.push(HistoryIndexEntry {
            id: c,
            captured_at: old,
            png_path: PathBuf::from("leaf.png"),
            json_path: PathBuf::from("leaf.png"),
        });
        s.write_index(&index).unwrap();
        s.delete(c).unwrap();
        assert!(victim.exists());
    }

    #[test]
    fn a_non_finite_annotation_never_hides_the_record() {
        // serde_json writes NaN as `null`, which then fails to load: the
        // whole record used to vanish from History and MCP.
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.ocr_text = Some("hello".into());
        let nan = crate::RectLike::new(f32::NAN, 0.0, 2.0, 2.0);
        r.annotation_model = vec![
            Annotation::Blur {
                rect: nan,
                radius: 2.0,
            },
            Annotation::Blur {
                rect: crate::RectLike::new(0.0, 0.0, 2.0, 2.0),
                radius: 2.0,
            },
        ];
        s.save(&r, &valid_png(4, 4)).unwrap();
        let listed = s.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].annotation_model.len(), 1);
        // A redaction list that loses one can't be complete: its text
        // goes (fail closed), the record stays.
        let mut scrubbed = scrubbed_of(&r);
        scrubbed.redactions = vec![Annotation::Pixelate {
            rect: nan,
            block_size: f32::INFINITY,
        }];
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        let listed = s.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].redacted && !listed[0].redactions_complete);
        assert_eq!(listed[0].ocr_text, None);
    }

    #[test]
    fn unpositioned_text_never_fills_a_scrubbed_record() {
        // Windows / Linux: a hand-drawn Blur over the whole image, then
        // the capture's plain recognition lands. Nothing can place its
        // words, so none of it may be stored or served.
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let mut scrubbed = scrubbed_of(&r);
        scrubbed.redactions = vec![Annotation::Blur {
            rect: crate::RectLike::new(0.0, 0.0, 4.0, 4.0),
            radius: 2.0,
        }];
        scrubbed.redactions_complete = true;
        scrubbed.ocr_text = None;
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        let plain = TextDocument::from_plain("Patient: John Smith");
        s.record_recognized_text(&r, &plain, true).unwrap();
        let listed = &s.list().unwrap()[0];
        assert!(listed.redacted && listed.redactions_complete);
        assert_eq!(listed.ocr_text, None);
    }

    #[test]
    fn a_scrub_with_unknown_text_keeps_text_masked_for_the_same_redactions() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let mut scrubbed = with_redaction(scrubbed_of(&r));
        scrubbed.ocr_text = None;
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        s.record_recognized_text(&r, &positioned_doc(), true)
            .unwrap();
        // A rewrite queued before the text landed, same redactions.
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        assert_eq!(
            s.list().unwrap()[0].ocr_text.as_deref(),
            Some("pin ████ ok")
        );
        // Different redactions: the old masking may not cover them.
        let mut more = scrubbed.clone();
        more.redactions.push(Annotation::Blur {
            rect: crate::RectLike::new(0.0, 0.0, 3.0, 2.0),
            radius: 2.0,
        });
        s.begin_scrub(&more).unwrap();
        assert_eq!(s.list().unwrap()[0].ocr_text, None);
    }

    #[test]
    fn recognized_text_fills_an_unredacted_record_only_when_asked() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.annotation_model = vec![Annotation::Blur {
            rect: crate::RectLike::new(0.0, 0.0, 2.0, 2.0),
            radius: 2.0,
        }];
        s.save(&r, &valid_png(4, 4)).unwrap();
        s.record_recognized_text(&r, &positioned_doc(), false)
            .unwrap();
        assert_eq!(s.list().unwrap()[0].ocr_text, None);
        s.record_recognized_text(&r, &positioned_doc(), true)
            .unwrap();
        let listed = &s.list().unwrap()[0];
        assert_eq!(listed.ocr_text.as_deref(), Some("pin 4821 ok"));
        assert_eq!(listed.annotation_model, r.annotation_model);
    }

    #[test]
    fn plain_update_never_unscrubs_a_redacted_record() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let mut scrubbed = r.clone();
        scrubbed.ocr_text = Some("██████".into());
        scrubbed.redacted = true;
        scrubbed.redactions_complete = true;
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        // A late OCR pass writes the raw text with stale annotations.
        r.ocr_text = Some("secret".into());
        r.annotation_model = vec![Annotation::Blur {
            rect: crate::RectLike::new(0.0, 0.0, 2.0, 2.0),
            radius: 2.0,
        }];
        s.update(&r).unwrap();
        assert_eq!(s.list().unwrap()[0], scrubbed);
        // An update that knows the record is scrubbed still applies.
        let mut later = scrubbed.clone();
        later.ocr_text = None;
        later.annotation_model = r.annotation_model.clone();
        s.update(&later).unwrap();
        let listed = &s.list().unwrap()[0];
        assert_eq!(listed.annotation_model, r.annotation_model);
        assert_eq!(listed.ocr_text.as_deref(), Some("██████"));
    }

    #[test]
    fn recognized_text_never_lands_in_a_record_of_another_frame() {
        use crate::text_lines::TextDocument;
        let dir = tempfile::tempdir().unwrap();
        let store = FsHistoryStore::new(dir.path());
        let mut r = CaptureRecord::new(Utc::now(), 4, 4, "d");
        r.geometry_id = Some("nonce:rot=1".into());
        let png = crate::encode_png_fast(&image::RgbaImage::new(4, 4)).unwrap();
        store.save(&r, &png).unwrap();
        let doc = TextDocument::from_plain("hello");
        let mut stale = r.clone();
        stale.geometry_id = None;
        store.record_recognized_text(&stale, &doc, true).unwrap();
        assert_eq!(
            store.list().unwrap()[0].ocr_text,
            None,
            "another frame's text is ignored"
        );
        store.record_recognized_text(&r, &doc, true).unwrap();
        assert_eq!(store.list().unwrap()[0].ocr_text.as_deref(), Some("hello"));
    }

    #[test]
    fn geometry_id_is_omitted_when_none_and_round_trips() {
        let mut r = CaptureRecord::new(Utc::now(), 4, 4, "d");
        assert!(!serde_json::to_string(&r).unwrap().contains("geometry_id"));
        r.geometry_id = Some("x".into());
        let back: CaptureRecord =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.geometry_id.as_deref(), Some("x"));
    }

    #[test]
    fn a_scrub_of_another_frame_never_keeps_text_masked_for_this_one() {
        let (_dir, s) = store();
        let r = record_at(Utc::now());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let mut scrubbed = with_redaction(scrubbed_of(&r));
        scrubbed.ocr_text = Some("pin ████ ok".into());
        s.rewrite(&scrubbed, &valid_png(4, 4)).unwrap();
        // Same redactions (by value), but in another document frame: the
        // stored masking may not cover them, so nothing is kept.
        let mut rotated = scrubbed.clone();
        rotated.geometry_id = Some("n:rot=1".into());
        rotated.ocr_text = None;
        s.rewrite(&rotated, &valid_png(4, 4)).unwrap();
        let listed = &s.list().unwrap()[0];
        assert_eq!(listed.geometry_id.as_deref(), Some("n:rot=1"));
        assert_eq!(listed.ocr_text, None);
    }

    #[test]
    fn an_oversized_geometry_id_loads_as_a_frame_no_editor_matches() {
        let mut r = CaptureRecord::new(Utc::now(), 4, 4, "d");
        r.geometry_id = Some("x".repeat(MAX_GEOMETRY_ID_LEN + 1));
        let back: CaptureRecord =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.geometry_id.as_deref(), Some(INVALID_GEOMETRY_ID));
        r.geometry_id = Some("x".repeat(MAX_GEOMETRY_ID_LEN));
        let back: CaptureRecord =
            serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back.geometry_id, r.geometry_id);
        // Absent stays `None` (older records).
        let mut plain = serde_json::to_value(CaptureRecord::new(Utc::now(), 4, 4, "d")).unwrap();
        plain.as_object_mut().unwrap().remove("geometry_id");
        let back: CaptureRecord = serde_json::from_value(plain).unwrap();
        assert_eq!(back.geometry_id, None);
    }

    #[test]
    fn a_plain_update_of_another_frame_is_ignored() {
        let (_dir, s) = store();
        let mut r = record_at(Utc::now());
        r.geometry_id = Some("n:rot=1".into());
        s.save(&r, &valid_png(4, 4)).unwrap();
        let mut stale = r.clone();
        stale.geometry_id = None;
        stale.annotation_model = vec![Annotation::Blur {
            rect: crate::RectLike::new(0.0, 0.0, 2.0, 2.0),
            radius: 2.0,
        }];
        s.update(&stale).unwrap();
        let listed = &s.list().unwrap()[0];
        assert_eq!(listed.geometry_id.as_deref(), Some("n:rot=1"));
        assert!(listed.annotation_model.is_empty());
        let mut same = r.clone();
        same.ocr_text = Some("t".into());
        s.update(&same).unwrap();
        assert_eq!(s.list().unwrap()[0].ocr_text.as_deref(), Some("t"));
    }

    /// A sidecar as an older build wrote it: `r` saved with a real 20 × 20
    /// PNG, then its JSON replaced by `edit` applied to the record's JSON.
    fn legacy_sidecar(
        s: &FsHistoryStore,
        edit: impl FnOnce(&mut serde_json::Value),
    ) -> CaptureRecord {
        let mut r = CaptureRecord::new(Utc::now(), 20, 20, "d");
        r.redacted = true;
        r.redactions = vec![Annotation::Pixelate {
            rect: crate::RectLike::new(10.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        }];
        s.save(&r, &valid_png(20, 20)).unwrap();
        let mut json = serde_json::to_value(&r).unwrap();
        edit(&mut json);
        std::fs::write(
            s.root().join(FsHistoryStore::json_path(&r)),
            serde_json::to_string_pretty(&json).unwrap(),
        )
        .unwrap();
        r
    }

    fn sidecar_text(s: &FsHistoryStore, r: &CaptureRecord) -> String {
        std::fs::read_to_string(s.root().join(FsHistoryStore::json_path(r))).unwrap()
    }

    #[test]
    fn older_crop_frame_ids_of_every_era_lose_their_text_on_load_once() {
        // DefaultHasher era, FNV-1a v1/v2 and the first v3 build all wrote
        // `crop:` + 16 hex digits; the text may hold cut-away words.
        for (era, id) in [
            ("DefaultHasher", "crop:9f3c2a1b0e7d6c54"),
            ("fnv v1", "crop:73d1ba24128a984d"),
            ("fnv v2", "crop:0aa1b2c3d4e5f607"),
            ("fnv v3, unprefixed", "crop:3268992da8e9215b"),
        ] {
            let (_dir, s) = store();
            let r = legacy_sidecar(&s, |j| {
                j["geometry_id"] = id.into();
                j["ocr_text"] = "outside-word ████ partial-tok".into();
            });
            let listed = s.list().unwrap().remove(0);
            assert_eq!(listed.ocr_text, None, "{era}");
            let hash = id.strip_prefix("crop:").unwrap();
            assert_eq!(
                listed.geometry_id,
                Some(format!("{CROP_FRAME_ID_PREFIX}{hash}")),
                "{era}"
            );
            // Persisted: the old text is gone from disk too.
            let on_disk = sidecar_text(&s, &r);
            assert!(!on_disk.contains("outside-word"), "{era}: {on_disk}");
            // Everything else is kept; the list isn't known complete, so
            // the record stays without text: recognized text is refused.
            assert!(listed.redacted && listed.redactions.len() == 1, "{era}");
            assert!(!listed.redactions_complete && !text_allowed(&listed));
            let doc = TextDocument::from_plain("fresh");
            s.record_recognized_text(&listed, &doc, false).unwrap();
            // Idempotent: nothing changes on the next load.
            let before = sidecar_text(&s, &r);
            let again = s.list().unwrap().remove(0);
            assert_eq!(again, listed, "{era}");
            assert_eq!(sidecar_text(&s, &r), before, "{era}: not rewritten");
        }
    }

    #[test]
    fn current_crop_frame_ids_and_other_frames_are_not_migrated() {
        for id in [
            Some("crop3:3268992da8e9215b"),
            Some("5b1d:parts=0Below;rot=1"),
            None,
        ] {
            let (_dir, s) = store();
            let r = legacy_sidecar(&s, |j| {
                if let Some(id) = id {
                    j["geometry_id"] = id.into();
                }
                // Written by a current build: a complete list.
                j["redactions_complete"] = true.into();
                j["ocr_text"] = "kept ████".into();
            });
            let before = sidecar_text(&s, &r);
            let listed = s.list().unwrap().remove(0);
            assert_eq!(listed.ocr_text.as_deref(), Some("kept ████"), "{id:?}");
            assert_eq!(listed.geometry_id.as_deref(), id);
            assert_eq!(sidecar_text(&s, &r), before, "{id:?}: not rewritten");
        }
    }

    #[test]
    fn a_scrub_cropped_before_crop_ids_existed_is_detected_by_its_misplaced_redactions() {
        // A 40 × 20 capture cropped to x 20–40: the stored PNG and size are
        // 20 × 20, but the redaction stayed in uncropped px (x 30–36),
        // wholly outside the stored image, and the text is the whole
        // document's.
        let (_dir, s) = store();
        let r = legacy_sidecar(&s, |j| {
            j["redactions"] = serde_json::to_value(vec![Annotation::Pixelate {
                rect: crate::RectLike::new(30.0, 2.0, 6.0, 4.0),
                block_size: 4.0,
            }])
            .unwrap();
            j["ocr_text"] = "left-of-crop ████".into();
        });
        let listed = s.list().unwrap().remove(0);
        assert_eq!(listed.ocr_text, None);
        assert!(listed.redactions.is_empty(), "never mask with them");
        assert!(!listed.redactions_complete, "unknown");
        assert!(listed.redacted, "still a scrub");
        assert!(!sidecar_text(&s, &r).contains("left-of-crop"));
        // Text recognized later can't be masked: it is never stored.
        s.record_recognized_text(&listed, &TextDocument::from_plain("x"), true)
            .unwrap();
        assert_eq!(s.list().unwrap().remove(0).ocr_text, None);
    }

    #[test]
    fn an_old_scrubbed_sidecar_without_the_flag_reads_as_unknown_and_serves_no_text() {
        let (_dir, s) = store();
        let r = legacy_sidecar(&s, |j| {
            j["ocr_text"] = "████ visible".into();
        });
        let listed = s.list().unwrap().remove(0);
        assert!(listed.redacted && !listed.redactions_complete);
        assert_eq!(listed.ocr_text, None);
        assert!(!sidecar_text(&s, &r).contains("visible"), "dropped on disk");
        // No write can put text on it.
        let mut with_text = listed.clone();
        with_text.ocr_text = Some("████ visible".into());
        s.update(&with_text).unwrap();
        s.rewrite(&with_text, &valid_png(20, 20)).unwrap();
        s.record_recognized_text(&listed, &TextDocument::from_plain("x"), true)
            .unwrap();
        assert_eq!(s.list().unwrap()[0].ocr_text, None);
        // The same record with a complete list keeps its text.
        with_text.redactions_complete = true;
        s.rewrite(&with_text, &valid_png(20, 20)).unwrap();
        assert_eq!(
            s.list().unwrap()[0].ocr_text.as_deref(),
            Some("████ visible")
        );
    }

    #[test]
    fn a_current_write_with_an_unknown_list_is_never_taken_for_misplaced() {
        // A 40 × 20 scrub over an unknown list, written by the current
        // build: its redactions clipped to the image, one of them only
        // meeting the right edge (x 40–45) before clipping.
        let px = |rect| Annotation::Pixelate {
            rect,
            block_size: 4.0,
        };
        let stored = crate::scrub::redactions_in_image(
            &[
                px(crate::RectLike::new(40.0, 2.0, 5.0, 4.0)),
                px(crate::RectLike::new(2.0, 20.0, 5.0, 4.0)),
                px(crate::RectLike::new(35.0, 2.0, 10.0, 4.0)),
            ],
            40,
            20,
        );
        assert_eq!(stored, vec![px(crate::RectLike::new(35.0, 2.0, 5.0, 4.0))]);
        let mut r = CaptureRecord::new(Utc::now(), 40, 20, "d");
        r.redacted = true;
        r.redactions = stored.clone();
        assert!(!migrate_legacy_record(&mut r), "nothing to migrate");
        assert_eq!(r.redactions, stored, "kept");
        // An edge-only rect (as stored by an older writer) is misplaced.
        r.redactions = vec![px(crate::RectLike::new(40.0, 2.0, 5.0, 4.0))];
        assert!(migrate_legacy_record(&mut r));
        assert!(r.redactions.is_empty());
    }

    #[test]
    fn the_misplaced_redaction_check_only_touches_legacy_records() {
        let outside = serde_json::to_value(vec![Annotation::Pixelate {
            rect: crate::RectLike::new(30.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        }])
        .unwrap();
        for (era, edit) in [
            (
                "complete list",
                (|j: &mut serde_json::Value| j["redactions_complete"] = true.into())
                    as fn(&mut serde_json::Value),
            ),
            ("current crop id", |j| j["geometry_id"] = "crop3:00".into()),
        ] {
            let (_dir, s) = store();
            let o = outside.clone();
            let r = legacy_sidecar(&s, move |j| {
                j["redactions"] = o;
                edit(j);
            });
            let before = sidecar_text(&s, &r);
            let listed = s.list().unwrap().remove(0);
            assert_eq!(listed.redactions.len(), 1, "{era}: kept");
            assert_eq!(sidecar_text(&s, &r), before, "{era}: not rewritten");
        }
    }
}
