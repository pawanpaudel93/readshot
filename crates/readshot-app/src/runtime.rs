//! iced daemon runtime — Phase A.
//!
//! Wires [`crate::app::App`] to the iced 0.14 daemon API so the
//! binary actually opens a window. This module owns:
//!
//! * `start` — boot fn that opens the welcome window and returns the
//!   initial `App` state.
//! * `update` — async-aware version of `App::update_sync` that
//!   produces `Task<Message>`s for capture/save/permission work.
//! * `view` / `title` / `theme` — per-window rendering hooks.
//! * `subscription` — drives the permission-poll timer and any
//!   external event channels (tray + hotkey come in a follow-up).
//!
//! Phase A scope (kept tight on purpose):
//!
//! * One window kind for now: a welcome / launcher window with a
//!   permission-status panel and a "Capture primary display" button.
//! * No multi-window overlay (Phase B) and no annotation editor
//!   (Phase C); the launcher saves the captured PNG to the user's
//!   Desktop with a timestamped filename.
//! * No tray icon or global hotkey yet — the launcher-button + URL
//!   scheme are the trigger surfaces for now.
//!
//! Everything testable without iced (state transitions, capture
//! orchestration helpers) is covered by sibling unit tests in
//! `app.rs` / `coordinator.rs` / `cli.rs` — this module's job is to
//! glue the typed state machine into iced and stay thin.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::widget::{button, column, container, row, text, Space};
use iced::window;
use iced::{Alignment, Element, Length, Subscription, Task, Theme};

use readshot_capture::{CaptureRequest, DisplayInfo};
use readshot_core::Preferences;

use crate::app::{App, Message, WindowKind};
use crate::coordinator::CaptureCoordinator;
use crate::permissions::{default_provider, PermissionStatus};
use crate::welcome::WelcomeState;

/// Bootstrap: build the App, open the welcome window, and return
/// both to iced. The window-open `Task` resolves to a window id
/// which we slot into [`App::windows`] so subsequent `view` calls
/// can dispatch to the correct widget tree.
pub fn start() -> (App, Task<Message>) {
    let permissions = Arc::from(default_provider());
    let coordinator = CaptureCoordinator::new(
        Arc::from(readshot_capture::default_capturer()),
        Arc::from(readshot_ocr::default_engine()),
        Arc::clone(&permissions),
        None,
    );
    let mut app = App::new(coordinator, permissions, Preferences::default());

    let (id, open_task) = window::open(welcome_window_settings());
    app.windows.register(id, WindowKind::Welcome);

    let task = open_task.map(|_id| Message::WelcomeWindowReady);
    (app, task)
}

fn welcome_window_settings() -> window::Settings {
    window::Settings {
        size: iced::Size::new(520.0, 380.0),
        min_size: Some(iced::Size::new(420.0, 320.0)),
        position: window::Position::Centered,
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

/// Per-window title.
pub fn title(state: &App, id: window::Id) -> String {
    match state.windows.kind(id) {
        Some(WindowKind::Welcome) => "Readshot".into(),
        None => "Readshot".into(),
    }
}

/// Theme: dark by default; preferences could switch this in a later phase.
pub fn theme(_state: &App, _id: window::Id) -> Theme {
    Theme::Dark
}

/// Background subscriptions — currently just the permission poll.
/// Tray + global-hotkey channels join here in Phase A.5.
pub fn subscription(state: &App) -> Subscription<Message> {
    if state.welcome.should_show() {
        // Polling at 500 ms keeps the UI responsive without burning
        // CPU. The TCC db is already cached in-process so each poll
        // is essentially a no-op syscall.
        iced::time::every(Duration::from_millis(500)).map(|_| Message::PermissionTick)
    } else {
        Subscription::none()
    }
}

/// Top-level update fn. Delegates testable transitions to
/// [`App::update_sync`] and adds the iced-only async branches.
pub fn update(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::WelcomeWindowReady => Task::none(),

        Message::PermissionTick => {
            let status = state.permissions.status();
            // Reuse the existing synchronous handler — it drives the
            // `WelcomeState` machine without touching iced state.
            state.update_sync(Message::PermissionPoll(status));
            Task::none()
        }

        Message::CaptureFullPrimaryRequested => {
            let coord = state.coordinator.clone();
            state.capture_in_flight = true;
            state.last_capture_status = None;
            Task::perform(capture_primary_to_desktop(coord), |result| {
                Message::CaptureSaved(result.map_err(|e| e.to_string()))
            })
        }

        Message::CaptureSaved(result) => {
            state.capture_in_flight = false;
            state.last_capture_status = Some(match &result {
                Ok(path) => format!("Saved to {}", path.display()),
                Err(e) => format!("Capture failed: {e}"),
            });
            Task::none()
        }

        // Synchronous transitions — reuse the existing handler.
        msg @ (Message::PermissionPoll(_)
        | Message::GrantPermissionRequested
        | Message::OpenPermissionSettingsRequested
        | Message::Settings(_)) => {
            state.update_sync(msg);
            Task::none()
        }

        // Phase B+ (overlay, editor, tray, hotkey, URL scheme) —
        // ignore for now so the daemon is well-behaved if a stray
        // message slips through during testing.
        _ => Task::none(),
    }
}

/// Top-level view dispatch. Phase A only owns the welcome window.
pub fn view(state: &App, id: window::Id) -> Element<'_, Message> {
    match state.windows.kind(id) {
        Some(WindowKind::Welcome) | None => welcome_view(state),
    }
}

