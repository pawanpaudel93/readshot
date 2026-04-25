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
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use iced::widget::{button, column, container, row, text, Space};
use iced::window;
use iced::{Alignment, Color, Element, Length, Subscription, Task, Theme};

use readshot_capture::{CaptureRequest, DisplayInfo};
use readshot_core::Preferences;
use readshot_ui::hotkey;

use crate::url_scheme::UrlAction;

/// Initial URL action set from `main.rs` before `iced::daemon` starts.
/// `start()` consumes this and feeds an extra `Message::UrlActionReceived`
/// task on boot, before the welcome window even has time to paint.
static INITIAL_URL_ACTION: OnceLock<Mutex<Option<UrlAction>>> = OnceLock::new();

/// Stash a parsed URL scheme action so the iced runtime sees it when
/// it boots. Call this from `main.rs` before the daemon takes over.
/// On macOS, true URL-event delivery via NSAppleEventManager isn't
/// hooked up here — Phase D part 2. argv-based delivery (e.g.
/// `open readshot://new` or running the binary with the URL as the
/// first argument) is supported today.
pub fn set_initial_url_action(action: UrlAction) {
    let slot = INITIAL_URL_ACTION.get_or_init(|| Mutex::new(None));
    *slot.lock().expect("INITIAL_URL_ACTION poisoned") = Some(action);
}

fn take_initial_url_action() -> Option<UrlAction> {
    INITIAL_URL_ACTION
        .get()
        .and_then(|m| m.lock().ok().and_then(|mut g| g.take()))
}

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

    // Surface the initial permission state in the log so users
    // (and us, when triaging issues) can see whether macOS TCC is
    // already returning Granted before the welcome window appears.
    tracing::info!(
        target: "readshot::permissions",
        "initial status: {:?}",
        app.coordinator.pre_capture_gate(),
    );

    // Register the user's preferred global hotkey. Failures are
    // logged-and-swallowed: the binary remains usable from the GUI
    // button + CLI / MCP surfaces if hotkey registration fails (e.g.
    // another app already owns the chord).
    if let Some(manager) = register_default_hotkey(&app.preferences) {
        app.hotkey_manager = Some(manager);
    }
    // Same fail-soft contract for the tray. Linux without an
    // appindicator daemon, or a Windows session without a Shell_Notify
    // surface, will simply not see the tray entry.
    app.tray = crate::tray::install();

    let (id, open_task) = window::open(welcome_window_settings());
    app.windows.register(id, WindowKind::Welcome);

    let mut tasks: Vec<Task<Message>> = vec![open_task.map(|_id| Message::WelcomeWindowReady)];
    if let Some(action) = take_initial_url_action() {
        tasks.push(Task::done(Message::UrlActionReceived(action)));
    }
    (app, Task::batch(tasks))
}

fn register_default_hotkey(prefs: &Preferences) -> Option<GlobalHotKeyManager> {
    let spec = match hotkey::parse(&prefs.capture_hotkey) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "readshot::hotkey",
                "could not parse capture_hotkey `{}`: {e}",
                prefs.capture_hotkey,
            );
            return None;
        }
    };
    let manager = match GlobalHotKeyManager::new() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(target: "readshot::hotkey", "GlobalHotKeyManager init failed: {e}");
            return None;
        }
    };
    if let Err(e) = manager.register(spec.to_global_hotkey()) {
        tracing::warn!(
            target: "readshot::hotkey",
            "failed to register `{}`: {e}",
            prefs.capture_hotkey,
        );
        return None;
    }
    tracing::info!(
        target: "readshot::hotkey",
        "registered global hotkey: {}",
        prefs.capture_hotkey,
    );
    Some(manager)
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

