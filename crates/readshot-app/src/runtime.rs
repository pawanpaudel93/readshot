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

    // The welcome window is a *first-run permission gate*, not the
    // app's main UI. Once Screen Recording is granted the user lives
    // inside the menu-bar tray icon + global hotkey, which is what
    // `LSUIElement=true` apps are supposed to look like. Skip
    // opening the welcome at boot if permission is already granted —
    // we'll only ever show it again to walk the user through a
    // re-grant.
    let mut tasks: Vec<Task<Message>> = Vec::new();
    if app.welcome.should_show() {
        let (id, open_task) = window::open(welcome_window_settings());
        app.windows.register(id, WindowKind::Welcome);
        tasks.push(open_task.map(|_id| Message::WelcomeWindowReady));
    }
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

/// Window settings for the annotation / actions editor. Default size
/// is wide enough to show the toolbar without wrapping and tall
/// enough to give a typical 16:9 capture comfortable headroom for
/// drawing.
fn editor_window_settings() -> window::Settings {
    window::Settings {
        size: iced::Size::new(1100.0, 760.0),
        min_size: Some(iced::Size::new(720.0, 480.0)),
        position: window::Position::Centered,
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

/// Window settings for one overlay window covering a single display.
///
/// Borderless transparent `AlwaysOnTop` window positioned at the
/// display's *global* logical origin and sized to its logical width
/// and height. Combining this with a transparent theme (see [`style`])
/// gives us a true see-through overlay so the user can see what they
/// are about to capture.
///
/// We avoid `fullscreen: true` here — it lets winit choose a monitor,
/// which is wrong on multi-display setups. Specific position + size
/// puts the window exactly where we want.
/// Window settings for a freshly-opened pin. Borderless,
/// always-on-top, sized to the captured image (capped at a sane max
/// so a giant 4K pin doesn't dominate the screen). The user
/// repositions by dragging anywhere on the body and dismisses via
/// the small `×` in the corner.
fn pin_window_settings(image_size: (u32, u32)) -> window::Settings {
    const MAX_W: f32 = 800.0;
    const MAX_H: f32 = 600.0;
    let (iw, ih) = (image_size.0 as f32, image_size.1 as f32);
    let scale = (MAX_W / iw).min(MAX_H / ih).min(1.0);
    let w = (iw * scale).max(120.0);
    let h = (ih * scale).max(80.0);
    window::Settings {
        size: iced::Size::new(w, h),
        min_size: Some(iced::Size::new(120.0, 80.0)),
        position: window::Position::Default,
        resizable: true,
        decorations: false,
        transparent: false,
        visible: true,
        fullscreen: false,
        level: window::Level::AlwaysOnTop,
        closeable: false,
        minimizable: false,
        ..Default::default()
    }
}

fn overlay_window_settings_for(display: &readshot_capture::DisplayInfo) -> window::Settings {
    let bounds = display.bounds;
    window::Settings {
        size: iced::Size::new(bounds.width(), bounds.height()),
        min_size: None,
        position: window::Position::Specific(iced::Point::new(bounds.x(), bounds.y())),
        resizable: false,
        decorations: false,
        transparent: true,
        visible: true,
        fullscreen: false,
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
        Some(WindowKind::Pin) => "Readshot — Pin".into(),
    }
}

/// Sentinel name for the transparent overlay theme — used by [`style`]
/// to detect overlay windows and return a fully see-through palette.
const OVERLAY_THEME_NAME: &str = "readshot-overlay-transparent";

/// Theme: dark by default. Overlay windows get a custom theme whose
/// `name()` is [`OVERLAY_THEME_NAME`] so [`style`] can identify them
/// and return a transparent base style. The palette colors don't
/// matter for the canvas-only overlay view, so we copy `Theme::Dark`
/// to avoid widget surprises if iced ever consults them.
pub fn theme(state: &App, id: window::Id) -> Theme {
    if matches!(state.windows.kind(id), Some(WindowKind::Overlay)) {
        Theme::custom(OVERLAY_THEME_NAME.to_string(), iced::theme::Palette::DARK)
    } else {
        Theme::Dark
    }
}

/// Per-window appearance. iced's wgpu surface uses
/// `style.background_color` as the *clear* color, so a transparent
/// background here is what actually lets the desktop show through
/// the overlay window. For non-overlay windows we delegate to the
/// theme's normal `Base::base()` so welcome/editor render with their
/// usual dark background.
pub fn style(_state: &App, theme: &Theme) -> iced::theme::Style {
    use iced::theme::Base;
    if theme.name() == OVERLAY_THEME_NAME {
        iced::theme::Style {
            background_color: Color::TRANSPARENT,
            text_color: Color::WHITE,
        }
    } else {
        theme.base()
    }
}

/// Background subscriptions — permission poll + global-hotkey drain
/// + (when the editor is open) ⌘Z / ⌘⇧Z keyboard shortcuts.
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
    if !state.overlay_displays.is_empty() {
        // Marching-ants tick — drives the dash-offset animation on
        // any open region overlay. 80 ms ≈ 12.5 fps which reads as
        // smooth motion without burning CPU.
        subs.push(iced::time::every(Duration::from_millis(80)).map(|_| Message::OverlayTick));
    }
    if state.editor.is_some() {
        // Keyboard sub: ⌘Z / Ctrl+Z = Undo, ⌘⇧Z / Ctrl+Shift+Z = Redo,
        // ⌘S / Ctrl+S = Save. Iced 0.14's `event::listen_with` is the
        // window-agnostic event tap; we filter to KeyPressed events
        // and only react when the editor window is the focused one
        // (the canvas captures key presses at the widget level for
        // Escape; that's why Esc isn't handled here).
        subs.push(iced::event::listen_with(|event, status, _window| {
            use iced::event::Status;
            use iced::keyboard::{Event as KbEvent, Key};
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                let cmd = modifiers.command();
                // ⌘-shortcuts are global to the editor — fire even
                // if a widget already saw the event.
                match (&key, cmd, modifiers.shift()) {
                    (Key::Character(c), true, false) if c.eq_ignore_ascii_case("z") => {
                        return Some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo));
                    }
                    (Key::Character(c), true, true) if c.eq_ignore_ascii_case("z") => {
                        return Some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo));
                    }
                    (Key::Character(c), true, false) if c.eq_ignore_ascii_case("s") => {
                        return Some(Message::EditorSaveRequested);
                    }
                    (Key::Character(c), true, false) if c.eq_ignore_ascii_case("w") => {
                        return Some(Message::EditorDiscardRequested);
                    }
                    (Key::Named(iced::keyboard::key::Named::Escape), _, _) => {
                        return Some(Message::EditorTextCancel);
                    }
                    _ => {}
                }
                // Single-letter shortcuts only fire when no widget
                // has captured the event — i.e. the user isn't
                // typing into the text-input banner.
                if status == Status::Ignored && !cmd && !modifiers.alt() && !modifiers.control() {
                    if let Key::Character(c) = &key {
                        // Tool selection: V/R/O/L/A/P/H/T/B/X/N/C
                        if let Some(t) = tool_for_key(c.as_str()) {
                            return Some(Message::EditorToolbar(
                                readshot_ui::ToolbarMessage::SelectTool(t),
                            ));
                        }
                        // Numeric width: 1..=9 → 1..=9 logical px.
                        if let Some(n) = c.chars().next().and_then(|ch| ch.to_digit(10)) {
                            if (1..=9).contains(&n) {
                                return Some(Message::EditorToolbar(
                                    readshot_ui::ToolbarMessage::SetLineWidth(n as f32),
                                ));
                            }
                        }
                        // [ / ] bump line width by 1 logical px.
                        match c.as_str() {
                            "[" => return Some(Message::EditorWidthBump(-1.0)),
                            "]" => return Some(Message::EditorWidthBump(1.0)),
                            "," => return Some(Message::EditorColorCycle(-1)),
                            "." => return Some(Message::EditorColorCycle(1)),
                            _ => {}
                        }
                    }
                }
            }
            None
        }));
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
            let was_showing = state.welcome.should_show();
            // Reuse the existing synchronous handler — it drives the
            // `WelcomeState` machine without touching iced state.
            state.update_sync(Message::PermissionPoll(status));
            // If we just transitioned out of the welcome state (i.e.
            // permission was granted), close every welcome window and
            // surface a one-time system notification so the user
            // doesn't think the app vanished — it's now in the menu
            // bar.
            if was_showing && !state.welcome.should_show() {
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
                return Task::batch(close_tasks);
            }
            Task::none()
        }

        Message::OverlayTick => {
            // Bump the dash-offset counter and let iced redraw the
            // overlay window(s) — the canvas's draw step reads
            // `state.overlay_tick` indirectly via the OverlayProgram
            // we build in `view`.
            state.overlay_tick = state.overlay_tick.wrapping_add(2);
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
            // List displays asynchronously; the result drives the
            // actual window-open work in `OverlayDisplaysListed` so we
            // can spawn one transparent overlay per monitor.
            let coord = state.coordinator.clone();
            Task::perform(async move { coord.list_displays().await }, |result| {
                Message::OverlayDisplaysListed(result.map_err(|e| e.to_string()))
            })
        }

        Message::OverlayDisplaysListed(Err(e)) => {
            state.last_capture_status =
                Some(format!("Capture failed: could not list displays — {e}"));
            Task::none()
        }
        Message::OverlayDisplaysListed(Ok(displays)) => {
            if displays.is_empty() {
                state.last_capture_status = Some("Capture failed: no displays detected.".into());
                return Task::none();
            }
            // Spawn one borderless transparent overlay per display,
            // positioned at that display's *global* logical origin.
            // We register the (window_id, display) mapping eagerly so
            // `view()` and the OverlaySelected message handler can
            // both look up which monitor a given overlay covers.
            let mut tasks: Vec<Task<Message>> = Vec::with_capacity(displays.len());
            // Fresh overlay session — clear any leftover toolbar state
            // from a previous capture flow.
            state.overlay_selections.clear();
            state.pending_intent = None;
            for d in &displays {
                let settings = overlay_window_settings_for(d);
                let (id, open_task) = window::open(settings);
                state.windows.register(id, WindowKind::Overlay);
                state.overlay_displays.insert(
                    id,
                    crate::app::OverlayDisplay {
                        display_id: d.id.clone(),
                        scale: d.scale,
                        width: d.bounds.width(),
                        height: d.bounds.height(),
                    },
                );
                tasks.push(open_task.map(Message::OverlayWindowReady));
            }
            Task::batch(tasks)
        }

        Message::OverlayWindowReady(_id) => Task::none(),

        Message::OverlaySelected {
            display_id,
            rect,
            intent,
        } => {
            // Close every overlay window — selection on any one of
            // them ends the multi-monitor session — before snapping
            // the screenshot so the dimming veil doesn't show up in
            // the captured pixels.
            state.pending_intent = Some(intent);
            state.overlay_selections.clear();
            let mut tasks = close_all_overlays(state);
            tasks.push(Task::done(Message::CaptureRegionRequested {
                display_id,
                rect,
            }));
            Task::batch(tasks)
        }

        Message::OverlaySelectionChanged { display_id, rect } => {
            match rect {
                Some(r) => {
                    state.overlay_selections.insert(display_id, r);
                }
                None => {
                    state.overlay_selections.remove(&display_id);
                }
            }
            Task::none()
        }

        Message::OverlayCancelled => {
            state.overlay_selections.clear();
            state.pending_intent = None;
            Task::batch(close_all_overlays(state))
        }

        Message::CaptureRegionRequested { display_id, rect } => {
            let coord = state.coordinator.clone();
            state.capture_in_flight = true;
            state.last_capture_status = None;
            // Region capture lands in the editor instead of saving
            // directly — the editor decides what to do with it.
            Task::perform(capture_region_to_image(coord, display_id, rect), |result| {
                Message::RegionCaptureCompleted(result.map_err(|e| e.to_string()))
            })
        }

        Message::RegionCaptureCompleted(result) => {
            state.capture_in_flight = false;
            let intent = state
                .pending_intent
                .take()
                .unwrap_or(crate::app::CaptureIntent::Editor);
            match result {
                Ok(image) => match intent {
                    crate::app::CaptureIntent::Editor => {
                        state.editor = Some(crate::editor::EditorSession::new(image));
                        let (id, open_task) = window::open(editor_window_settings());
                        state.windows.register(id, WindowKind::Editor);
                        open_task.map(Message::EditorWindowReady)
                    }
                    crate::app::CaptureIntent::CopyToClipboard => {
                        Task::perform(copy_image_to_clipboard(image), |r| {
                            Message::OverlayCopyDone(r.map_err(|e| e.to_string()))
                        })
                    }
                    crate::app::CaptureIntent::CopyTextDirect => {
                        let coord = state.coordinator.clone();
                        Task::perform(ocr_then_copy(coord, image), |r| {
                            Message::OverlayCopyTextDone(r.map_err(|e| e.to_string()))
                        })
                    }
                    crate::app::CaptureIntent::SaveDirect => {
                        let seed = state.last_save_dir.clone();
                        Task::perform(save_image_via_picker(image, seed), |r| {
                            Message::OverlaySaveDone(r.map_err(|e| e.to_string()))
                        })
                    }
                    crate::app::CaptureIntent::Pin => {
                        let size = (image.width(), image.height());
                        let handle = iced::widget::image::Handle::from_rgba(
                            image.width(),
                            image.height(),
                            image.as_raw().clone(),
                        );
                        let (id, open_task) = window::open(pin_window_settings(size));
                        state.windows.register(id, WindowKind::Pin);
                        state.pins.insert(id, handle.clone());
                        open_task.map(move |opened| Message::PinWindowReady(opened, handle.clone()))
                    }
                },
                Err(e) => {
                    state.last_capture_status = Some(format!("Capture failed: {e}"));
                    Task::none()
                }
            }
        }

        Message::OverlayCopyDone(result) => {
            state.last_capture_status = Some(match result {
                Ok(()) => "Copied to clipboard.".into(),
                Err(e) => format!("Copy failed: {e}"),
            });
            Task::none()
        }
        Message::OverlaySaveDone(result) => {
            if let Ok(Some(path)) = &result {
                if let Some(parent) = path.parent() {
                    state.last_save_dir = Some(parent.to_path_buf());
                }
            }
            state.last_capture_status = Some(match result {
                Ok(Some(p)) => format!("Saved to {}", p.display()),
                Ok(None) => "Save cancelled.".into(),
                Err(e) => format!("Save failed: {e}"),
            });
            Task::none()
        }
        Message::OverlayCopyTextDone(result) => {
            state.last_capture_status = Some(match result {
                Ok(text) if text.is_empty() => "No text recognised.".into(),
                Ok(text) => {
                    let n = text.chars().count();
                    format!(
                        "Copied {n} character{} of text.",
                        if n == 1 { "" } else { "s" }
                    )
                }
                Err(e) => format!("Copy text failed: {e}"),
            });
            Task::none()
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
            ed.status = Some("Choose a save location…".into());
            let img = ed.model.flatten();
            let seed = state.last_save_dir.clone();
            Task::perform(save_image_via_picker(img, seed), |r| {
                Message::EditorSaved(r.map_err(|e| e.to_string()))
            })
        }
        Message::EditorSaved(result) => {
            // Remember the parent directory of any successful save —
            // next save's picker will seed itself there instead of
            // bouncing back to ~/Desktop.
            if let Ok(Some(path)) = &result {
                if let Some(parent) = path.parent() {
                    state.last_save_dir = Some(parent.to_path_buf());
                }
            }
            if let Some(ed) = state.editor.as_mut() {
                ed.busy = false;
                ed.status = Some(match result {
                    Ok(Some(p)) => format!("Saved to {}", p.display()),
                    Ok(None) => "Save cancelled.".into(),
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
            let img = ed.model.flatten();
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
            let img = ed.model.flatten();
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

        // Toolbar selections — tool / colour / line-width changes
        // and undo/redo. Tool/colour/width changes don't need a
        // re-render (annotations haven't changed), but undo/redo do.
        Message::EditorToolbar(msg) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            match msg {
                readshot_ui::ToolbarMessage::SelectTool(t) => ed.model.set_tool(t),
                readshot_ui::ToolbarMessage::SelectColor(c) => ed.model.set_color(c),
                readshot_ui::ToolbarMessage::SetLineWidth(w) => ed.model.set_line_width(w),
                readshot_ui::ToolbarMessage::Undo => {
                    if ed.model.undo() {
                        ed.refresh_image();
                    }
                }
                readshot_ui::ToolbarMessage::Redo => {
                    if ed.model.redo() {
                        ed.refresh_image();
                    }
                }
            }
            Task::none()
        }

        // Canvas events — mostly drag previews (which don't need
        // their own state mutation in this cut; the canvas's internal
        // DrawState already drives the live painting) and one-shot
        // commits which mutate the model.
        Message::EditorCanvas(msg) => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            match msg {
                readshot_ui::CanvasMessage::DragStarted
                | readshot_ui::CanvasMessage::DragMoved(_)
                | readshot_ui::CanvasMessage::PolylineMoved(_)
                | readshot_ui::CanvasMessage::Cancelled => {
                    // Preview-only events. The canvas's own State holds
                    // the drag points; a redraw is automatic.
                }
                readshot_ui::CanvasMessage::RequestText(p) => {
                    // Text tool clicked — open the inline text-input
                    // banner. The eventual Annotation::Text lands at
                    // exactly the click point regardless of how long
                    // the user takes to type.
                    ed.pending_text = Some(crate::editor::PendingText {
                        origin: p,
                        content: String::new(),
                    });
                }
                readshot_ui::CanvasMessage::CommitAnnotation(annotation) => {
                    handle_commit_annotation(ed, annotation);
                }
            }
            Task::none()
        }

        Message::EditorTextChanged(content) => {
            if let Some(ed) = state.editor.as_mut() {
                if let Some(pending) = ed.pending_text.as_mut() {
                    pending.content = content;
                }
            }
            Task::none()
        }
        Message::EditorTextCommit => {
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            if let Some(pending) = ed.pending_text.take() {
                let trimmed = pending.content.trim();
                if !trimmed.is_empty() {
                    let annotation = readshot_core::Annotation::Text {
                        content: trimmed.to_string(),
                        origin: pending.origin,
                        color: ed.model.current_color(),
                        font_family: "system-ui".to_string(),
                        // Tie text size to the line-width slider so
                        // it's discoverable without a separate control.
                        size: text_size_from_line_width(ed.model.current_line_width()),
                    };
                    ed.model.commit_annotation(annotation);
                    ed.refresh_image();
                }
            }
            Task::none()
        }
        Message::EditorTextCancel => {
            if let Some(ed) = state.editor.as_mut() {
                ed.pending_text = None;
            }
            Task::none()
        }
        Message::EditorWidthBump(delta) => {
            if let Some(ed) = state.editor.as_mut() {
                let next = ed.model.current_line_width() + delta;
                ed.model.set_line_width(next);
            }
            Task::none()
        }
        Message::EditorColorCycle(dir) => {
            if let Some(ed) = state.editor.as_mut() {
                let palette = readshot_ui::editor::toolbar::PALETTE;
                let current = ed.model.current_color();
                let idx = palette
                    .iter()
                    .position(|c| swatch_eq(*c, current))
                    .unwrap_or(0) as i32;
                let len = palette.len() as i32;
                let next = ((idx + dir).rem_euclid(len)) as usize;
                ed.model.set_color(palette[next]);
            }
            Task::none()
        }

        Message::EditorPinRequested => {
            // Snapshot the editor's currently-flattened image, open
            // a fresh pin window with that image, then close the
            // editor. The pin lives independently from there on.
            let Some(ed) = state.editor.as_mut() else {
                return Task::none();
            };
            let img = ed.model.flatten();
            let size = (img.width(), img.height());
            let handle = iced::widget::image::Handle::from_rgba(
                img.width(),
                img.height(),
                img.as_raw().clone(),
            );
            // Close the editor window if any.
            let editor_id = ed.window_id;
            state.editor = None;
            let mut tasks: Vec<Task<Message>> = Vec::new();
            if let Some(id) = editor_id {
                state.windows.forget(id);
                tasks.push(window::close(id));
            }
            // Open the pin window. We register both kind + image
            // handle eagerly so the first `view` call paints the
            // pin instead of the "(no capture)" fallback.
            let (id, open_task) = window::open(pin_window_settings(size));
            state.windows.register(id, WindowKind::Pin);
            state.pins.insert(id, handle.clone());
            tasks
                .push(open_task.map(move |opened| Message::PinWindowReady(opened, handle.clone())));
            Task::batch(tasks)
        }
        Message::PinWindowReady(id, handle) => {
            // The eager insert above already covers most cases; this
            // re-key handles the (rare) scenario where the iced
            // runtime hands us a different id than the one returned
            // by `window::open` synchronously. Idempotent insert.
            state.pins.entry(id).or_insert(handle);
            Task::none()
        }
        Message::PinClosePressed(id) => {
            state.windows.forget(id);
            state.pins.remove(&id);
            window::close(id)
        }
        Message::PinDragRequested(id) => window::drag(id),

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
            // restart. We spawn a fresh Launch Services launch and
            // hard-exit so the daemon can't get into a half-shutdown
            // race (`iced::exit()` returns a Task that may not have
            // finished by the time the new instance comes up).
            #[cfg(target_os = "macos")]
            {
                match relaunch_via_launch_services() {
                    Ok(()) => {
                        tracing::info!(target: "readshot::permissions", "respawned via Launch Services; exiting");
                        // Tiny sleep so `open -n` definitely got the
                        // request through before this PID disappears.
                        std::thread::sleep(Duration::from_millis(150));
                        std::process::exit(0);
                    }
                    Err(e) => {
                        tracing::warn!(target: "readshot::permissions", "relaunch failed: {e}");
                    }
                }
            }
            iced::exit()
        }

        // GrantPermissionRequested: register the bundle with TCC,
        // then *also* open System Settings on macOS. For ad-hoc
        // signed apps the system prompt typically doesn't fire, so
        // chaining the deep-link is what gets users unstuck.
        Message::GrantPermissionRequested => {
            state.update_sync(Message::GrantPermissionRequested);
            state.permissions.open_settings();
            Task::none()
        }

        // Synchronous transitions — reuse the existing handler.
        msg @ (Message::PermissionPoll(_)
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
        Some(WindowKind::Overlay) => overlay_view(state, id),
        Some(WindowKind::Editor) => editor_view(state),
        Some(WindowKind::Pin) => pin_view(state, id),
    }
}

