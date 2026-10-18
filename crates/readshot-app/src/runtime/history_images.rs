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
//!
//! A handle holds the file's encoded bytes, so the cache is bounded by
//! total bytes ([`MAX_BYTES`]) and entries ([`MAX_ENTRIES`]), evicting the
//! least recently used. The entry just used is always kept, however
//! large: dropping it would give the next frame a new id and re-decode
//! the image every frame.

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

/// Encoded bytes the cache may hold: a page of thumbnails plus a few full
/// previews.
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Most entries kept (a History page shows far fewer thumbnails).
const MAX_ENTRIES: usize = 256;

struct Entry {
    stamp: Stamp,
    handle: Handle,
    bytes: usize,
    used: u64,
}

/// An LRU cache of handles bounded by total bytes and entry count.
struct HandleCache {
    max_bytes: usize,
    max_entries: usize,
    entries: HashMap<PathBuf, Entry>,
    bytes: usize,
    clock: u64,
}

impl HandleCache {
    fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            max_bytes,
            max_entries,
            entries: HashMap::new(),
            bytes: 0,
            clock: 0,
        }
    }

    fn handle(&mut self, path: &Path) -> Handle {
        let Some(now) = stamp(path) else {
            self.remove(path);
            return Handle::from_path(path);
        };
        self.clock += 1;
        if let Some(e) = self.entries.get_mut(path) {
            if e.stamp == now {
                e.used = self.clock;
                return e.handle.clone();
            }
        }
        // Gone or replaced: never serve the old handle again.
        self.remove(path);
        let Ok(bytes) = std::fs::read(path) else {
            return Handle::from_path(path);
        };
        let size = bytes.len();
        let handle = Handle::from_bytes(bytes);
        self.bytes += size;
        self.entries.insert(
            path.to_path_buf(),
            Entry {
                stamp: now,
                handle: handle.clone(),
                bytes: size,
                used: self.clock,
            },
        );
        self.evict(path);
        handle
    }

    fn remove(&mut self, path: &Path) {
        if let Some(e) = self.entries.remove(path) {
            self.bytes -= e.bytes;
        }
    }

    /// Drop least recently used entries until within budget, never `keep`.
    fn evict(&mut self, keep: &Path) {
        while self.bytes > self.max_bytes || self.entries.len() > self.max_entries {
            let oldest = self
                .entries
                .iter()
                .filter(|(p, _)| p.as_path() != keep)
                .min_by_key(|(_, e)| e.used)
                .map(|(p, _)| p.clone());
            match oldest {
                Some(p) => self.remove(&p),
                None => break,
            }
        }
    }
}

fn cache() -> &'static Mutex<HandleCache> {
    static CACHE: OnceLock<Mutex<HandleCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HandleCache::new(MAX_BYTES, MAX_ENTRIES)))
}

/// A handle showing `path` as it is on disk now: the same handle while
/// the file is unchanged, a new one (new id) once it was replaced.
pub(crate) fn history_image_handle(path: &Path) -> Handle {
    match cache().lock() {
        Ok(mut cache) => cache.handle(path),
        Err(_) => Handle::from_path(path),
    }
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

    #[test]
    fn the_cache_holds_its_byte_budget_evicting_the_least_recently_used() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = |name: &str, len: usize| {
            let p = dir.path().join(name);
            std::fs::write(&p, vec![7u8; len]).unwrap();
            p
        };
        let mut cache = HandleCache::new(1000, 10);
        let a = file("a.png", 400);
        let b = file("b.png", 400);
        let c = file("c.png", 400);
        let a_id = cache.handle(&a).id();
        cache.handle(&b);
        cache.handle(&a); // a is now more recent than b
        cache.handle(&c);
        assert!(cache.bytes <= 1000, "{}", cache.bytes);
        assert_eq!(
            cache.bytes,
            cache.entries.values().map(|e| e.bytes).sum::<usize>()
        );
        assert!(cache.entries.contains_key(&a) && !cache.entries.contains_key(&b));
        assert_eq!(cache.handle(&a).id(), a_id, "a recent entry keeps its id");
        // An entry larger than the whole budget is kept alone (else every
        // frame would re-read and re-decode it).
        let big = file("big.png", 5000);
        let big_id = cache.handle(&big).id();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.handle(&big).id(), big_id);
        // The entry count is bounded too.
        let mut small = HandleCache::new(usize::MAX, 3);
        for i in 0..6 {
            small.handle(&file(&format!("{i}.png"), 10));
        }
        assert_eq!(small.entries.len(), 3);
        // A replaced or removed file's bytes leave the budget.
        std::fs::remove_file(&big).unwrap();
        cache.handle(&big);
        assert_eq!(cache.bytes, 0);
    }
}
