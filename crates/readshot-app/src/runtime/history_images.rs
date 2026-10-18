//! Image handles for the History window's thumbnails and preview.
//!
//! iced 0.14 keys a `Handle::from_path` by a hash of the path alone, so a
//! raster it decoded stays in the renderer's cache under that id. A scrub
//! replaces a record's PNG and thumbnail **at the same paths**: with path
//! handles, a History window kept open on the record would go on drawing
//! the unredacted image. Handles here are keyed by the file as it is now
//! (size, modification time, and on Unix the inode, which a scrub's
//! rename always changes): a replaced file gets a new handle, with a new
//! id, so the old raster is never drawn again (and is trimmed by iced
//! once unused).

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use iced::widget::image::Handle;

/// What identifies a file's present contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    file_id: u64,
}

#[cfg(unix)]
fn file_id(m: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.ino()
}

#[cfg(not(unix))]
fn file_id(_m: &Metadata) -> u64 {
    0
}

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some(Stamp {
        len: m.len(),
        modified: m.modified().ok(),
        file_id: file_id(&m),
    })
}

/// Most handles kept; past it the cache starts over (a History page shows
/// far fewer).
const MAX_CACHED: usize = 512;

type Cache = Mutex<HashMap<PathBuf, (Stamp, Handle)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A handle showing `path` as it is on disk now: the same handle while
/// the file is unchanged, a new one (new id) once it was replaced.
pub(crate) fn history_image_handle(path: &Path) -> Handle {
    let Some(now) = stamp(path) else {
        return Handle::from_path(path);
    };
    let Ok(mut cache) = cache().lock() else {
        return Handle::from_path(path);
    };
    if let Some((s, h)) = cache.get(path) {
        if *s == now {
            return h.clone();
        }
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Handle::from_path(path);
    };
    let handle = Handle::from_bytes(bytes);
    if cache.len() >= MAX_CACHED {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (now, handle.clone()));
    handle
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_file_gets_a_new_handle() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("x.png");
        std::fs::write(&path, b"before").unwrap();
        let first = history_image_handle(&path);
        assert_eq!(history_image_handle(&path).id(), first.id(), "unchanged");
        // A scrub renames a staged file over it.
        let staged = dir.path().join("x.stage.png");
        std::fs::write(&staged, b"after, redacted").unwrap();
        std::fs::rename(&staged, &path).unwrap();
        let replaced = history_image_handle(&path);
        assert_ne!(replaced.id(), first.id());
        assert!(matches!(&replaced, Handle::Bytes(_, b) if b.as_ref() == b"after, redacted"));
        // The same path's id differs from the path-keyed handle iced would
        // reuse for any contents.
        assert_ne!(first.id(), Handle::from_path(&path).id());
    }
}