fn editor_view(state: &App) -> Element<'_, Message> {
    use iced::widget::canvas::Canvas;
    use iced::widget::{stack, tooltip, Space as IcedSpace};
    use readshot_ui::editor::{canvas::EditorCanvas, toolbar, ToolState};

    let Some(ed) = state.editor.as_ref() else {
        return container(text("(no capture)"))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };

    let active_tool = ed.model.active_tool();
    let active_color = ed.model.current_color();
    let line_width = ed.model.current_line_width();
    let busy = ed.busy;

    // ===== Toolbar — slim, icon-only =====
    // Tools are grouped: Select | shapes | freehand | effects | pin |
    // crop. Each button is a 32×32 square wrapped in a tooltip that
    // shows the long name + keyboard shortcut, so the visible
    // toolbar can stay compact without losing affordance.
    let tool_groups: &[&[ToolState]] = &[
        &[ToolState::Select],
        &[
            ToolState::Rectangle,
            ToolState::Ellipse,
            ToolState::Line,
            ToolState::Arrow,
        ],
        &[ToolState::Pen, ToolState::Highlighter, ToolState::Text],
        &[ToolState::Blur, ToolState::Pixelate],
        &[ToolState::NumberedPin],
        &[ToolState::Crop],
    ];

    let mut tool_row = row![].spacing(3).align_y(Alignment::Center);
    for (idx, group) in tool_groups.iter().enumerate() {
        if idx > 0 {
            tool_row = tool_row.push(toolbar_divider());
        }
        for t in group.iter() {
            tool_row = tool_row.push(tool_button(*t, active_tool, busy));
        }
    }

    // ===== Color palette — 26×26 swatches =====
    let palette_row = toolbar::PALETTE.iter().fold(
        row![].spacing(5).align_y(Alignment::Center),
        |row, swatch| {
            let is_selected = swatch_eq(*swatch, active_color);
            let color = Color::from_rgba(swatch.r, swatch.g, swatch.b, swatch.a);
            let mut btn = button(
                IcedSpace::new()
                    .width(Length::Fixed(20.0))
                    .height(Length::Fixed(20.0)),
            )
            .padding(0)
            .style(move |_theme, status| {
                let border = if is_selected {
                    iced::Border {
                        color: Color::WHITE,
                        width: 2.5,
                        radius: 6.0.into(),
                    }
                } else if matches!(status, button::Status::Hovered) {
                    iced::Border {
                        color: Color::from_rgba(1.0, 1.0, 1.0, 0.55),
                        width: 1.5,
                        radius: 6.0.into(),
                    }
                } else {
                    iced::Border {
                        color: Color::from_rgba(1.0, 1.0, 1.0, 0.18),
                        width: 1.0,
                        radius: 6.0.into(),
                    }
                };
                button::Style {
                    background: Some(color.into()),
                    text_color: Color::TRANSPARENT,
                    border,
                    ..Default::default()
                }
            });
            if !busy {
                btn = btn.on_press(Message::EditorToolbar(
                    readshot_ui::ToolbarMessage::SelectColor(*swatch),
                ));
            }
            row.push(btn)
        },
    );

    let width_label = text(format!("{line_width:.0}px"))
        .size(11)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7))
        .width(Length::Fixed(34.0));
    let width_slider = iced::widget::slider(
        toolbar::MIN_LINE_WIDTH..=toolbar::MAX_LINE_WIDTH,
        line_width,
        |v| Message::EditorToolbar(readshot_ui::ToolbarMessage::SetLineWidth(v)),
    )
    .step(0.5)
    .width(Length::Fixed(140.0));

    let undo_btn = ghost_icon_button("↶", "Undo (⌘Z)", !busy && ed.model.can_undo(), || {
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo)
    });
    let redo_btn = ghost_icon_button(
        "↷",
        "Redo (⌘⇧Z)",
        !busy && ed.model.can_redo(),
        || Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
    );

    // Single combined toolbar: tools | divider | colors | width |
    // spacer | undo redo. Wraps gracefully if the window narrows by
    // staying horizontally scrollable in spirit (we let iced handle
    // overflow; in practice 1100px fits everything).
    let toolbar_inner = row![
        tool_row,
        toolbar_divider(),
        palette_row,
        toolbar_divider(),
        width_label,
        width_slider,
        IcedSpace::new().width(Length::Fill),
        undo_btn,
        redo_btn,
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .padding([6, 10]);

    let toolbar_row = container(toolbar_inner).style(|theme: &Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.weak.color.into()),
            border: iced::Border {
                color: palette.background.strong.color,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        }
    });

    // ===== Image area — letterboxed image with canvas overlay =====
    // Both layers share the same container; the canvas Program knows
    // the image's effective dimensions + crop offset so cursor maps
    // to base-image coordinates regardless of zoom / letterbox /
    // crop. See `canvas_to_base` in readshot-ui.
    let canvas_program = EditorCanvas {
        active_tool,
        color: active_color,
        line_width,
        next_pin_number: ed.next_pin_number,
        image_size: ed.effective_image_size(),
        image_offset: ed.crop_offset(),
    };
    let canvas: Element<'_, readshot_ui::CanvasMessage> = Canvas::new(canvas_program)
        .width(Length::Fill)
        .height(Length::Fill)
        .into();
    let canvas: Element<'_, Message> = canvas.map(Message::EditorCanvas);

    let image_layer = container(
        iced::widget::image(ed.image_handle.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(iced::ContentFit::Contain),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .center_x(Length::Fill)
    .center_y(Length::Fill);
    let canvas_layer = container(canvas).width(Length::Fill).height(Length::Fill);
    let image_area = container(stack![image_layer, canvas_layer])
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(0)
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                // Subtle inset rather than the previous near-black
                // bezel — lets the actual image be the visual focus.
                background: Some(Color::from_rgba(0.13, 0.14, 0.18, 1.0).into()),
                border: iced::Border {
                    color: palette.background.strong.color,
                    width: 1.0,
                    radius: 10.0.into(),
                },
                ..Default::default()
            }
        });

    // ===== Bottom row — dims/hint + actions in one strip =====
    let action_btn = |label: &'static str, msg: Message, kind: ActionKind| {
        let lbl = text(label).size(13).color(Color::WHITE);
        let mut b = button(lbl)
            .padding([8, 16])
            .style(move |theme, status| action_button_style(theme, status, kind));
        if !busy {
            b = b.on_press(msg);
        }
        b
    };

    let (img_w, img_h) = ed.effective_image_size();
    let dims = text(format!("{img_w} × {img_h} px"))
        .size(11)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55));
    let bullet = text("·")
        .size(11)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.35));
    // When the user just opened the editor and hasn't drawn anything
    // yet, show a discoverable "press a key to pick a tool" hint
    // in place of the per-tool guidance — the keyboard shortcuts
    // aren't visible anywhere in the chrome until the user hovers
    // a tool button, so this is the surface that surfaces them.
    let hint_str = if ed.model.annotations().is_empty()
        && active_tool == ToolState::Select
        && ed.status.is_none()
    {
        "Press R / O / L / A / P / H / T / B / X / N / C to pick a tool · ⌘Z to undo"
    } else {
        tool_hint(active_tool)
    };
    let hint = text(hint_str)
        .size(11)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.72));
    let toast: Element<'_, Message> = match ed.status.as_deref() {
        Some(s) => text(s)
            .size(11)
            .color(Color::from_rgba(0.65, 0.95, 0.75, 1.0))
            .into(),
        None => IcedSpace::new().height(Length::Fixed(0.0)).into(),
    };

    let bottom_row = row![
        dims,
        IcedSpace::new().width(Length::Fixed(8.0)),
        bullet,
        IcedSpace::new().width(Length::Fixed(8.0)),
        hint,
        IcedSpace::new().width(Length::Fixed(8.0)),
        toast,
        IcedSpace::new().width(Length::Fill),
        action_btn(
            "Discard",
            Message::EditorDiscardRequested,
            ActionKind::Danger,
        ),
        IcedSpace::new().width(Length::Fixed(8.0)),
        action_btn("Pin", Message::EditorPinRequested, ActionKind::Secondary,),
        action_btn(
            "Copy Text",
            Message::EditorCopyTextRequested,
            ActionKind::Secondary,
        ),
        action_btn(
            "Copy",
            Message::EditorCopyImageRequested,
            ActionKind::Secondary,
        ),
        action_btn("Save", Message::EditorSaveRequested, ActionKind::Primary,),
    ]
    .spacing(6)
    .align_y(Alignment::Center);

    // ===== Text-input banner =====
    let text_banner: Element<'_, Message> = if let Some(pending) = ed.pending_text.as_ref() {
        let input = iced::widget::text_input("Type and press Enter…", &pending.content)
            .on_input(Message::EditorTextChanged)
            .on_submit(Message::EditorTextCommit)
            .padding(8)
            .size(14)
            .width(Length::Fill);
        let commit = button(text("Add Text").size(13).color(Color::WHITE))
            .padding([8, 14])
            .style(|theme, status| action_button_style(theme, status, ActionKind::Primary))
            .on_press(Message::EditorTextCommit);
        let cancel = button(text("Cancel").size(13).color(Color::WHITE))
            .padding([8, 14])
            .style(|theme, status| action_button_style(theme, status, ActionKind::Secondary))
            .on_press(Message::EditorTextCancel);
        container(
            row![
                text("T").size(13).color(Color::WHITE),
                input,
                commit,
                cancel,
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .padding(6),
        )
        .style(|theme: &Theme| {
            let palette = theme.extended_palette();
            container::Style {
                background: Some(palette.background.weak.color.into()),
                border: iced::Border {
                    color: palette.primary.base.color,
                    width: 1.5,
                    radius: 10.0.into(),
                },
                ..Default::default()
            }
        })
        .into()
    } else {
        IcedSpace::new().height(Length::Fixed(0.0)).into()
    };

    let _ = tooltip::Position::Bottom; // kept for future direct uses
    container(
        column![toolbar_row, image_area, text_banner, bottom_row]
            .spacing(8)
            .padding(10)
            .align_x(Alignment::Start),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

/// Render a pin window — borderless, always-on-top, draggable.
/// Click anywhere on the image body initiates a native window
/// drag; a small "×" button in the corner closes it.
fn pin_view(state: &App, id: window::Id) -> Element<'_, Message> {
    use iced::widget::{mouse_area, stack};
    let Some(handle) = state.pins.get(&id) else {
        return container(text("(no pin)"))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };
    let img = iced::widget::image(handle.clone())
        .width(Length::Fill)
        .height(Length::Fill)
        .content_fit(iced::ContentFit::Contain);
    let drag_layer: Element<'_, Message> = mouse_area(img)
        .on_press(Message::PinDragRequested(id))
        .on_double_click(Message::PinClosePressed(id))
        .interaction(iced::mouse::Interaction::Grab)
        .into();
    // Close button — sits in the top-right corner with subtle
    // styling so it's discoverable without dominating the pin.
    let close = button(
        text("×")
            .size(16)
            .color(Color::WHITE)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(0)
    .width(Length::Fixed(22.0))
    .height(Length::Fixed(22.0))
    .style(|_, status| {
        let bg = match status {
            button::Status::Hovered => Color::from_rgba(0.85, 0.25, 0.25, 0.95),
            _ => Color::from_rgba(0.0, 0.0, 0.0, 0.55),
        };
        button::Style {
            background: Some(bg.into()),
            text_color: Color::WHITE,
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.4),
                width: 1.0,
                radius: 11.0.into(),
            },
            ..Default::default()
        }
    })
    .on_press(Message::PinClosePressed(id));
    let close_layer = container(close)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(6)
        .align_x(Alignment::End)
        .align_y(Alignment::Start);
    container(stack![drag_layer, close_layer])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.05, 0.05, 0.06, 1.0).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.18),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        })
        .into()
}

