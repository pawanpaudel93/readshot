//! One running Readshot per user on Windows and Linux.
//!
//! macOS needs none of this: Launch Services never starts a second copy
//! of the app, it sends the running one a reopen (or `readshot://`)
//! AppleEvent (see `app_events` and `url_events`). Elsewhere a second
//! launch would start a second menu icon that can't register the capture
//! shortcut, so `main` calls [`acquire`] first:
//!
//! - The first copy takes an advisory lock on `readshot.lock` in a
//!   per-user directory ([`lock_dir`]) and listens on a loopback port.
//!   The port and a random token go in `readshot.instance` beside it.
//! - A later copy finds the lock held, sends the token and its request
//!   (reopen, or the `readshot://` URL it was launched with) to that
//!   port, and exits 0. The first copy feeds the request into the same
//!   queues the macOS AppleEvent handlers fill.
//! - The OS drops the lock when its holder exits, crashed or not, so a
//!   lock file left behind never blocks the next launch.
//!
//! Standard library only: `File::try_lock` and `std::net`.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const LOCK_FILE: &str = "readshot.lock";
const INFO_FILE: &str = "readshot.instance";
/// What a second launch with no URL asks for: the macOS reopen.
const REOPEN: &str = "reopen";
const SCHEME: &str = "readshot://";
/// A request line is a token and a short URL; anything longer is junk.
const MAX_REQUEST_LEN: u64 = 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);

static LISTENING: AtomicBool = AtomicBool::new(false);

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

    fn decode(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw == REOPEN {
            Some(WakeRequest::Reopen)
        } else if raw
            .get(..SCHEME.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(SCHEME))
        {
            Some(WakeRequest::Url(raw.to_string()))
        } else {
            None
        }
    }
}

/// The result of [`acquire`].
#[derive(Debug)]
pub enum Instance {
    /// This is the only copy. Keep the guard alive for the whole run:
    /// dropping it releases the lock.
    Primary(InstanceGuard),
    /// Another copy holds the lock and was asked to wake up (or could
    /// not be reached, which is logged). Exit 0.
    Secondary,
}

/// Holds the lock for as long as it lives.
#[derive(Debug)]
pub struct InstanceGuard {
    _lock: File,
}

/// Whether a running copy listens for wake requests, so the runtime
/// polls the reopen and URL queues off macOS too.
pub fn is_listening() -> bool {
    LISTENING.load(Ordering::Acquire)
}

/// The per-user directory for the lock: `$XDG_RUNTIME_DIR/readshot` on
/// Linux when set (per-user, mode 0700), else the per-user local data
/// dir (`%LOCALAPPDATA%\pawanpaudel\Readshot\data` on Windows,
/// `~/.local/share/readshot` on Linux).
pub fn lock_dir() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")?;
    Some(
        dirs.runtime_dir()
            .unwrap_or_else(|| dirs.data_local_dir())
            .to_path_buf(),
    )
}

/// Become the running copy, or wake the one that is.
///
/// `request` is what this launch would do; it is sent to the running
/// copy when there is one. `on_wake` runs on a listener thread for each
/// request a later launch sends.
pub fn acquire(
    dir: &Path,
    request: &WakeRequest,
    on_wake: impl Fn(WakeRequest) + Send + 'static,
) -> io::Result<Instance> {
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
            if let Err(e) = wake_running(dir, request) {
                tracing::warn!(
                    target: readshot_core::log::cat::APP,
                    "another Readshot is running but couldn't be woken: {e}"
                );
            }
            return Ok(Instance::Secondary);
        }
        Err(TryLockError::Error(e)) => return Err(e),
    }
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let port = listener.local_addr()?.port();
    let token = new_token();
    write_info(dir, port, &token)?;
    std::thread::Builder::new()
        .name("single-instance".into())
        .spawn(move || serve(&listener, &token, &on_wake))?;
    // Tests run many primaries in one process; only a real launch
    // switches the runtime's polling on.
    #[cfg(not(test))]
    LISTENING.store(true, Ordering::Release);
    Ok(Instance::Primary(InstanceGuard { _lock: lock }))
}

/// The production wake handler: the same queues the macOS AppleEvent
/// handlers fill, drained by the runtime's ticks.
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

fn write_info(dir: &Path, port: u16, token: &str) -> io::Result<()> {
    let path = dir.join(INFO_FILE);
    let tmp = dir.join(format!("{INFO_FILE}.tmp"));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    writeln!(file, "{port} {token}")?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path)
}

fn read_info(dir: &Path) -> io::Result<(u16, String)> {
    let raw = std::fs::read_to_string(dir.join(INFO_FILE))?;
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed instance file");
    let (port, token) = raw.trim().split_once(' ').ok_or_else(bad)?;
    Ok((port.parse().map_err(|_| bad())?, token.to_string()))
}

