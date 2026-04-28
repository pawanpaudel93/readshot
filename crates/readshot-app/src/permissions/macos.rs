//! macOS Screen Recording permission.
//!
//! Two CoreGraphics functions do the work:
//!
//! * `CGPreflightScreenCaptureAccess` — returns whether the app
//!   currently has the grant. Cheap; safe to poll. Returns `false`
//!   even after a fresh user grant until the app is **relaunched** —
//!   this is a TCC quirk we can't work around in-process.
//! * `CGRequestScreenCaptureAccess` — adds the app to System Settings
//!   → Privacy & Security → Screen Recording and triggers the OS
//!   prompt the first time it is called for a given (bundle id,
//!   cdhash) pair. Subsequent calls after a user denial are
//!   silent — the user must toggle the row in System Settings.
//!
//! `open_settings` deep-links into the Privacy pane. macOS 13+
//! ("System Settings") and 12- ("System Preferences") use different
//! URL schemes, so we try both in order.

use core_graphics::access::ScreenCaptureAccess;

use super::{PermissionStatus, PermissionsProvider};

pub struct MacOsPermissions;

impl PermissionsProvider for MacOsPermissions {
    fn status(&self) -> PermissionStatus {
        let access = ScreenCaptureAccess;
        if access.preflight() {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
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