/// One toolbar tool button with a hover tooltip + keyboard hint.
fn tool_button<'a>(
    t: readshot_ui::editor::ToolState,
    active: readshot_ui::editor::ToolState,
    busy: bool,
) -> Element<'a, Message> {
    use iced::widget::tooltip;
    let glyph = tool_glyph(t);
    let (long, key) = tool_label_and_key(t);
    let is_active = t == active;
    let mut b = button(
        text(glyph)
            .size(15)
            .color(Color::WHITE)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(0)
    .width(Length::Fixed(32.0))
    .height(Length::Fixed(32.0))
    .style(move |theme: &Theme, status| toolbar_button_style(theme, status, is_active));
    if !busy {
        b = b.on_press(Message::EditorToolbar(
            readshot_ui::ToolbarMessage::SelectTool(t),
        ));
    }
    let tip = container(text(format!("{long}  {key}")).size(11).color(Color::WHITE))
        .padding([4, 8])
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.9).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        });
    tooltip::Tooltip::new(b, tip, tooltip::Position::Bottom)
        .gap(4)
        .into()
}

/// Compact ghost-style action button used for Undo / Redo. `gen`
/// produces the message lazily so we only build it when enabled.
fn ghost_icon_button<'a, F>(
    glyph: &'static str,
    tip: &'static str,
    enabled: bool,
    gen: F,
) -> Element<'a, Message>
where
    F: Fn() -> Message + 'a,
{
    use iced::widget::tooltip;
    let mut b = button(
        text(glyph)
            .size(15)
            .color(Color::WHITE)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(0)
    .width(Length::Fixed(32.0))
    .height(Length::Fixed(32.0))
    .style(move |theme: &Theme, status| toolbar_ghost_style(theme, status, enabled));
    if enabled {
        b = b.on_press(gen());
    }
    let pop = container(text(tip).size(11).color(Color::WHITE))
        .padding([4, 8])
        .style(|_| container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.9).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.15),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        });
    tooltip::Tooltip::new(b, pop, tooltip::Position::Bottom)
        .gap(4)
        .into()
}

