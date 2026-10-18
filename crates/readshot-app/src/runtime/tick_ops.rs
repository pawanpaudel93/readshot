//! Background ticks (hotkeys, tray, URL scheme, reopen, theme) and
//! the tray and URL actions they deliver.
//! Moved out of `update_message`; `use super::*` keeps the arms'
//! names resolving as they did there.

use super::*;

/// `Message::HotkeyTick`: drain the global hotkey presses.
pub(super) fn on_hotkey_tick(state: &mut App) -> Task<Message> {
    // Drain anything the OS-side handler queued. We collapse
    // repeated presses into one action per tick so a mashed
    // hotkey doesn't stack pending windows/captures.
    let mut capture = false;
    let mut history = false;
    let mut settings = false;
    let mut scroll_stop = false;
    let mut scroll_discard = false;
    let receiver = GlobalHotKeyEvent::receiver();
    while let Ok(event) = receiver.try_recv() {
        match global_hotkey_action(&event, &state.hotkey_actions) {
            Some(GlobalHotkeyAction::Capture) => capture = true,
            Some(GlobalHotkeyAction::History) => history = true,
            Some(GlobalHotkeyAction::Settings) => settings = true,
            Some(GlobalHotkeyAction::ScrollStop) => scroll_stop = true,
            Some(GlobalHotkeyAction::ScrollDiscard) => scroll_discard = true,
            None => {}
        }
    }
    if scroll_discard {
        return update(state, Message::ScrollCaptureCancelRequested);
    }
    if scroll_stop {
        return update(state, Message::ScrollCaptureStopRequested);
    }
    if settings {
        return update(state, Message::OpenSettingsRequested);
    }
    if history {
        if state.welcome.should_show() {
            return show_or_focus_welcome(state);
        }
        return update(state, Message::OpenHistoryRequested);
    }
    if capture {
        if state.welcome.should_show() {
            // Hotkey works the same as a tray click: when
            // permission isn't granted yet, point the user
            // back at the welcome window so they can fix it.
            return show_or_focus_welcome(state);
        }
        if !state.capture_in_flight {
            return update(state, Message::OpenOverlayRequested);
        }
    }
    Task::none()
}

/// `Message::TrayTick`: drain tray clicks and the notification path's
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
        .map(|a| Task::done(Message::TrayActionPerformed(a)))
        .chain(std::iter::once(undelivered))
        .collect();
    Task::batch(tasks)
}

/// `Message::UrlTick`: drain `readshot://` actions, and, with no tray (it
/// failed to install) and so no `TrayTick`, the notification path's news.
pub(super) fn on_url_tick(state: &mut App) -> Task<Message> {
    let notices = if state.tray.is_none() {
        process_notices(state)
    } else {
        Task::none()
    };
    let tasks: Vec<Task<Message>> = crate::url_events::drain_actions()
        .into_iter()
        .map(|action| Task::done(Message::UrlActionReceived(action)))
        .chain(std::iter::once(notices))
        .collect();
    Task::batch(tasks)
}

/// `Message::AppReopenTick`.
pub(super) fn on_app_reopen_tick() -> Task<Message> {
    if crate::app_events::take_reopen_requests() == 0 {
        Task::none()
    } else {
        Task::done(Message::AppReopenRequested)
    }
}

/// `Message::TrayActionPerformed`.
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
                update(state, Message::OpenOverlayRequested)
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
                // quick-action toolbar still shows so the user
                // can change their mind, but they no longer have
                // to hunt for the Scrolling Capture button — the default
                // matches the menu entry they clicked.
                state.pending_intent = Some(crate::app::CaptureIntent::ScrollCapture);
                update(state, Message::OpenOverlayRequested)
            }
        }
        crate::tray::TrayAction::RetakeLastRegion => {
            state.pending_intent = Some(crate::app::CaptureIntent::Editor);
            update(state, Message::RetakeLastRegionRequested)
        }
        crate::tray::TrayAction::History => {
            if state.welcome.should_show() {
                show_or_focus_welcome(state)
            } else {
                update(state, Message::OpenHistoryRequested)
            }
        }
        crate::tray::TrayAction::Settings => {
            // Settings doesn't gate on permission — the user
            // might want to flip preferences before granting
            // Screen Recording.
            update(state, Message::OpenSettingsRequested)
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

/// `Message::SystemThemeChanged`.
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

/// `Message::UrlActionReceived`.
pub(super) fn on_url_action(state: &mut App, action: UrlAction) -> Task<Message> {
    match action {
        UrlAction::NewCapture => {
            tracing::info!(target: readshot_core::log::cat::URL, "readshot:// → opening overlay");
            if state.welcome.should_show() {
                state.pending_url_after_permission = Some(UrlAction::NewCapture);
                return show_or_focus_welcome(state);
            }
            update(state, Message::OpenOverlayRequested)
        }
        UrlAction::Unknown(path) => {
            tracing::warn!(target: readshot_core::log::cat::URL, "ignoring unknown readshot:// path: {path}");
            Task::none()
        }
    }
}

/// `Message::ThemePollTick`.
pub(super) fn on_theme_poll_tick(state: &mut App) -> Task<Message> {
    let os = os_theme_mode();
    if os != state.theme_mode {
        state.theme_mode = os;
    }
    Task::none()
}
