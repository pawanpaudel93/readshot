//! App-level messages: restart and the screen-recording permission.
//! Moved out of `update_message`; `use super::*` keeps the arms'
//! names resolving as they did there.

use super::*;

/// Every [`AppMsg`].
pub(super) fn handle_app_message(state: &mut App, message: AppMsg) -> Task<Message> {
    match message {
        // Explicit checks (Recheck, after Allow) may probe; the periodic
        // tick must not, or macOS re-raises its permission alert each time.
        tick @ (AppMsg::PermissionTick | AppMsg::PermissionPollTick) => {
            on_permission_tick(state, tick)
        }
        AppMsg::ReopenRequested => {
            recheck_notifications_quietly();
            show_or_focus_welcome(state)
        }
        AppMsg::QuitRequested => quit_readshot(&state.coordinator),
        AppMsg::RestartRequested => on_restart_requested(),
        // GrantPermissionRequested: just trigger the macOS permission
        // prompt by poking ScreenCaptureKit
        // (`SCShareableContent::get()` runs inside `list_displays`).
        // The OS prompt itself carries an "Open System Settings"
        // button — letting macOS drive that flow avoids our app
        // racing it with a second `open URL` call. The
        // "Open Settings again" button in the AwaitingGrant state
        // covers users whose prompt never appears (e.g. previously
        // denied) by deep-linking directly.
        AppMsg::GrantPermissionRequested => on_grant_permission(state),
        // Clear the stale row, re-register this signature with macOS (the
        // SCK probe inside `list_displays` does that), and open the pane
        // so the user can switch Readshot on for this build.
        AppMsg::ResetPermissionRequested => on_reset_permission(state),
        // Synchronous transitions — reuse the existing handler.
        msg @ (AppMsg::PermissionPoll(_) | AppMsg::OpenPermissionSettingsRequested) => {
            state.apply_app_sync(msg);
            Task::none()
        }
    }
}

/// `AppMsg::RestartRequested`.
fn on_restart_requested() -> Task<Message> {
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

/// `AppMsg::GrantPermissionRequested`.
fn on_grant_permission(state: &mut App) -> Task<Message> {
    state.update_sync(Message::App(AppMsg::GrantPermissionRequested));
    let coord = state.coordinator.clone();
    Task::perform(
        async move {
            // The error case (no grant yet) is exactly when
            // macOS shows its prompt — that's what we want.
            let _ = coord.list_displays().await;
        },
        |()| Message::App(AppMsg::PermissionTick),
    )
}

/// `AppMsg::ResetPermissionRequested`.
fn on_reset_permission(state: &mut App) -> Task<Message> {
    state.update_sync(Message::App(AppMsg::ResetPermissionRequested));
    let coord = state.coordinator.clone();
    Task::perform(
        async move {
            let _ = coord.list_displays().await;
        },
        |()| Message::App(AppMsg::OpenPermissionSettingsRequested),
    )
}

/// `AppMsg::PermissionTick` and `AppMsg::PermissionPollTick`: re-read
/// the permission and close the Welcome window once it is granted.
fn on_permission_tick(state: &mut App, tick: AppMsg) -> Task<Message> {
    let status = if matches!(tick, AppMsg::PermissionPollTick) {
        state.permissions.status_quiet()
    } else {
        state.permissions.status()
    };
    let was_showing = state.welcome.should_show();
    let was_onboarding_completed = state.preferences.onboarding_completed;
    // Reuse the existing synchronous handler — it drives the
    // `WelcomeState` machine without touching iced state.
    state.update_sync(Message::App(AppMsg::PermissionPoll(status)));
    // If the permission gate just cleared for a user who has
    // not yet completed onboarding, keep the visible welcome
    // window open so it can redraw as the "You're all set"
    // state. Returning users get the old tray-only behavior:
    // close the permission window and show a notification.
    if was_showing && !state.welcome.should_show() {
        let pending_url = state.pending_url_after_permission.take();
        if !was_onboarding_completed {
            return pending_url
                .map(|action| Task::done(Message::Tick(TickMsg::UrlAction(action))))
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
            close_tasks.push(Task::done(Message::Tick(TickMsg::UrlAction(action))));
        }
        return Task::batch(close_tasks);
    }
    Task::none()
}