fn welcome_view(state: &App) -> Element<'_, Message> {
    let status_line = text(permission_blurb(state.welcome));
    let action: Element<'_, Message> = match state.welcome {
        WelcomeState::Granted => row![
            text("Screen Recording is granted. You're all set.").width(Length::Fill),
        ]
        .into(),
        WelcomeState::Pending => row![
            text("Readshot needs Screen Recording permission to capture your screen.")
                .width(Length::Fill),
            button("Grant access").on_press(Message::GrantPermissionRequested),
        ]
        .spacing(16)
        .align_y(Alignment::Center)
        .into(),
        WelcomeState::AwaitingGrant => row![
            text("Waiting for your decision in System Settings…").width(Length::Fill),
        ]
        .into(),
        WelcomeState::Denied => row![
            text("Permission denied. Open System Settings to flip the toggle.")
                .width(Length::Fill),
            button("Open Settings").on_press(Message::OpenPermissionSettingsRequested),
        ]
        .spacing(16)
        .align_y(Alignment::Center)
        .into(),
    };

    let mut capture_btn = button("Capture primary display").width(Length::Fill);
    if !state.welcome.should_show() && !state.capture_in_flight {
        capture_btn = capture_btn.on_press(Message::CaptureFullPrimaryRequested);
    }

    let toast: Element<'_, Message> = match &state.last_capture_status {
        Some(s) => text(s).into(),
        None => Space::new().height(Length::Fixed(0.0)).into(),
    };

    let body = column![
        text("Readshot").size(28),
        text("Cross-platform screenshot + offline OCR.").size(14),
        Space::new().height(Length::Fixed(16.0)),
        status_line,
        action,
        Space::new().height(Length::Fixed(16.0)),
        capture_btn,
        Space::new().height(Length::Fixed(8.0)),
        toast,
        Space::new().width(Length::Fill).height(Length::Fill),
    ]
    .spacing(8)
    .padding(20)
    .align_x(Alignment::Start);

    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn permission_blurb(state: WelcomeState) -> &'static str {
    match state {
        WelcomeState::Granted => "Permission: granted",
        WelcomeState::Pending => "Permission: not requested",
        WelcomeState::AwaitingGrant => "Permission: prompted, awaiting response",
        WelcomeState::Denied => "Permission: denied",
    }
}

