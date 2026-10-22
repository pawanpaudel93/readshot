// Extracted from runtime.rs (pure code-move). `use super::*` pulls in
// sibling/parent items; the explicit imports mirror runtime.rs's preamble.

#[cfg(target_os = "macos")]
use std::path::Path;

use iced::Task;

use crate::app::Message;
use crate::editor::StatusTone;

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
    /// Redacted text that can't be masked: nothing was copied.
    #[error("{}", super::text_panel::COPY_TEXT_WITHHELD)]
    Withheld,
}

/// Whether the OS is set to reduce motion (macOS: System Settings →
/// Accessibility → Display → Reduce motion). Read at startup and
/// refreshed cheaply when an animated surface opens, so Readshot can
/// freeze the scroll window's recording pulse for users who asked for
/// less motion. Non-macOS returns `false` — the animated chrome is
/// macOS-only surface area today.
#[cfg(target_os = "macos")]
pub(crate) fn os_reduce_motion() -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // AppKit accessibility reads are main-thread APIs. In the running
    // app `start`/`update` run on the app thread; in unit tests (nextest
    // worker threads) `MainThreadMarker::new()` is `None`, so skip the
    // call and report "no reduce motion" rather than touch AppKit off
    // the main thread. Mirrors `updater::assert_main_thread`.
    if objc2::MainThreadMarker::new().is_none() {
        return false;
    }
    let Some(class) = AnyClass::get(c"NSWorkspace") else {
        return false;
    };
    // SAFETY: on the main thread (checked above). `sharedWorkspace`
    // returns the process-wide NSWorkspace singleton (never nil once
    // AppKit is up); `accessibilityDisplayShouldReduceMotion` is a BOOL
    // property read with no arguments.
    unsafe {
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return false;
        }
        let reduce: bool = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn os_reduce_motion() -> bool {
    false
}

/// Bring Readshot forward and open AppKit's standard About panel, which
/// reads the bundle: the app icon, name, version and copyright. Readshot
/// is a menu-bar (LSUIElement) app, so without the activation the panel
/// would open behind the frontmost app. Other platforms have no About
/// item in the tray.
#[cfg(target_os = "macos")]
pub(crate) fn show_about_panel() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    if objc2::MainThreadMarker::new().is_none() {
        return;
    }
    let Some(class) = AnyClass::get(c"NSApplication") else {
        return;
    };
    // SAFETY: on the main thread (checked above). `sharedApplication` is
    // the process-wide singleton, null-checked before use; activation and
    // `orderFrontStandardAboutPanel:` (whose sender may be nil) are
    // main-thread AppKit calls.
    unsafe {
        let app: *mut AnyObject = msg_send![class, sharedApplication];
        if let Some(app) = app.as_ref() {
            activate_app(app);
            let _: () = msg_send![app, orderFrontStandardAboutPanel: Option::<&AnyObject>::None];
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn show_about_panel() {}

/// Bring Readshot to the front: `-[NSApplication activate]`, the macOS 14
/// API (Readshot's minimum), falling back to the deprecated
/// `activateIgnoringOtherApps:` only if a runtime lacks it.
///
/// # Safety
/// `app` must be the shared `NSApplication`, and this must run on the
/// main thread.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn activate_app(app: &objc2::runtime::AnyObject) {
    use objc2::{msg_send, sel};
    // SAFETY: per this function's contract; `respondsToSelector:` is safe
    // on any object, and each branch sends only the selector it checked.
    unsafe {
        let modern: bool = msg_send![app, respondsToSelector: sel!(activate)];
        if modern {
            let _: () = msg_send![app, activate];
        } else {
            let _: () = msg_send![app, activateIgnoringOtherApps: true];
        }
    }
}

/// The OS light/dark appearance. macOS reads
/// `NSApplication.effectiveAppearance.name` (contains "Dark" in dark
/// mode); off the main thread and on other platforms this is `Dark`.
#[cfg(target_os = "macos")]
pub(crate) fn os_theme_mode() -> crate::runtime::ThemeMode {
    use crate::runtime::ThemeMode;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    if objc2::MainThreadMarker::new().is_none() {
        return ThemeMode::Dark;
    }
    let Some(class) = AnyClass::get(c"NSApplication") else {
        return ThemeMode::Dark;
    };
    // SAFETY: on the main thread (checked above). `sharedApplication`
    // is the process-wide singleton; `effectiveAppearance` returns an
    // NSAppearance whose `name` is an NSString. Every pointer is
    // null-checked before use; `UTF8String` yields a C string valid for
    // the current autorelease scope, which we copy out immediately.
    unsafe {
        let app: *mut AnyObject = msg_send![class, sharedApplication];
        if app.is_null() {
            return ThemeMode::Dark;
        }
        let appearance: *mut AnyObject = msg_send![app, effectiveAppearance];
        if appearance.is_null() {
            return ThemeMode::Dark;
        }
        let name: *mut AnyObject = msg_send![appearance, name];
        if name.is_null() {
            return ThemeMode::Dark;
        }
        let utf8: *const std::ffi::c_char = msg_send![name, UTF8String];
        if utf8.is_null() {
            return ThemeMode::Dark;
        }
        let name = std::ffi::CStr::from_ptr(utf8).to_string_lossy();
        if name.contains("Dark") {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        }
    }
}

/// Off macOS the app never calls this outside the (macOS-only) poll; the
/// OS appearance arrives as `SystemThemeChanged` (see `READS_OS_THEME`).
#[cfg(not(target_os = "macos"))]
pub(crate) fn os_theme_mode() -> crate::runtime::ThemeMode {
    crate::runtime::ThemeMode::Dark
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
        tracing::info!(target: readshot_core::log::cat::APP, "quit requested; terminating process");
        disarm_relaunch_on_system_quit();
        std::process::exit(0);
    }
    #[cfg(not(target_os = "macos"))]
    {
        iced::exit()
    }
}

