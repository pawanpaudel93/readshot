//! Crash-safe file writes.
//!
//! [`write_atomic`] stages the new contents to a sibling temp file
//! and then renames it onto the target path. POSIX `rename(2)` is
//! atomic within a single filesystem, so a crash either leaves the
//! previous contents intact or replaces them with the new contents —
//! never a half-written file.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `contents` to `path` atomically. The parent directory must
/// already exist; the temp file lives next to the target so the
/// final `rename` is on the same filesystem. The temp suffix
/// includes the process id so two processes writing the same path
/// concurrently don't fight over the same staging file. On any
/// failure the temp file is cleaned up before the error is returned.
pub(crate) fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp_path = temp_path_for(path)?;
    if let Err(e) = fs::write(&tmp_path, contents) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

fn temp_path_for(path: &Path) -> io::Result<std::path::PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let mut tmp_name = file_name.to_os_string();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    tmp_name.push(format!(".tmp.{}.{}", std::process::id(), counter));
    Ok(parent.join(tmp_name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn temp_paths_are_unique_for_repeated_writes_in_one_process() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");

        let first = temp_path_for(&path).unwrap();
        let second = temp_path_for(&path).unwrap();

        assert_ne!(first, second);
    }

    #[test]
    fn creates_new_file_with_expected_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn overwrites_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        fs::write(&path, b"old").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[test]
    fn does_not_leave_tmp_file_on_success() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        write_atomic(&path, b"x").unwrap();
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "stale tmp files left behind");
    }

    #[test]
    fn returns_error_when_parent_missing() {
        let path = Path::new("/nonexistent-readshot-test-dir/out.txt");
        assert!(write_atomic(path, b"x").is_err());
    }
}
