//! Built-in uninstall: plan the removal, then execute it.
//!
//! The design keeps the *decision* (what to remove) apart from the
//! *side effects* (removing it):
//!
//! * [`plan_uninstall`] is a read-only planner. Given injectable paths
//!   and a `purge` flag it returns an ordered [`UninstallAction`] list.
//!   It probes the filesystem read-only (e.g. to apply the symlink
//!   safety rule) but never mutates anything, so it is unit-testable
//!   against `tempfile` directories.
//! * [`execute`] performs each action and returns one [`ActionReport`]
//!   per action *without aborting on the first failure*, so a locked
//!   file can't leave the rest of the uninstall half-done.
//! * System commands that would touch the live machine (`tccutil`,
//!   `launchctl`) go through the [`UninstallSystem`] trait, so tests
//!   inject a recorder and never reset a real permission or login item.
//!
//! Safety rules baked in here:
//!
//! * The `readshot` / `readshot-mcp` command-line entries are removed
//!   **only** when they are symlinks that point into a `Readshot.app`
//!   bundle. A regular file, or a symlink that points elsewhere, is
//!   left untouched — see [`points_into_readshot_app`].
//! * The app bundle is moved to the Trash (never `rm -rf`'d) and only
//!   when the caller is actually running from a `*.app` bundle.
//! * Preferences / history / logs are deleted only under `purge`.

use std::io;
use std::path::{Path, PathBuf};

/// Bundle id used for the Screen Recording TCC grant. Must match
/// `CFBundleIdentifier` in `packaging/macos/Info.plist` and the private
/// `BUNDLE_ID` in `permissions/macos.rs`.
pub const BUNDLE_ID: &str = "np.com.pawanpaudel.readshot";

/// Login LaunchAgents Readshot may have installed. The first pair is
/// current (written by `startup.rs`); the second is a legacy id from
/// before the bundle id changed. An uninstall clears both so nothing is
/// left trying to relaunch the app at login.
const LOGIN_AGENTS: &[(&str, &str)] = &[
    (
        "np.com.pawanpaudel.readshot.login",
        "np.com.pawanpaudel.readshot.login.plist",
    ),
    (
        "dev.pawanpaudel93.readshot.login",
        "dev.pawanpaudel93.readshot.login.plist",
    ),
];

/// Bundle ids whose macOS-managed per-app storage a purge clears: the
/// current id and the legacy one from before the rename.
const APP_STORAGE_IDS: &[&str] = &[BUNDLE_ID, "dev.pawanpaudel93.readshot"];

/// Per-app locations macOS (and Sparkle, via URLSession / WebKit) create
/// under `~/Library` for a bundle id, relative to `~/Library`. `{id}` is
/// substituted. Sparkle's download cache also uses the product name.
const APP_STORAGE_PATHS: &[&str] = &[
    "Caches/{id}",
    "HTTPStorages/{id}",
    "HTTPStorages/{id}.binarycookies",
    "WebKit/{id}",
    "Saved Application State/{id}.savedState",
];
const EXTRA_STORAGE_PATHS: &[&str] = &["Caches/np.com.pawanpaudel.Readshot"];

/// Command-line entry points the installer symlinks into `--bin-dir`.
const CLI_BIN_NAMES: &[&str] = &["readshot", "readshot-mcp"];

/// The `*.app` bundle directory name whose interior a CLI symlink must
/// traverse before we treat the link as ours to remove.
const APP_BUNDLE_NAME: &str = "Readshot.app";

