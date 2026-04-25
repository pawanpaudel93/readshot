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
        // prompt was shown (TCC does that asynchronously) or what
        // the user picked. Logging the value helps users triage:
        // `true` here while the welcome window still shows Denied
        // means "you must quit and relaunch — TCC's cache only
        // refreshes between processes for ScreenCapture grants".
        tracing::info!(
            target: "readshot::permissions",
            "CGRequestScreenCaptureAccess returned {granted}",
        );
    }

    fn open_settings(&self) {
        // Try the modern macOS 13+ URL first; if `open` returns
        // a non-zero status, fall back to the older form. Either
        // launches System Settings → Privacy → Screen Recording.
        let urls = [
            // Ventura+ ("System Settings.app"): Apple changed the
            // pane bundle identifier in macOS 13. Both forms are
            // accepted on macOS 14 / Sonoma; some macOS 15 builds
            // only accept the new one.
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_ScreenCapture",
            // Monterey- ("System Preferences.app").
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
        ];
        for url in urls {
            let status = std::process::Command::new("open").arg(url).status();
            if matches!(status, Ok(s) if s.success()) {
                return;
            }
        }
        tracing::warn!(
            target: "readshot::permissions",
            "could not open Screen Recording pane — both deep-links failed",
        );
    }
}
