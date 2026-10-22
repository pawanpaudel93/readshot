//! Background ticks (hotkeys, tray, URL scheme, reopen, theme) and
//! the tray and URL actions they deliver.
//! Moved out of `update_message`; `use super::*` keeps the arms'
//! names resolving as they did there.

use super::*;

/// Every [`TickMsg`].
pub(super) fn handle_tick_message(state: &mut App, message: TickMsg) -> Task<Message> {
    match message {
        TickMsg::Hotkey => on_hotkey_tick(state),

        TickMsg::Tray => on_tray_tick(state),

        TickMsg::Url => on_url_tick(state),

        TickMsg::AppReopen => on_app_reopen_tick(),

        TickMsg::TrayAction(action) => on_tray_action(state, action),

        TickMsg::ThemePoll => on_theme_poll_tick(state),
        TickMsg::SystemThemeChanged(mode) => on_system_theme_changed(state, mode),
        TickMsg::UrlAction(action) => on_url_action(state, action),
    }
}

/// `TickMsg::Hotkey`: drain the global hotkey presses.
pub(super) fn on_hotkey_tick(state: &mut App) -> Task<Message> {
    let receiver = GlobalHotKeyEvent::receiver();
    let mut pressed = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        pressed.extend(global_hotkey_action(&event, &state.hotkey_actions));
    }
    run_hotkey_actions(state, &pressed)
}

/// Act on one tick's hotkey presses. Repeated presses collapse into one
/// action per tick so a mashed hotkey doesn't stack pending windows or
/// captures; a scroll session's keys win over a new capture.
pub(super) fn run_hotkey_actions(state: &mut App, pressed: &[GlobalHotkeyAction]) -> Task<Message> {
    let has = |a: GlobalHotkeyAction| pressed.contains(&a);
    if has(GlobalHotkeyAction::ScrollDiscard) {
        return update(state, Message::Scroll(ScrollMsg::CancelRequested));
    }
    if has(GlobalHotkeyAction::ScrollStop) {
        return update(state, Message::Scroll(ScrollMsg::StopRequested));
    }
    if has(GlobalHotkeyAction::Capture) {
        if state.welcome.should_show() {
            // Hotkey works the same as a tray click: when
            // permission isn't granted yet, point the user
            // back at the welcome window so they can fix it.
            return show_or_focus_welcome(state);
        }
        if !state.capture_in_flight {
            return update(state, Message::Capture(CaptureMsg::OpenOverlayRequested));
        }
    } else if has(GlobalHotkeyAction::CaptureWindow) {
        return update(state, Message::Capture(CaptureMsg::WindowCaptureRequested));
    }
    Task::none()
}

/// `TickMsg::Tray`: drain tray clicks and the notification path's
/// news ([`process_notices`]).
pub(super) fn on_tray_tick(state: &mut App) -> Task<Message> {
    let undelivered = process_notices(state);
    let actions = match &state.tray {
        Some(t) => t.drain(),
        None => Vec::new(),
    };
    // We chain Tasks for each action — typically a single
    // click → a single TrayActionPerformed message. Multiple
    // are fine too (no-op coalescing happens later).
    let tasks: Vec<Task<Message>> = actions
        .into_iter()
        .map(|a| Task::done(Message::Tick(TickMsg::TrayAction(a))))
        .chain(std::iter::once(undelivered))
        .collect();
    Task::batch(tasks)
}

/// `TickMsg::Url`: drain `readshot://` actions, and, with no tray (it
/// failed to install) and so no `TrayTick`, the notification path's news.
pub(super) fn on_url_tick(state: &mut App) -> Task<Message> {
    let notices = if state.tray.is_none() {
        process_notices(state)
    } else {
        Task::none()
    };
    let tasks: Vec<Task<Message>> = crate::url_events::drain_actions()
        .into_iter()
        .map(|action| Task::done(Message::Tick(TickMsg::UrlAction(action))))
        .chain(std::iter::once(notices))
        .collect();
    Task::batch(tasks)
}

/// `TickMsg::AppReopen`.
pub(super) fn on_app_reopen_tick() -> Task<Message> {
    // Windows / Linux: requests later launches left (none on macOS, or
    // when this copy didn't take the single-instance lock).
    for request in crate::single_instance::take_requests() {
        crate::single_instance::deliver(request);
    }
    if crate::app_events::take_reopen_requests() == 0 {
        Task::none()
    } else {
        Task::done(Message::App(AppMsg::ReopenRequested))
    }
}

