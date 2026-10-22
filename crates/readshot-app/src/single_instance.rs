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
//!   reopen/URL tick ([`take_requests`], on the dir `main` handed over
//!   with [`register`]) and feeds each into the queues the macOS
//!   AppleEvent handlers fill. Only regular files are read, at most
//!   1 KiB each; links and special files are removed unread, and a
//!   `.tmp` left by a launch that failed mid-write goes after a minute.
//! - The OS drops the lock when its holder exits, crashed or not, so a
//!   lock file left behind never blocks the next launch.
//!
//! No network surface: the directory's own permissions (per user; 0700
//! under `$XDG_RUNTIME_DIR`, the profile's ACL under `%LOCALAPPDATA%`)
//! are the access control. Standard library only.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const LOCK_FILE: &str = "readshot.lock";
const REQUEST_EXT: &str = "req";
/// What a second launch with no URL asks for: the macOS reopen.
const REOPEN: &str = "reopen";
const SCHEME: &str = "readshot://";
/// A request is `reopen` or a short URL; a bigger file is not ours.
const MAX_REQUEST_LEN: u64 = 1024;
/// A `.tmp` older than this is from a launch that failed mid-write: a
/// live one is renamed within microseconds.
const STALE_TMP_AGE: Duration = Duration::from_secs(60);

/// The mailbox of this process's copy, handed over by `main` once it
/// holds the lock ([`register`]); the runtime copies it into its state.
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
    dir: PathBuf,
}

impl InstanceGuard {
    /// The directory later launches leave their requests in.
    pub fn mailbox(&self) -> &Path {
        &self.dir
    }
}

/// Make `guard`'s mailbox the one this process's runtime reads
/// ([`registered_mailbox`]). Called by `main`; tests pass a dir to the
/// runtime directly instead, so they don't share this global.
pub fn register(guard: &InstanceGuard) {
    let _ = MAILBOX.set(guard.dir.clone());
}

/// The mailbox [`register`] set, if this process holds the lock.
pub fn registered_mailbox() -> Option<PathBuf> {
    MAILBOX.get().cloned()
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
    // them rather than act on them now. Only those named before the
    // lock was taken: a launch that found it held a moment ago keeps its
    // request for the tick.
    drop_stale_requests(dir, SystemTime::now());
    Ok(Instance::Primary(InstanceGuard {
        _lock: lock,
        dir: dir.to_path_buf(),
    }))
}

/// Requests later launches left in `dir`, oldest first, each taken
/// once. Polled by the runtime's reopen tick with the registered
/// mailbox. Cheap enough for the UI thread: a directory listing, and at
/// most 1 KiB read from each regular file.
pub fn take_requests(dir: &Path) -> Vec<WakeRequest> {
    drain(dir)
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
    // (`request_time` reads the time back from this name.)
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

/// When a request named `wake-<nanos>-…` was left, if the name says.
fn request_time(path: &Path) -> Option<SystemTime> {
    let name = path.file_name()?.to_str()?;
    let nanos: u64 = name
        .strip_prefix("wake-")?
        .split('-')
        .next()?
        .parse()
        .ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos))
}

/// Remove the requests in `dir` left before `before` (or with no time
/// in their name). Newer ones stay for [`drain`].
fn drop_stale_requests(dir: &Path, before: SystemTime) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.extension().is_some_and(|e| e == REQUEST_EXT)
            && request_time(&path).is_none_or(|t| t < before)
        {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Read and remove every `*.req` in `dir`, oldest first (the names sort
/// by time). Only a regular file is read (by `symlink_metadata`, so a
/// link isn't followed and a FIFO never blocks the read), and at most
/// [`MAX_REQUEST_LEN`] of it; anything else, or anything bigger, is
/// removed unread. A `.tmp` older than [`STALE_TMP_AGE`] is removed too.
fn drain(dir: &Path) -> Vec<WakeRequest> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut requests: Vec<PathBuf> = Vec::new();
    for path in entries.flatten().map(|e| e.path()) {
        match path.extension().and_then(|e| e.to_str()) {
            Some(REQUEST_EXT) => requests.push(path),
            Some("tmp") => {
                let old = std::fs::symlink_metadata(&path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > STALE_TMP_AGE);
                if old {
                    let _ = std::fs::remove_file(&path);
                }
            }
            _ => {}
        }
    }
    requests.sort();
    requests
        .into_iter()
        .filter_map(|path| {
            let raw = read_request(&path);
            let _ = std::fs::remove_file(&path);
            raw.map(|raw| WakeRequest::decode(&raw))
        })
        .collect()
}

/// A request file's text: a regular file (not followed if a link) of at
/// most [`MAX_REQUEST_LEN`] bytes, else `None`.
fn read_request(path: &Path) -> Option<String> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_file() || meta.len() > MAX_REQUEST_LEN {
        return None;
    }
    let mut raw = String::new();
    File::open(path)
        .ok()?
        .take(MAX_REQUEST_LEN + 1)
        .read_to_string(&mut raw)
        .ok()?;
    (raw.len() as u64 <= MAX_REQUEST_LEN).then_some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// fix1 review n1: only regular files are read, and never more than
    /// the cap; a link is removed unread, whatever it points to. Through
    /// `/dev/zero` (a 0-byte special file to `metadata`) an unchecked
    /// read would never end. (No FIFO is made here: spawning `mkfifo`
    /// from a test forks while other tests hold the lock file open.)
    #[cfg(unix)]
    #[test]
    fn links_and_special_files_are_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"readshot://new").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("wake-1.req")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", dir.path().join("wake-2.req")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let path = dir.path().to_path_buf();
        std::thread::spawn(move || tx.send(drain(&path)).unwrap());
        let got = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("drain kept reading a special file");
        assert_eq!(got, Vec::new());
        assert!(target.exists(), "a link's target is never touched");
        assert!(!dir.path().join("wake-2.req").exists());
    }

    /// fix1 review n2: the cleanup when a copy takes the lock drops only
    /// requests from before it; one another launch left a moment later
    /// is kept for the tick.
    #[test]
    fn startup_cleanup_keeps_a_request_left_after_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(format!("wake-{:032}-1-0.req", 5)),
            b"reopen",
        )
        .unwrap();
        let first = acquire(dir.path(), None).unwrap();
        assert!(matches!(first, Instance::Primary(_)));
        let locked_at = SystemTime::now();
        let url = WakeRequest::Url("readshot://new".into());
        assert!(matches!(
            acquire(dir.path(), Some(&url)).unwrap(),
            Instance::Secondary
        ));
        // A cleanup that runs after this (a slow startup) keeps it.
        drop_stale_requests(dir.path(), locked_at);
        assert_eq!(drain(dir.path()), vec![url]);
        drop(first);
    }

    /// fix1 review n3: a `.tmp` a failed or crashed launch left behind is
    /// removed once it is old; a fresh one (a rename about to happen) is
    /// left alone.
    #[test]
    fn old_temp_files_are_removed_and_fresh_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let old = dir.path().join("wake-1.tmp");
        let fresh = dir.path().join("wake-2.tmp");
        std::fs::write(&old, b"reopen").unwrap();
        std::fs::write(&fresh, b"reopen").unwrap();
        let long_ago = std::time::SystemTime::now() - Duration::from_secs(3600);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        assert_eq!(drain(dir.path()), Vec::new());
        assert!(!old.exists());
        assert!(fresh.exists());
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