/// Async helper: capture the primary display's full bounds and write
/// the PNG to `~/Desktop/Readshot-<timestamp>.png`. Returns the saved
/// path on success.
async fn capture_primary_to_desktop(
    coord: CaptureCoordinator,
) -> Result<PathBuf, CaptureRunError> {
    let displays = coord.list_displays().await?;
    let primary = pick_primary(&displays).ok_or(CaptureRunError::NoDisplays)?;
    let req = CaptureRequest {
        display_id: primary.id.clone(),
        rect: primary.bounds,
        scale: primary.scale,
        hide_cursor: true,
    };
    let img = coord.capture_region(req).await?;

    let dir = directories::UserDirs::new()
        .and_then(|d| d.desktop_dir().map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let path = dir.join(format!("Readshot-{stamp}.png"));
    img.save_with_format(&path, image::ImageFormat::Png)?;
    Ok(path)
}

fn pick_primary(displays: &[DisplayInfo]) -> Option<&DisplayInfo> {
    displays
        .iter()
        .find(|d| d.is_primary)
        .or_else(|| displays.first())
}

/// Capture-flow error type, surfaced to the welcome window's toast
/// region. Bridged through `Message::CaptureSaved` as a `String` so
/// `Message: Clone + Debug` stays trivially derivable.
#[derive(Debug, thiserror::Error)]
enum CaptureRunError {
    #[error(transparent)]
    Capture(#[from] readshot_core::error::CaptureError),
    #[error(transparent)]
    Image(#[from] image::ImageError),
    #[error("no displays available")]
    NoDisplays,
}

/// Helper for the previous synchronous status check used by tests.
#[allow(dead_code)]
pub(crate) fn permission_status_blurb(s: PermissionStatus) -> &'static str {
    match s {
        PermissionStatus::Granted => "granted",
        PermissionStatus::Denied => "denied",
        PermissionStatus::NotApplicable => "not applicable",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::fake::FakePermissions;
    use readshot_capture::fake::FakeCapturer;
    use readshot_ocr::fake::FakeOcrEngine;

    fn build_app(perms: Arc<FakePermissions>) -> App {
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hi")),
            perms.clone(),
            None,
        );
        App::new(coord, perms, Preferences::default())
    }

    #[test]
    fn permission_tick_drives_welcome_state_machine() {
        let perms = Arc::new(FakePermissions::denied());
        let mut app = build_app(perms.clone());
        // Pre-condition: welcome shows.
        assert!(app.welcome.should_show());
        // After the OS flips to granted, a tick clears welcome.
        perms.flip_to_granted();
        let _ = update(&mut app, Message::PermissionTick);
        assert!(!app.welcome.should_show());
    }

    #[test]
    fn capture_request_marks_in_flight_and_clears_on_completion() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let _ = update(&mut app, Message::CaptureFullPrimaryRequested);
        assert!(app.capture_in_flight);
        let _ = update(
            &mut app,
            Message::CaptureSaved(Ok(PathBuf::from("/tmp/x.png"))),
        );
        assert!(!app.capture_in_flight);
        let toast = app.last_capture_status.as_deref().unwrap();
        assert!(toast.contains("/tmp/x.png"));
    }

    #[test]
    fn capture_failure_message_lands_in_toast() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let _ = update(
            &mut app,
            Message::CaptureSaved(Err("permission denied".into())),
        );
        assert!(!app.capture_in_flight);
        let toast = app.last_capture_status.as_deref().unwrap();
        assert!(toast.contains("permission denied"));
    }

    #[test]
    fn pick_primary_prefers_primary_then_first() {
        let primary = DisplayInfo {
            id: "p".into(),
            bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 100.0, 100.0).unwrap(),
            scale: 1.0,
            name: "main".into(),
            is_primary: true,
        };
        let secondary = DisplayInfo {
            id: "s".into(),
            bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 100.0, 100.0).unwrap(),
            scale: 1.0,
            name: "side".into(),
            is_primary: false,
        };
        assert_eq!(
            pick_primary(&[secondary.clone(), primary.clone()]).unwrap().id,
            "p"
        );
        assert_eq!(pick_primary(&[secondary.clone()]).unwrap().id, "s");
        assert!(pick_primary(&[]).is_none());
    }

    #[tokio::test]
    async fn capture_primary_to_desktop_writes_png() {
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hi")),
            perms,
            None,
        );
        // The fake capturer reports a 256x256 fake-0 display.
        let path = capture_primary_to_desktop(coord).await.unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        // Cleanup so we don't litter the dev's Desktop on repeated runs.
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn permission_blurb_changes_with_state() {
        assert_eq!(permission_blurb(WelcomeState::Granted), "Permission: granted");
        assert_eq!(permission_blurb(WelcomeState::Denied), "Permission: denied");
    }
}