/// Everything the planner needs, as explicit paths so it can be driven
/// against temp directories in tests. Build the real values with
/// [`default_uninstall_paths`].
#[derive(Clone, Debug)]
pub struct UninstallPaths {
    /// Directory holding the `readshot` / `readshot-mcp` symlinks
    /// (default: `~/.local/bin`).
    pub bin_dir: PathBuf,
    /// Directory holding user LaunchAgents (`~/Library/LaunchAgents`).
    pub launch_agents_dir: PathBuf,
    /// The `*.app` bundle to move to the Trash, when running from one.
    /// `None` skips the trash step entirely (e.g. a `cargo run` binary).
    pub app_bundle: Option<PathBuf>,
    /// Directory holding `preferences.toml`.
    pub config_dir: PathBuf,
    /// Directory holding history + logs. On macOS this is the same path
    /// as `config_dir`; the planner de-duplicates.
    pub data_dir: PathBuf,
    /// The user's Trash directory (`~/.Trash`).
    pub trash_dir: PathBuf,
    /// The user's `~/Library`, for macOS-managed per-app storage
    /// (defaults plist, caches, HTTP/WebKit storage) cleared on purge.
    pub library_dir: PathBuf,
}

/// What a purge-deletion covers, for plain-language descriptions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataKind {
    Preferences,
    Data,
    /// `config_dir` and `data_dir` are the same directory (macOS).
    All,
}

/// One typed step of an uninstall, in execution order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UninstallAction {
    /// Unload the login LaunchAgent and delete its plist.
    RemoveLoginItem { label: String, plist: PathBuf },
    /// Delete a CLI symlink that points into a `Readshot.app` bundle.
    RemoveCliSymlink { path: PathBuf, target: PathBuf },
    /// Reset the Screen Recording TCC grant for the bundle id.
    ResetScreenRecording { bundle_id: String },
    /// Delete a user-data directory (only planned under `purge`).
    DeleteDir { path: PathBuf, kind: DataKind },
    /// Clear the app's macOS defaults domain (`~/Library/Preferences/<id>.plist`,
    /// where Sparkle keeps its state). Goes through `defaults delete` so
    /// `cfprefsd` doesn't write a cached copy back. Purge only.
    ClearAppDefaults { domain: String, plist: PathBuf },
    /// Delete a macOS-managed per-app cache / storage file or directory
    /// (Caches, HTTPStorages, WebKit, saved state). Purge only.
    DeleteAppStorage { path: PathBuf },
    /// Move the app bundle to the Trash.
    TrashAppBundle { bundle: PathBuf, trash_dir: PathBuf },
}

impl UninstallAction {
    /// One plain-language line describing the step, for the CLI plan and
    /// the log.
    pub fn describe(&self) -> String {
        match self {
            UninstallAction::RemoveLoginItem { plist, .. } => {
                format!("Remove the launch-at-login item ({})", plist.display())
            }
            UninstallAction::RemoveCliSymlink { path, .. } => {
                format!("Remove the command-line tool {}", path.display())
            }
            UninstallAction::ResetScreenRecording { bundle_id } => {
                format!("Reset the Screen Recording permission ({bundle_id})")
            }
            UninstallAction::DeleteDir { path, kind } => match kind {
                DataKind::Preferences => format!("Delete preferences ({})", path.display()),
                DataKind::Data => format!("Delete history and logs ({})", path.display()),
                DataKind::All => {
                    format!("Delete preferences, history, and logs ({})", path.display())
                }
            },
            UninstallAction::ClearAppDefaults { plist, .. } => {
                format!("Delete app settings ({})", plist.display())
            }
            UninstallAction::DeleteAppStorage { path } => {
                format!("Delete cached data ({})", path.display())
            }
            UninstallAction::TrashAppBundle { bundle, .. } => {
                format!("Move {} to the Trash", bundle.display())
            }
        }
    }
}