/// The running copy may still be starting (lock taken, port not yet
/// written), so retry for a moment before giving up.
fn wake_running(dir: &Path, request: &WakeRequest) -> io::Result<()> {
    let mut last = io::Error::other("no attempt");
    for _ in 0..20 {
        match send(dir, request) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(last)
}

fn send(dir: &Path, request: &WakeRequest) -> io::Result<()> {
    let (port, token) = read_info(dir)?;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, IO_TIMEOUT)?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    writeln!(stream, "{token} {}", request.encode())?;
    stream.flush()?;
    // Wait for the ack so a stale port owned by something else (an old
    // copy's port reused) counts as a failure, not a delivery.
    let mut ack = String::new();
    BufReader::new(stream).read_line(&mut ack)?;
    if ack.trim() == "ok" {
        Ok(())
    } else {
        Err(io::Error::other("the running copy refused the request"))
    }
}

fn serve(listener: &TcpListener, token: &str, on_wake: &impl Fn(WakeRequest)) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if let Some(request) = read_request(&stream, token) {
            let mut writer = &stream;
            let _ = writeln!(writer, "ok");
            on_wake(request);
        }
    }
}

/// One request line, checked against the token. Anything else (another
/// local process probing the port) is dropped.
fn read_request(stream: &TcpStream, token: &str) -> Option<WakeRequest> {
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;
    let mut line = String::new();
    BufReader::new(io::Read::take(stream, MAX_REQUEST_LEN))
        .read_line(&mut line)
        .ok()?;
    let (sent, rest) = line.trim_end().split_once(' ')?;
    if sent != token {
        return None;
    }
    WakeRequest::decode(rest)
}

/// 128 random bits from std's per-process SipHash keys, hex encoded.
fn new_token() -> String {
    use std::hash::BuildHasher;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let a = std::collections::hash_map::RandomState::new().hash_one((seed, std::process::id()));
    let b = std::collections::hash_map::RandomState::new().hash_one((a, seed));
    format!("{a:016x}{b:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn recorder() -> (
        impl Fn(WakeRequest) + Send + 'static,
        mpsc::Receiver<WakeRequest>,
    ) {
        let (tx, rx) = mpsc::channel();
        (move |r| tx.send(r).unwrap(), rx)
    }

    #[test]
    fn a_second_launch_wakes_the_first_and_steps_aside() {
        let dir = tempfile::tempdir().unwrap();
        let (on_wake, woken) = recorder();
        let first = acquire(dir.path(), &WakeRequest::Reopen, on_wake).unwrap();
        assert!(matches!(first, Instance::Primary(_)));

        let second = acquire(dir.path(), &WakeRequest::Reopen, |_| {
            panic!("a secondary never serves")
        })
        .unwrap();
        assert!(matches!(second, Instance::Secondary));
        assert_eq!(
            woken.recv_timeout(Duration::from_secs(5)).unwrap(),
            WakeRequest::Reopen
        );

        let url = WakeRequest::Url("readshot://new".into());
        let third = acquire(dir.path(), &url, |_| {}).unwrap();
        assert!(matches!(third, Instance::Secondary));
        assert_eq!(woken.recv_timeout(Duration::from_secs(5)).unwrap(), url);
        drop(first);
    }

    #[test]
    fn a_lock_left_by_a_dead_copy_does_not_block_the_next_launch() {
        let dir = tempfile::tempdir().unwrap();
        // What a crash leaves: the lock file (no holder) and an
        // instance file naming a port nobody serves.
        std::fs::write(dir.path().join(LOCK_FILE), b"").unwrap();
        std::fs::write(dir.path().join(INFO_FILE), b"1 deadbeef\n").unwrap();
        let first = acquire(dir.path(), &WakeRequest::Reopen, |_| {}).unwrap();
        assert!(matches!(first, Instance::Primary(_)));
        drop(first);
        // Its own exit releases the lock too.
        let again = acquire(dir.path(), &WakeRequest::Reopen, |_| {}).unwrap();
        assert!(matches!(again, Instance::Primary(_)));
    }

    #[test]
    fn a_request_without_the_token_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let (on_wake, woken) = recorder();
        let _first = acquire(dir.path(), &WakeRequest::Reopen, on_wake).unwrap();
        let (port, _token) = read_info(dir.path()).unwrap();
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        writeln!(stream, "wrong-token reopen").unwrap();
        drop(stream);
        assert!(woken.recv_timeout(Duration::from_millis(500)).is_err());
    }

    #[test]
    fn requests_round_trip_and_junk_is_refused() {
        assert_eq!(WakeRequest::decode("reopen"), Some(WakeRequest::Reopen));
        assert_eq!(
            WakeRequest::decode("Readshot://new"),
            Some(WakeRequest::Url("Readshot://new".into()))
        );
        assert_eq!(WakeRequest::decode("https://example.com"), None);
        assert_eq!(WakeRequest::decode(""), None);
        assert_ne!(new_token(), new_token());
    }
}
