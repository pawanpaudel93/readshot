//! Which History records are visible and selected (search, keyboard
//! moves) and where their files are: logic behind the History window,
//! kept out of its view.

use std::path::PathBuf;

// Search semantics live in readshot-core (shared with the MCP
// server's search_captures tool) so the two surfaces can't drift.
pub(crate) use readshot_core::history::record_matches_query as record_matches;

pub(crate) fn visible_history_ids(
    records: &[readshot_core::CaptureRecord],
    query: &str,
) -> Vec<readshot_core::Uuid> {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .map(|r| r.id)
        .collect()
}

pub(crate) fn history_record_visible(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    id: readshot_core::Uuid,
) -> bool {
    visible_history_ids(records, query).contains(&id)
}

pub(crate) fn preferred_history_selection(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    current: Option<readshot_core::Uuid>,
) -> Option<readshot_core::Uuid> {
    let visible = visible_history_ids(records, query);
    current
        .filter(|id| visible.contains(id))
        .or_else(|| visible.first().copied())
}

pub(crate) fn adjacent_history_selection(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    current: Option<readshot_core::Uuid>,
    delta: isize,
) -> Option<readshot_core::Uuid> {
    let visible = visible_history_ids(records, query);
    if visible.is_empty() {
        return None;
    }
    let current_idx = current
        .and_then(|id| visible.iter().position(|candidate| *candidate == id))
        .unwrap_or(0);
    let max = visible.len() as isize - 1;
    let next_idx = (current_idx as isize + delta).clamp(0, max) as usize;
    visible.get(next_idx).copied()
}

pub(crate) fn visible_history_text(
    records: &[readshot_core::CaptureRecord],
    query: &str,
) -> String {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .filter_map(|r| r.ocr_text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

pub(crate) fn visible_history_text_count(
    records: &[readshot_core::CaptureRecord],
    query: &str,
) -> usize {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .filter(|r| {
            r.ocr_text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        })
        .count()
}

/// Load + decode a PNG file off the main thread so a multi-MB Retina
/// capture doesn't stall the iced runtime. Returns the decoded image
/// in the same `RgbaImage` shape the capture pipeline produces.
pub(crate) async fn load_png_async(path: PathBuf) -> Result<image::RgbaImage, image::ImageError> {
    tokio::task::spawn_blocking(move || -> Result<image::RgbaImage, image::ImageError> {
        let dyn_img = image::ImageReader::open(&path)?
            .with_guessed_format()?
            .decode()?;
        Ok(dyn_img.to_rgba8())
    })
    .await
    .unwrap_or_else(|join_err| {
        // Surface a join failure as an IO error so the caller's
        // `Result<RgbaImage, ImageError>` handling stays uniform.
        Err(image::ImageError::IoError(std::io::Error::other(format!(
            "blocking task panicked: {join_err}"
        ))))
    })
}

/// The image of `record` History may show or hand out: `None` while it
/// is withheld (`scrub_pending`, a redaction being written). Its image is
/// then set aside and nothing is at its path, except in the instant
/// after a crash between marking it and setting it aside: this check
/// covers that too.
pub(crate) fn history_png_path(
    root: &std::path::Path,
    record: &readshot_core::CaptureRecord,
) -> Option<PathBuf> {
    (!record.scrub_pending).then(|| readshot_core::FsHistoryStore::abs_png_path(root, record))
}

/// `record`'s thumbnail, as [`history_png_path`].
pub(crate) fn history_thumbnail_path(
    root: &std::path::Path,
    record: &readshot_core::CaptureRecord,
) -> Option<PathBuf> {
    (!record.scrub_pending).then(|| readshot_core::FsHistoryStore::abs_thumbnail_path(root, record))
}