/// 1px vertical divider between toolbar groups.
fn toolbar_divider() -> Element<'static, Message> {
    container(iced::widget::Space::new())
        .width(Length::Fixed(1.0))
        .height(Length::Fixed(20.0))
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.12).into()),
            ..Default::default()
        })
        .into()
}

fn toolbar_button_style(theme: &Theme, status: button::Status, is_active: bool) -> button::Style {
    let palette = theme.extended_palette();
    let base = if is_active {
        palette.primary.strong.color
    } else if matches!(status, button::Status::Hovered) {
        palette.background.strongest.color
    } else {
        Color::TRANSPARENT
    };
    button::Style {
        background: Some(base.into()),
        text_color: Color::WHITE,
        border: iced::Border {
            radius: 6.0.into(),
            width: if is_active { 0.0 } else { 1.0 },
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
        },
        ..Default::default()
    }
}

fn toolbar_ghost_style(_theme: &Theme, status: button::Status, enabled: bool) -> button::Style {
    let bg = match (enabled, status) {
        (true, button::Status::Hovered) => Color::from_rgba(1.0, 1.0, 1.0, 0.10),
        _ => Color::TRANSPARENT,
    };
    button::Style {
        background: Some(bg.into()),
        text_color: if enabled {
            Color::WHITE
        } else {
            Color::from_rgba(1.0, 1.0, 1.0, 0.35)
        },
        border: iced::Border {
            radius: 6.0.into(),
            width: 1.0,
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
        },
        ..Default::default()
    }
}

