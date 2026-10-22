//! One running Readshot per user on Windows and Linux.
//!
//! macOS needs none of this: Launch Services never starts a second copy
//! of the app, it sends the running one a reopen (or `readshot://`)
//! AppleEvent (see `app_events` and `url_events`). Elsewhere a second
//! launch would start a second menu icon that can't register the capture
//! shortcut, so `main` calls [`acquire`] first:
//!
//! - The first copy takes an advisory lock on `readshot.lock` in a
//!   per-user directory ([`lock_dir`]).
//! - A later copy finds the lock held, drops a request file
//!   (`wake-<pid>-<n>.req`, written to a temp name and renamed) into the
//!   same directory, and exits 0 at once. It never waits on the first
//!   copy, which may be slow, starting or hung.
//! - The first copy picks the files up on the runtime's 500 ms
//!   reopen/URL tick ([`take_requests`]) and feeds each into the queues
//!   the macOS AppleEvent handlers fill.
//! - The OS drops the lock when its holder exits, crashed or not, so a
//!   lock file left behind never blocks the next launch.
//!
//! No network surface: the directory's own permissions (per user; 0700
//! under `$XDG_RUNTIME_DIR`, the profile's ACL under `%LOCALAPPDATA%`)
//! are the access control. Standard library only.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const LOCK_FILE: &str = "readshot.lock";
const REQUEST_EXT: &str = "req";
/// What a second launch with no URL asks for: the macOS reopen.
const REOPEN: &str = "reopen";
const SCHEME: &str = "readshot://";
/// A request is `reopen` or a short URL; a bigger file is not ours.
const MAX_REQUEST_LEN: u64 = 1024;

/// The mailbox the first copy drains, set when it takes the lock.
static MAILBOX: OnceLock<PathBuf> = OnceLock::new();

/// What a second launch asks the running copy to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WakeRequest {
    /// Show or focus the ready window, like a macOS reopen.
    Reopen,
    /// A `readshot://` URL the second copy was launched with.
    Url(String),
}

impl WakeRequest {
    fn encode(&self) -> &str {
        match self {
            WakeRequest::Reopen => REOPEN,
            WakeRequest::Url(url) => url,
        }
    }

    /// A request file's contents. Anything that isn't a `readshot://`
    /// URL is a reopen.
    fn decode(raw: &str) -> Self {
        let raw = raw.trim();
        if raw
            .get(..SCHEME.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(SCHEME))
        {
            WakeRequest::Url(raw.to_string())
        } else {
            WakeRequest::Reopen
        }
    }
}

/// The result of [`acquire`].
#[derive(Debug)]
pub enum Instance {
    /// This is the only copy. Keep the guard alive for the whole run:
    /// dropping it releases the lock.
    Primary(InstanceGuard),
    /// Another copy holds the lock and was left its request. Exit 0.
    Secondary,
}

/// Holds the lock for as long as it lives.
#[derive(Debug)]
pub struct InstanceGuard {
    _lock: File,
}

/// The per-user directory for the lock and the requests:
/// `$XDG_RUNTIME_DIR/readshot` on Linux when set (per-user, mode 0700),
/// else the per-user local data dir (`%LOCALAPPDATA%\pawanpaudel\Readshot\data`
/// on Windows, `~/.local/share/readshot` on Linux).
pub fn lock_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")?;
    Some(
        dirs.runtime_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .to_path_buf(),
    )
}

/// Become the running copy, or leave the running one `request` and
/// return at once. `None` (a login launch) leaves nothing: a silent
/// launch must not pop the running copy's window.
pub fn acquire(dir: &Path, request: Option<&WakeRequest>) -> io::Result<Instance> {
    std::fs::create_dir_all(dir)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            if let Some(request) = request {
                if let Err(e) = leave_request(dir, request) {
                    tracing::warn!(
                        target: readshot_core::log::cat::APP,
                        "another Readshot is running but couldn't be asked to show itself: {e}"
                    );
                }
            }
            return Ok(Instance::Secondary);
        }
        Err(TryLockError::Error(e)) => return Err(e),
    }
    // Requests left for a copy that has since exited are stale: drop
    // them rather than act on them now.
    let _ = drain(dir);
    let _ = MAILBOX.set(dir.to_path_buf());
    Ok(Instance::Primary(InstanceGuard { _lock: lock }))
}

/// Requests later launches left for this copy, oldest first (empty
/// unless [`acquire`] made this the running copy). Polled by the
/// runtime's reopen tick.
pub fn take_requests() -> Vec<WakeRequest> {
    MAILBOX.get().map(|dir| drain(dir)).unwrap_or_default()
}