/// Window settings for the annotation / actions editor.
fn editor_window_settings() -> window::Settings {
    window::Settings {
        size: iced::Size::new(720.0, 560.0),
        min_size: Some(iced::Size::new(360.0, 240.0)),
        position: window::Position::Centered,
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

/// Window settings for the region-capture overlay.
///
/// We open a borderless `AlwaysOnTop` transparent window the size of
/// the primary display. Resizable=false because the overlay should
/// always cover the whole display; closeable=false because the user
/// cancels with ESC, not the missing close button.
fn overlay_window_settings() -> window::Settings {
    window::Settings {
        // 1.0×1.0 placeholder — winit then picks the actual primary
        // display's size when we set `fullscreen: true`. (For multi-
        // display in Phase B-ext we'd switch to per-monitor positions
        // and explicit sizes.)
        size: iced::Size::new(1.0, 1.0),
        position: window::Position::Specific(iced::Point::new(0.0, 0.0)),
        resizable: false,
        decorations: false,
        transparent: true,
        visible: true,
        fullscreen: true,
        level: window::Level::AlwaysOnTop,
        closeable: false,
        minimizable: false,
        ..Default::default()
    }
}

/// Per-window title.
pub fn title(state: &App, id: window::Id) -> String {
    match state.windows.kind(id) {
        Some(WindowKind::Welcome) | None => "Readshot".into(),
        Some(WindowKind::Overlay) => "Readshot — Region capture".into(),
        Some(WindowKind::Editor) => "Readshot — Editor".into(),
    }
}

/// Theme: dark by default; preferences could switch this in a later phase.
pub fn theme(_state: &App, _id: window::Id) -> Theme {
    Theme::Dark
}

/// Background subscriptions — permission poll + global-hotkey drain.
pub fn subscription(state: &App) -> Subscription<Message> {
    let mut subs = Vec::new();
    if state.welcome.should_show() {
        // Polling at 500 ms keeps the UI responsive without burning
        // CPU. The TCC db is already cached in-process so each poll
        // is essentially a no-op syscall.
        subs.push(iced::time::every(Duration::from_millis(500)).map(|_| Message::PermissionTick));
    }
    if state.hotkey_manager.is_some() {
        // 50 ms drain — `try_recv` is a non-blocking peek at the
        // crossbeam channel `global-hotkey` writes to from its OS
        // event handler. Cheap when there are no events.
        subs.push(iced::time::every(Duration::from_millis(50)).map(|_| Message::HotkeyTick));
    }
    if state.tray.is_some() {
        // 100 ms is fine for tray clicks — humans can't tell the
        // difference between a 50 ms and 100 ms tray menu response.
        subs.push(iced::time::every(Duration::from_millis(100)).map(|_| Message::TrayTick));
    }
    Subscription::batch(subs)
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

        Message::HotkeyTick => {
            // Drain anything the OS-side handler queued. We collapse
            // multiple presses into a single capture request so a
            // mashed hotkey doesn't stack pending captures.
            let mut fired = false;
            let receiver = GlobalHotKeyEvent::receiver();
            while receiver.try_recv().is_ok() {
                fired = true;
            }
            if fired && !state.welcome.should_show() && !state.capture_in_flight {
                // The hotkey defaults to opening the region overlay.
                return update(state, Message::OpenOverlayRequested);
            }
            Task::none()
        }

        Message::TrayTick => {
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
                .collect();
            Task::batch(tasks)
        }

        Message::TrayActionPerformed(action) => match action {
            crate::tray::TrayAction::Capture => {
                if !state.welcome.should_show() && !state.capture_in_flight {
                    // Tray "Capture" → open the region overlay, same
                    // as the welcome window's region button.
                    update(state, Message::OpenOverlayRequested)
                } else {
                    Task::none()
                }
            }
            crate::tray::TrayAction::ShowWindow => {
                // No window-show wiring yet (we always have one). Once
                // the welcome window can hide to the tray, this
                // dispatches `window::change_mode` to bring it back.
                Task::none()
            }
            crate::tray::TrayAction::Quit => iced::exit(),
        },

        Message::CaptureFullPrimaryRequested => {
            let coord = state.coordinator.clone();
            state.capture_in_flight = true;
            state.last_capture_status = None;
            Task::perform(capture_primary_to_desktop(coord), |result| {
                Message::CaptureSaved(result.map_err(|e| e.to_string()))
            })
        }

        Message::OpenOverlayRequested => {
            if state.welcome.should_show() || state.capture_in_flight {
                return Task::none();
            }
            // Don't stack overlays.
            if state.windows.kinds().any(|k| k == WindowKind::Overlay) {
                return Task::none();
            }
            let (id, open_task) = window::open(overlay_window_settings());
            state.windows.register(id, WindowKind::Overlay);
            open_task.map(Message::OverlayWindowReady)
        }

        Message::OverlayWindowReady(_id) => Task::none(),

        Message::OverlaySelected(rect) => {
            // Close the overlay window first (it's the most recently
            // registered Overlay-kind id) so the capture itself doesn't
            // include the dimming veil.
            let mut close_tasks: Vec<Task<Message>> = Vec::new();
            let overlay_ids: Vec<_> = state
                .windows
                .iter()
                .filter_map(|(id, k)| (*k == WindowKind::Overlay).then_some(*id))
                .collect();
            for id in overlay_ids {
                state.windows.forget(id);
                close_tasks.push(window::close(id));
            }
            // Kick off the region capture once the overlay's been
            // dismissed. We chain via a discrete Task::done so the
            // close request lands first.
            close_tasks
                .push(Task::done(Message::CaptureRegionRequested(rect)));
            Task::batch(close_tasks)
        }

        Message::OverlayCancelled => {
            let overlay_ids: Vec<_> = state
                .windows
                .iter()
                .filter_map(|(id, k)| (*k == WindowKind::Overlay).then_some(*id))
                .collect();
            let mut close_tasks: Vec<Task<Message>> = Vec::new();
            for id in overlay_ids {
                state.windows.forget(id);
                close_tasks.push(window::close(id));
            }
            Task::batch(close_tasks)
        }

        Message::CaptureRegionRequested(rect) => {
            let coord = state.coordinator.clone();
            state.capture_in_flight = true;
            state.last_capture_status = None;
            // Region capture lands in the editor instead of saving
            // directly — the editor decides what to do with it.
            Task::perform(capture_region_to_image(coord, rect), |result| {
                Message::RegionCaptureCompleted(result.map_err(|e| e.to_string()))
            })
        }

        Message::RegionCaptureCompleted(result) => {
            state.capture_in_flight = false;
            match result {
                Ok(image) => {
                    state.editor = Some(crate::editor::EditorState::new(image));
                    let (id, open_task) = window::open(editor_window_settings());
                    state.windows.register(id, WindowKind::Editor);
                    open_task.map(Message::EditorWindowReady)
                }
                Err(e) => {
                    state.last_capture_status = Some(format!("Capture failed: {e}"));
                    Task::none()
                }
            }
        }

        Message::EditorWindowReady(id) => {
            if let Some(ed) = &mut state.editor {
                ed.window_id = Some(id);
            }
            Task::none()
        }

        Message::EditorSaveRequested => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            ed.busy = true;
            ed.status = Some("Saving…".into());
            let img = ed.image.clone();
            Task::perform(save_image_to_desktop(img), |r| {
                Message::EditorSaved(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorSaved(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                ed.status = Some(match result {
                    Ok(p) => format!("Saved to {}", p.display()),
                    Err(e) => format!("Save failed: {e}"),
                });
            }
            Task::none()
        }

        Message::EditorCopyImageRequested => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            ed.busy = true;
            ed.status = Some("Copying…".into());
            let img = ed.image.clone();
            Task::perform(copy_image_to_clipboard(img), |r| {
                Message::EditorCopyImageDone(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyImageDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                ed.status = Some(match result {
                    Ok(()) => "Copied to clipboard.".into(),
                    Err(e) => format!("Copy failed: {e}"),
                });
            }
            Task::none()
        }

        Message::EditorCopyTextRequested => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            ed.busy = true;
            ed.status = Some("Recognising text…".into());
            let img = ed.image.clone();
            let coord = state.coordinator.clone();
            Task::perform(ocr_then_copy(coord, img), |r| {
                Message::EditorCopyTextDone(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorCopyTextDone(result) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                ed.status = Some(match result {
                    Ok(text) if text.is_empty() => "No text recognised.".into(),
                    Ok(text) => format!(
                        "Copied {} character{} of text.",
                        text.chars().count(),
                        if text.chars().count() == 1 { "" } else { "s" },
                    ),
                    Err(e) => format!("Copy text failed: {e}"),
                });
            }
            Task::none()
        }

        Message::EditorDiscardRequested => {
            let id = state.editor.as_ref().and_then(|e| e.window_id);
            state.editor = None;
            if let Some(id) = id {
                state.windows.forget(id);
                window::close(id)
            } else {
                Task::none()
            }
        }

        Message::CaptureSaved(result) => {
            state.capture_in_flight = false;
            state.last_capture_status = Some(match &result {
                Ok(path) => format!("Saved to {}", path.display()),
                Err(e) => format!("Capture failed: {e}"),
            });
            Task::none()
        }

        Message::UrlActionReceived(action) => match action {
            UrlAction::NewCapture => {
                tracing::info!(target: "readshot::url", "readshot:// → opening overlay");
                update(state, Message::OpenOverlayRequested)
            }
            UrlAction::Unknown(path) => {
                tracing::warn!(target: "readshot::url", "ignoring unknown readshot:// path: {path}");
                Task::none()
            }
        },

        Message::QuitRequested => iced::exit(),

        Message::RestartRequested => {
            // macOS Screen Recording's TCC grant is cached per-process
            // by `CGPreflightScreenCaptureAccess` — once a process has
            // seen "denied", it can't ever see "granted" without a
            // restart. We respawn the bundle via Launch Services so
            // the new process picks up the fresh grant, then exit.
            #[cfg(target_os = "macos")]
            {
                if let Err(e) = relaunch_via_launch_services() {
                    tracing::warn!(target: "readshot::permissions", "relaunch failed: {e}");
                }
            }
            iced::exit()
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

/// Top-level view dispatch.
pub fn view(state: &App, id: window::Id) -> Element<'_, Message> {
    match state.windows.kind(id) {
        Some(WindowKind::Welcome) | None => welcome_view(state),
        Some(WindowKind::Overlay) => overlay_view(),
        Some(WindowKind::Editor) => editor_view(state),
    }
}

fn editor_view(state: &App) -> Element<'_, Message> {
    let Some(ed) = state.editor.as_ref() else {
        return container(text("(no capture)"))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };
    let img = iced::widget::image(ed.handle()).width(Length::Fill);

    let make_btn = |label: &'static str, msg: Message| {
        let mut b = button(label);
        if !ed.busy {
            b = b.on_press(msg);
        }
        b
    };

    let actions = row![
        make_btn("Save", Message::EditorSaveRequested),
        make_btn("Copy", Message::EditorCopyImageRequested),
        make_btn("Copy Text", Message::EditorCopyTextRequested),
        make_btn("Discard", Message::EditorDiscardRequested),
    ]
    .spacing(8);

    let toast: Element<'_, Message> = match &ed.status {
        Some(s) => text(s).into(),
        None => Space::new().height(Length::Fixed(0.0)).into(),
    };

    container(
        column![img, actions, toast]
            .spacing(8)
            .padding(12)
            .align_x(Alignment::Start),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn overlay_view<'a>() -> Element<'a, Message> {
    use iced::widget::canvas::Canvas;
    use iced::widget::stack;

    let canvas = Canvas::new(crate::overlay::OverlayProgram)
        .width(Length::Fill)
        .height(Length::Fill);

    let canvas_layer = container(canvas)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_theme| iced::widget::container::Style {
            // Fully transparent container so the canvas's veil + selection
            // is the only thing visible. Without this the iced default
            // background paints over the wgpu transparent layer.
            background: Some(Color::TRANSPARENT.into()),
            ..Default::default()
        });

    // Floating hint near the top of the overlay. We give it a
    // semi-opaque dark capsule so the text reads regardless of the
    // wallpaper underneath.
    let hint = container(
        text("Drag to select a region · Enter to capture all · Esc to cancel")
            .size(13)
            .color(Color::WHITE),
    )
    .padding(8)
    .style(|_| iced::widget::container::Style {
        background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.55).into()),
        text_color: Some(Color::WHITE),
        border: iced::Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });

    let hint_layer = container(hint)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(24)
        .align_x(Alignment::Center)
        .align_y(Alignment::Start);

    stack![canvas_layer, hint_layer].into()
}

fn welcome_view(state: &App) -> Element<'_, Message> {
    let status_line = text(permission_blurb(state.welcome));
    let action: Element<'_, Message> = match state.welcome {
        WelcomeState::Granted => row![
            text("Screen Recording is granted. You're all set.").width(Length::Fill),
        ]
        .into(),
        WelcomeState::Pending => column![
            text("Readshot needs Screen Recording permission to capture your screen.")
                .width(Length::Fill),
            text(
                "Click Grant access. After you toggle Readshot ON in \
                 System Settings → Privacy & Security → Screen \
                 Recording, click Restart Readshot — macOS won't \
                 update Screen Recording in a running process."
            )
            .size(12)
            .width(Length::Fill),
            row![
                button("Grant access").on_press(Message::GrantPermissionRequested),
                button("Open Settings").on_press(Message::OpenPermissionSettingsRequested),
                button("Restart Readshot").on_press(Message::RestartRequested),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into(),
        WelcomeState::AwaitingGrant => column![
            text("Toggle Readshot on in System Settings → Privacy & \
                  Security → Screen Recording, then click Restart \
                  Readshot below to pick up the fresh grant.")
                .width(Length::Fill),
            row![
                button("Open Settings").on_press(Message::OpenPermissionSettingsRequested),
                button("Restart Readshot").on_press(Message::RestartRequested),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into(),
        WelcomeState::Denied => column![
            text("Permission denied. Open System Settings → Privacy & \
                  Security → Screen Recording, toggle Readshot on, \
                  then click Restart Readshot.")
                .width(Length::Fill),
            row![
                button("Open Settings").on_press(Message::OpenPermissionSettingsRequested),
                button("Restart Readshot").on_press(Message::RestartRequested),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into(),
    };

    // Flameshot-style: one trigger surfaces the overlay; the user
    // chooses region (drag) or full screen (Enter) once it's open.
    let mut capture_btn = button("Capture screen").width(Length::Fill);
    if !state.welcome.should_show() && !state.capture_in_flight {
        capture_btn = capture_btn.on_press(Message::OpenOverlayRequested);
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
        text("Drag to select a region · Enter to capture all · Esc to cancel")
            .size(11),
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
    capture_request_to_desktop(coord, req).await
}

async fn capture_request_to_desktop(
    coord: CaptureCoordinator,
    req: CaptureRequest,
) -> Result<PathBuf, CaptureRunError> {
    let img = coord.capture_region(req).await?;
    save_to_desktop(&img)
}

/// Returns the captured image instead of saving — the editor flow
/// uses this so the user can choose what to do with the bytes.
async fn capture_region_to_image(
    coord: CaptureCoordinator,
    rect: readshot_core::geom::Rect,
) -> Result<image::RgbaImage, CaptureRunError> {
    let displays = coord.list_displays().await?;
    let primary = pick_primary(&displays).ok_or(CaptureRunError::NoDisplays)?;
    let req = CaptureRequest {
        display_id: primary.id.clone(),
        rect,
        scale: primary.scale,
        hide_cursor: true,
    };
    Ok(coord.capture_region(req).await?)
}

/// Save an already-captured image to a timestamped Desktop PNG.
async fn save_image_to_desktop(img: image::RgbaImage) -> Result<PathBuf, CaptureRunError> {
    save_to_desktop(&img)
}

fn save_to_desktop(img: &image::RgbaImage) -> Result<PathBuf, CaptureRunError> {
    let dir = directories::UserDirs::new()
        .and_then(|d| d.desktop_dir().map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let path = dir.join(format!("Readshot-{stamp}.png"));
    img.save_with_format(&path, image::ImageFormat::Png)?;
    Ok(path)
}

/// Push an RGBA image to the system clipboard. Runs the arboard
/// init synchronously inside `spawn_blocking` because some platforms
/// (X11 specifically) hold internal mutexes that don't play well
/// with reentrant async runtimes.
async fn copy_image_to_clipboard(img: image::RgbaImage) -> Result<(), ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut ctx = arboard::Clipboard::new()?;
        let data = arboard::ImageData {
            width: img.width() as usize,
            height: img.height() as usize,
            bytes: std::borrow::Cow::Borrowed(img.as_raw()),
        };
        ctx.set_image(data)?;
        Ok::<(), ClipboardError>(())
    })
    .await
    .map_err(|e| ClipboardError::Join(e.to_string()))?
}

/// Run the OCR engine on the captured image and copy the result to
/// the clipboard. Returns the text (so the editor can show its size
/// in the toast).
async fn ocr_then_copy(
    coord: CaptureCoordinator,
    img: image::RgbaImage,
) -> Result<String, OcrCopyError> {
    let result = coord
        .recognise(readshot_ocr::OCRRequest {
            image: img,
            languages: Vec::new(),
            use_language_correction: true,
        })
        .await?;
    let text = result;
    if !text.is_empty() {
        let to_copy = text.clone();
        tokio::task::spawn_blocking(move || -> Result<(), ClipboardError> {
            let mut ctx = arboard::Clipboard::new()?;
            ctx.set_text(&to_copy)?;
            Ok(())
        })
        .await
        .map_err(|e| OcrCopyError::Clipboard(ClipboardError::Join(e.to_string())))??;
    }
    Ok(text)
}

#[derive(Debug, thiserror::Error)]
enum ClipboardError {
    #[error(transparent)]
    Arboard(#[from] arboard::Error),
    #[error("clipboard worker join failed: {0}")]
    Join(String),
}

#[derive(Debug, thiserror::Error)]
enum OcrCopyError {
    #[error(transparent)]
    Ocr(#[from] readshot_core::error::OCRError),
    #[error(transparent)]
    Clipboard(#[from] ClipboardError),
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

/// Spawn `open <bundle.app>` so Launch Services starts a fresh
/// copy of Readshot. We use the bundle path discovered via
/// `CFBundleCopyBundleURL`-equivalent (env-derived from the running
/// executable) so the user's installed location is honoured.
#[cfg(target_os = "macos")]
fn relaunch_via_launch_services() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    // .../Readshot.app/Contents/MacOS/Readshot → .../Readshot.app
    let bundle = exe
        .parent() // MacOS
        .and_then(|p| p.parent()) // Contents
        .and_then(|p| p.parent()) // *.app
        .map(|p| p.to_path_buf())
        .unwrap_or(exe);
    // Fully detach the child so it doesn't die when we exit.
    std::process::Command::new("open")
        .arg("-n")
        .arg(&bundle)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
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

    #[test]
    fn register_default_hotkey_returns_none_for_garbage_string() {
        let mut prefs = Preferences::default();
        prefs.capture_hotkey = "not a hotkey at all".into();
        assert!(register_default_hotkey(&prefs).is_none());
    }

    #[test]
    fn hotkey_tick_no_events_is_noop() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        // No manager registered → tick does nothing meaningful.
        let _ = update(&mut app, Message::HotkeyTick);
        assert!(!app.capture_in_flight);
    }
}