#[derive(Clone, Copy)]
enum ActionKind {
    Primary,
    Secondary,
    Danger,
}

fn action_button_style(theme: &Theme, status: button::Status, kind: ActionKind) -> button::Style {
    let palette = theme.extended_palette();
    let (base, hover, text_color) = match kind {
        ActionKind::Primary => (
            palette.primary.base.color,
            palette.primary.strong.color,
            palette.primary.base.text,
        ),
        ActionKind::Secondary => (
            palette.background.strong.color,
            palette.background.strongest.color,
            Color::WHITE,
        ),
        ActionKind::Danger => (
            Color::from_rgba(0.50, 0.13, 0.16, 1.0),
            Color::from_rgba(0.65, 0.18, 0.22, 1.0),
            Color::WHITE,
        ),
    };
    let bg = match status {
        button::Status::Hovered => hover,
        _ => base,
    };
    button::Style {
        background: Some(bg.into()),
        text_color,
        border: iced::Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// One-glyph icon for each tool — used inside the 32×32 square
/// toolbar buttons. The full name + keyboard shortcut live in the
/// hover tooltip via [`tool_label_and_key`].
fn tool_glyph(tool: readshot_ui::editor::ToolState) -> &'static str {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => "↖",
        T::Rectangle => "▭",
        T::Ellipse => "◯",
        T::Line => "／",
        T::Arrow => "→",
        T::Pen => "✎",
        T::Highlighter => "▰",
        T::Text => "T",
        T::Blur => "◈",
        T::Pixelate => "▦",
        T::NumberedPin => "①",
        T::Crop => "⬚",
    }
}