/// The production delivery: the same queues the macOS AppleEvent
/// handlers fill.
pub fn deliver(request: WakeRequest) {
    match request {
        WakeRequest::Reopen => crate::app_events::queue_reopen_request(),
        WakeRequest::Url(url) => {
            if let Err(e) = crate::url_events::deliver_url_string(&url) {
                tracing::warn!(
                    target: readshot_core::log::cat::URL,
                    "ignoring forwarded URL `{url}`: {e}"
                );
            }
        }
    }
}

/// Write the request under a temp name, then rename it to `*.req`, so
/// the first copy never reads half a file.
fn leave_request(dir: &Path, request: &WakeRequest) -> io::Result<()> {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let stem = format!("wake-{nanos:032}-{}-{n}", std::process::id());
    let tmp = dir.join(format!("{stem}.tmp"));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    file.write_all(request.encode().as_bytes())?;
    drop(file);
    std::fs::rename(&tmp, dir.join(format!("{stem}.{REQUEST_EXT}")))
}

/// Read and remove every `*.req` in `dir`, oldest first (the names sort
/// by time). Unreadable or oversized files are removed and skipped.
fn drain(dir: &Path) -> Vec<WakeRequest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == REQUEST_EXT))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let small = std::fs::metadata(&path).is_ok_and(|m| m.len() <= MAX_REQUEST_LEN);
            let raw = small.then(|| std::fs::read_to_string(&path).ok()).flatten();
            let _ = std::fs::remove_file(&path);
            raw.map(|raw| WakeRequest::decode(&raw))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_second_launch_leaves_its_request_and_steps_aside() {
        let dir = tempfile::tempdir().unwrap();
        let first = acquire(dir.path(), Some(&WakeRequest::Reopen)).unwrap();
        assert!(matches!(first, Instance::Primary(_)));

        let second = acquire(dir.path(), Some(&WakeRequest::Reopen)).unwrap();
        assert!(matches!(second, Instance::Secondary));
        let url = WakeRequest::Url("readshot://new".into());
        assert!(matches!(
            acquire(dir.path(), Some(&url)).unwrap(),
            Instance::Secondary
        ));
        assert_eq!(drain(dir.path()), vec![WakeRequest::Reopen, url]);
        assert_eq!(drain(dir.path()), Vec::new(), "each is taken once");
        drop(first);
    }

    /// W5d review I1: a second launch never waits on the first. Here the
    /// lock is held by a copy that never answers (or is still starting).
    #[test]
    fn a_second_launch_never_waits_on_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let held = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(dir.path().join(LOCK_FILE))
            .unwrap();
        held.try_lock().unwrap();
        let started = std::time::Instant::now();
        let second = acquire(dir.path(), Some(&WakeRequest::Reopen)).unwrap();
        assert!(matches!(second, Instance::Secondary));
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the second launch waited {:?}",
            started.elapsed()
        );
    }

    /// W5d review m1: a login launch while a copy runs leaves nothing,
    /// so the running copy's window doesn't pop up.
    #[test]
    fn a_login_launch_leaves_no_request() {
        let dir = tempfile::tempdir().unwrap();
        let _first = acquire(dir.path(), None).unwrap();
        assert!(matches!(
            acquire(dir.path(), None).unwrap(),
            Instance::Secondary
        ));
        assert_eq!(drain(dir.path()), Vec::new());
    }

    #[test]
    fn a_lock_left_by_a_dead_copy_does_not_block_the_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        // What a crash leaves: the lock file (no holder) and a request
        // nobody took. The new copy is primary and drops the stale one.
        std::fs::write(dir.path().join(LOCK_FILE), b"").unwrap();
        std::fs::write(dir.path().join("wake-0-1-0.req"), b"reopen").unwrap();
        let first = acquire(dir.path(), Some(&WakeRequest::Reopen)).unwrap();
        assert!(matches!(first, Instance::Primary(_)));
        assert_eq!(drain(dir.path()), Vec::new());
        drop(first);
        // Its own exit releases the lock too.
        let again = acquire(dir.path(), Some(&WakeRequest::Reopen)).unwrap();
        assert!(matches!(again, Instance::Primary(_)));
    }

    #[test]
    fn junk_and_half_written_files_are_not_requests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("wake-1.tmp"), b"readshot://new").unwrap();
        std::fs::write(dir.path().join("wake-2.req"), vec![b'x'; 4096]).unwrap();
        std::fs::write(dir.path().join("wake-3.req"), b"https://example.com").unwrap();
        assert_eq!(drain(dir.path()), vec![WakeRequest::Reopen]);
        assert!(dir.path().join("wake-1.tmp").exists(), "not renamed yet");
        assert_eq!(
            WakeRequest::decode("Readshot://new"),
            WakeRequest::Url("Readshot://new".into())
        );
    }
}