/// Build the ordered uninstall plan from `paths`. Read-only: it inspects
/// the filesystem to decide which steps apply but changes nothing.
pub fn plan_uninstall(paths: &UninstallPaths, purge: bool) -> Vec<UninstallAction> {
    let mut actions = Vec::new();

    // 1. Login LaunchAgents (current + legacy), only when present.
    for (label, filename) in LOGIN_AGENTS {
        let plist = paths.launch_agents_dir.join(filename);
        if plist.exists() {
            actions.push(UninstallAction::RemoveLoginItem {
                label: (*label).to_string(),
                plist,
            });
        }
    }

    // 2. CLI symlinks — only ones that point into a Readshot.app bundle.
    for name in CLI_BIN_NAMES {
        let path = paths.bin_dir.join(name);
        if let Some(target) = readshot_app_symlink_target(&path) {
            actions.push(UninstallAction::RemoveCliSymlink { path, target });
        }
    }

    // 3. Screen Recording permission.
    actions.push(UninstallAction::ResetScreenRecording {
        bundle_id: BUNDLE_ID.to_string(),
    });

    // 4. User data (purge only). De-duplicate when the two dirs coincide.
    if purge {
        if paths.config_dir == paths.data_dir {
            actions.push(UninstallAction::DeleteDir {
                path: paths.config_dir.clone(),
                kind: DataKind::All,
            });
        } else {
            actions.push(UninstallAction::DeleteDir {
                path: paths.config_dir.clone(),
                kind: DataKind::Preferences,
            });
            actions.push(UninstallAction::DeleteDir {
                path: paths.data_dir.clone(),
                kind: DataKind::Data,
            });
        }
    }

    // 4b. macOS-managed per-app storage (purge only, and only what exists).
    if purge {
        for id in APP_STORAGE_IDS {
            let plist = paths
                .library_dir
                .join("Preferences")
                .join(format!("{id}.plist"));
            if plist.exists() {
                actions.push(UninstallAction::ClearAppDefaults {
                    domain: (*id).to_string(),
                    plist,
                });
            }
            for rel in APP_STORAGE_PATHS {
                let path = paths.library_dir.join(rel.replace("{id}", id));
                if std::fs::symlink_metadata(&path).is_ok() {
                    actions.push(UninstallAction::DeleteAppStorage { path });
                }
            }
        }
        for rel in EXTRA_STORAGE_PATHS {
            let path = paths.library_dir.join(rel);
            if std::fs::symlink_metadata(&path).is_ok() {
                actions.push(UninstallAction::DeleteAppStorage { path });
            }
        }
    }

    // 5. Move the app bundle to the Trash — only when running from one.
    if let Some(bundle) = &paths.app_bundle {
        actions.push(UninstallAction::TrashAppBundle {
            bundle: bundle.clone(),
            trash_dir: paths.trash_dir.clone(),
        });
    }

    actions
}

/// Render the plan as plain text for the CLI.
pub fn render_plan(actions: &[UninstallAction]) -> String {
    if actions.is_empty() {
        return "Nothing to remove — Readshot does not appear to be installed for this user.\n"
            .to_string();
    }
    let mut out = String::from("This will:\n");
    for action in actions {
        out.push_str("  - ");
        out.push_str(&action.describe());
        out.push('\n');
    }
    out
}

/// Read-only probe applying the CLI-symlink safety rule: return the link
/// target when `path` is a symlink pointing into a `Readshot.app`
/// bundle, otherwise `None`. A regular file or a foreign link yields
/// `None`, so it is never scheduled for deletion.
fn readshot_app_symlink_target(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.file_type().is_symlink() {
        return None;
    }
    let target = std::fs::read_link(path).ok()?;
    points_into_readshot_app(&target).then_some(target)
}

/// True when `target` traverses a `Readshot.app` bundle — i.e. it has a
/// `Readshot.app` path component that is not the final one (the link
/// points at a file *inside* the bundle, such as
/// `…/Readshot.app/Contents/MacOS/readshot`).
pub fn points_into_readshot_app(target: &Path) -> bool {
    use std::path::Component;
    let components: Vec<_> = target.components().collect();
    for (i, component) in components.iter().enumerate() {
        if let Component::Normal(name) = component {
            if *name == std::ffi::OsStr::new(APP_BUNDLE_NAME) && i + 1 < components.len() {
                return true;
            }
        }
    }
    false
}

/// Side-effecting operations outside the filesystem, behind a trait so
/// tests never invoke the real `tccutil` / `launchctl`.
pub trait UninstallSystem {
    /// Unload a login LaunchAgent by `label` (its plist is at `plist`).
    /// Best-effort — a not-loaded agent is not an error.
    fn unload_login_item(&self, label: &str, plist: &Path) -> io::Result<()>;

