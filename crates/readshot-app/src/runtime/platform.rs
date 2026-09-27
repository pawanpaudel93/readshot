// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.

use std::path::Path;

use iced::Task;

use readshot_capture::DisplayInfo;

use crate::app::Message;

pub(crate) fn cli_tools_setup_commands(shell: crate::cli_tools::Shell) -> String {
    crate::cli_tools::setup_commands(shell)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ClipboardError {
    #[error(transparent)]
    Arboard(#[from] arboard::Error),
    #[error("clipboard worker join failed: {0}")]
    Join(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum OcrCopyError {
    #[error(transparent)]
    Ocr(#[from] readshot_core::error::OCRError),
    #[error(transparent)]
    Clipboard(#[from] ClipboardError),
}

pub(crate) fn pick_primary(displays: &[DisplayInfo]) -> Option<&DisplayInfo> {
    displays
        .iter()
        .find(|d| d.is_primary)
        .or_else(|| displays.first())
}

pub(crate) fn quit_readshot(coord: &crate::coordinator::CaptureCoordinator) -> Task<Message> {
    // Drain queued history sidecar writes first so quitting straight
    // after an annotation edit can't lose the trailing write.
    coord.flush_history_updates();
    #[cfg(target_os = "macos")]
    {
        // Menu-bar apps can otherwise linger as an LSUIElement process
        // after iced has closed its windows/tray. A stale process makes
        // the next Finder/open launch deliver a reopen AppleEvent to a
        // non-visible instance instead of starting cleanly.
        tracing::info!(target: "readshot::lifecycle", "quit requested; terminating process");
        disarm_relaunch_on_system_quit();
        std::process::exit(0);
    }
    #[cfg(not(target_os = "macos"))]
    {
        iced::exit()
    }
}

/// Capture-flow error type, surfaced to the welcome window's toast
/// region. Bridged through `Message::CaptureSaved` as a `String` so
/// `Message: Clone + Debug` stays trivially derivable.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CaptureRunError {
    #[error(transparent)]
    Capture(#[from] readshot_core::error::CaptureError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("no displays available")]
    NoDisplays,
}

/// Spawn `open <bundle.app>` so Launch Services starts a fresh
/// copy of Readshot. We use the bundle path discovered via
/// `CFBundleCopyBundleURL`-equivalent (env-derived from the running
/// executable) so the user's installed location is honoured.
#[cfg(target_os = "macos")]
pub(crate) fn relaunch_via_launch_services() -> std::io::Result<()> {
    spawn_relaunch(RelaunchMode::NewInstance)
}

/// `NewInstance` (`open -n`) is for our own Restart button, where this
/// process is about to exit. `ReuseRunning` (plain `open`) is for the
/// system-quit safety net: if macOS already reopened Readshot, `open`
/// just brings that copy forward instead of starting a second one.
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RelaunchMode {
    NewInstance,
    ReuseRunning,
}

#[cfg(target_os = "macos")]
fn spawn_relaunch(mode: RelaunchMode) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    // .../Readshot.app/Contents/MacOS/readshot → .../Readshot.app
    let bundle = exe
        .parent() // MacOS
        .and_then(|p| p.parent()) // Contents
        .and_then(|p| p.parent()) // *.app
        .map(|p| p.to_path_buf())
        .unwrap_or(exe);
    let command = relaunch_command_for_bundle(&bundle, mode);
    // Fully detach the helper so it survives this process exiting.
    std::process::Command::new(command.program)
        .args(command.args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

// ---- Relaunch after macOS "Quit & Reopen" -------------------------------
//
// After the user grants Screen Recording, macOS offers "Quit & Reopen".
// On some systems it quits Readshot but never reopens it, leaving the
// user with no app. If this process started without the permission and
// is then quit by anything other than our own Quit / Restart, relaunch
// on the way out. `atexit` runs for both NSApp's terminate path and
// `std::process::exit`.
#[cfg(target_os = "macos")]
static STARTED_WITHOUT_PERMISSION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(target_os = "macos")]
static EXIT_IS_OURS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(target_os = "macos")]
static RELAUNCH_HOOK: std::sync::Once = std::sync::Once::new();

/// Arm the safety net when the process starts without Screen Recording.
pub(crate) fn arm_relaunch_on_system_quit(started_without_permission: bool) {
    #[cfg(target_os = "macos")]
    {
        use std::sync::atomic::Ordering;
        if !started_without_permission {
            return;
        }
        STARTED_WITHOUT_PERMISSION.store(true, Ordering::SeqCst);
        RELAUNCH_HOOK.call_once(|| {
            // SAFETY: `relaunch_on_exit` is a plain `extern "C" fn()` with
            // no captured state; registering it has no other effect.
            unsafe {
                libc::atexit(relaunch_on_exit);
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = started_without_permission;
}

/// Our own Quit and Restart exits are deliberate — don't relaunch.
pub(crate) fn disarm_relaunch_on_system_quit() {
    #[cfg(target_os = "macos")]
    EXIT_IS_OURS.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(target_os = "macos")]
pub(crate) fn should_relaunch_on_exit(
    started_without_permission: bool,
    exit_is_ours: bool,
) -> bool {
    started_without_permission && !exit_is_ours
}

#[cfg(target_os = "macos")]
extern "C" fn relaunch_on_exit() {
    use std::sync::atomic::Ordering;
    if should_relaunch_on_exit(
        STARTED_WITHOUT_PERMISSION.load(Ordering::SeqCst),
        EXIT_IS_OURS.load(Ordering::SeqCst),
    ) {
        // Best effort: nothing useful to do on failure this late.
        let _ = spawn_relaunch(RelaunchMode::ReuseRunning);
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RelaunchCommand {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

#[cfg(target_os = "macos")]
pub(crate) fn relaunch_command_for_bundle(bundle: &Path, mode: RelaunchMode) -> RelaunchCommand {
    let script = match mode {
        RelaunchMode::NewInstance => "sleep 0.35; exec /usr/bin/open -n \"$1\"",
        RelaunchMode::ReuseRunning => "sleep 0.6; exec /usr/bin/open \"$1\"",
    };
    RelaunchCommand {
        program: "/bin/sh",
        args: vec![
            "-c".into(),
            script.into(),
            "readshot-relaunch".into(),
            bundle.to_string_lossy().to_string(),
        ],
    }
}

/// Show a one-line system notification announcing that Readshot is
/// alive in the menu bar after the welcome window dismisses itself
/// post-grant. Without this, users who triggered the grant flow
/// might think the app crashed when the window disappeared.
///
/// macOS: `osascript display notification …` is the lowest-friction
/// way to do this without a third-party crate. The notification
/// mentions the configured capture hotkey so the user knows how to
/// trigger a screenshot from anywhere.
///
/// Other platforms: no-op for now. Linux libnotify / Windows toast
/// land when those platform backends do.
pub(crate) fn notify_running_in_menu_bar(hotkey: &str) {
    #[cfg(target_os = "macos")]
    {
        let body = format!(
            "Readshot is running in the menu bar. Press {hotkey} to capture, or click the icon."
        );
        show_macos_notification("Readshot is ready", &body);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hotkey;
    }
}

pub(crate) fn notify_update_check_started() {
    #[cfg(target_os = "macos")]
    {
        show_macos_notification("Readshot updates", "Checking for updates…");
    }
}

/// Surface manual updater failures to the user. The tray action is a
/// user-initiated command; silently logging here makes the menu item
/// look broken when Sparkle is missing or cannot load.
pub(crate) fn notify_update_check_failed(error: &str) {
    #[cfg(target_os = "macos")]
    {
        let body = format!("Could not check for updates: {error}");
        show_macos_notification("Readshot updates", &body);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = error;
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn show_macos_notification(title: &str, body: &str) {
    let script = macos_notification_script(title, body);
    let spawn = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Err(e) = spawn {
        tracing::warn!(
            target: "readshot::notify",
            "osascript spawn failed: {e}"
        );
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn macos_notification_script(title: &str, body: &str) -> String {
    format!(
        r#"display notification "{}" with title "{}""#,
        applescript_escape(body),
        applescript_escape(title)
    )
}

#[cfg(target_os = "macos")]
pub(crate) fn applescript_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // Newlines / carriage returns / NULs would otherwise close
            // the string literal or break osascript parsing entirely.
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            // AppleScript's line-continuation char joins onto the
            // next statement. AppleScript strings have no `\u`
            // escape, so the only safe option is to drop it.
            '\u{00AC}' => {}
            c if (c as u32) < 0x20 => {
                // Drop other control chars; they have no useful
                // representation inside an AppleScript string.
            }
            c => out.push(c),
        }
    }
    out
}