/// Long name + keyboard shortcut shown in the tooltip when the user
/// hovers a tool button. Keys mirror common screenshot-editor muscle
/// memory (V/R/O/L/A/P/H/T/B/X/N/C).
fn tool_label_and_key(tool: readshot_ui::editor::ToolState) -> (&'static str, &'static str) {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => ("Select", "V"),
        T::Rectangle => ("Rectangle", "R"),
        T::Ellipse => ("Ellipse", "O"),
        T::Line => ("Line", "L"),
        T::Arrow => ("Arrow", "A"),
        T::Pen => ("Pen", "P"),
        T::Highlighter => ("Highlighter", "H"),
        T::Text => ("Text", "T"),
        T::Blur => ("Blur", "B"),
        T::Pixelate => ("Pixelate", "X"),
        T::NumberedPin => ("Numbered pin", "N"),
        T::Crop => ("Crop", "C"),
    }
}

/// Map a single character to a tool. Used by the editor keyboard
/// subscription so muscle-memory shortcuts work without modifiers.
fn tool_for_key(c: &str) -> Option<readshot_ui::editor::ToolState> {
    use readshot_ui::editor::ToolState as T;
    match c.to_ascii_lowercase().as_str() {
        "v" => Some(T::Select),
        "r" => Some(T::Rectangle),
        "o" => Some(T::Ellipse),
        "l" => Some(T::Line),
        "a" => Some(T::Arrow),
        "p" => Some(T::Pen),
        "h" => Some(T::Highlighter),
        "t" => Some(T::Text),
        "b" => Some(T::Blur),
        "x" => Some(T::Pixelate),
        "n" => Some(T::NumberedPin),
        "c" => Some(T::Crop),
        _ => None,
    }
}