/// Capture-flow error type. Carried in messages (such as
/// `CaptureMsg::RegionCaptured`) as a `String` so
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
#[cfg(target_os = "macos")]
pub(crate) fn disarm_relaunch_on_system_quit() {
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
/// macOS: posted as Readshot through UNUserNotificationCenter
/// ([`show_macos_notification`]). The notification mentions the
/// configured capture hotkey so the user knows how to trigger a
/// screenshot from anywhere.
///
/// Other platforms: no-op for now. Linux libnotify / Windows toast
/// land when those platform backends do.
pub(crate) fn notify_running_in_menu_bar(hotkey: &str) {
    #[cfg(target_os = "macos")]
    {
        let body = format!(
            "Readshot is running in the menu bar. Press {hotkey} to capture, or click the icon."
        );
        show_macos_notification("Readshot is ready", &body, StatusTone::Notice);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hotkey;
    }
}

/// Raise a capture problem (a failed capture, save or copy, a retake
/// with no region) that no open window shows (WCAG 4.1.3-style status:
/// a hotkey or tray capture that fails must not fail silently).
pub(crate) fn notify_capture_problem(body: &str, tone: StatusTone) {
    #[cfg(test)]
    {
        let _ = tone;
        CAPTURE_NOTICES.with(|n| n.borrow_mut().push(body.to_string()));
    }
    #[cfg(all(target_os = "macos", not(test)))]
    {
        show_macos_notification("Readshot", body, tone);
    }
    #[cfg(not(any(target_os = "macos", test)))]
    {
        let _ = (body, tone);
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static CAPTURE_NOTICES: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn notify_update_check_started() {
    #[cfg(target_os = "macos")]
    {
        // Work starting: the app never sees it end, so a notice.
        show_macos_notification(
            "Readshot updates",
            "Checking for updates…",
            StatusTone::Notice,
        );
    }
}

/// The notification for a failed manual update check. Every
/// `UpdaterError` is a fault in this copy of the app (Sparkle missing or
/// unloadable; network problems are shown by Sparkle itself), so the way
/// back is a fresh install, never "check your connection".
pub(crate) fn update_check_failure_message(e: &crate::updater::UpdaterError) -> String {
    let reason = match e {
        crate::updater::UpdaterError::Unsupported => {
            "updates work only in the Readshot app installed from the website"
        }
        #[cfg(target_os = "macos")]
        _ => "the updater in this copy of Readshot is missing or damaged",
    };
    format!(
        "Couldn’t check for updates — {reason}. Reinstall Readshot from {UPDATE_SITE} to get updates."
    )
}

const UPDATE_SITE: &str = "readshot.pawanpaudel.com.np";

/// Surface manual updater failures to the user. The tray action is a
/// user-initiated command; silently logging here makes the menu item
/// look broken when Sparkle is missing or cannot load.
pub(crate) fn notify_update_check_failed(e: &crate::updater::UpdaterError) {
    #[cfg(target_os = "macos")]
    {
        show_macos_notification(
            "Readshot updates",
            &update_check_failure_message(e),
            StatusTone::Error,
        );
    }
    // No notification channel off macOS yet: the log is the record.
    #[cfg(not(target_os = "macos"))]
    {
        tracing::warn!(
            target: readshot_core::log::cat::UPDATER,
            "{}",
            update_check_failure_message(e)
        );
    }
}

/// Why a notification's text did not reach the user as a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NotShownReason {
    /// The user turned Readshot's notifications off (or declined the
    /// one-time prompt).
    NotificationsOff,
    /// Not running from Readshot.app (`cargo run`), so nothing can post.
    // Only the macOS notification path produces notices.
    #[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
    NotBundled,
}

/// A notification that was not shown, to be shown in the app instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UndeliveredNotice {
    pub(crate) reason: NotShownReason,
    pub(crate) text: String,
    /// The tone its writer chose when posting it.
    pub(crate) tone: StatusTone,
}

// Process-wide in the app; per test thread under `cfg(test)`, so one
// test's refusal cannot reroute another test's capture problems.
#[cfg(not(test))]
static UNDELIVERED: std::sync::Mutex<Vec<UndeliveredNotice>> = std::sync::Mutex::new(Vec::new());
#[cfg(all(target_os = "macos", not(test)))]
static POSTED_TONES: std::sync::Mutex<Vec<(String, StatusTone)>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(not(test))]
static NOTIFICATIONS_OFF: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
thread_local! {
    static UNDELIVERED: std::sync::Mutex<Vec<UndeliveredNotice>> = const { std::sync::Mutex::new(Vec::new()) };
    static NOTIFICATIONS_OFF: std::sync::atomic::AtomicBool = const { std::sync::atomic::AtomicBool::new(false) };
}

