//! Welcome window messages.
//! Moved out of `update_message`; `use super::*` keeps the arms'
//! names resolving as they did there.

use super::*;

/// `Message::WelcomeWindowReady`.
pub(super) fn on_welcome_window_ready(state: &mut App, id: iced::window::Id) -> Task<Message> {
    if !state.welcome.should_show() {
        state.mark_onboarding_completed();
    }
    window::gain_focus(id)
}

/// `Message::WelcomeKeyPressed`.
pub(super) fn on_welcome_key_pressed(
    state: &mut App,
    window: iced::window::Id,
    key: iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Task<Message> {
    // Validate the focused window really is the welcome window —
    // the keyboard subscription is global to the daemon.
    if state.windows.kind(window) != Some(WindowKind::Welcome) {
        return Task::none();
    }
    match welcome_key_message(state.welcome, state.capture_in_flight, &key, modifiers) {
        Some(message) => update(state, message),
        None => Task::none(),
    }
}

/// `Message::WelcomeStatusDismissed`.
pub(super) fn on_welcome_status_dismissed(state: &mut App) -> Task<Message> {
    state.last_capture_status = None;
    Task::none()
}