/// One-line guidance for the currently active tool — replaces the
/// blank slate the user used to face when they didn't know what
/// click would do what.
fn tool_hint(tool: readshot_ui::editor::ToolState) -> &'static str {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => "Select tool — drag tools commit on release. ⌘Z undoes, ⌘⇧Z redoes.",
        T::Rectangle => "Rectangle — drag to outline a region.",
        T::Ellipse => "Ellipse — drag the bounding box.",
        T::Line => "Line — drag from start to end.",
        T::Arrow => "Arrow — drag from base toward the target.",
        T::Pen => "Pen — drag to free-draw.",
        T::Highlighter => "Highlighter — drag over text; semi-transparent.",
        T::Text => "Text — click to place; type and press Enter to commit.",
        T::Blur => "Blur — drag a region; radius scales with width.",
        T::Pixelate => "Pixelate — drag a region; block size scales with width.",
        T::NumberedPin => "Numbered pin — click to drop the next number.",
        T::Crop => "Crop — drag to keep only that region.",
    }
}

fn swatch_eq(a: readshot_core::Rgba, b: readshot_core::Rgba) -> bool {
    (a.r - b.r).abs() < 1e-3
        && (a.g - b.g).abs() < 1e-3
        && (a.b - b.b).abs() < 1e-3
        && (a.a - b.a).abs() < 1e-3
}

fn overlay_view(state: &App, id: window::Id) -> Element<'_, Message> {
    use iced::widget::canvas::Canvas;
    use iced::widget::stack;

    // Each overlay window's canvas needs to know which display it
    // covers so the resulting `OverlaySelected` message routes the
    // capture to the right monitor. Fall back to an empty id only as
    // a defence against a view() call before OpenOverlayRequested
    // populated the map — that path won't actually publish a useful
    // message, but it avoids an unwrap.
    let overlay_record = state.overlay_displays.get(&id);
    let display_id = overlay_record
        .map(|d| d.display_id.clone())
        .unwrap_or_default();
    let scale = overlay_record.map(|d| d.scale).unwrap_or(1.0);

    let canvas = Canvas::new(crate::overlay::OverlayProgram {
        display_id,
        // Multiply the runtime tick to advance the dash pattern
        // smoothly (each dash period is ~10 logical px).
        dash_offset: state.overlay_tick as usize,
        scale,
    })
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
        text("Drag to select · hold Shift for square · Enter for full screen · Esc to cancel")
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

    // Floating action toolbar — only when this display has a
    // committed selection. Layered above the canvas so its buttons
    // intercept clicks before the overlay's "click outside =
    // restart drag" path sees them.
    let toolbar_layer = overlay_record
        .and_then(|d| {
            state
                .overlay_selections
                .get(&d.display_id)
                .map(|rect| (d, rect))
        })
        .map(|(d, rect)| overlay_toolbar_layer(&d.display_id, rect, d.width, d.height));

    // Once the toolbar is up the introductory hint is just noise.
    if let Some(toolbar) = toolbar_layer {
        stack![canvas_layer, toolbar].into()
    } else {
        stack![canvas_layer, hint_layer].into()
    }
}

/// Estimated visual size of the floating overlay toolbar. Used for
/// edge-aware reflow without measuring real layout (which iced
/// doesn't expose mid-build).
const OVERLAY_TOOLBAR_HEIGHT: f32 = 44.0;
const OVERLAY_TOOLBAR_GAP: f32 = 8.0;

/// Build a positioned action toolbar (Capture / Copy / Save / Pin /
/// Cancel) anchored to the right edge of `rect`. Falls back to
/// "above" then "inside" if "below" would clip the window.
fn overlay_toolbar_layer<'a>(
    display_id: &readshot_capture::DisplayId,
    rect: &readshot_core::geom::Rect,
    bounds_w: f32,
    bounds_h: f32,
) -> Element<'a, Message> {
    use crate::app::CaptureIntent;

    let make_btn = |label: &'static str, msg: Message| {
        button(text(label).size(13).color(Color::WHITE))
            .padding([6, 10])
            .style(|_, status| {
                let base = Color::from_rgba(1.0, 1.0, 1.0, 0.0);
                let hovered = Color::from_rgba(1.0, 1.0, 1.0, 0.12);
                let pressed = Color::from_rgba(1.0, 1.0, 1.0, 0.22);
                let bg = match status {
                    iced::widget::button::Status::Hovered => hovered,
                    iced::widget::button::Status::Pressed => pressed,
                    _ => base,
                };
                iced::widget::button::Style {
                    background: Some(bg.into()),
                    text_color: Color::WHITE,
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            })
            .on_press(msg)
    };

    let buttons = row![
        make_btn(
            "✓ Capture",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::Editor,
            },
        ),
        make_btn(
            "Copy Image",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::CopyToClipboard,
            },
        ),
        make_btn(
            "Copy Text",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::CopyTextDirect,
            },
        ),
        make_btn(
            "Save",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::SaveDirect,
            },
        ),
        make_btn(
            "Pin",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::Pin,
            },
        ),
        make_btn("✕", Message::OverlayCancelled),
    ]
    .spacing(4)
    .align_y(Alignment::Center);

    let bar = container(buttons)
        .padding(6)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.78).into()),
            border: iced::Border {
                radius: 8.0.into(),
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.18),
                width: 1.0,
            },
            ..Default::default()
        });

    let sel_x = rect.x();
    let sel_y = rect.y();
    let sel_w = rect.width();
    let sel_h = rect.height();
    let below_y = sel_y + sel_h + OVERLAY_TOOLBAR_GAP;
    let above_y = sel_y - OVERLAY_TOOLBAR_GAP - OVERLAY_TOOLBAR_HEIGHT;
    let toolbar_y = if below_y + OVERLAY_TOOLBAR_HEIGHT <= bounds_h {
        below_y
    } else if above_y >= 0.0 {
        above_y
    } else {
        // Both placements clip — fall back to inside the selection.
        sel_y + OVERLAY_TOOLBAR_GAP
    };

    let right_pad = (bounds_w - (sel_x + sel_w)).max(0.0);

    container(bar)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(iced::Padding {
            top: toolbar_y.max(0.0),
            right: right_pad,
            bottom: 0.0,
            left: 0.0,
        })
        .align_x(Alignment::End)
        .align_y(Alignment::Start)
        .into()
}