fn with_undelivered<R>(f: impl FnOnce(&std::sync::Mutex<Vec<UndeliveredNotice>>) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&UNDELIVERED)
    }
    #[cfg(test)]
    {
        UNDELIVERED.with(f)
    }
}

#[cfg(all(target_os = "macos", test))]
thread_local! {
    static POSTED_TONES: std::sync::Mutex<Vec<(String, StatusTone)>> = const { std::sync::Mutex::new(Vec::new()) };
}

#[cfg(target_os = "macos")]
fn with_posted_tones<R>(f: impl FnOnce(&std::sync::Mutex<Vec<(String, StatusTone)>>) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&POSTED_TONES)
    }
    #[cfg(test)]
    {
        POSTED_TONES.with(f)
    }
}

/// Remember the tone of a notification being posted: the notification
/// center hands back only its body when it refuses it
/// ([`posted_tone`]). The newest [`MAX_UNDELIVERED`] are kept.
#[cfg(target_os = "macos")]
pub(crate) fn remember_posted_tone(body: &str, tone: StatusTone) {
    with_posted_tones(|tones| {
        if let Ok(mut tones) = tones.lock() {
            if tones.len() >= MAX_UNDELIVERED {
                tones.remove(0);
            }
            tones.push((body.to_string(), tone));
        }
    });
}

/// The tone a notification with this body was posted with (the newest
/// one); a notice when it is no longer known.
#[cfg(target_os = "macos")]
pub(crate) fn posted_tone(body: &str) -> StatusTone {
    with_posted_tones(|tones| {
        tones.lock().ok().and_then(|tones| {
            tones
                .iter()
                .rev()
                .find(|(b, _)| b == body)
                .map(|(_, tone)| *tone)
        })
    })
    .unwrap_or(StatusTone::Notice)
}

fn with_notifications_off<R>(f: impl FnOnce(&std::sync::atomic::AtomicBool) -> R) -> R {
    #[cfg(not(test))]
    {
        f(&NOTIFICATIONS_OFF)
    }
    #[cfg(test)]
    {
        NOTIFICATIONS_OFF.with(f)
    }
}

/// The most notices kept for the runtime to show; older ones are dropped
/// first. Nothing drains the queue while no tick runs, and a notice is
/// only worth showing while it is recent.
pub(crate) const MAX_UNDELIVERED: usize = 16;

/// Queue `text`, with the `tone` it was posted with, for the runtime
/// ([`take_undelivered_notices`]): the notification path learns about a
/// denial asynchronously, off the main thread, so it cannot touch the app
/// state itself.
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn record_undelivered(reason: NotShownReason, text: &str, tone: StatusTone) {
    if reason == NotShownReason::NotificationsOff {
        with_notifications_off(|off| off.store(true, std::sync::atomic::Ordering::Relaxed));
    }
    with_undelivered(|queue| {
        if let Ok(mut queue) = queue.lock() {
            if queue.len() >= MAX_UNDELIVERED {
                queue.remove(0);
            }
            queue.push(UndeliveredNotice {
                reason,
                text: text.to_string(),
                tone,
            });
        }
    });
}