/// `TickMsg::TrayAction`.
pub(super) fn on_tray_action(state: &mut App, action: crate::tray::TrayAction) -> Task<Message> {
    match action {
        crate::tray::TrayAction::Capture => {
            if state.welcome.should_show() {
                // Permission gate not cleared yet — surface the
                // welcome window so the user can grant access
                // instead of silently no-opping.
                show_or_focus_welcome(state)
            } else if state.capture_in_flight {
                Task::none()
            } else {
                state.pending_intent = Some(crate::app::CaptureIntent::Editor);
                update(state, Message::Capture(CaptureMsg::OpenOverlayRequested))
            }
        }
        crate::tray::TrayAction::ScrollCapture => {
            if state.welcome.should_show() {
                show_or_focus_welcome(state)
            } else if state.capture_in_flight || state.scroll_session.is_some() {
                Task::none()
            } else {
                // Pre-set the intent so the overlay's mouse-up /
                // Enter confirm immediately starts a scroll
                // session instead of routing to the editor. The
                // overlay auto-confirms this intent, so no quick-action
                // toolbar appears (`overlay_auto_confirm_intent`): the
                // menu entry the user clicked is the choice.
                state.pending_intent = Some(crate::app::CaptureIntent::ScrollCapture);
                update(state, Message::Capture(CaptureMsg::OpenOverlayRequested))
            }
        }
        crate::tray::TrayAction::CaptureWindow => {
            update(state, Message::Capture(CaptureMsg::WindowCaptureRequested))
        }
        crate::tray::TrayAction::RetakeLastRegion => {
            state.pending_intent = Some(crate::app::CaptureIntent::Editor);
            update(
                state,
                Message::Capture(CaptureMsg::RetakeLastRegionRequested),
            )
        }
        crate::tray::TrayAction::History => {
            if state.welcome.should_show() {
                show_or_focus_welcome(state)
            } else {
                update(state, Message::History(HistoryMsg::OpenRequested))
            }
        }
        crate::tray::TrayAction::Settings => {
            // Settings doesn't gate on permission — the user
            // might want to flip preferences before granting
            // Screen Recording.
            update(state, Message::Settings(SettingsMsg::OpenRequested))
        }
        crate::tray::TrayAction::CheckForUpdates => {
            notify_update_check_started();
            if let Err(e) = crate::updater::check_for_updates() {
                tracing::warn!(target: readshot_core::log::cat::UPDATER, "manual update check failed: {e}");
                notify_update_check_failed(&e);
            }
            Task::none()
        }
        crate::tray::TrayAction::NotificationSettings => {
            recheck_notifications_or_open_settings();
            Task::none()
        }
        crate::tray::TrayAction::About => {
            show_about_panel();
            Task::none()
        }
        crate::tray::TrayAction::Quit => quit_readshot(&state.coordinator),
    }
}

/// `TickMsg::SystemThemeChanged`.
pub(super) fn on_system_theme_changed(state: &mut App, mode: iced::theme::Mode) -> Task<Message> {
    state.theme_mode = match mode {
        iced::theme::Mode::Light => ThemeMode::Light,
        iced::theme::Mode::Dark => ThemeMode::Dark,
        // Off macOS `None` is "no news", not "dark": an X11 window
        // has no theme of its own, and iced reports that as `None`
        // every time one opens, which would wipe the portal's
        // answer.
        iced::theme::Mode::None if !READS_OS_THEME => state.theme_mode,
        iced::theme::Mode::None => ThemeMode::Dark,
    };
    Task::none()
}

/// `TickMsg::UrlAction`.
pub(super) fn on_url_action(state: &mut App, action: UrlAction) -> Task<Message> {
    match action {
        UrlAction::NewCapture => {
            tracing::info!(target: readshot_core::log::cat::URL, "readshot:// → opening overlay");
            if state.welcome.should_show() {
                state.pending_url_after_permission = Some(UrlAction::NewCapture);
                return show_or_focus_welcome(state);
            }
            update(state, Message::Capture(CaptureMsg::OpenOverlayRequested))
        }
        UrlAction::Unknown(path) => {
            tracing::warn!(target: readshot_core::log::cat::URL, "ignoring unknown readshot:// path: {path}");
            Task::none()
        }
    }
}

/// `TickMsg::ThemePoll`.
pub(super) fn on_theme_poll_tick(state: &mut App) -> Task<Message> {
    let os = os_theme_mode();
    if os != state.theme_mode {
        state.theme_mode = os;
    }
    Task::none()
}
