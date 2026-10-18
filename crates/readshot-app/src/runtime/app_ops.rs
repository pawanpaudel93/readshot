//! App-level messages: restart and the screen-recording permission.
//! Moved out of `update_message`; `use super::*` keeps the arms'
//! names resolving as they did there.

use super::*;

/// `Message::RestartRequested`.
pub(super) fn on_restart_requested() -> Task<Message> {
    // macOS Screen Recording's TCC grant is cached per-process
    // by `CGPreflightScreenCaptureAccess` — once a process has
    // seen "denied", it can't ever see "granted" without a
    // restart. We spawn a fresh Launch Services launch and
    // hard-exit so the daemon can't get into a half-shutdown
    // race (`iced::exit()` returns a Task that may not have
    // finished by the time the new instance comes up).
    #[cfg(target_os = "macos")]
    {
        disarm_relaunch_on_system_quit();
        match relaunch_via_launch_services() {
            Ok(()) => {
                tracing::info!(target: readshot_core::log::cat::PERMS, "respawned via Launch Services; exiting");
                // Tiny sleep so `open -n` definitely got the
                // request through before this PID disappears.
                std::thread::sleep(Duration::from_millis(150));
                std::process::exit(0);
            }
            Err(e) => {
                tracing::warn!(target: readshot_core::log::cat::PERMS, "relaunch failed: {e}");
            }
        }
    }
    iced::exit()
}

/// `Message::GrantPermissionRequested`.
pub(super) fn on_grant_permission(state: &mut App) -> Task<Message> {
    state.update_sync(Message::GrantPermissionRequested);
    let coord = state.coordinator.clone();
    Task::perform(
        async move {
            // The error case (no grant yet) is exactly when
            // macOS shows its prompt — that's what we want.
            let _ = coord.list_displays().await;
        },
        |()| Message::PermissionTick,
    )
}

/// `Message::ResetPermissionRequested`.
pub(super) fn on_reset_permission(state: &mut App) -> Task<Message> {
    state.update_sync(Message::ResetPermissionRequested);
    let coord = state.coordinator.clone();
    Task::perform(
        async move {
            let _ = coord.list_displays().await;
        },
        |()| Message::OpenPermissionSettingsRequested,
    )
}

/// `Message::PermissionTick` and `Message::PermissionPollTick`: re-read
/// the permission and close the Welcome window once it is granted.
pub(super) fn on_permission_tick(state: &mut App, tick: Message) -> Task<Message> {
    let status = if matches!(tick, Message::PermissionPollTick) {
        state.permissions.status_quiet()
    } else {
        state.permissions.status()
    };
    let was_showing = state.welcome.should_show();
    let was_onboarding_completed = state.preferences.onboarding_completed;
    // Reuse the existing synchronous handler — it drives the
    // `WelcomeState` machine without touching iced state.
    state.update_sync(Message::PermissionPoll(status));
    // If the permission gate just cleared for a user who has
    // not yet completed onboarding, keep the visible welcome
    // window open so it can redraw as the "You're all set"
    // state. Returning users get the old tray-only behavior:
    // close the permission window and show a notification.
    if was_showing && !state.welcome.should_show() {
        let pending_url = state.pending_url_after_permission.take();
        if !was_onboarding_completed {
            return pending_url
                .map(|action| Task::done(Message::UrlActionReceived(action)))
                .unwrap_or_else(Task::none);
        }
        let welcome_ids: Vec<_> = state
            .windows
            .iter()
            .filter_map(|(id, k)| (*k == WindowKind::Welcome).then_some(*id))
            .collect();
        let mut close_tasks: Vec<Task<Message>> = Vec::with_capacity(welcome_ids.len());
        for id in welcome_ids {
            state.windows.forget(id);
            close_tasks.push(window::close(id));
        }
        if !close_tasks.is_empty() {
            notify_running_in_menu_bar(&state.preferences.capture_hotkey);
        }
        if let Some(action) = pending_url {
            close_tasks.push(Task::done(Message::UrlActionReceived(action)));
        }
        return Task::batch(close_tasks);
    }
    Task::none()
}