/// Readshot's notifications work again: a post was accepted, or a check
/// found them allowed. Capture problems go back to notifications, and the
/// runtime takes the tray's "Notifications Are Off" item down
/// ([`process_notices`](super::process_notices)).
#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn record_notifications_on() {
    with_notifications_off(|off| off.store(false, std::sync::atomic::Ordering::Relaxed));
}

/// The notices queued since the last call.
pub(crate) fn take_undelivered_notices() -> Vec<UndeliveredNotice> {
    with_undelivered(|queue| {
        queue
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    })
}

/// Whether a notification has been refused because Readshot's
/// notifications are off (this run).
pub(crate) fn notifications_off() -> bool {
    with_notifications_off(|off| off.load(std::sync::atomic::Ordering::Relaxed))
}

/// Where the user turns Readshot's notifications back on.
pub(crate) const NOTIFICATIONS_OFF_HINT: &str =
    "Notifications are off for Readshot. Turn them on in System Settings › Notifications › Readshot.";

/// The tray's "Notifications Are Off" item: if the user has turned
/// Readshot's notifications back on meanwhile, the item goes (as after any
/// accepted post); otherwise System Settings › Notifications opens. The
/// answer comes asynchronously, from the notification center.
pub(crate) fn recheck_notifications_or_open_settings() {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        // SAFETY: the callback is a plain `extern "C"` function with the
        // signature the shim declares; the shim takes no other pointer.
        let status = unsafe { readshot_notify_check(Some(notification_check_callback)) };
        if status != 0 {
            // Not running from Readshot.app: nothing to ask.
            open_notification_settings();
        }
    }
    #[cfg(not(all(target_os = "macos", not(test))))]
    {
        open_notification_settings();
    }
}

/// While Readshot's notifications are known to be off, ask (without
/// opening anything) whether the user has turned them back on; if so the
/// off state ends, as after an accepted post. The tray item does this on
/// a click, but with no tray (it failed to install) nothing else would,
/// and capture problems would go to the Welcome window for the rest of
/// the run. Called when the user is back in the app: Launch Services
/// reopening it, and Settings opening.
pub(crate) fn recheck_notifications_quietly() {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        if notifications_off() {
            // SAFETY: the callback is a plain `extern "C"` function with
            // the signature the shim declares; the shim takes no other
            // pointer. Outside Readshot.app it asks nothing (returns 1).
            let _ = unsafe { readshot_notify_check(Some(notification_quiet_check_callback)) };
        }
    }
}

/// [`recheck_notifications_quietly`]'s answer: 3 = on again; anything
/// else leaves the off state as it is.
#[cfg(all(target_os = "macos", not(test)))]
extern "C" fn notification_quiet_check_callback(level: i32, _message: *const std::ffi::c_char) {
    if level == 3 {
        tracing::info!(target: readshot_core::log::cat::APP, "notifications are on again");
        record_notifications_on();
    }
}

/// Open System Settings › Notifications (macOS); a no-op elsewhere.
pub(crate) fn open_notification_settings() {
    #[cfg(target_os = "macos")]
    {
        let url = "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=np.com.pawanpaudel.readshot";
        if let Err(e) = std::process::Command::new("open")
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            tracing::warn!(
                target: readshot_core::log::cat::APP,
                "could not open Notifications settings: {e}"
            );
        }
    }
}

/// Post a notification as Readshot (its name and icon, listed under
/// Readshot in System Settings › Notifications) through
/// UNUserNotificationCenter, via the Swift shim `src/macos_notify.swift`.
///
/// Never blocks and never fails loudly: running outside an app bundle
/// (`cargo run`, tests) posts nothing, and a user who turned Readshot's
/// notifications off gets the text in the log instead.
#[cfg(target_os = "macos")]
pub(crate) fn show_macos_notification(title: &str, body: &str, tone: StatusTone) {
    remember_posted_tone(body, tone);
    let (title_c, body_c) = notification_c_strings(title, body);
    // SAFETY: both pointers are valid NUL-terminated strings that outlive
    // the call (the shim copies them before returning), and the callback
    // is a plain `extern "C"` function with the signature the shim
    // declares.
    let status = unsafe {
        readshot_notify_post(
            title_c.as_ptr(),
            body_c.as_ptr(),
            Some(notification_log_callback),
        )
    };
    match status {
        0 => {}
        1 => {
            tracing::info!(
                target: readshot_core::log::cat::APP,
                "not running from Readshot.app, so no notification: {title} — {body}"
            );
            record_undelivered(NotShownReason::NotBundled, body, tone);
        }
        other => tracing::warn!(
            target: readshot_core::log::cat::APP,
            "notification shim refused \"{title}\" (status {other})"
        ),
    }
}