fn welcome_view(state: &App) -> Element<'_, Message> {
    let status_line = text(permission_blurb(state.welcome));
    let action: Element<'_, Message> = match state.welcome {
        WelcomeState::Granted => {
            row![text("Screen Recording is granted. You're all set.").width(Length::Fill),].into()
        }
        // Pending: user hasn't asked us to register yet. We collapse
        // CGRequestScreenCaptureAccess + open_settings into a single
        // primary button so there is only one thing to click. Ad-hoc
        // builds get no system prompt anyway, so registering and
        // deep-linking together is the lowest-friction path.
        WelcomeState::Pending => column![
            text("Step 1 of 2 — Enable Readshot in System Settings")
                .size(15)
                .width(Length::Fill),
            text(
                "Click the button below. macOS will register Readshot \
                 and open Privacy & Security → Screen Recording. Toggle \
                 Readshot ON, then come back here for step 2."
            )
            .size(12)
            .width(Length::Fill),
            button(text("Open System Settings").size(14))
                .padding(10)
                .on_press(Message::GrantPermissionRequested),
        ]
        .spacing(10)
        .into(),
        // After the user clicked the Pending button OR after we
        // observed Denied (still no grant in this process). Both end
        // up here so the user sees one consistent next step.
        WelcomeState::AwaitingGrant | WelcomeState::Denied => column![
            text("Step 2 of 2 — Restart Readshot")
                .size(15)
                .width(Length::Fill),
            text(
                "Once Readshot is toggled ON in System Settings, click \
                 Restart Readshot now. macOS only honours new Screen \
                 Recording grants on a fresh launch — this app will \
                 relaunch itself for you."
            )
            .size(12)
            .width(Length::Fill),
            row![
                button(text("Restart Readshot now").size(14))
                    .padding(10)
                    .on_press(Message::RestartRequested),
                button(text("Open Settings again").size(14))
                    .padding(10)
                    .on_press(Message::OpenPermissionSettingsRequested),
            ]
            .spacing(8),
        ]
        .spacing(10)
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
        text("Drag · hold Shift for square · Enter for full screen · Esc to cancel").size(11),
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
async fn capture_primary_to_desktop(coord: CaptureCoordinator) -> Result<PathBuf, CaptureRunError> {
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
///
/// Looks up the named display so the request carries its real HiDPI
/// scale; without that, the macOS backend would render at half
/// resolution on Retina monitors.
async fn capture_region_to_image(
    coord: CaptureCoordinator,
    display_id: readshot_capture::DisplayId,
    rect: readshot_core::geom::Rect,
) -> Result<image::RgbaImage, CaptureRunError> {
    let displays = coord.list_displays().await?;
    let display = displays
        .iter()
        .find(|d| d.id == display_id)
        .ok_or(CaptureRunError::NoDisplays)?;
    let req = CaptureRequest {
        display_id: display.id.clone(),
        rect,
        scale: display.scale,
        hide_cursor: true,
    };
    Ok(coord.capture_region(req).await?)
}

/// Walks every registered overlay window, returns close tasks for
/// each, and clears both the `Windows` registry and the per-overlay
/// display map. Any subsequent `OverlaySelected` for these ids is a
/// no-op.
fn close_all_overlays(state: &mut App) -> Vec<Task<Message>> {
    let overlay_ids: Vec<_> = state
        .windows
        .iter()
        .filter_map(|(id, k)| (*k == WindowKind::Overlay).then_some(*id))
        .collect();
    let mut tasks: Vec<Task<Message>> = Vec::with_capacity(overlay_ids.len());
    for id in overlay_ids {
        state.windows.forget(id);
        state.overlay_displays.remove(&id);
        tasks.push(window::close(id));
    }
    tasks
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

/// Open a native file-save dialog seeded at `seed_dir` (or
/// `~/Desktop` if `None` / not a directory) with a timestamped
/// default filename, then write the PNG to whatever the user
/// picks. Returns `Ok(Some(path))` on success, `Ok(None)` when
/// the user cancels.
async fn save_image_via_picker(
    img: image::RgbaImage,
    seed_dir: Option<PathBuf>,
) -> Result<Option<PathBuf>, CaptureRunError> {
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let default_name = format!("Readshot-{stamp}.png");
    let initial_dir = seed_dir
        .filter(|p| p.is_dir())
        .or_else(|| directories::UserDirs::new().and_then(|d| d.desktop_dir().map(PathBuf::from)))
        .unwrap_or_else(std::env::temp_dir);
    let handle = rfd::AsyncFileDialog::new()
        .add_filter("PNG image", &["png"])
        .set_directory(&initial_dir)
        .set_file_name(&default_name)
        .save_file()
        .await;
    let Some(handle) = handle else {
        return Ok(None);
    };
    let mut path = handle.path().to_path_buf();
    // Force a `.png` extension — rfd doesn't always append the
    // filter's extension on macOS when the user types a bare name.
    if path
        .extension()
        .is_none_or(|e| !e.eq_ignore_ascii_case("png"))
    {
        path.set_extension("png");
    }
    img.save_with_format(&path, image::ImageFormat::Png)?;
    Ok(Some(path))
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

/// Show a one-line system notification announcing that Readshot is
/// alive in the menu bar after the welcome window dismisses itself
/// post-grant. Without this, users who triggered the grant flow
/// might think the app crashed when the window disappeared.
///
/// macOS: `osascript display notification …` is the lowest-friction
/// way to do this without a third-party crate. The notification
/// mentions the configured capture hotkey so the user knows how to
/// trigger a screenshot from anywhere.
///
/// Other platforms: no-op for now. Linux libnotify / Windows toast
/// land when those platform backends do.
fn notify_running_in_menu_bar(hotkey: &str) {
    #[cfg(target_os = "macos")]
    {
        let body = format!(
            "Readshot is running in the menu bar. Press {hotkey} to capture, or click the icon."
        );
        let script = format!(
            r#"display notification "{}" with title "Readshot is ready""#,
            body.replace('"', "\\\"")
        );
        let spawn = std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if let Err(e) = spawn {
            tracing::warn!(
                target: "readshot::notify",
                "osascript spawn failed: {e}"
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hotkey;
    }
}

/// Map an editor line-width to a text point-size. The Text tool
/// shares the line-width slider so the user has one knob — small
/// width = small text, big width = headline. The clamp keeps text
/// readable on either end.
fn text_size_from_line_width(line_width: f32) -> f32 {
    (line_width * 4.0 + 8.0).clamp(12.0, 96.0)
}

/// Apply a committed annotation to the editor session. Crop is
/// stored as a regular `Annotation` rather than baked into the base
/// image, so undo / redo work uniformly: the renderer translates
/// every other annotation by the crop offset, and removing the Crop
/// annotation (via undo) brings the full pre-crop image back. The
/// canvas's cursor mapping consults `effective_image_size` +
/// `crop_offset` so clicks after a crop still hit the right base
/// pixel.
fn handle_commit_annotation(
    ed: &mut crate::editor::EditorSession,
    annotation: readshot_core::Annotation,
) {
    if matches!(annotation, readshot_core::Annotation::NumberedPin { .. }) {
        ed.next_pin_number = ed.next_pin_number.saturating_add(1);
    }
    let cropped = matches!(annotation, readshot_core::Annotation::Crop { .. });
    ed.model.commit_annotation(annotation);
    ed.refresh_image();
    if cropped {
        let (w, h) = ed.effective_image_size();
        ed.status = Some(format!("Cropped to {w} × {h} px. ⌘Z to undo."));
    }
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
            pick_primary(&[secondary.clone(), primary.clone()])
                .unwrap()
                .id,
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
        assert_eq!(
            permission_blurb(WelcomeState::Granted),
            "Permission: granted"
        );
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
