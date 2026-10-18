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

/// Encoded bytes of **full-size** images (previews, and rows whose
/// thumbnail isn't there yet) the cache keeps from earlier view builds.
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Thumbnails kept from earlier view builds (each is small; a History
/// page shows far fewer).
const MAX_ENTRIES: usize = 256;

struct Entry {
    stamp: Stamp,
    handle: Handle,
    bytes: usize,
    /// A full-size image (counts against `max_bytes`), not a thumbnail
    /// (counts against `max_entries`).
    full: bool,
    used: u64,
    /// The view build that last asked for it.
    generation: u64,
}

/// Whether `path` is a full-size image rather than a History thumbnail.
fn is_full_size(path: &Path) -> bool {
    !path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".thumb.png"))
}

/// An LRU cache of handles. Entries asked for during the current view
/// build ([`HandleCache::begin_build`]) are never evicted: a view that
/// shows more than the cap must not evict the very entries it needs next
/// (an LRU under such a cyclic scan misses every time, re-reading and
/// re-decoding on the UI thread). Older entries are evicted, least
/// recently used first, past [`MAX_BYTES`] of full-size images or
/// [`MAX_ENTRIES`] thumbnails.
struct HandleCache {
    max_bytes: usize,
    max_entries: usize,
    entries: HashMap<PathBuf, Entry>,
    full_bytes: usize,
    thumbs: usize,
    clock: u64,
    generation: u64,
}

impl HandleCache {
    fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self {
            max_bytes,
            max_entries,
            entries: HashMap::new(),
            full_bytes: 0,
            thumbs: 0,
            clock: 0,
            generation: 0,
        }
    }

    /// A new view build starts: entries of earlier builds become
    /// evictable. Eviction runs only when an entry is added, so a build
    /// that asks for the same handles as the last one finds them all.
    fn begin_build(&mut self) {
        self.generation += 1;
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
                e.generation = self.generation;
                return e.handle.clone();
            }
        }
        // Gone or replaced: never serve the old handle again.
        self.remove(path);
        let Ok(bytes) = std::fs::read(path) else {
            return Handle::from_path(path);
        };
        let size = bytes.len();
        let full = is_full_size(path);
        let handle = Handle::from_bytes(bytes);
        if full {
            self.full_bytes += size;
        } else {
            self.thumbs += 1;
        }
        self.entries.insert(
            path.to_path_buf(),
            Entry {
                stamp: now,
                handle: handle.clone(),
                bytes: size,
                full,
                used: self.clock,
                generation: self.generation,
            },
        );
        self.evict();
        handle
    }

    fn remove(&mut self, path: &Path) {
        if let Some(e) = self.entries.remove(path) {
            if e.full {
                self.full_bytes -= e.bytes;
            } else {
                self.thumbs -= 1;
            }
        }
    }

    /// The least recently used entry of an earlier build, of the given
    /// size class.
    fn oldest_evictable(&self, full: bool) -> Option<PathBuf> {
        self.entries
            .iter()
            .filter(|(_, e)| e.full == full && e.generation < self.generation)
            .min_by_key(|(_, e)| e.used)
            .map(|(p, _)| p.clone())
    }

    /// Bring earlier builds' entries within budget; this build's stay.
    fn evict(&mut self) {
        while self.full_bytes > self.max_bytes {
            match self.oldest_evictable(true) {
                Some(p) => self.remove(&p),
                None => break,
            }
        }
        while self.thumbs > self.max_entries {
            match self.oldest_evictable(false) {
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

/// Call once at the start of every History view build, before asking for
/// handles: see [`HandleCache`].
pub(crate) fn begin_history_view_build() {
    if let Ok(mut cache) = cache().lock() {
        cache.begin_build();
    }
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

    fn files(dir: &Path, prefix: &str, n: usize, len: usize) -> Vec<PathBuf> {
        (0..n)
            .map(|i| {
                let p = dir.join(format!("{prefix}{i}"));
                std::fs::write(&p, vec![7u8; len]).unwrap();
                p
            })
            .collect()
    }

    #[test]
    fn a_view_larger_than_the_cache_never_misses_on_the_next_build() {
        // The probe: one view asks for more handles than the cache holds
        // (Show More ×5, or full PNGs while thumbnails are missing), twice.
        let dir = tempfile::TempDir::new().unwrap();
        let rows: Vec<PathBuf> = (0..301)
            .map(|i| {
                let p = dir.path().join(format!("{i}.thumb.png"));
                std::fs::write(&p, vec![1u8; 100]).unwrap();
                p
            })
            .collect();
        let previews = files(dir.path(), "full", 60, 2000);
        let mut cache = HandleCache::new(10_000, 256);
        let build = |cache: &mut HandleCache| -> Vec<String> {
            cache.begin_build();
            rows.iter()
                .chain(&previews)
                .map(|p| format!("{:?}", cache.handle(p).id()))
                .collect()
        };
        let first = build(&mut cache);
        let second = build(&mut cache);
        let misses = first.iter().zip(&second).filter(|(a, b)| a != b).count();
        assert_eq!(misses, 0);
    }

    #[test]
    fn earlier_builds_are_held_to_the_budgets_least_recently_used_first() {
        let dir = tempfile::TempDir::new().unwrap();
        let full = files(dir.path(), "full", 3, 400);
        let mut cache = HandleCache::new(1000, 2);
        cache.begin_build();
        let a_id = cache.handle(&full[0]).id();
        cache.handle(&full[1]);
        cache.begin_build();
        cache.handle(&full[0]); // a is now more recent than b
        cache.handle(&full[2]);
        // Over budget (1200 > 1000), but b is the only earlier-build entry.
        assert!(!cache.entries.contains_key(&full[1]));
        assert!(cache.full_bytes <= 1000, "{}", cache.full_bytes);
        assert_eq!(
            cache.handle(&full[0]).id(),
            a_id,
            "a recent entry keeps its id"
        );
        // An entry larger than the whole budget is kept while it is shown,
        // and evicted once a later build no longer asks for it.
        let big = files(dir.path(), "big", 1, 5000).remove(0);
        cache.begin_build();
        let big_id = cache.handle(&big).id();
        assert_eq!(cache.handle(&big).id(), big_id);
        cache.begin_build();
        cache.handle(&full[0]);
        cache.handle(&full[1]); // an insert applies the budget
        assert!(!cache.entries.contains_key(&big));
        assert!(cache.full_bytes <= 1000);
        // Thumbnails are bounded by count, not by the full-size budget.
        let thumbs: Vec<PathBuf> = (0..5)
            .map(|i| {
                let p = dir.path().join(format!("{i}.thumb.png"));
                std::fs::write(&p, vec![1u8; 900]).unwrap();
                p
            })
            .collect();
        cache.begin_build();
        thumbs.iter().for_each(|p| {
            cache.handle(p);
        });
        assert_eq!(cache.thumbs, 5, "all shown now");
        cache.begin_build();
        let extra = dir.path().join("x.thumb.png");
        std::fs::write(&extra, vec![1u8; 900]).unwrap();
        cache.handle(&extra);
        assert_eq!(cache.thumbs, 2);
        // A removed file's entry and bytes leave the cache.
        std::fs::remove_file(&full[0]).unwrap();
        cache.handle(&full[0]);
        assert!(!cache.entries.contains_key(&full[0]));
    }
}
