//! macOS Screen Recording permission.
//!
//! macOS 15 Sequoia split this consent into two TCC keys:
//!
//! * Legacy "Screen Recording" — what `CGPreflightScreenCaptureAccess`
//!   consults. Sequoia keeps populating it for back-compat, but new
//!   apps that only use ScreenCaptureKit don't get a row there.
//! * "Screen & System Audio Recording" — what ScreenCaptureKit
//!   (`SCShareableContent`) actually consults at capture time.
//!
//! Readshot captures via SCK, so a user grant in the new pane is what
//! lets us capture — but `CGPreflightScreenCaptureAccess` still
//! reports false. Without a second probe the welcome window would
//! stay stuck on "grant permission" forever even after the user
//! toggled the right row.
//!
//! Strategy: try CG preflight first (cheap, non-blocking). If it
//! reports denied, fall through to an SCK probe (`SCShareableContent::get`)
//! which the screencapturekit crate runs synchronously. The first
//! call may take ~20-50 ms; subsequent calls are cached by the OS.
//!
//! `open_settings` deep-links into the Privacy pane. macOS 13+
//! ("System Settings") and 12- ("System Preferences") use different
//! URL schemes, so we try both in order.

use core_graphics::access::ScreenCaptureAccess;
use screencapturekit::shareable_content::SCShareableContent;

use super::{PermissionStatus, PermissionsProvider};

pub struct MacOsPermissions;

/// Must match `CFBundleIdentifier` in packaging/macos/Info.plist.
const BUNDLE_ID: &str = "np.com.pawanpaudel.readshot";

impl PermissionsProvider for MacOsPermissions {
    fn status(&self) -> PermissionStatus {
        let access = ScreenCaptureAccess;
        if access.preflight() {
            return PermissionStatus::Granted;
        }
        // CG preflight reports denied on Sequoia even after a SCK-pane
        // grant. Probe SCK directly so the welcome window doesn't get
        // stuck. A successful call means TCC currently allows capture.
        match SCShareableContent::get() {
            Ok(_) => PermissionStatus::Granted,
            Err(e) => {
                tracing::trace!(
                    target: "readshot::permissions",
                    "SCK probe denied: {e:?}",
                );
                PermissionStatus::Denied
            }
        }
    }

    fn request(&self) {
        // Intentional no-op. The actual TCC registration goes through
        // ScreenCaptureKit's `SCShareableContent::get()`, invoked from
        // the runtime via the capture coordinator's `list_displays`.
        //
        // The previous implementation called
        // `CGRequestScreenCaptureAccess` (CoreGraphics), which on
        // macOS 15 Sequoia registers the bundle in the *legacy*
        // "Screen Recording" pane — separate from the
        // SCK-driven "Screen & System Audio Recording" pane that
        // ScreenCaptureKit actually consults. The result was two
        // privacy panes both showing Readshot, each requiring its
        // own toggle. SCK-only registration consolidates to one.
        tracing::debug!(
            target: "readshot::permissions",
            "request() — no-op on macOS; SCK does registration via the coordinator",
        );
    }

    fn status_quiet(&self) -> PermissionStatus {
        // CoreGraphics preflight never prompts. A grant made after this
        // process started is not visible until relaunch anyway, which the
        // Quit & Reopen path (and our relaunch safety net) provides.
        if ScreenCaptureAccess.preflight() {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        }
    }

    fn reset(&self) {
        // `tccutil reset <service> <bundle id>` only touches this app's
        // own row and needs no admin rights.
        let status = std::process::Command::new("/usr/bin/tccutil")
            .args(["reset", "ScreenCapture", BUNDLE_ID])
            .status();
        match status {
            Ok(s) if s.success() => tracing::info!(
                target: "readshot::permissions",
                "reset stored Screen Recording decision",
            ),
            _ => tracing::warn!(
                target: "readshot::permissions",
                "could not reset Screen Recording decision via tccutil",
            ),
        }
    }

    fn open_settings(&self) {
        // Just the modern macOS 13+ URL. The Monterey / System
        // Preferences fallback was firing alongside the modern URL
        // on some Sequoia builds, opening Settings twice. macOS 14+
        // is our minimum target, so the legacy form isn't needed.
        const URL: &str =
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture";
        // If System Settings is already running (e.g. macOS opened it
        // autonomously in response to CGRequestScreenCaptureAccess on
        // Sequoia), skip the explicit `open` so we don't navigate
        // away from whatever pane the user is on or double-stack.
        if system_settings_is_running() {
            tracing::info!(
                target: "readshot::permissions",
                "System Settings already running — skipping deep-link",
            );
            return;
        }
        let status = std::process::Command::new("open").arg(URL).status();
        if !matches!(status, Ok(s) if s.success()) {
            tracing::warn!(
                target: "readshot::permissions",
                "could not open Screen Recording pane — `open {URL}` failed",
            );
        }
    }
}

fn system_settings_is_running() -> bool {
    // pgrep is fast and ubiquitous on macOS; -x matches the literal
    // process name. macOS 13+ is "System Settings"; 12- was "System
    // Preferences" — checking both keeps this robust if a user is on
    // an older OS than our minimum.
    for name in ["System Settings", "System Preferences"] {
        let ok = std::process::Command::new("pgrep")
            .args(["-x", name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return true;
        }
    }
    false
}