/// C strings for the shim. An interior NUL would make `CString::new`
/// fail (and truncate the text on the Swift side), so it is dropped.
#[cfg(target_os = "macos")]
pub(crate) fn notification_c_strings(
    title: &str,
    body: &str,
) -> (std::ffi::CString, std::ffi::CString) {
    let c =
        |s: &str| std::ffi::CString::new(s.replace('\0', "")).expect("interior NULs were removed");
    (c(title), c(body))
}

#[cfg(target_os = "macos")]
extern "C" fn notification_log_callback(level: i32, message: *const std::ffi::c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: the shim passes a NUL-terminated string that stays valid for
    // the duration of this call.
    let message = unsafe { std::ffi::CStr::from_ptr(message) }.to_string_lossy();
    if level == 2 {
        tracing::info!(
            target: readshot_core::log::cat::APP,
            "notifications are off for Readshot; not shown: {message}"
        );
        record_undelivered(
            NotShownReason::NotificationsOff,
            &message,
            posted_tone(&message),
        );
    } else if level == 3 {
        record_notifications_on();
    } else if level == 0 {
        tracing::info!(target: readshot_core::log::cat::APP, "{message}");
    } else {
        tracing::warn!(target: readshot_core::log::cat::APP, "{message}");
    }
}

/// [`recheck_notifications_or_open_settings`]'s answer: 3 = on, 4 = still
/// off (then the setting opens).
#[cfg(all(target_os = "macos", not(test)))]
extern "C" fn notification_check_callback(level: i32, _message: *const std::ffi::c_char) {
    match level {
        3 => {
            tracing::info!(target: readshot_core::log::cat::APP, "notifications are on again");
            record_notifications_on();
        }
        4 => open_notification_settings(),
        _ => {}
    }
}

#[cfg(target_os = "macos")]
unsafe extern "C" {
    /// `src/macos_notify.swift`. Returns 0 when handed to the notification
    /// center, 1 when not running from an app bundle (nothing posted), 2
    /// for a null argument. `log` receives `(0 = info | 1 = warn, text)`,
    /// `(2, body)` when notifications are off for Readshot, or `(3, "")`
    /// once the post was accepted, possibly later and on another thread.
    fn readshot_notify_post(
        title: *const std::ffi::c_char,
        body: *const std::ffi::c_char,
        log: Option<extern "C" fn(i32, *const std::ffi::c_char)>,
    ) -> i32;
    /// `src/macos_notify.swift`. Asks whether notifications are allowed;
    /// `log` receives `(3, "")` if they are, `(4, "")` if not, later and on
    /// another thread. Returns 0, or 1 when not running from an app bundle
    /// (nothing asked). Tests never ask (`recheck_notifications_or_open_settings`).
    #[cfg_attr(test, allow(dead_code))]
    fn readshot_notify_check(log: Option<extern "C" fn(i32, *const std::ffi::c_char)>) -> i32;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RevealCommand {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

pub(crate) fn reveal_command_for_path(path: &std::path::Path) -> RevealCommand {
    let path_string = path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        RevealCommand {
            program: "open",
            args: vec!["-R".into(), path_string],
        }
    }
    #[cfg(target_os = "windows")]
    {
        // explorer.exe needs the path in the SAME token as `/select,`
        // (`/select,C:\dir\file.png`). Passed as two separate argv entries
        // Explorer ignores the selection and just opens the default folder.
        RevealCommand {
            program: "explorer",
            args: vec![format!("/select,{path_string}")],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(path_string);
        RevealCommand {
            program: "xdg-open",
            args: vec![dir],
        }
    }
}

pub(crate) fn reveal_path(path: &std::path::Path) -> Result<(), std::io::Error> {
    let command = reveal_command_for_path(path);
    std::process::Command::new(command.program)
        .args(command.args)
        .spawn()?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OpenFolderCommand {
    pub(crate) program: &'static str,
    pub(crate) args: Vec<String>,
}

pub(crate) fn open_folder_command_for_path(path: &std::path::Path) -> OpenFolderCommand {
    let path_string = path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        OpenFolderCommand {
            program: "open",
            args: vec![path_string],
        }
    }
    #[cfg(target_os = "windows")]
    {
        OpenFolderCommand {
            program: "explorer",
            args: vec![path_string],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        OpenFolderCommand {
            program: "xdg-open",
            args: vec![path_string],
        }
    }
}

pub(crate) fn open_folder_path(path: &std::path::Path) -> Result<(), std::io::Error> {
    let command = open_folder_command_for_path(path);
    std::process::Command::new(command.program)
        .args(command.args)
        .spawn()?;
    Ok(())
}