    /// Reset the Screen Recording TCC grant for `bundle_id`.
    fn reset_screen_recording(&self, bundle_id: &str) -> io::Result<()>;

    /// Clear the app's defaults domain so `cfprefsd` drops its cached
    /// copy (best-effort; the plist file is removed afterwards too).
    fn clear_defaults(&self, domain: &str) -> io::Result<()>;
}

/// Production [`UninstallSystem`] that shells out to macOS tooling.
#[derive(Clone, Copy, Debug, Default)]
pub struct RealSystem;

impl UninstallSystem for RealSystem {
    fn unload_login_item(&self, label: &str, plist: &Path) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            // `bootout` is the modern per-GUI-session API; fall back to
            // the older `unload` for good measure. Both are best-effort:
            // an agent that isn't currently loaded just returns non-zero.
            let uid = unsafe { libc::getuid() };
            let domain = format!("gui/{uid}/{label}");
            let booted = std::process::Command::new("/bin/launchctl")
                .args(["bootout", &domain])
                .status();
            if !matches!(booted, Ok(status) if status.success()) {
                let _ = std::process::Command::new("/bin/launchctl")
                    .arg("unload")
                    .arg(plist)
                    .status();
            }
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (label, plist);
            Ok(())
        }
    }

    fn clear_defaults(&self, domain: &str) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            // Non-zero just means the domain was already empty.
            let _ = std::process::Command::new("/usr/bin/defaults")
                .args(["delete", domain])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = domain;
            Ok(())
        }
    }

    fn reset_screen_recording(&self, bundle_id: &str) -> io::Result<()> {
        #[cfg(target_os = "macos")]
        {
            let status = std::process::Command::new("/usr/bin/tccutil")
                .args(["reset", "ScreenCapture", bundle_id])
                .status()?;
            if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("tccutil exited non-zero"))
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = bundle_id;
            Ok(())
        }
    }
}

