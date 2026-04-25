//! First-run welcome window state.
//!
//! Shown on macOS (and Linux when the compositor is Wayland) before
//! the user has granted Screen Recording permission. The window
//! explains what consent the app is about to ask for and provides a
//! single "Grant Screen Recording Access" button.
//!
//! The iced view function lives in `crate::app::App::view_welcome`;
//! this module owns the state-machine.

use crate::permissions::PermissionStatus;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WelcomeState {
    /// Not yet shown (first launch, permissions not yet checked).
    #[default]
    Pending,
    /// Window is visible and the user has clicked "Grant"; we're
    /// polling the OS for the result.
    AwaitingGrant,
    /// User granted; the App will dismiss the window.
    Granted,
    /// User denied. We show a "Open Settings" link.
    Denied,
}

impl WelcomeState {
    /// Decide whether the welcome window needs to be shown given the
    /// platform's current permission status.
    ///
    /// * `Granted` / `NotApplicable` → no window needed.
    /// * `Denied` (first launch on macOS) → show the welcome.
    pub fn from_status(status: PermissionStatus) -> Self {
        match status {
            PermissionStatus::Granted | PermissionStatus::NotApplicable => Self::Granted,
            PermissionStatus::Denied => Self::Pending,
        }
    }

    /// Apply a permission-status update from a polling tick. Returns
    /// `true` if the visible state changed.
    pub fn observe(&mut self, status: PermissionStatus) -> bool {
        let next = match (status, *self) {
            (PermissionStatus::Granted | PermissionStatus::NotApplicable, _) => Self::Granted,
            (PermissionStatus::Denied, Self::AwaitingGrant) => Self::Denied,
            (PermissionStatus::Denied, _) => Self::Pending,
        };
        if next != *self {
            *self = next;
            true
        } else {
            false
        }
    }

    pub fn should_show(&self) -> bool {
        !matches!(self, Self::Granted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn granted_or_na_means_no_welcome() {
        assert_eq!(
            WelcomeState::from_status(PermissionStatus::Granted),
            WelcomeState::Granted,
        );
        assert_eq!(
            WelcomeState::from_status(PermissionStatus::NotApplicable),
            WelcomeState::Granted,
        );
    }

    #[test]
    fn denied_starts_in_pending() {
        assert_eq!(
            WelcomeState::from_status(PermissionStatus::Denied),
            WelcomeState::Pending,
        );
    }

    #[test]
    fn observing_grant_dismisses_welcome() {
        let mut s = WelcomeState::Pending;
        let changed = s.observe(PermissionStatus::Granted);
        assert!(changed);
        assert!(!s.should_show());
    }

    #[test]
    fn observing_denial_after_grant_request_marks_denied() {
        let mut s = WelcomeState::AwaitingGrant;
        let changed = s.observe(PermissionStatus::Denied);
        assert!(changed);
        assert_eq!(s, WelcomeState::Denied);
    }

    #[test]
    fn no_state_change_returns_false() {
        let mut s = WelcomeState::Pending;
        assert!(!s.observe(PermissionStatus::Denied));
    }
}
