//! Windows screen-capture permission — none required for non-elevated
//! content. UAC-elevated windows return blank frames if Readshot
//! itself isn't elevated; that's surfaced as a `CaptureError` at
//! capture time, not as a permission-layer denial.

use super::{PermissionStatus, PermissionsProvider};

pub struct WindowsPermissions;

impl PermissionsProvider for WindowsPermissions {
    fn status(&self) -> PermissionStatus {
        PermissionStatus::NotApplicable
    }

    fn request(&self) {
        // No-op.
    }

    fn open_settings(&self) {
        // No relevant settings pane on Windows; intentionally no-op.
    }
}