/// Failure of a single uninstall step.
#[derive(Debug, thiserror::Error)]
pub enum UninstallError {
    #[error("could not remove {path}: {source}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not move {from} to the Trash: {source}")]
    Trash {
        from: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not reset the Screen Recording permission for {bundle_id}: {source}")]
    Tcc {
        bundle_id: String,
        #[source]
        source: io::Error,
    },
}

/// Outcome of an action that neither failed nor did nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionStatus {
    /// The action changed something.
    Done,
    /// The action was a no-op, with a short reason (already gone, not a
    /// Readshot symlink, …).
    Skipped(&'static str),
}

/// One executed action and its result.
#[derive(Debug)]
pub struct ActionReport {
    pub action: UninstallAction,
    pub outcome: Result<ActionStatus, UninstallError>,
}

/// Execute every action in order, collecting one report each. Never
/// aborts early: a failure is recorded and the remaining steps still
/// run.
pub fn execute(actions: &[UninstallAction], system: &dyn UninstallSystem) -> Vec<ActionReport> {
    actions
        .iter()
        .map(|action| ActionReport {
            action: action.clone(),
            outcome: execute_one(action, system),
        })
        .collect()
}

fn execute_one(
    action: &UninstallAction,
    system: &dyn UninstallSystem,
) -> Result<ActionStatus, UninstallError> {
    match action {
        UninstallAction::RemoveLoginItem { label, plist } => {
            // Unloading is best-effort; the durable change is deleting
            // the plist so it can't be re-loaded at the next login.
            let _ = system.unload_login_item(label, plist);
            remove_file_if_present(plist)
        }
        UninstallAction::RemoveCliSymlink { path, .. } => {
            // Re-apply the safety rule at execution time: never delete a
            // regular file or a link that no longer points into the app.
            match readshot_app_symlink_target(path) {
                Some(_) => {
                    std::fs::remove_file(path).map_err(|source| UninstallError::Remove {
                        path: path.clone(),
                        source,
                    })?;
                    Ok(ActionStatus::Done)
                }
                None => Ok(ActionStatus::Skipped("not a Readshot symlink")),
            }
        }
        UninstallAction::ResetScreenRecording { bundle_id } => {
            system
                .reset_screen_recording(bundle_id)
                .map_err(|source| UninstallError::Tcc {
                    bundle_id: bundle_id.clone(),
                    source,
                })?;
            Ok(ActionStatus::Done)
        }
        UninstallAction::DeleteDir { path, .. } => remove_dir_if_present(path),
        UninstallAction::ClearAppDefaults { domain, plist } => {
            let _ = system.clear_defaults(domain);
            remove_file_if_present(plist)
        }
        UninstallAction::DeleteAppStorage { path } => {
            // Files (e.g. `.binarycookies`) and directories both appear here.
            match std::fs::symlink_metadata(path) {
                Ok(meta) if meta.is_dir() => remove_dir_if_present(path),
                Ok(_) => remove_file_if_present(path),
                Err(_) => Ok(ActionStatus::Skipped("already gone")),
            }
        }
        UninstallAction::TrashAppBundle { bundle, trash_dir } => trash_bundle(bundle, trash_dir),
    }
}

fn remove_file_if_present(path: &Path) -> Result<ActionStatus, UninstallError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(ActionStatus::Done),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ActionStatus::Skipped("already gone")),
        Err(source) => Err(UninstallError::Remove {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn remove_dir_if_present(path: &Path) -> Result<ActionStatus, UninstallError> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(ActionStatus::Done),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(ActionStatus::Skipped("already gone")),
        Err(source) => Err(UninstallError::Remove {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn trash_bundle(bundle: &Path, trash_dir: &Path) -> Result<ActionStatus, UninstallError> {
    if !bundle.exists() {
        return Ok(ActionStatus::Skipped("already gone"));
    }
    std::fs::create_dir_all(trash_dir).map_err(|source| UninstallError::Trash {
        from: bundle.to_path_buf(),
        source,
    })?;
    let dest = unique_trash_path(trash_dir, bundle);
    std::fs::rename(bundle, &dest).map_err(|source| UninstallError::Trash {
        from: bundle.to_path_buf(),
        source,
    })?;
    Ok(ActionStatus::Done)
}

/// Pick a destination inside the Trash that does not already exist, so a
/// previously trashed copy is never clobbered (`Readshot.app`,
/// `Readshot 1.app`, …).
fn unique_trash_path(trash_dir: &Path, bundle: &Path) -> PathBuf {
    let file_name = bundle
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from(APP_BUNDLE_NAME));
    let first = trash_dir.join(&file_name);
    if !first.exists() {
        return first;
    }
    let name = Path::new(&file_name);
    let stem = name
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Readshot");
    let ext = name.extension().and_then(|s| s.to_str());
    for n in 1..10_000 {
        let candidate_name = match ext {
            Some(ext) => format!("{stem} {n}.{ext}"),
            None => format!("{stem} {n}"),
        };
        let candidate = trash_dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    // Vanishingly unlikely fallback: fold in the pid for uniqueness.
    let pid = std::process::id();
    match ext {
        Some(ext) => trash_dir.join(format!("{stem}-{pid}.{ext}")),
        None => trash_dir.join(format!("{stem}-{pid}")),
    }
}

/// Build the real uninstall paths from this user's environment.
///
/// `app_bundle` is `Some` only when the running executable lives inside
/// a `*.app` bundle, so a `cargo run` / plain-binary invocation never
/// attempts to trash an app.
pub fn default_uninstall_paths() -> UninstallPaths {
    let home = home_dir();
    let (config_dir, data_dir) =
        directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")
            .map(|dirs| {
                (
                    dirs.config_dir().to_path_buf(),
                    dirs.data_local_dir().to_path_buf(),
                )
            })
            .unwrap_or_else(|| (home.join(".readshot"), home.join(".readshot")));

    UninstallPaths {
        bin_dir: home.join(".local").join("bin"),
        launch_agents_dir: home.join("Library").join("LaunchAgents"),
        app_bundle: current_app_bundle(),
        config_dir,
        data_dir,
        trash_dir: home.join(".Trash"),
        library_dir: home.join("Library"),
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// The `*.app` bundle the running binary lives in, if any:
/// `…/Readshot.app/Contents/MacOS/readshot` → `…/Readshot.app`.
fn current_app_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let macos = exe.parent()?;
    if macos.file_name()? != std::ffi::OsStr::new("MacOS") {
        return None;
    }
    let contents = macos.parent()?;
    if contents.file_name()? != std::ffi::OsStr::new("Contents") {
        return None;
    }
    let app = contents.parent()?;
    if app.extension()? != std::ffi::OsStr::new("app") {
        return None;
    }
    Some(app.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Recording [`UninstallSystem`]: captures calls, never touches the
    /// real machine.
    #[derive(Default)]
    struct RecordingSystem {
        unloaded: Mutex<Vec<String>>,
        tcc_reset: Mutex<Vec<String>>,
        defaults_cleared: Mutex<Vec<String>>,
    }

    impl UninstallSystem for RecordingSystem {
        fn unload_login_item(&self, label: &str, _plist: &Path) -> io::Result<()> {
            self.unloaded.lock().unwrap().push(label.to_string());
            Ok(())
        }
        fn reset_screen_recording(&self, bundle_id: &str) -> io::Result<()> {
            self.tcc_reset.lock().unwrap().push(bundle_id.to_string());
            Ok(())
        }
        fn clear_defaults(&self, domain: &str) -> io::Result<()> {
            self.defaults_cleared
                .lock()
                .unwrap()
                .push(domain.to_string());
            Ok(())
        }
    }

    #[test]
    fn purge_clears_macos_app_storage_but_only_what_exists() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths_in(root.path(), None);
        let lib = &paths.library_dir;
        std::fs::create_dir_all(lib.join("Preferences")).unwrap();
        std::fs::write(
            lib.join("Preferences/np.com.pawanpaudel.readshot.plist"),
            b"x",
        )
        .unwrap();
        std::fs::create_dir_all(lib.join("Caches/np.com.pawanpaudel.readshot")).unwrap();
        std::fs::create_dir_all(lib.join("HTTPStorages")).unwrap();
        std::fs::write(
            lib.join("HTTPStorages/np.com.pawanpaudel.readshot.binarycookies"),
            b"x",
        )
        .unwrap();
        std::fs::create_dir_all(lib.join("HTTPStorages/dev.pawanpaudel93.readshot")).unwrap();
        // Unrelated app storage must never be touched.
        std::fs::create_dir_all(lib.join("Caches/com.example.other")).unwrap();

        // Without purge nothing under ~/Library is planned.
        assert!(!plan_uninstall(&paths, false).iter().any(|a| matches!(
            a,
            UninstallAction::ClearAppDefaults { .. } | UninstallAction::DeleteAppStorage { .. }
        )));

        let plan = plan_uninstall(&paths, true);
        let system = RecordingSystem::default();
        for report in execute(&plan, &system) {
            assert!(report.outcome.is_ok(), "{:?}", report);
        }
        assert!(!lib
            .join("Preferences/np.com.pawanpaudel.readshot.plist")
            .exists());
        assert!(!lib.join("Caches/np.com.pawanpaudel.readshot").exists());
        assert!(!lib
            .join("HTTPStorages/np.com.pawanpaudel.readshot.binarycookies")
            .exists());
        assert!(!lib.join("HTTPStorages/dev.pawanpaudel93.readshot").exists());
        assert!(lib.join("Caches/com.example.other").exists());
        assert_eq!(
            system.defaults_cleared.lock().unwrap().as_slice(),
            &["np.com.pawanpaudel.readshot".to_string()]
        );
    }

    fn make_symlink(link: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    /// A `UninstallPaths` rooted entirely inside `root` so nothing can
    /// escape the temp dir.
    fn paths_in(root: &Path, app_bundle: Option<PathBuf>) -> UninstallPaths {
        UninstallPaths {
            bin_dir: root.join("bin"),
            launch_agents_dir: root.join("LaunchAgents"),
            app_bundle,
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            trash_dir: root.join("Trash"),
            library_dir: root.join("Library"),
        }
    }

    #[test]
    fn points_into_readshot_app_only_matches_interior_paths() {
        assert!(points_into_readshot_app(Path::new(
            "/Applications/Readshot.app/Contents/MacOS/readshot"
        )));
        // Pointing *at* the bundle (not into it) is not a bin symlink.
        assert!(!points_into_readshot_app(Path::new(
            "/Applications/Readshot.app"
        )));
        // A foreign link is never ours.
        assert!(!points_into_readshot_app(Path::new(
            "/opt/homebrew/bin/readshot"
        )));
        assert!(!points_into_readshot_app(Path::new(
            "/Applications/Other.app/Contents/MacOS/readshot"
        )));
    }

    #[test]
    fn planner_includes_only_readshot_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = paths_in(root, None);
        std::fs::create_dir_all(&paths.bin_dir).unwrap();

        // `readshot` → into a Readshot.app bundle (should be removed).
        let bundle_bin = root.join("Readshot.app/Contents/MacOS");
        std::fs::create_dir_all(&bundle_bin).unwrap();
        std::fs::write(bundle_bin.join("readshot"), b"bin").unwrap();
        make_symlink(
            &paths.bin_dir.join("readshot"),
            &bundle_bin.join("readshot"),
        );

        // `readshot-mcp` → a plain regular file (must be left alone).
        std::fs::write(paths.bin_dir.join("readshot-mcp"), b"not a link").unwrap();

        let plan = plan_uninstall(&paths, false);
        let symlinks: Vec<_> = plan
            .iter()
            .filter_map(|a| match a {
                UninstallAction::RemoveCliSymlink { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(symlinks, vec![paths.bin_dir.join("readshot")]);
    }

    #[test]
    fn planner_leaves_foreign_symlinks_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = paths_in(root, None);
        std::fs::create_dir_all(&paths.bin_dir).unwrap();

        let elsewhere = root.join("elsewhere/readshot");
        std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
        std::fs::write(&elsewhere, b"bin").unwrap();
        make_symlink(&paths.bin_dir.join("readshot"), &elsewhere);

        let plan = plan_uninstall(&paths, false);
        assert!(
            !plan
                .iter()
                .any(|a| matches!(a, UninstallAction::RemoveCliSymlink { .. })),
            "a symlink pointing outside a Readshot.app bundle must not be scheduled"
        );
    }

    #[test]
    fn planner_finds_current_and_legacy_login_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = paths_in(root, None);
        std::fs::create_dir_all(&paths.launch_agents_dir).unwrap();
        for (_, filename) in LOGIN_AGENTS {
            std::fs::write(paths.launch_agents_dir.join(filename), b"<plist/>").unwrap();
        }

        let plan = plan_uninstall(&paths, false);
        let labels: Vec<_> = plan
            .iter()
            .filter_map(|a| match a {
                UninstallAction::RemoveLoginItem { label, .. } => Some(label.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            labels,
            vec![
                "np.com.pawanpaudel.readshot.login".to_string(),
                "dev.pawanpaudel93.readshot.login".to_string(),
            ]
        );
    }

    #[test]
    fn planner_purge_dedupes_identical_config_and_data_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut paths = paths_in(root, None);
        paths.data_dir = paths.config_dir.clone();

        let with_purge = plan_uninstall(&paths, true);
        let dirs: Vec<_> = with_purge
            .iter()
            .filter(|a| matches!(a, UninstallAction::DeleteDir { .. }))
            .collect();
        assert_eq!(dirs.len(), 1, "identical dirs collapse to one delete");
        assert!(matches!(
            dirs[0],
            UninstallAction::DeleteDir {
                kind: DataKind::All,
                ..
            }
        ));

        // Without purge there are no data deletions at all.
        let without_purge = plan_uninstall(&paths, false);
        assert!(!without_purge
            .iter()
            .any(|a| matches!(a, UninstallAction::DeleteDir { .. })));
    }

    #[test]
    fn planner_trashes_bundle_only_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        let without = plan_uninstall(&paths_in(root, None), false);
        assert!(!without
            .iter()
            .any(|a| matches!(a, UninstallAction::TrashAppBundle { .. })));

        let bundle = root.join("Readshot.app");
        let with = plan_uninstall(&paths_in(root, Some(bundle.clone())), false);
        assert!(with.iter().any(
            |a| matches!(a, UninstallAction::TrashAppBundle { bundle: b, .. } if *b == bundle)
        ));
    }

    #[test]
    fn executor_removes_symlink_and_leaves_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = paths_in(root, None);
        std::fs::create_dir_all(&paths.bin_dir).unwrap();
        let bundle_bin = root.join("Readshot.app/Contents/MacOS");
        std::fs::create_dir_all(&bundle_bin).unwrap();
        std::fs::write(bundle_bin.join("readshot"), b"bin").unwrap();
        let link = paths.bin_dir.join("readshot");
        make_symlink(&link, &bundle_bin.join("readshot"));
        let regular = paths.bin_dir.join("readshot-mcp");
        std::fs::write(&regular, b"real file").unwrap();

        let plan = plan_uninstall(&paths, false);
        let system = RecordingSystem::default();
        let reports = execute(&plan, &system);

        assert!(!link.exists(), "the Readshot symlink is removed");
        assert!(regular.exists(), "a regular file is never removed");
        assert_eq!(system.tcc_reset.lock().unwrap().as_slice(), &[BUNDLE_ID]);
        assert!(reports.iter().all(|r| r.outcome.is_ok()));
    }

    #[test]
    fn executor_moves_bundle_to_trash_with_unique_name() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let bundle = root.join("Applications/Readshot.app");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("marker"), b"x").unwrap();
        let paths = paths_in(root, Some(bundle.clone()));
        // A prior copy already sits in the Trash.
        std::fs::create_dir_all(paths.trash_dir.join("Readshot.app")).unwrap();

        let plan = plan_uninstall(&paths, false);
        let system = RecordingSystem::default();
        let reports = execute(&plan, &system);

        assert!(!bundle.exists(), "the installed bundle is moved away");
        assert!(
            paths
                .trash_dir
                .join("Readshot 1.app")
                .join("marker")
                .exists(),
            "moved next to the existing copy under a unique name"
        );
        assert!(reports.iter().all(|r| r.outcome.is_ok()));
    }

    #[test]
    fn executor_deletes_login_plist_and_records_unload() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = paths_in(root, None);
        std::fs::create_dir_all(&paths.launch_agents_dir).unwrap();
        let plist = paths
            .launch_agents_dir
            .join("np.com.pawanpaudel.readshot.login.plist");
        std::fs::write(&plist, b"<plist/>").unwrap();

        let plan = plan_uninstall(&paths, false);
        let system = RecordingSystem::default();
        execute(&plan, &system);

        assert!(!plist.exists(), "the login plist is deleted");
        assert_eq!(
            system.unloaded.lock().unwrap().as_slice(),
            &["np.com.pawanpaudel.readshot.login"]
        );
    }

    #[test]
    fn executor_reports_each_step_without_aborting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let paths = paths_in(root, None);
        // Purge with directories that do not exist: each delete is a
        // Skipped no-op, and the tcc reset still runs after them.
        let plan = plan_uninstall(&paths, true);
        let system = RecordingSystem::default();
        let reports = execute(&plan, &system);

        assert_eq!(reports.len(), plan.len());
        assert!(reports.iter().all(|r| r.outcome.is_ok()));
        assert_eq!(system.tcc_reset.lock().unwrap().len(), 1);
    }
}
