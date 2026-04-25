//! macOS Screen Recording permission.
//!
//! Two CoreGraphics functions do the work:
//!
//! * `CGPreflightScreenCaptureAccess` — returns whether the app
//!   currently has the grant. Cheap; safe to poll.
//! * `CGRequestScreenCaptureAccess` — registers the app in System
//!   Settings → Privacy & Security → Screen Recording and triggers
//!   the OS prompt the first time it is called from a given app.
//!
//! `open_settings` deep-links into the Privacy pane with the standard
//! preferences URL scheme.

use core_graphics::access::{
    ScreenCaptureAccess,
};

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
        // Discard the bool — TCC processes the prompt asynchronously
        // and the user's answer surfaces via subsequent `status` polls.
        let _ = access.request();
    }

    fn open_settings(&self) {
        // `open` is a stable macOS CLI for launching URL schemes; we
        // shell out rather than depending on `NSWorkspace` directly.
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
            .status();
    }
}
