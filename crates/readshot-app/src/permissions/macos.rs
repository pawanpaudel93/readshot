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
        let access = ScreenCaptureAccess;
        let granted = access.request();
        // The return tells us whether the OS already considered us
        // granted at call time. It does NOT indicate whether the
        // system prompt was shown — for ad-hoc-signed apps on recent
        // macOS, the call silently *registers* the bundle in
        // System Settings without ever drawing a prompt, so the user
        // sees nothing. We surface the value here so the runtime
        // can drive the user to System Settings if request() fails.
        tracing::info!(
            target: "readshot::permissions",
            "CGRequestScreenCaptureAccess returned {granted}",
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
