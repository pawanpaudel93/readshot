//! Fake permissions provider used by the coordinator's tests.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::{PermissionStatus, PermissionsProvider};

pub struct FakePermissions {
    granted: AtomicBool,
    pub request_calls: AtomicU32,
    pub open_settings_calls: AtomicU32,
}

impl FakePermissions {
    pub fn granted() -> Self {
        Self {
            granted: AtomicBool::new(true),
            request_calls: AtomicU32::new(0),
            open_settings_calls: AtomicU32::new(0),
        }
    }

    pub fn denied() -> Self {
        Self {
            granted: AtomicBool::new(false),
            request_calls: AtomicU32::new(0),
            open_settings_calls: AtomicU32::new(0),
        }
    }

    pub fn flip_to_granted(&self) {
        self.granted.store(true, Ordering::SeqCst);
    }
}

impl PermissionsProvider for FakePermissions {
    fn status(&self) -> PermissionStatus {
        if self.granted.load(Ordering::SeqCst) {
            PermissionStatus::Granted
        } else {
            PermissionStatus::Denied
        }
    }

    fn request(&self) {
        self.request_calls.fetch_add(1, Ordering::SeqCst);
    }

    fn open_settings(&self) {
        self.open_settings_calls.fetch_add(1, Ordering::SeqCst);
    }
}
