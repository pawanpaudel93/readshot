//! Linux screen-capture permission — fires per-session via
//! `xdg-desktop-portal`. The app-level status is `NotApplicable`
//! because consent isn't a persistent grant; the portal asks each
//! capture session and the user can accept-once or always-allow on
//! their compositor.
//!
//! The capture coordinator surfaces a portal denial as a
//! `CaptureError::PermissionDenied` at the moment it actually
//! happens, so users get a banner with a "Retry" button rather than
//! a no-op startup prompt.

use super::{PermissionStatus, PermissionsProvider};

pub struct LinuxPermissions;

impl PermissionsProvider for LinuxPermissions {
    fn status(&self) -> PermissionStatus {
        PermissionStatus::NotApplicable
    }

    fn request(&self) {
        // No-op — portal handles consent at capture time.
    }

    fn open_settings(&self) {
        // No portable "screen recording settings" pane; xdg-desktop-portal
        // exposes its grants per-compositor in inconsistent places.
    }
}
