//! Per-OS screen-capture permissions.
//!
//! Three OSes, three trust models:
//!
//! * macOS — TCC (Transparency, Consent, and Control). The user must
//!   tick "Screen Recording" for Readshot in System Settings before
//!   the app can read the screen. We poll
//!   [`PermissionStatus`] via `CGPreflightScreenCaptureAccess` and
//!   trigger the system prompt with `CGRequestScreenCaptureAccess`.
//! * Windows — no consent is required for non-elevated content; the
//!   provider always reports [`PermissionStatus::NotApplicable`].
//! * Linux — the `xdg-desktop-portal` ScreenCast dialog is the
//!   consent and fires *per capture session*, so the app-level status
//!   is also `NotApplicable`. The portal dialog itself happens inside
//!   the `Capturer`.
//!
//! The trait is small on purpose: the higher layers branch on the
//! enum, not on the OS, so the welcome window (only shown on macOS)
//! and the editor's "permission denied" toast share a single code
//! path.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(test)]
pub mod fake;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionStatus {
    /// User has explicitly granted consent.
    Granted,
    /// User has explicitly denied consent. Shows a banner with a
    /// settings deep-link.
    Denied,
    /// This OS doesn't require consent at the app-permission layer
    /// (Windows, Linux). The capture path may still need consent at
    /// session-creation time (Linux portal); that's modelled as a
    /// capture-time error rather than here.
    NotApplicable,
}

pub trait PermissionsProvider: Send + Sync {
    /// Current consent state. Cheap to call; used to drive the welcome
    /// window's polling loop.
    fn status(&self) -> PermissionStatus;

    /// Trigger the system prompt that adds the app to the relevant
    /// privacy pane. No-op on OSes where consent isn't applicable.
    /// The OS handles user interaction asynchronously; the provider
    /// returns immediately.
    fn request(&self);

    /// Open the platform's settings UI at the screen-recording pane.
    /// Used by the editor's "permission denied" toast.
    fn open_settings(&self);

    /// Forget any stored Screen Recording decision for this app so the
    /// next request registers the *current* code signature. macOS keys a
    /// grant to the signature it was given to, so after an update signed
    /// differently the old row stays "on" in System Settings but no longer
    /// applies. No-op where the platform has no such store.
    fn reset(&self) {}
}

/// Construct the platform-default provider.
#[allow(clippy::needless_return)]
pub fn default_provider() -> Box<dyn PermissionsProvider> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(macos::MacOsPermissions);
    }
    #[cfg(target_os = "windows")]
    {
        return Box::new(windows::WindowsPermissions);
    }
    #[cfg(target_os = "linux")]
    {
        return Box::new(linux::LinuxPermissions);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        compile_error!("readshot-app: unsupported target_os");
    }
}
