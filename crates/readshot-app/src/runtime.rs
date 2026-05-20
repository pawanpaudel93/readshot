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

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager};
use iced::widget::{button, column, container, responsive, row, scrollable, text, Space};
use iced::window;
use iced::{Alignment, Color, Element, Length, Subscription, Task, Theme};

use readshot_capture::{CaptureRequest, DisplayInfo};
use readshot_core::Preferences;
use readshot_ui::{hotkey, SettingsMessage};

use crate::url_scheme::UrlAction;

/// Initial URL action set from `main.rs` before `iced::daemon` starts.
/// `start()` consumes this and feeds an extra `Message::UrlActionReceived`
/// task on boot, before the welcome window even has time to paint.
static INITIAL_URL_ACTION: OnceLock<Mutex<Option<UrlAction>>> = OnceLock::new();
static CLI_INTERACTIVE_REQUEST: OnceLock<Mutex<Option<CliInteractiveRequest>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct CliInteractiveRequest {
    pub output: PathBuf,
    pub hide_cursor: bool,
}

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

/// Configure the daemon as a hidden one-shot child used by
/// `readshot capture --interactive`. The normal CLI process launches
/// this mode, waits for it to write the selected PNG, then continues
/// with output formatting, clipboard, or OCR.
pub fn set_cli_interactive_request(request: CliInteractiveRequest) {
    let slot = CLI_INTERACTIVE_REQUEST.get_or_init(|| Mutex::new(None));
    *slot.lock().expect("CLI_INTERACTIVE_REQUEST poisoned") = Some(request);
}

fn take_cli_interactive_request() -> Option<CliInteractiveRequest> {
    CLI_INTERACTIVE_REQUEST
        .get()
        .and_then(|m| m.lock().ok().and_then(|mut g| g.take()))
}

use crate::app::{App, HistoryKeyboardAction, Message, WindowKind};
use crate::coordinator::CaptureCoordinator;
use crate::permissions::{default_provider, PermissionStatus};
use crate::welcome::WelcomeState;

/// Bootstrap: build the App, open the welcome window, and return
/// both to iced. The window-open `Task` resolves to a window id
/// which we slot into [`App::windows`] so subsequent `view` calls
/// can dispatch to the correct widget tree.
pub fn start() -> (App, Task<Message>) {
    let permissions = Arc::from(default_provider());
    let history_store: Option<std::sync::Arc<dyn readshot_core::HistoryStore>> =
        default_history_root().map(|root| {
            std::sync::Arc::new(readshot_core::FsHistoryStore::new(root))
                as std::sync::Arc<dyn readshot_core::HistoryStore>
        });
    let coordinator = CaptureCoordinator::new(
        Arc::from(readshot_capture::default_capturer()),
        Arc::from(readshot_ocr::default_engine()),
        Arc::clone(&permissions),
        history_store,
    );
    // History is the foundation for "every screenshot is searchable" —
    // turn it on by default for first-launch users. Returning users
    // get whatever they previously saved to `preferences.toml`.
    let preferences_path = default_preferences_path();
    let prefs = load_preferences(preferences_path.as_deref());
    let history_root = default_history_root();
    let mut app = App::new(coordinator, permissions, prefs);
    app.history_root = history_root;
    app.preferences_path = preferences_path;
    let cli_interactive_request = take_cli_interactive_request();
    if let Some(request) = cli_interactive_request.as_ref() {
        app.cli_interactive_output = Some(request.output.clone());
        app.pending_intent = Some(crate::app::CaptureIntent::CliInteractive);
        app.pending_hide_cursor = request.hide_cursor;
    }

    // Surface the initial permission state in the log so users
    // (and us, when triaging issues) can see whether macOS TCC is
    // already returning Granted before the welcome window appears.
    tracing::info!(
        target: "readshot::permissions",
        "initial status: {:?}",
        app.coordinator.pre_capture_gate(),
    );

    if cli_interactive_request.is_some() {
        let task = if app.welcome.should_show() {
            Task::done(Message::CliInteractiveWritten(Err(
                "Screen Recording permission is required for interactive capture".into(),
            )))
        } else {
            Task::done(Message::OpenOverlayRequested)
        };
        return (app, task);
    }

    // Register the user's preferred global hotkey. Failures are
    // logged-and-swallowed: the binary remains usable from the GUI
    // button + CLI / MCP surfaces if hotkey registration fails (e.g.
    // another app already owns the chord).
    if let Some(manager) = register_default_hotkey(&app.preferences) {
        app.hotkey_manager = Some(manager);
    }
    if app.preferences.launch_at_login {
        if let Err(e) = crate::startup::set_launch_at_login(true) {
            tracing::warn!(target: "readshot::startup", "launch-at-login setup failed: {e}");
        }
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = crate::url_events::install_platform_handler() {
        tracing::warn!(target: "readshot::url", "running-app URL handler unavailable: {e}");
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = crate::updater::install() {
        tracing::warn!(target: "readshot::updater", "Sparkle updater unavailable: {e}");
    }
    // Same fail-soft contract for the tray. Linux without an
    // appindicator daemon, or a Windows session without a Shell_Notify
    // surface, will simply not see the tray entry.
    let hotkey_label = pretty_hotkey(&app.preferences.capture_hotkey);
    app.tray = crate::tray::install(Some(&hotkey_label));

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
    } else {
        // No welcome window — user is already past the permission
        // gate. Surface a system notification so a relaunch is
        // visible: macOS's "Quit & Reopen" or our own "Restart
        // Readshot now" both bring the app back as an LSUIElement
        // agent, with no Dock icon and (often) no foregrounded
        // window. Without the toast the user can't easily tell the
        // app actually came back.
        notify_running_in_menu_bar(&app.preferences.capture_hotkey);
    }
    if let Some(action) = take_initial_url_action() {
        tasks.push(Task::done(Message::UrlActionReceived(action)));
    }
    (app, Task::batch(tasks))
}

/// Per-platform suggested default for `Preferences::capture_hotkey`.
/// macOS users have ⌘ muscle memory; everyone else uses Ctrl.
#[cfg(target_os = "macos")]
fn default_capture_hotkey() -> &'static str {
    "cmd+shift+x"
}
#[cfg(not(target_os = "macos"))]
fn default_capture_hotkey() -> &'static str {
    "ctrl+shift+x"
}

/// Render a hotkey string like "cmd+shift+x" as the macOS-native
/// glyph form "⌘⇧X". Falls back to the input string if there's no
/// recognisable component (so parse failures still display
/// *something* instead of an empty label).
pub fn pretty_hotkey(s: &str) -> String {
    let lower = s.trim().to_lowercase();
    if lower.is_empty() {
        return String::new();
    }
    let mut modifiers = String::new();
    let mut keys: Vec<String> = Vec::new();
    for raw in lower.split('+') {
        let p = raw.trim();
        if p.is_empty() {
            continue;
        }
        match p {
            "cmd" | "command" | "meta" | "super" | "win" => modifiers.push('\u{2318}'),
            "ctrl" | "control" => modifiers.push('\u{2303}'),
            "shift" => modifiers.push('\u{21E7}'),
            "alt" | "option" | "opt" => modifiers.push('\u{2325}'),
            other => keys.push(other.to_uppercase()),
        }
    }
    let pretty: String = format!("{modifiers}{}", keys.join(""));
    if pretty.is_empty() {
        s.to_string()
    } else {
        pretty
    }
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

fn refresh_hotkey_registration(state: &mut App) {
    // Drop the old manager first — that releases the OS-level chord.
    // Only then try the new one; if the new chord fails to parse or
    // conflicts with another app, the field stays `None`.
    state.hotkey_manager = None;
    state.hotkey_manager = register_default_hotkey(&state.preferences);
}

fn set_hotkey_registration_notice(state: &mut App) {
    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    state.settings_hotkey_error = None;
    state.settings_hotkey_status = None;
    if pretty.is_empty() {
        state.settings_hotkey_error = Some("That shortcut could not be read.".to_string());
    } else if state.hotkey_manager.is_some() {
        state.settings_hotkey_status = Some(format!("{pretty} is ready."));
    } else {
        state.settings_hotkey_error = Some(format!("{pretty} could not be registered globally."));
    }
}

fn sync_tray_capture_hotkey_label(state: &App) {
    let Some(tray) = state.tray.as_ref() else {
        return;
    };
    let label = pretty_hotkey(&state.preferences.capture_hotkey);
    if label.is_empty() {
        tray.set_capture_hotkey_label(None);
    } else {
        tray.set_capture_hotkey_label(Some(&label));
    }
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

/// Window settings for the persistent capture browser. Standard
/// resizable window with native chrome — this isn't a transient
/// overlay, it's a regular workspace window the user stays inside
/// while triaging history.
fn history_window_settings() -> window::Settings {
    window::Settings {
        size: iced::Size::new(900.0, 700.0),
        min_size: Some(iced::Size::new(540.0, 400.0)),
        position: window::Position::Centered,
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

/// Window settings for the preferences window.
fn settings_window_settings() -> window::Settings {
    window::Settings {
        size: iced::Size::new(720.0, 640.0),
        min_size: Some(iced::Size::new(520.0, 480.0)),
        position: window::Position::Centered,
        resizable: true,
        decorations: true,
        transparent: false,
        visible: true,
        ..Default::default()
    }
}

fn cli_tools_window_settings() -> window::Settings {
    window::Settings {
        size: iced::Size::new(760.0, 620.0),
        min_size: Some(iced::Size::new(560.0, 420.0)),
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

/// Logical width × height of the floating HUD shown while a scroll-
/// capture session is active. Kept as constants so the placement
/// helper below can pick a spot that doesn't overlap the capture
/// region (where it would otherwise show up in every captured frame
/// and ruin the stitch).
const SCROLL_HUD_WIDTH: f32 = 320.0;
const SCROLL_HUD_HEIGHT: f32 = 360.0;
const SCROLL_HUD_PREVIEW_HEIGHT: f32 = 170.0;
const SCROLL_HUD_GAP: f32 = 16.0;

/// Pick a HUD position that doesn't overlap the captured rect.
///
/// Order of preference: right of the rect → left of the rect → below →
/// above → fallback to the display's top-right corner clamped into
/// the display bounds. Returns logical-pixel screen coordinates.
fn scroll_hud_position(
    rect: readshot_core::geom::Rect,
    display_bounds: Option<(f32, f32)>,
) -> iced::Point {
    let (dw, dh) = display_bounds.unwrap_or((1440.0, 900.0));
    let w = SCROLL_HUD_WIDTH;
    let h = SCROLL_HUD_HEIGHT;
    let g = SCROLL_HUD_GAP;
    // Right of the selection.
    if rect.right() + g + w <= dw {
        return iced::Point::new(rect.right() + g, rect.y().clamp(0.0, (dh - h).max(0.0)));
    }
    // Left of the selection.
    if rect.x() - g - w >= 0.0 {
        return iced::Point::new(rect.x() - g - w, rect.y().clamp(0.0, (dh - h).max(0.0)));
    }
    // Below the selection.
    if rect.bottom() + g + h <= dh {
        return iced::Point::new(rect.x().clamp(0.0, (dw - w).max(0.0)), rect.bottom() + g);
    }
    // Above the selection.
    if rect.y() - g - h >= 0.0 {
        return iced::Point::new(rect.x().clamp(0.0, (dw - w).max(0.0)), rect.y() - g - h);
    }
    // Last resort: top-right corner clamped into the display.
    iced::Point::new((dw - w - g).max(0.0), g)
}

/// Floating HUD shown while a scrolling-capture session is active.
/// Borderless, always-on-top, positioned outside the captured rect
/// (see [`scroll_hud_position`]) so the HUD doesn't appear in the
/// frames being stitched.
fn scroll_hud_window_settings(position: iced::Point) -> window::Settings {
    window::Settings {
        size: iced::Size::new(SCROLL_HUD_WIDTH, SCROLL_HUD_HEIGHT),
        min_size: Some(iced::Size::new(SCROLL_HUD_WIDTH, SCROLL_HUD_HEIGHT)),
        position: window::Position::Specific(position),
        resizable: false,
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
        Some(WindowKind::History) => "Readshot — History".into(),
        Some(WindowKind::Settings) => "Readshot — Settings".into(),
        Some(WindowKind::CliTools) => "Readshot — Command Line Tools".into(),
        Some(WindowKind::ScrollHud) => "Readshot — Scrolling Capture".into(),
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
    #[cfg(target_os = "macos")]
    {
        // macOS delivers `readshot://` opens to an AppKit AppleEvent
        // callback while the app is already running. That callback
        // queues parsed actions; this tick drains them back into iced.
        subs.push(iced::time::every(Duration::from_millis(100)).map(|_| Message::UrlTick));
    }
    if state.scroll_session.is_some() {
        // Drive the scrolling-capture frame loop. The capture
        // coordinator's async future is debounced inside the session
        // (the tick handler bails out if a capture is still in
        // flight) so a slow backend can't pile up requests.
        subs.push(
            iced::time::every(Duration::from_millis(SCROLL_FRAME_INTERVAL_MS))
                .map(|_| Message::ScrollCaptureTick),
        );
        // Esc anywhere ends the session — same affordance as the
        // Stop button on the HUD.
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::{key::Named, Event as KbEvent, Key};
            if let iced::Event::Keyboard(KbEvent::KeyPressed {
                key: Key::Named(Named::Escape),
                ..
            }) = event
            {
                return Some(Message::ScrollCaptureStopRequested);
            }
            None
        }));
    }
    if !state.overlay_displays.is_empty() {
        // Marching-ants tick — drives the dash-offset animation on
        // any open region overlay. 80 ms ≈ 12.5 fps which reads as
        // smooth motion without burning CPU.
        subs.push(iced::time::every(Duration::from_millis(80)).map(|_| Message::OverlayTick));
        // Global Shift watcher — iced 0.14 does not pipe keyboard
        // events into canvas widgets, so the overlay's "hold Shift =
        // square" constraint relies on this subscription forwarding
        // the modifier state into App.overlay_shift_held.
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            match event {
                iced::Event::Keyboard(KbEvent::KeyPressed { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::KeyReleased { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::ModifiersChanged(modifiers)) => {
                    Some(Message::OverlayShiftChanged(modifiers.shift()))
                }
                _ => None,
            }
        }));
    }
    if state.editor.is_some() {
        // Keyboard sub: ⌘Z / Ctrl+Z = Undo, ⌘⇧Z / Ctrl+Shift+Z = Redo,
        // ⌘S / Ctrl+S = Save. Iced 0.14's `event::listen_with` is the
        // window-agnostic event tap; we filter to KeyPressed events
        // and only react when the editor window is the focused one
        // (the canvas captures key presses at the widget level for
        // Escape; that's why Esc isn't handled here).
        subs.push(iced::event::listen_with(|event, status, window| {
            use iced::event::Status;
            use iced::keyboard::{Event as KbEvent, Key};
            if let iced::Event::Window(iced::window::Event::Rescaled(scale)) = event {
                return Some(Message::WindowRescaled(window, scale));
            }
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
                    (Key::Character(c), true, false) if c == "+" || c == "=" => {
                        return Some(Message::EditorZoomIn);
                    }
                    (Key::Character(c), true, false) if c == "-" => {
                        return Some(Message::EditorZoomOut);
                    }
                    (Key::Character(c), true, false) if c == "0" => {
                        return Some(Message::EditorZoomActual);
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
    if state.settings_recording_hotkey {
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                if shortcut_cancelled_by_keypress(&key, modifiers) {
                    return Some(Message::SettingsHotkeyRecordingCancelled);
                }
                return Some(
                    shortcut_string_from_keypress(&key, modifiers)
                        .map(Message::SettingsHotkeyRecorded)
                        .unwrap_or(Message::SettingsHotkeyRecordingInvalid),
                );
            }
            None
        }));
    }
    if state.history_window_id.is_some() {
        subs.push(iced::event::listen_with(|event, status, window| {
            use iced::event::Status;
            use iced::keyboard::{key::Named, Event as KbEvent, Key};
            if status != Status::Ignored {
                return None;
            }
            let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event else {
                return None;
            };
            if modifiers.command() && !modifiers.alt() && !modifiers.control() {
                if let Key::Character(c) = &key {
                    if c.eq_ignore_ascii_case("c") {
                        return Some(Message::HistoryKeyboardShortcut(
                            window,
                            if modifiers.shift() {
                                HistoryKeyboardAction::CopyImage
                            } else {
                                HistoryKeyboardAction::CopyText
                            },
                        ));
                    }
                }
            }
            if modifiers.command() || modifiers.alt() || modifiers.control() || modifiers.shift() {
                return None;
            }
            match key {
                Key::Named(Named::ArrowUp) => Some(Message::HistoryKeyboardShortcut(
                    window,
                    HistoryKeyboardAction::Previous,
                )),
                Key::Named(Named::ArrowDown) => Some(Message::HistoryKeyboardShortcut(
                    window,
                    HistoryKeyboardAction::Next,
                )),
                Key::Named(Named::Enter) => Some(Message::HistoryKeyboardShortcut(
                    window,
                    HistoryKeyboardAction::Open,
                )),
                Key::Named(Named::Delete) | Key::Named(Named::Backspace) => Some(
                    Message::HistoryKeyboardShortcut(window, HistoryKeyboardAction::Delete),
                ),
                _ => None,
            }
        }));
    }
    // Watch for the OS X-button closing any tracked window. Without
    // this, `state.windows` accumulates stale ids, and helpers like
    // `show_or_focus_welcome` end up calling `gain_focus` on dead
    // windows (silent no-op) instead of opening a fresh one.
    subs.push(window::close_events().map(Message::WindowClosed));
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

        Message::OverlayShiftChanged(held) => {
            // No-op when state didn't actually flip — keeps the redraw
            // path from firing on every key event.
            if state.overlay_shift_held == held {
                return Task::none();
            }
            state.overlay_shift_held = held;
            // The overlay rebuilds OverlayProgram from `state` on every
            // view(); a redraw picks the new shift state up and the
            // OverlayTick's 80 ms loop covers most of the rest.
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
            if fired {
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

        Message::UrlTick => {
            let tasks: Vec<Task<Message>> = crate::url_events::drain_actions()
                .into_iter()
                .map(|action| Task::done(Message::UrlActionReceived(action)))
                .collect();
            Task::batch(tasks)
        }

        Message::TrayActionPerformed(action) => match action {
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
                    // Open the overlay normally — the user still picks
                    // their region via the standard selector. The
                    // toolbar's ↕ Scroll button confirms with the
                    // ScrollCapture intent. Surfacing the entry from
                    // the tray just shortens "how do I start a
                    // scrolling capture" to a single menu click.
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
            crate::tray::TrayAction::InstallCommandLineTools => {
                update(state, Message::OpenCliToolsRequested)
            }
            crate::tray::TrayAction::CheckForUpdates => {
                notify_update_check_started();
                if let Err(e) = crate::updater::check_for_updates() {
                    tracing::warn!(target: "readshot::updater", "manual update check failed: {e}");
                    notify_update_check_failed(&e.to_string());
                }
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
            // List displays asynchronously; the result drives the
            // actual window-open work in `OverlayDisplaysListed` so we
            // can spawn one transparent overlay per monitor.
            let coord = state.coordinator.clone();
            Task::perform(async move { coord.list_displays().await }, |result| {
                Message::OverlayDisplaysListed(result.map_err(|e| e.to_string()))
            })
        }

        Message::RetakeLastRegionRequested => {
            if state.welcome.should_show() {
                return show_or_focus_welcome(state);
            }
            if state.capture_in_flight {
                return Task::none();
            }
            let Some((display_id, last)) =
                state
                    .last_region_display_id
                    .as_ref()
                    .and_then(|display_id| {
                        state
                            .last_regions
                            .get(display_id)
                            .map(|last| (display_id.clone(), *last))
                    })
            else {
                state.last_capture_status = Some(
                    "No previous region to retake. Capture a region first, then use Retake Last Region."
                        .into(),
                );
                return Task::none();
            };

            state
                .pending_intent
                .get_or_insert(crate::app::CaptureIntent::Editor);
            state.pending_display_id = Some(display_id.clone());
            state.pending_display_scale = Some(last.display_scale);
            update(
                state,
                Message::CaptureRegionRequested {
                    display_id,
                    rect: last.rect,
                },
            )
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
            state
                .pending_intent
                .get_or_insert(crate::app::CaptureIntent::Editor);
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
            let overlay_record = state
                .overlay_displays
                .values()
                .find(|d| d.display_id == display_id);
            let display_scale = overlay_record.map(|d| d.scale).unwrap_or(1.0);
            let display_bounds = overlay_record.map(|d| (d.width, d.height));
            state.last_regions.insert(
                display_id.clone(),
                crate::app::LastRegion {
                    rect,
                    display_scale,
                },
            );
            state.last_region_display_id = Some(display_id.clone());
            if let Some(tray) = state.tray.as_ref() {
                tray.set_retake_last_region_enabled(true);
            }
            state.pending_intent = Some(intent);
            state.pending_display_id = Some(display_id.clone());
            state.pending_display_scale = Some(display_scale);
            state.pending_display_bounds = display_bounds;
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
            state.pending_display_id = None;
            state.pending_display_scale = None;
            state.pending_hide_cursor = true;
            if state.cli_interactive_output.is_some() {
                state.cli_interactive_output = None;
                return Task::batch(
                    close_all_overlays(state)
                        .into_iter()
                        .chain(std::iter::once(iced::exit())),
                );
            }
            Task::batch(close_all_overlays(state))
        }

        Message::CaptureRegionRequested { display_id, rect } => {
            let coord = state.coordinator.clone();
            let hide_cursor = state.pending_hide_cursor;
            state.capture_in_flight = true;
            state.last_capture_status = None;
            // Region capture lands in the editor instead of saving
            // directly — the editor decides what to do with it.
            Task::perform(
                capture_region_to_image(coord, display_id, rect, hide_cursor),
                |result| Message::RegionCaptureCompleted(result.map_err(|e| e.to_string())),
            )
        }

        Message::RegionCaptureCompleted(result) => {
            state.capture_in_flight = false;
            let intent = state
                .pending_intent
                .take()
                .unwrap_or(crate::app::CaptureIntent::Editor);
            let display_id = state.pending_display_id.take().unwrap_or_default();
            let display_scale = state.pending_display_scale.take().unwrap_or(1.0);
            state.pending_hide_cursor = true;
            match result {
                Ok(image) => {
                    let history_record = history_record_for_capture(
                        &image,
                        &display_id,
                        state.preferences.history_retention,
                    );
                    // Persist a history record in parallel with the
                    // user-visible intent action. The history task is
                    // gated on `preferences.history_retention` and
                    // logs internally on failure, so it can never
                    // block or fail the user-visible flow.
                    let history_task = if intent == crate::app::CaptureIntent::CliInteractive {
                        Task::none()
                    } else {
                        persist_history_task(
                            state.coordinator.clone(),
                            image.clone(),
                            history_record.clone(),
                            state.preferences.history_retention,
                            state.preferences.clone(),
                        )
                    };
                    let intent_task = match intent {
                        crate::app::CaptureIntent::Editor => {
                            state.editor = Some(match history_record {
                                Some(record) => {
                                    crate::editor::EditorSession::from_history_with_display_scale(
                                        image,
                                        record,
                                        display_scale,
                                    )
                                }
                                None => crate::editor::EditorSession::new_with_display_scale(
                                    image,
                                    display_scale,
                                ),
                            });
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
                            let prefs = state.preferences.clone();
                            Task::perform(ocr_then_copy(coord, image, prefs), |r| {
                                Message::OverlayCopyTextDone(r.map_err(|e| e.to_string()))
                            })
                        }
                        crate::app::CaptureIntent::SaveDirect => {
                            let seed =
                                preferred_save_seed_dir(&state.preferences, &state.last_save_dir);
                            let template = state.preferences.filename_template.clone();
                            Task::perform(save_image_via_picker(image, seed, template), |r| {
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
                            state
                                .pins
                                .insert(id, crate::app::PinState::new(handle.clone()));
                            open_task
                                .map(move |opened| Message::PinWindowReady(opened, handle.clone()))
                        }
                        crate::app::CaptureIntent::CliInteractive => {
                            let Some(output) = state.cli_interactive_output.take() else {
                                return Task::done(Message::CliInteractiveWritten(Err(
                                    "missing CLI interactive output path".into(),
                                )));
                            };
                            Task::perform(write_cli_interactive_capture(image, output), |r| {
                                Message::CliInteractiveWritten(r.map_err(|e| e.to_string()))
                            })
                        }
                        crate::app::CaptureIntent::ScrollCapture => {
                            // Start a new scrolling-capture session.
                            // The first frame is the just-captured `image`;
                            // subsequent frames come from the timer-tick loop.
                            // `last_regions` was populated by OverlaySelected just
                            // before this branch ran, so the lookup is safe.
                            let Some(last) = state.last_regions.get(&display_id).copied() else {
                                state.last_capture_status =
                                    Some("Scroll capture: missing region".into());
                                return Task::batch([history_task]);
                            };
                            let display_bounds = state.pending_display_bounds.take();
                            let mut session = crate::app::ScrollSession::new(
                                display_id.clone(),
                                last.rect,
                                display_scale,
                            );
                            session.frames.push(image);
                            state.scroll_session = Some(session);
                            let hud_pos = scroll_hud_position(last.rect, display_bounds);
                            let (id, open_task) = window::open(scroll_hud_window_settings(hud_pos));
                            state.windows.register(id, WindowKind::ScrollHud);
                            open_task.map(Message::ScrollHudWindowReady)
                        }
                    };
                    Task::batch([history_task, intent_task])
                }
                Err(e) => {
                    state.last_capture_status = Some(format!("Capture failed: {e}"));
                    if state.cli_interactive_output.is_some() {
                        Task::done(Message::CliInteractiveWritten(Err(e)))
                    } else {
                        Task::none()
                    }
                }
            }
        }

        Message::HistoryRecordPersisted(result) => {
            match result {
                Ok(true) => {
                    // OCR completed for the just-saved record. Refresh
                    // the browser if it's open so the new text is
                    // searchable / visible.
                    if state.history_window_id.is_some() {
                        let coord = state.coordinator.clone();
                        return Task::perform(async move { coord.history_list() }, |r| {
                            Message::HistoryListLoaded(r.map_err(|e| e.to_string()))
                        });
                    }
                }
                Ok(false) => {} // history off or OCR failed (already logged)
                Err(e) => tracing::warn!(target: "readshot::history", "persist failed: {e}"),
            }
            Task::none()
        }

        Message::OpenHistoryRequested => {
            // Single-instance — if the window already exists just
            // refocus it and refresh the list.
            if let Some(id) = state.history_window_id {
                let coord = state.coordinator.clone();
                return Task::batch([
                    window::gain_focus(id),
                    Task::perform(async move { coord.history_list() }, |r| {
                        Message::HistoryListLoaded(r.map_err(|e| e.to_string()))
                    }),
                ]);
            }
            let (id, open_task) = window::open(history_window_settings());
            state.windows.register(id, WindowKind::History);
            state.history_window_id = Some(id);
            state.history_status = None;
            let coord = state.coordinator.clone();
            Task::batch([
                open_task.map(Message::HistoryWindowReady),
                Task::perform(async move { coord.history_list() }, |r| {
                    Message::HistoryListLoaded(r.map_err(|e| e.to_string()))
                }),
            ])
        }
        Message::HistoryWindowReady(id) => {
            // Window settings already register at open-time; the
            // ready callback just records the id in case iced hands
            // back a different one.
            state.history_window_id = Some(id);
            Task::none()
        }
        Message::HistoryListLoaded(result) => {
            match result {
                Ok(records) => {
                    let previous = state.history_selected_id;
                    state.history_records = records;
                    state.history_selected_id = preferred_history_selection(
                        &state.history_records,
                        &state.history_search,
                        previous,
                    );
                    state.history_status = None;
                }
                Err(e) => {
                    state.history_records.clear();
                    state.history_selected_id = None;
                    state.history_status = Some(format!("Couldn't read history: {e}"));
                }
            }
            Task::none()
        }
        Message::HistoryClosed => {
            let id = state.history_window_id.take();
            state.history_records.clear();
            state.history_status = None;
            state.history_selected_id = None;
            match id {
                Some(id) => {
                    state.windows.forget(id);
                    window::close(id)
                }
                None => Task::none(),
            }
        }
        Message::WindowClosed(id) => {
            // Always drop the id from `Windows` so helpers that
            // probe the live-window map (e.g. `show_or_focus_welcome`)
            // don't try to focus a dead window.
            state.windows.forget(id);
            if state.history_window_id == Some(id) {
                state.history_window_id = None;
                state.history_records.clear();
                state.history_status = None;
                state.history_search.clear();
                state.history_selected_id = None;
            }
            if state.settings_window_id == Some(id) {
                state.settings_window_id = None;
                state.settings_recording_hotkey = false;
                state.settings_hotkey_error = None;
                state.settings_hotkey_status = None;
                state.settings_status = None;
                state.settings_reset_all_pending = false;
            }
            if state.cli_tools_window_id == Some(id) {
                state.cli_tools_window_id = None;
                state.cli_tools_status = None;
            }
            Task::none()
        }
        Message::WindowRescaled(id, scale) => {
            if matches!(state.windows.kind(id), Some(WindowKind::Editor)) {
                if let Some(ed) = state.editor.as_mut() {
                    let was_actual = ed.zoom_is_actual_size();
                    ed.display_scale = scale.max(f32::EPSILON);
                    if was_actual {
                        ed.zoom = ed.actual_size_zoom();
                    }
                }
            }
            Task::none()
        }
        Message::HistorySearchChanged(q) => {
            state.history_search = q;
            state.history_selected_id = preferred_history_selection(
                &state.history_records,
                &state.history_search,
                state.history_selected_id,
            );
            Task::none()
        }
        Message::HistorySelect(id) => {
            if history_record_visible(&state.history_records, &state.history_search, id) {
                state.history_selected_id = Some(id);
            }
            Task::none()
        }
        Message::HistorySelectPrevious => {
            state.history_selected_id = adjacent_history_selection(
                &state.history_records,
                &state.history_search,
                state.history_selected_id,
                -1,
            );
            Task::none()
        }
        Message::HistorySelectNext => {
            state.history_selected_id = adjacent_history_selection(
                &state.history_records,
                &state.history_search,
                state.history_selected_id,
                1,
            );
            Task::none()
        }
        Message::HistoryOpenSelected => match state.history_selected_id {
            Some(id) => update(state, Message::HistoryOpenInEditor(id)),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        Message::HistoryDeleteSelected => match state.history_selected_id {
            Some(id) => update(state, Message::HistoryDelete(id)),
            None => {
                state.history_status = Some("No history capture selected.".into());
                Task::none()
            }
        },
        Message::HistoryKeyboardShortcut(window_id, action) => {
            if state.history_window_id != Some(window_id) {
                return Task::none();
            }
            match action {
                HistoryKeyboardAction::Previous => update(state, Message::HistorySelectPrevious),
                HistoryKeyboardAction::Next => update(state, Message::HistorySelectNext),
                HistoryKeyboardAction::Open => update(state, Message::HistoryOpenSelected),
                HistoryKeyboardAction::Delete => update(state, Message::HistoryDeleteSelected),
                HistoryKeyboardAction::CopyText => match state.history_selected_id {
                    Some(id) => update(state, Message::HistoryCopyText(id)),
                    None => {
                        state.history_status = Some("No history capture selected.".into());
                        Task::none()
                    }
                },
                HistoryKeyboardAction::CopyImage => match state.history_selected_id {
                    Some(id) => update(state, Message::HistoryCopyImage(id)),
                    None => {
                        state.history_status = Some("No history capture selected.".into());
                        Task::none()
                    }
                },
            }
        }
        Message::HistoryClearAllRequested => {
            if let Err(e) = state.coordinator.clear_history() {
                state.history_status = Some(format!("Clear history failed: {e}"));
                return Task::none();
            }
            state.history_records.clear();
            state.history_search.clear();
            state.history_selected_id = None;
            state.history_status = Some("History cleared.".into());
            Task::none()
        }
        Message::HistoryOpenInEditor(id) => {
            let Some((path, record)) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| (history_png_path(root, r), r.clone()))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            Task::perform(
                async move { load_png_async(path).await.map(|img| (img, record)) },
                |r| Message::HistoryOpenInEditorReady(r.map_err(|e| e.to_string())),
            )
        }
        Message::HistoryOpenInEditorReady(result) => match result {
            Ok((image, record)) => {
                state.editor = Some(crate::editor::EditorSession::from_history(image, record));
                let (id, open_task) = window::open(editor_window_settings());
                state.windows.register(id, WindowKind::Editor);
                open_task.map(Message::EditorWindowReady)
            }
            Err(e) => {
                state.history_status = Some(format!("Open failed: {e}"));
                Task::none()
            }
        },
        Message::HistoryReveal(id) => {
            let Some(path) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| history_png_path(root, r))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            if let Err(e) = reveal_path(&path) {
                state.history_status = Some(format!("Reveal failed: {e}"));
            }
            Task::none()
        }
        Message::HistoryCopyImage(id) => {
            let Some(path) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| history_png_path(root, r))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            Task::perform(
                async move {
                    let img = load_png_async(path).await.map_err(|e| e.to_string())?;
                    copy_image_to_clipboard(img)
                        .await
                        .map_err(|e| e.to_string())
                },
                Message::HistoryCopyImageDone,
            )
        }
        Message::HistoryCopyImageDone(result) => {
            state.history_status = Some(match result {
                Ok(()) => "Copied image to clipboard.".into(),
                Err(e) => format!("Copy image failed: {e}"),
            });
            Task::none()
        }
        Message::HistoryCopyText(id) => {
            match state
                .history_records
                .iter()
                .find(|r| r.id == id)
                .and_then(|r| r.ocr_text.as_ref())
            {
                Some(text) if !text.is_empty() => {
                    let text = text.clone();
                    Task::perform(
                        async move {
                            copy_text_to_clipboard(text.clone())
                                .await
                                .map_err(|e| e.to_string())?;
                            Ok(text)
                        },
                        Message::HistoryCopyTextDone,
                    )
                }
                _ => {
                    state.history_status = Some("No OCR text on this capture yet.".into());
                    Task::none()
                }
            }
        }
        Message::HistoryCopyTextDone(result) => {
            state.history_status = Some(match result {
                Ok(text) if text.is_empty() => "No OCR text on this capture yet.".into(),
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
        Message::HistoryCopyVisibleTextRequested => {
            let text = visible_history_text(&state.history_records, &state.history_search);
            if text.trim().is_empty() {
                state.history_status = Some("No OCR text in the visible captures.".into());
                return Task::none();
            }
            let count = visible_history_text_count(&state.history_records, &state.history_search);
            Task::perform(
                async move {
                    copy_text_to_clipboard(text)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(count)
                },
                Message::HistoryCopyVisibleTextDone,
            )
        }
        Message::HistoryCopyVisibleTextDone(result) => {
            state.history_status = Some(match result {
                Ok(count) => format!(
                    "Copied OCR text from {count} capture{}.",
                    if count == 1 { "" } else { "s" }
                ),
                Err(e) => format!("Copy visible text failed: {e}"),
            });
            Task::none()
        }
        Message::HistoryPin(id) => {
            let Some(path) = state
                .history_root
                .as_ref()
                .zip(state.history_records.iter().find(|r| r.id == id))
                .map(|(root, r)| history_png_path(root, r))
            else {
                state.history_status = Some("Capture not found.".into());
                return Task::none();
            };
            Task::perform(load_png_async(path), |r| {
                Message::HistoryPinReady(r.map_err(|e| e.to_string()))
            })
        }
        Message::HistoryPinReady(result) => match result {
            Ok(image) => {
                let size = (image.width(), image.height());
                let handle = iced::widget::image::Handle::from_rgba(
                    image.width(),
                    image.height(),
                    image.as_raw().clone(),
                );
                let (wid, open_task) = window::open(pin_window_settings(size));
                state.windows.register(wid, WindowKind::Pin);
                state
                    .pins
                    .insert(wid, crate::app::PinState::new(handle.clone()));
                open_task.map(move |opened| Message::PinWindowReady(opened, handle.clone()))
            }
            Err(e) => {
                state.history_status = Some(format!("Pin failed: {e}"));
                Task::none()
            }
        },
        Message::HistoryDelete(id) => {
            if let Err(e) = state.coordinator.delete_history(id) {
                state.history_status = Some(format!("Delete failed: {e}"));
                return Task::none();
            }
            // Reload the list so the deleted record disappears.
            let coord = state.coordinator.clone();
            Task::perform(async move { coord.history_list() }, |r| {
                Message::HistoryListLoaded(r.map_err(|e| e.to_string()))
            })
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
        Message::CliInteractiveWritten(result) => {
            if let Err(e) = result {
                tracing::warn!(target: "readshot::cli", "interactive capture failed: {e}");
            }
            state.cli_interactive_output = None;
            state.pending_intent = None;
            state.pending_display_id = None;
            state.pending_display_scale = None;
            state.pending_hide_cursor = true;
            iced::exit()
        }

        Message::ScrollHudWindowReady(id) => {
            if let Some(session) = state.scroll_session.as_mut() {
                session.hud_window_id = Some(id);
            }
            Task::none()
        }

        Message::ScrollHudDragRequested => {
            match state.scroll_session.as_ref().and_then(|s| s.hud_window_id) {
                Some(id) => window::drag(id),
                None => Task::none(),
            }
        }

        Message::ScrollCaptureTick => {
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            if session.stopping || session.capture_in_flight {
                return Task::none();
            }
            // Hard limits: bail out if the user has been at it too long.
            if session.frames.len() >= SCROLL_MAX_FRAMES {
                return Task::done(Message::ScrollCaptureStopRequested);
            }
            session.capture_in_flight = true;
            let coord = state.coordinator.clone();
            let request = readshot_capture::CaptureRequest {
                display_id: session.display_id.clone(),
                rect: session.rect,
                scale: session.scale,
                hide_cursor: true,
            };
            Task::perform(
                async move { coord.capture_region(request).await },
                |result| Message::ScrollCaptureFrame(result.map_err(|e| e.to_string())),
            )
        }

        Message::ScrollCaptureFrame(result) => {
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            session.capture_in_flight = false;
            match result {
                Ok(image) => {
                    // Quick motion check vs. previous frame — compare a
                    // few horizontal rows in the middle of the image.
                    // Skips appending if the page hasn't moved, and
                    // bumps the no-motion counter so we can auto-stop
                    // when the user pauses scrolling.
                    let moved = match session.frames.last() {
                        Some(prev) => frames_differ(prev, &image),
                        None => true,
                    };
                    if moved {
                        // Cache an iced Handle once per accepted frame
                        // so the HUD's live preview doesn't re-clone
                        // ~8 MB of RGBA on every redraw.
                        let handle = iced::widget::image::Handle::from_rgba(
                            image.width(),
                            image.height(),
                            image.as_raw().clone(),
                        );
                        session.no_motion_count = 0;
                        session.frames.push(image);
                        session.frame_tick = session.frame_tick.wrapping_add(1);
                        session.last_frame_at = Some(std::time::Instant::now());
                        session.last_frame_handle = Some(handle);
                    } else {
                        session.no_motion_count += 1;
                    }
                    // Session never auto-stops on stillness — the user
                    // explicitly clicks Stop & Stitch (or Cancel) when
                    // they're done. Auto-stop on no-motion was killing
                    // sessions every time the user paused to read.
                    Task::none()
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::scroll", "frame capture failed: {e}");
                    session.no_motion_count += 1;
                    Task::none()
                }
            }
        }

        Message::ScrollCaptureCancelRequested => {
            // Discard the session without stitching. Close the HUD,
            // drop the captured frames, clear the status — a clean
            // "never happened" path the user can take if the page
            // wasn't the right one or the scroll went wrong.
            let Some(session) = state.scroll_session.take() else {
                return Task::none();
            };
            let close_task = match session.hud_window_id {
                Some(id) => {
                    state.windows.forget(id);
                    window::close(id)
                }
                None => Task::none(),
            };
            state.last_capture_status = Some("Scroll capture cancelled.".into());
            close_task
        }

        Message::ScrollCaptureStopRequested => {
            let Some(session) = state.scroll_session.as_mut() else {
                return Task::none();
            };
            if session.stopping {
                return Task::none();
            }
            session.stopping = true;
            let frames = std::mem::take(&mut session.frames);
            let hud_id = session.hud_window_id;
            // Close the HUD now — the stitch task runs on its own.
            let close_task = match hud_id {
                Some(id) => {
                    state.windows.forget(id);
                    window::close(id)
                }
                None => Task::none(),
            };
            let stitch_task = Task::perform(stitch_frames_async(frames), |r| {
                Message::ScrollCaptureStitched(r.map_err(|e| e.to_string()))
            });
            Task::batch([close_task, stitch_task])
        }

        Message::ScrollCaptureStitched(result) => {
            let session = state.scroll_session.take();
            let display_scale = session.as_ref().map(|s| s.scale).unwrap_or(1.0);
            match result {
                Ok(image) => {
                    let (w, h) = (image.width(), image.height());
                    let mut ed =
                        crate::editor::EditorSession::new_with_display_scale(image, display_scale);
                    ed.status = Some(format!("Scrolling capture stitched into {w} × {h}px."));
                    state.editor = Some(ed);
                    let (id, open_task) = window::open(editor_window_settings());
                    state.windows.register(id, WindowKind::Editor);
                    open_task.map(Message::EditorWindowReady)
                }
                Err(e) => {
                    state.last_capture_status = Some(format!("Scroll capture failed: {e}"));
                    tracing::warn!(target: "readshot::scroll", "stitch failed: {e}");
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
            ed.status = Some("Choose a save location…".into());
            let img = ed.model.flatten();
            let seed = preferred_save_seed_dir(&state.preferences, &state.last_save_dir);
            let template = state.preferences.filename_template.clone();
            Task::perform(save_image_via_picker(img, seed, template), |r| {
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
            let prefs = state.preferences.clone();
            Task::perform(ocr_then_copy(coord, img, prefs), |r| {
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
                        sync_editor_history(ed, &state.coordinator);
                    }
                }
                readshot_ui::ToolbarMessage::Redo => {
                    if ed.model.redo() {
                        ed.refresh_image();
                        sync_editor_history(ed, &state.coordinator);
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
                    sync_editor_history(ed, &state.coordinator);
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
                    sync_editor_history(ed, &state.coordinator);
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
        Message::EditorZoomIn => {
            if let Some(ed) = state.editor.as_mut() {
                ed.zoom = ed.zoom.zoom_in();
            }
            Task::none()
        }
        Message::EditorZoomInFromDisplayScale(scale) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.zoom = ed.zoom.zoom_in_from_display_scale(scale);
            }
            Task::none()
        }
        Message::EditorZoomOut => {
            if let Some(ed) = state.editor.as_mut() {
                ed.zoom = ed.zoom.zoom_out();
            }
            Task::none()
        }
        Message::EditorZoomOutFromDisplayScale(scale) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.zoom = ed.zoom.zoom_out_from_display_scale(scale);
            }
            Task::none()
        }
        Message::EditorZoomActual => {
            if let Some(ed) = state.editor.as_mut() {
                ed.zoom = ed.actual_size_zoom();
            }
            Task::none()
        }
        Message::EditorZoomFit => {
            if let Some(ed) = state.editor.as_mut() {
                ed.zoom = crate::editor::EditorZoom::Fit;
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
            state
                .pins
                .insert(id, crate::app::PinState::new(handle.clone()));
            tasks
                .push(open_task.map(move |opened| Message::PinWindowReady(opened, handle.clone())));
            Task::batch(tasks)
        }
        Message::PinWindowReady(id, handle) => {
            // The eager insert above already covers most cases; this
            // re-key handles the (rare) scenario where the iced
            // runtime hands us a different id than the one returned
            // by `window::open` synchronously. Idempotent insert.
            state
                .pins
                .entry(id)
                .or_insert_with(|| crate::app::PinState::new(handle));
            Task::none()
        }
        Message::PinClosePressed(id) => {
            state.windows.forget(id);
            state.pins.remove(&id);
            window::close(id)
        }
        Message::PinDragRequested(id) => window::drag(id),
        Message::PinOpacityChanged(id, opacity) => {
            if let Some(pin) = state.pins.get_mut(&id) {
                pin.opacity = opacity.clamp(0.2, 1.0);
            }
            Task::none()
        }
        Message::PinLockToggled(id) => {
            if let Some(pin) = state.pins.get_mut(&id) {
                pin.locked = !pin.locked;
            }
            Task::none()
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

        // GrantPermissionRequested: just trigger the macOS permission
        // prompt by poking ScreenCaptureKit
        // (`SCShareableContent::get()` runs inside `list_displays`).
        // The OS prompt itself carries an "Open System Settings"
        // button — letting macOS drive that flow avoids our app
        // racing it with a second `open URL` call. The
        // "Open Settings again" button in the AwaitingGrant state
        // covers users whose prompt never appears (e.g. previously
        // denied) by deep-linking directly.
        Message::GrantPermissionRequested => {
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

        // Synchronous transitions — reuse the existing handler.
        msg @ (Message::PermissionPoll(_) | Message::OpenPermissionSettingsRequested) => {
            state.update_sync(msg);
            Task::none()
        }

        // Settings mutations are mostly synchronous (the apply fn is
        // pure on `Preferences`) but a hotkey change has a
        // side-effect: the OS-level chord registration has to be
        // re-issued. We branch off the joined arm here so that side-
        // effect lives next to the state update.
        Message::Settings(submsg) => {
            let needs_rehotkey = matches!(submsg, SettingsMessage::SetCaptureHotkey(_));
            let launch_at_login = match &submsg {
                SettingsMessage::SetLaunchAtLogin(v) => Some(*v),
                _ => None,
            };
            if !matches!(submsg, SettingsMessage::SetCaptureHotkey(_)) {
                state.settings_hotkey_status = None;
            }
            state.settings_reset_all_pending = false;
            state.update_sync(Message::Settings(submsg));
            if needs_rehotkey {
                refresh_hotkey_registration(state);
                set_hotkey_registration_notice(state);
                sync_tray_capture_hotkey_label(state);
            }
            if let Some(enabled) = launch_at_login {
                if let Err(e) = crate::startup::set_launch_at_login(enabled) {
                    tracing::warn!(
                        target: "readshot::startup",
                        "launch-at-login update failed: {e}",
                    );
                    state.update_sync(Message::Settings(SettingsMessage::SetLaunchAtLogin(
                        !enabled,
                    )));
                }
            }
            Task::none()
        }

        Message::OpenSettingsRequested => {
            // Single-instance — focus the existing window if one is
            // already up.
            if let Some(id) = state.settings_window_id {
                return window::gain_focus(id);
            }
            let (id, open_task) = window::open(settings_window_settings());
            state.windows.register(id, WindowKind::Settings);
            state.settings_window_id = Some(id);
            open_task.map(Message::SettingsWindowReady)
        }
        Message::SettingsChooseSaveFolderRequested => {
            Task::perform(pick_settings_save_folder(), |r| {
                Message::SettingsSaveFolderPicked(r.map_err(|e| e.to_string()))
            })
        }
        Message::SettingsSaveFolderPicked(result) => {
            if let Ok(Some(path)) = result {
                return update(
                    state,
                    Message::Settings(SettingsMessage::SetSaveFolder(path)),
                );
            }
            Task::none()
        }
        Message::SettingsOpenSaveFolderRequested => {
            let folder = settings_save_folder_to_open(&state.preferences);
            Task::perform(
                async move { open_folder_path(&folder).map_err(|e| e.to_string()) },
                Message::SettingsOpenSaveFolderDone,
            )
        }
        Message::SettingsOpenSaveFolderDone(result) => {
            state.settings_status = Some(match result {
                Ok(()) => "Opened save folder.".to_string(),
                Err(e) => format!("Could not open save folder: {e}"),
            });
            Task::none()
        }
        Message::SettingsStartHotkeyRecording => {
            state.settings_recording_hotkey = true;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            state.settings_status = None;
            state.settings_reset_all_pending = false;
            Task::none()
        }
        Message::SettingsHotkeyRecorded(shortcut) => {
            state.settings_recording_hotkey = false;
            state.settings_hotkey_error = None;
            update(
                state,
                Message::Settings(SettingsMessage::SetCaptureHotkey(shortcut)),
            )
        }
        Message::SettingsHotkeyRecordingInvalid => {
            state.settings_recording_hotkey = true;
            state.settings_hotkey_status = None;
            state.settings_hotkey_error = Some(
                "Use at least one modifier, such as Command, Control, Option, or Shift."
                    .to_string(),
            );
            Task::none()
        }
        Message::SettingsHotkeyRecordingCancelled => {
            state.settings_recording_hotkey = false;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            Task::none()
        }
        Message::SettingsResetAllRequested => {
            state.settings_reset_all_pending = true;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            state.settings_status = None;
            Task::none()
        }
        Message::SettingsResetAllCancelled => {
            state.settings_reset_all_pending = false;
            Task::none()
        }
        Message::SettingsResetAllConfirmed => {
            state.settings_reset_all_pending = false;
            state.settings_recording_hotkey = false;
            state.settings_hotkey_error = None;
            state.settings_hotkey_status = None;
            state.settings_status = Some("Settings reset to defaults.".to_string());
            let old_launch_at_login = state.preferences.launch_at_login;
            reset_settings_to_defaults(state);
            refresh_hotkey_registration(state);
            set_hotkey_registration_notice(state);
            sync_tray_capture_hotkey_label(state);
            if old_launch_at_login {
                if let Err(e) = crate::startup::set_launch_at_login(false) {
                    tracing::warn!(
                        target: "readshot::startup",
                        "launch-at-login reset failed: {e}",
                    );
                    state.update_sync(Message::Settings(SettingsMessage::SetLaunchAtLogin(true)));
                }
            }
            Task::none()
        }
        Message::SettingsWindowReady(id) => {
            // Belt-and-braces: window-open already records the id, but
            // iced may hand back a different one in some platforms.
            state.settings_window_id = Some(id);
            Task::none()
        }
        Message::OpenCliToolsRequested => {
            if let Some(id) = state.cli_tools_window_id {
                return window::gain_focus(id);
            }
            state.cli_tools_status = None;
            let (id, open_task) = window::open(cli_tools_window_settings());
            state.windows.register(id, WindowKind::CliTools);
            state.cli_tools_window_id = Some(id);
            open_task.map(Message::CliToolsWindowReady)
        }
        Message::CliToolsWindowReady(id) => {
            state.cli_tools_window_id = Some(id);
            Task::none()
        }
        Message::CliToolsCopyRequested(shell) => Task::perform(
            copy_text_to_clipboard(cli_tools_setup_commands(shell)),
            move |result| Message::CliToolsCopyDone(shell, result.map_err(|e| e.to_string())),
        ),
        Message::CliToolsCopyDone(shell, result) => {
            state.cli_tools_status = Some(match result {
                Ok(()) => format!(
                    "{} commands copied. Paste them into Terminal.",
                    shell.label()
                ),
                Err(e) => format!("Copy failed: {e}"),
            });
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
        Some(WindowKind::History) => history_view(state),
        Some(WindowKind::Settings) => settings_view(state),
        Some(WindowKind::CliTools) => cli_tools_view(state),
        Some(WindowKind::ScrollHud) => scroll_hud_view(state),
    }
}

/// Floating HUD showing the live state of a scrolling-capture session.
/// Renders a live preview of the most recently captured frame so the
/// user can see what's actually being captured, a frame counter with
/// capacity progress bar, the current activity status, and the
/// Stop / Cancel buttons.
fn scroll_hud_view(state: &App) -> Element<'_, Message> {
    use iced::widget::mouse_area;
    let session = state.scroll_session.as_ref();
    let frame_count = session.map(|s| s.frames.len()).unwrap_or(0);
    let stopping = session.map(|s| s.stopping).unwrap_or(false);
    let elapsed_secs = session
        .map(|s| s.started_at.elapsed().as_secs_f32())
        .unwrap_or(0.0);
    // Flash the counter for ~150 ms after each accepted frame so the
    // user gets a clear "yes, that scroll registered" signal.
    let flash = session
        .and_then(|s| s.last_frame_at)
        .map(|t| t.elapsed().as_millis() < 150)
        .unwrap_or(false);
    let cap = SCROLL_MAX_FRAMES;
    let progress = (frame_count as f32 / cap as f32).clamp(0.0, 1.0);

    // Header — title + recording / stitching chip.
    let (chip_label, chip_color): (&'static str, Color) = if stopping {
        ("Stitching", Color::from_rgba(1.0, 1.0, 1.0, 0.55))
    } else {
        ("● Recording", Color::from_rgba(0.95, 0.4, 0.4, 0.95))
    };
    let elapsed_label = format!(
        "{:02}:{:02}",
        (elapsed_secs as u32) / 60,
        (elapsed_secs as u32) % 60
    );
    // Whole header doubles as the drag handle. iced delegates to the
    // OS window drag on press so the user can reposition the HUD
    // without it stealing focus from the page they're scrolling.
    let header_row = row![
        text("⋮⋮ Scrolling Capture")
            .size(14)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.92)),
        Space::new().width(Length::Fill),
        text(elapsed_label)
            .size(10)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.50)),
        Space::new().width(Length::Fixed(8.0)),
        text(chip_label).size(10).color(chip_color),
    ]
    .align_y(Alignment::Center);
    let header = mouse_area(header_row)
        .on_press(Message::ScrollHudDragRequested)
        .interaction(iced::mouse::Interaction::Grab);

    // Live preview of the most recent accepted frame so the user can
    // confirm what's in the capture region while they scroll. Letter-
    // boxed inside a fixed-size bay so HUD layout stays stable across
    // very tall / very wide rects.
    let preview: Element<'_, Message> = match session.and_then(|s| s.last_frame_handle.clone()) {
        Some(handle) => container(
            iced::widget::image(handle)
                .width(Length::Fill)
                .height(Length::Fill)
                .content_fit(iced::ContentFit::Contain),
        )
        .width(Length::Fill)
        .height(Length::Fixed(SCROLL_HUD_PREVIEW_HEIGHT))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.4).into()),
            border: iced::Border {
                color: Color::from_rgba(0.45, 0.55, 1.0, 0.55),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into(),
        None => container(
            text("Waiting for first frame…")
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.45)),
        )
        .width(Length::Fill)
        .height(Length::Fixed(SCROLL_HUD_PREVIEW_HEIGHT))
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.04).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.12),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .into(),
    };

    // Frame counter — big number plus capacity tail. The number
    // briefly tints blue when a new frame just landed so the user
    // gets a visible "scroll registered" pulse.
    let counter_color = if flash {
        Color::from_rgba(0.65, 0.78, 1.0, 1.0)
    } else {
        Color::WHITE
    };
    let counter_row = row![
        text(format!("{frame_count}")).size(22).color(counter_color),
        text(format!("/ {cap} frames"))
            .size(11)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55)),
    ]
    .spacing(6)
    .align_y(Alignment::End);

    // Slim capacity bar — uses the same blue accent as the history
    // selected row so the chrome reads as part of the same app.
    let bar_w_max = SCROLL_HUD_WIDTH - 28.0;
    let filled_w = (bar_w_max * progress).max(2.0).min(bar_w_max);
    let bar = container(
        container(
            Space::new()
                .width(Length::Fixed(filled_w))
                .height(Length::Fixed(4.0)),
        )
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.45, 0.55, 1.0, 0.85).into()),
            border: iced::Border {
                radius: 2.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }),
    )
    .width(Length::Fixed(bar_w_max))
    .height(Length::Fixed(4.0))
    .style(|_| iced::widget::container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.08).into()),
        border: iced::Border {
            radius: 2.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });

    // Status line — what the user should be doing right now.
    let status_text = if stopping {
        "Stitching frames into one tall image…".to_string()
    } else {
        "Scroll the page underneath. Click Stop & Stitch (or Esc) when done.".to_string()
    };
    let status = text(status_text)
        .size(11)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.65))
        .wrapping(iced::widget::text::Wrapping::Word);

    // Buttons — Stop (primary) + Cancel (discards session).
    let stop_btn: Element<'_, Message> = if stopping {
        button(text("Stitching…").size(12))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .into()
    } else {
        button(text("Stop & Stitch").size(12))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Primary))
            .on_press(Message::ScrollCaptureStopRequested)
            .into()
    };
    let cancel_btn: Element<'_, Message> = if stopping {
        Space::new().width(Length::Fixed(0.0)).into()
    } else {
        button(text("Cancel").size(12))
            .padding([6, 12])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
            .on_press(Message::ScrollCaptureCancelRequested)
            .into()
    };
    let actions = row![stop_btn, cancel_btn]
        .spacing(8)
        .align_y(Alignment::Center);

    let body = column![
        header,
        preview,
        counter_row,
        bar,
        Space::new().height(Length::Fixed(2.0)),
        status,
        Space::new().height(Length::Fixed(4.0)),
        actions,
    ]
    .spacing(7)
    .align_x(Alignment::Start);
    container(body)
        .padding(14)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.07, 0.07, 0.08, 0.96).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.18),
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn cli_tools_view(state: &App) -> Element<'_, Message> {
    let common = crate::cli_tools::common_commands();
    let verify = crate::cli_tools::verify_commands();
    let status = state.cli_tools_status.as_deref().unwrap_or("");

    let shell_sections =
        crate::cli_tools::Shell::ALL
            .into_iter()
            .fold(column![].spacing(12), |sections, shell| {
                sections.push(
                    container(
                        column![
                            row![
                                text(format!("For {}", shell.label())).size(18),
                                Space::new().width(Length::Fill),
                                button(text(format!("Copy {} Commands", shell.label())).size(13))
                                    .on_press(Message::CliToolsCopyRequested(shell)),
                            ]
                            .spacing(12)
                            .align_y(Alignment::Center),
                            container(text(crate::cli_tools::shell_commands(shell)).size(13))
                                .padding(12)
                                .width(Length::Fill),
                        ]
                        .spacing(8),
                    )
                    .padding(10)
                    .width(Length::Fill),
                )
            });

    container(
        column![
            text("Command Line Tools").size(28),
            text("Choose your shell and copy only that command block into Terminal.").size(14),
            text(status).size(13),
            scrollable(
                column![
                    text("Run for every shell").size(18),
                    container(text(common).size(13))
                        .padding(12)
                        .width(Length::Fill),
                    shell_sections,
                    text("Verify").size(18),
                    container(text(verify).size(13))
                        .padding(12)
                        .width(Length::Fill),
                ]
                .spacing(12)
            )
            .direction(iced::widget::scrollable::Direction::Vertical(
                slim_scrollbar(),
            ))
            .spacing(10.0)
            .height(Length::Fill),
        ]
        .spacing(14)
        .padding(24)
        .width(Length::Fill)
        .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn history_view(state: &App) -> Element<'_, Message> {
    use iced::widget::{image as image_widget, mouse_area, text_input};

    let total = state.history_records.len();
    // Apply the live search filter. Empty query → every record.
    // Match is case-insensitive substring against `ocr_text` and the
    // human-readable timestamp (so "april" / "14:32" both work).
    let q = state.history_search.trim().to_lowercase();
    let visible: Vec<&readshot_core::CaptureRecord> = if q.is_empty() {
        state.history_records.iter().collect()
    } else {
        state
            .history_records
            .iter()
            .filter(|r| record_matches(r, &q))
            .collect()
    };
    let shown = visible.len();

    // Header: search box + count line + status.
    let count_line = if total == 0 {
        "No captures yet — take one and it shows up here.".to_string()
    } else if q.is_empty() {
        format!("{total} capture{}", if total == 1 { "" } else { "s" })
    } else {
        format!("{shown} of {total} match \u{201C}{q}\u{201D}")
    };
    let search_box = text_input("Search OCR text or timestamp…", &state.history_search)
        .on_input(Message::HistorySearchChanged)
        .width(Length::Fill)
        .padding(8)
        .size(13);
    let clear_button = {
        let b = button(text("Clear All").size(12))
            .padding([8, 10])
            .style(|t, s| action_button_style(t, s, ActionKind::Danger));
        if total > 0 {
            b.on_press(Message::HistoryClearAllRequested)
        } else {
            b
        }
    };
    let has_visible_text = visible.iter().any(|r| {
        r.ocr_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    });
    let copy_visible_button = {
        let b = button(text("Copy Visible Text").size(12))
            .padding([8, 10])
            .style(|t, s| action_button_style(t, s, ActionKind::Secondary));
        if has_visible_text {
            b.on_press(Message::HistoryCopyVisibleTextRequested)
        } else {
            b
        }
    };
    let search_row = row![search_box, copy_visible_button, clear_button]
        .spacing(8)
        .align_y(Alignment::Center);
    let selected = state
        .history_selected_id
        .and_then(|id| visible.iter().copied().find(|record| record.id == id));
    let selected_panel: Element<'_, Message> = if visible.is_empty() {
        Space::new().height(Length::Fixed(0.0)).into()
    } else {
        history_selected_panel(selected)
    };
    let header = container(
        column![
            search_row,
            text(count_line).size(12),
            state
                .history_status
                .as_deref()
                .map(|s| text(s).size(12).color(Color::from_rgb(0.95, 0.55, 0.25)))
                .unwrap_or_else(|| text("")),
            selected_panel,
        ]
        .spacing(6),
    )
    .padding([12, 16]);

    // Records list — filtered.
    let mut col = column![].spacing(8).padding(iced::Padding {
        top: 0.0,
        right: 16.0,
        bottom: 16.0,
        left: 16.0,
    });
    if let Some(root) = state.history_root.as_ref() {
        if visible.is_empty() {
            let empty = if total == 0 {
                empty_state_card(
                    "No captures yet",
                    "Take a screenshot and it will appear here with its image, OCR text, and quick actions.",
                    Some(("Capture", Message::OpenOverlayRequested)),
                )
            } else {
                empty_state_card(
                    "No matching captures",
                    "Try a different search or clear the filter to see the full history.",
                    Some(("Clear Search", Message::HistorySearchChanged(String::new()))),
                )
            };
            col = col.push(container(empty).padding([28, 0]));
        }
        for r in &visible {
            let png_path = history_png_path(root, r);
            let preview_path = history_thumbnail_path(root, r);
            let stamp = r
                .captured_at
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d  %H:%M:%S")
                .to_string();
            let dims = format!("{} × {} px", r.width_px, r.height_px);
            let snippet = r
                .ocr_text
                .as_ref()
                .map(|t| {
                    let trimmed = t.chars().take(120).collect::<String>();
                    if t.chars().count() > 120 {
                        format!("{trimmed}…")
                    } else {
                        trimmed
                    }
                })
                .unwrap_or_else(|| "(no OCR text yet)".to_string());

            let thumb: Element<Message> = if preview_path.exists() || png_path.exists() {
                let path = if preview_path.exists() {
                    preview_path
                } else {
                    png_path
                };
                // Letterbox the thumbnail inside a fixed 132×92 frame
                // with a dim background so tall portrait captures don't
                // sit flush against the row text — the dim border reads
                // as deliberate framing rather than image stretching.
                container(
                    image_widget(image_widget::Handle::from_path(path))
                        .width(Length::Fixed(132.0))
                        .height(Length::Fixed(92.0))
                        .content_fit(iced::ContentFit::Contain),
                )
                .width(Length::Fixed(132.0))
                .height(Length::Fixed(92.0))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(|_| iced::widget::container::Style {
                    background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.28).into()),
                    border: iced::Border {
                        radius: 4.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into()
            } else {
                container(text("Missing"))
                    .width(Length::Fixed(132.0))
                    .height(Length::Fixed(92.0))
                    .center_x(Length::Fill)
                    .center_y(Length::Fill)
                    .into()
            };

            let meta = column![
                text(stamp).size(13),
                text(dims)
                    .size(11)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.6)),
                text(snippet)
                    .size(11)
                    .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
            ]
            .spacing(4)
            .width(Length::Fill);

            let is_selected = state.history_selected_id == Some(r.id);
            let actions = history_row_actions(r.id, is_selected);
            let row_widget = container(
                row![thumb, meta, actions]
                    .spacing(12)
                    .align_y(Alignment::Center),
            )
            .padding(10)
            .style(move |_| iced::widget::container::Style {
                background: Some(
                    if is_selected {
                        history_selected_fill()
                    } else {
                        Color::from_rgba(1.0, 1.0, 1.0, 0.04)
                    }
                    .into(),
                ),
                border: iced::Border {
                    color: if is_selected {
                        history_selected_border()
                    } else {
                        Color::TRANSPARENT
                    },
                    width: if is_selected { 1.0 } else { 0.0 },
                    radius: 6.0.into(),
                },
                ..Default::default()
            });
            col = col.push(
                mouse_area(row_widget)
                    .on_press(Message::HistorySelect(r.id))
                    .on_double_click(Message::HistoryOpenInEditor(r.id))
                    .interaction(iced::mouse::Interaction::Pointer),
            );
        }
    } else {
        col = col.push(container(empty_state_card(
            "History unavailable",
            "Readshot could not resolve the history folder for this session.",
            None,
        )));
    }

    column![
        header,
        scrollable(col)
            .direction(iced::widget::scrollable::Direction::Vertical(
                slim_scrollbar(),
            ))
            .spacing(10.0)
            .height(Length::Fill)
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

fn history_selected_panel<'a>(
    record: Option<&'a readshot_core::CaptureRecord>,
) -> Element<'a, Message> {
    let Some(record) = record else {
        return container(
            text("Select a capture to preview details. Use ↑/↓, Enter, and Delete in this window.")
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55)),
        )
        .padding([8, 10])
        .width(Length::Fill)
        .style(history_selected_panel_style)
        .into();
    };

    let stamp = record
        .captured_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let text_count = record
        .ocr_text
        .as_deref()
        .map(|text| text.trim().chars().count())
        .unwrap_or(0);
    let has_text = text_count > 0;
    let id = record.id;

    container(responsive(move |available| {
        let summary = column![
            text("Selected capture")
                .size(11)
                .color(settings_muted_text()),
            text(format!(
                "{} · {} x {} px · {} OCR chars",
                stamp, record.width_px, record.height_px, text_count
            ))
            .size(12),
        ]
        .spacing(3);
        let secondary = |label: &'static str, msg: Message| {
            button(text(label).size(12))
                .padding([6, 9])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(msg)
        };
        let copy_text_button: Element<'_, Message> = if has_text {
            secondary("Copy Text", Message::HistoryCopyText(id)).into()
        } else {
            button(text("Copy Text").size(12))
                .padding([6, 9])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .into()
        };
        let primary_actions = row![
            secondary("Open", Message::HistoryOpenInEditor(id)),
            secondary("Reveal", Message::HistoryReveal(id)),
            copy_text_button,
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let secondary_actions = row![
            secondary("Copy Image", Message::HistoryCopyImage(id)),
            secondary("Pin", Message::HistoryPin(id)),
            button(text("Delete").size(12))
                .padding([6, 9])
                .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                .on_press(Message::HistoryDelete(id)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if available.width < 600.0 {
            column![summary, primary_actions, secondary_actions]
                .spacing(8)
                .into()
        } else if available.width < 820.0 {
            row![
                summary.width(Length::Fill),
                column![primary_actions, secondary_actions].spacing(6)
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                summary.width(Length::Fill),
                primary_actions,
                secondary_actions
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
        }
    }))
    .height(Length::Shrink)
    .padding([8, 10])
    .width(Length::Fill)
    .style(history_selected_panel_style)
    .into()
}

/// Shared accent colour for the history row's selected state — used
/// by the row background, the "Selected" chip, the selected border,
/// and the chip's foreground text so all three read as the same
/// accent rather than three sibling blues drifting apart.
const HISTORY_SELECTED_RGB: (f32, f32, f32) = (0.45, 0.55, 1.0);
const HISTORY_SELECTED_FILL_ALPHA: f32 = 0.14;
const HISTORY_SELECTED_CHIP_ALPHA: f32 = 0.20;
const HISTORY_SELECTED_BORDER_ALPHA: f32 = 0.55;

fn history_selected_fill() -> Color {
    let (r, g, b) = HISTORY_SELECTED_RGB;
    Color::from_rgba(r, g, b, HISTORY_SELECTED_FILL_ALPHA)
}

fn history_selected_chip() -> Color {
    let (r, g, b) = HISTORY_SELECTED_RGB;
    Color::from_rgba(r, g, b, HISTORY_SELECTED_CHIP_ALPHA)
}

fn history_selected_border() -> Color {
    let (r, g, b) = HISTORY_SELECTED_RGB;
    Color::from_rgba(r, g, b, HISTORY_SELECTED_BORDER_ALPHA)
}

fn history_selected_panel_style(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.045).into()),
        border: iced::Border {
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

/// Compact row-level affordance. The whole row is already pressable
/// via `mouse_area`, so non-selected rows just show a chevron hint
/// that the row leads somewhere; selected rows show a labelled chip
/// so the active row is unambiguous. The real Open / Copy / Reveal
/// commands live in the selected-capture panel above the list.
fn history_row_actions<'a>(_id: readshot_core::Uuid, is_selected: bool) -> Element<'a, Message> {
    if is_selected {
        return container(
            text("Selected")
                .size(12)
                .color(Color::from_rgb(0.88, 0.9, 1.0)),
        )
        .padding([6, 10])
        .style(|_| iced::widget::container::Style {
            background: Some(history_selected_chip().into()),
            border: iced::Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into();
    }
    container(
        text("›")
            .size(18)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.35)),
    )
    .padding([6, 12])
    .into()
}

/// True when `record` matches the lowercase search query `q`. Tries
/// the OCR text and the timestamp formatted in the local zone — the
/// latter lets users search "2026-04" or "14:32" naturally.
fn record_matches(record: &readshot_core::CaptureRecord, q: &str) -> bool {
    if let Some(ocr) = record.ocr_text.as_deref() {
        if ocr.to_lowercase().contains(q) {
            return true;
        }
    }
    if record.display_id.to_lowercase().contains(q) {
        return true;
    }
    let dims = format!(
        "{}x{} {} {}",
        record.width_px, record.height_px, record.width_px, record.height_px
    );
    if dims.contains(q) {
        return true;
    }
    let stamp = record
        .captured_at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
        .to_lowercase();
    stamp.contains(q)
}

fn visible_history_ids(
    records: &[readshot_core::CaptureRecord],
    query: &str,
) -> Vec<readshot_core::Uuid> {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .map(|r| r.id)
        .collect()
}

fn history_record_visible(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    id: readshot_core::Uuid,
) -> bool {
    visible_history_ids(records, query).contains(&id)
}

fn preferred_history_selection(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    current: Option<readshot_core::Uuid>,
) -> Option<readshot_core::Uuid> {
    let visible = visible_history_ids(records, query);
    current
        .filter(|id| visible.contains(id))
        .or_else(|| visible.first().copied())
}

fn adjacent_history_selection(
    records: &[readshot_core::CaptureRecord],
    query: &str,
    current: Option<readshot_core::Uuid>,
    delta: isize,
) -> Option<readshot_core::Uuid> {
    let visible = visible_history_ids(records, query);
    if visible.is_empty() {
        return None;
    }
    let current_idx = current
        .and_then(|id| visible.iter().position(|candidate| *candidate == id))
        .unwrap_or(0);
    let max = visible.len() as isize - 1;
    let next_idx = (current_idx as isize + delta).clamp(0, max) as usize;
    visible.get(next_idx).copied()
}

fn visible_history_text(records: &[readshot_core::CaptureRecord], query: &str) -> String {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .filter_map(|r| r.ocr_text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn visible_history_text_count(records: &[readshot_core::CaptureRecord], query: &str) -> usize {
    let q = query.trim().to_lowercase();
    records
        .iter()
        .filter(|r| q.is_empty() || record_matches(r, &q))
        .filter(|r| {
            r.ocr_text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty())
        })
        .count()
}

/// Load + decode a PNG file off the main thread so a multi-MB Retina
/// capture doesn't stall the iced runtime. Returns the decoded image
/// in the same `RgbaImage` shape the capture pipeline produces.
async fn load_png_async(path: PathBuf) -> Result<image::RgbaImage, image::ImageError> {
    tokio::task::spawn_blocking(move || -> Result<image::RgbaImage, image::ImageError> {
        let dyn_img = image::ImageReader::open(&path)?
            .with_guessed_format()?
            .decode()?;
        Ok(dyn_img.to_rgba8())
    })
    .await
    .unwrap_or_else(|join_err| {
        // Surface a join failure as an IO error so the caller's
        // `Result<RgbaImage, ImageError>` handling stays uniform.
        Err(image::ImageError::IoError(std::io::Error::other(format!(
            "blocking task panicked: {join_err}"
        ))))
    })
}

/// Resolve the absolute PNG path for a history record. The on-disk
/// layout is `<root>/<YYYY>/<MM>/<uuid>.png`; we mirror that here so
/// the browser can render thumbnails without round-tripping through
/// the index.
fn history_png_path(root: &std::path::Path, record: &readshot_core::CaptureRecord) -> PathBuf {
    use chrono::Datelike;
    root.join(format!("{:04}", record.captured_at.year()))
        .join(format!("{:02}", record.captured_at.month()))
        .join(format!("{}.png", record.id))
}

fn history_thumbnail_path(
    root: &std::path::Path,
    record: &readshot_core::CaptureRecord,
) -> PathBuf {
    use chrono::Datelike;
    root.join(format!("{:04}", record.captured_at.year()))
        .join(format!("{:02}", record.captured_at.month()))
        .join(format!("{}.thumb.png", record.id))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RevealCommand {
    program: &'static str,
    args: Vec<String>,
}

fn reveal_command_for_path(path: &Path) -> RevealCommand {
    let path_string = path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        RevealCommand {
            program: "open",
            args: vec!["-R".into(), path_string],
        }
    }
    #[cfg(target_os = "windows")]
    {
        RevealCommand {
            program: "explorer",
            args: vec!["/select,".into(), path_string],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let dir = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or(path_string);
        RevealCommand {
            program: "xdg-open",
            args: vec![dir],
        }
    }
}

fn reveal_path(path: &Path) -> Result<(), std::io::Error> {
    let command = reveal_command_for_path(path);
    std::process::Command::new(command.program)
        .args(command.args)
        .spawn()?;
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OpenFolderCommand {
    program: &'static str,
    args: Vec<String>,
}

fn open_folder_command_for_path(path: &Path) -> OpenFolderCommand {
    let path_string = path.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        OpenFolderCommand {
            program: "open",
            args: vec![path_string],
        }
    }
    #[cfg(target_os = "windows")]
    {
        OpenFolderCommand {
            program: "explorer",
            args: vec![path_string],
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        OpenFolderCommand {
            program: "xdg-open",
            args: vec![path_string],
        }
    }
}

fn open_folder_path(path: &Path) -> Result<(), std::io::Error> {
    let command = open_folder_command_for_path(path);
    std::process::Command::new(command.program)
        .args(command.args)
        .spawn()?;
    Ok(())
}

fn editor_view(state: &App) -> Element<'_, Message> {
    use iced::widget::canvas::Canvas;
    use iced::widget::{stack, Space as IcedSpace};
    use readshot_ui::editor::{canvas::EditorCanvas, toolbar, ToolState};

    let Some(ed) = state.editor.as_ref() else {
        return container(empty_state_card(
            "No capture open",
            "Start a capture to annotate, copy, save, pin, or extract text.",
            Some(("Capture", Message::OpenOverlayRequested)),
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
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
                // Swap the selected ring colour to a dark stroke when the
                // swatch itself is bright, otherwise white-on-white makes
                // the selected swatch invisible. sRGB relative luminance.
                let lum = 0.2126 * swatch.r + 0.7152 * swatch.g + 0.0722 * swatch.b;
                let selected_ring = if lum > 0.72 {
                    Color::from_rgba(0.0, 0.0, 0.0, 0.85)
                } else {
                    Color::WHITE
                };
                let border = if is_selected {
                    iced::Border {
                        color: selected_ring,
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

    let undo_depth = ed.model.undo_depth();
    let redo_depth = ed.model.redo_depth();
    let undo_tip = if undo_depth > 0 {
        format!(
            "Undo (⌘Z) · {undo_depth} action{}",
            if undo_depth == 1 { "" } else { "s" }
        )
    } else {
        "Undo (⌘Z)".to_string()
    };
    let redo_tip = if redo_depth > 0 {
        format!(
            "Redo (⌘⇧Z) · {redo_depth} action{}",
            if redo_depth == 1 { "" } else { "s" }
        )
    } else {
        "Redo (⌘⇧Z)".to_string()
    };
    let undo_btn = ghost_icon_button(
        crate::editor_icons::EditorIcon::Undo,
        undo_tip,
        !busy && ed.model.can_undo(),
        || Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
    );
    let redo_btn = ghost_icon_button(
        crate::editor_icons::EditorIcon::Redo,
        redo_tip,
        !busy && ed.model.can_redo(),
        || Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
    );

    // Single combined toolbar: tools | divider | colors | width |
    // spacer | undo redo. Keep it horizontally scrollable so narrow
    // editor windows do not crush the controls into the image area.
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

    let toolbar_row = container(
        scrollable(toolbar_inner)
            .direction(iced::widget::scrollable::Direction::Horizontal(
                slim_scrollbar(),
            ))
            .height(Length::Shrink)
            .width(Length::Fill),
    )
    .style(|theme: &Theme| {
        let palette = theme.extended_palette();
        container::Style {
            background: Some(palette.background.weak.color.into()),
            border: iced::Border {
                color: palette.background.strong.color,
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        }
    });

    // ===== Image area — letterboxed image with canvas overlay =====
    // Both layers share the same container; the canvas Program knows
    // the image's effective dimensions + crop offset so cursor maps
    // to base-image coordinates regardless of zoom / letterbox /
    // crop. See `canvas_to_base` in readshot-ui.
    let image_handle = ed.image_handle.clone();
    let (image_w, image_h) = ed.effective_image_size();
    let image_offset = ed.crop_offset();
    let zoom = ed.zoom;
    let display_scale = ed.display_scale;
    let next_pin_number = ed.next_pin_number;
    let image_area_content = responsive(move |available| {
        let iw = image_w as f32;
        let ih = image_h as f32;
        let fit_scale = editor_fit_scale(available, image_w, image_h);
        let scale = zoom.explicit_scale().unwrap_or(fit_scale);
        let displayed_w = (iw * scale).max(1.0);
        let displayed_h = (ih * scale).max(1.0);
        let content_w = displayed_w.max(available.width);
        let content_h = displayed_h.max(available.height);

        let filter = editor_image_filter(scale, display_scale);

        let image_layer = container(
            iced::widget::image(image_handle.clone())
                .width(Length::Fixed(displayed_w))
                .height(Length::Fixed(displayed_h))
                .content_fit(iced::ContentFit::Contain)
                .filter_method(filter),
        )
        .width(Length::Fixed(content_w))
        .height(Length::Fixed(content_h))
        .center_x(Length::Fill)
        .center_y(Length::Fill);

        let canvas_program = EditorCanvas {
            active_tool,
            color: active_color,
            line_width,
            next_pin_number,
            image_size: (image_w, image_h),
            image_offset,
            display_scale: Some(scale),
        };
        let canvas: Element<'_, readshot_ui::CanvasMessage> = Canvas::new(canvas_program)
            .width(Length::Fixed(content_w))
            .height(Length::Fixed(content_h))
            .into();
        let canvas: Element<'_, Message> = canvas.map(Message::EditorCanvas);
        let canvas_layer = container(canvas)
            .width(Length::Fixed(content_w))
            .height(Length::Fixed(content_h));

        let content = container(stack![image_layer, canvas_layer])
            .width(Length::Fixed(content_w))
            .height(Length::Fixed(content_h));

        let zoom_row = row![
            text("Zoom")
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55)),
            zoom_button(
                "-",
                "Zoom out",
                Message::EditorZoomOutFromDisplayScale(scale),
                zoom.can_zoom_out_from_display_scale(scale),
                false,
                busy,
            ),
            text(zoom.label_for_display_scale(display_scale))
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.72))
                .width(Length::Fixed(50.0))
                .align_x(iced::alignment::Horizontal::Center),
        ];
        let zoom_controls = container(
            zoom_row
                .push(zoom_button(
                    "+",
                    "Zoom in",
                    Message::EditorZoomInFromDisplayScale(scale),
                    zoom.can_zoom_in_from_display_scale(scale),
                    false,
                    busy,
                ))
                .push(zoom_button(
                    "1:1",
                    "Actual pixels (100%)",
                    Message::EditorZoomActual,
                    true,
                    zoom.is_actual_size_for_display_scale(display_scale),
                    busy,
                ))
                .push(zoom_button(
                    "Fit",
                    "Fit to view",
                    Message::EditorZoomFit,
                    true,
                    zoom.is_fit(),
                    busy,
                ))
                .spacing(4)
                .align_y(Alignment::Center),
        )
        .padding([3, 6])
        .style(|_theme: &Theme| container::Style {
            background: Some(Color::from_rgba(0.03, 0.035, 0.045, 0.82).into()),
            border: iced::Border {
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.14),
                width: 1.0,
                radius: 8.0.into(),
            },
            ..Default::default()
        });
        let zoom_layer = container(zoom_controls)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(10)
            .align_x(Alignment::Start)
            .align_y(Alignment::End);

        let scroll_layer = scrollable(content)
            .direction(iced::widget::scrollable::Direction::Both {
                vertical: slim_scrollbar(),
                horizontal: slim_scrollbar(),
            })
            .width(Length::Fill)
            .height(Length::Fill);

        stack![scroll_layer, zoom_layer].into()
    })
    .width(Length::Fill)
    .height(Length::Fill);
    let image_area = container(image_area_content)
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

    // ===== Bottom row — dims/hint + actions =====
    let (img_w, img_h) = ed.effective_image_size();
    // When the user just opened the editor and hasn't drawn anything
    // yet, show a discoverable "press a key to pick a tool" hint
    // in place of the per-tool guidance — the keyboard shortcuts
    // aren't visible anywhere in the chrome until the user hovers
    // a tool button, so this is the surface that surfaces them.
    let hint_str = if ed.model.annotations().is_empty()
        && active_tool == ToolState::Select
        && ed.status.is_none()
    {
        "Press V / R / O / L / A / P / H / T / B / X / N / C to pick a tool · ⌘Z to undo"
    } else {
        tool_hint(active_tool)
    };
    let hint_text = hint_str.to_string();
    let status_text = ed.status.clone();
    let bottom_row: Element<'_, Message> = responsive(move |available| {
        let layout = editor_bottom_layout(available.width);
        let compact = layout == EditorBottomLayout::Compact;
        let dims = text(format!("{img_w} × {img_h} px"))
            .size(11)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55));
        let hint_copy = if compact {
            editor_compact_hint(active_tool).to_string()
        } else {
            hint_text.clone()
        };
        let hint = text(hint_copy)
            .size(11)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.72))
            .width(Length::Fill)
            .wrapping(if compact {
                iced::widget::text::Wrapping::Word
            } else {
                iced::widget::text::Wrapping::None
            });
        let toast: Element<'_, Message> = match status_text.clone() {
            Some(s) => {
                let lower = s.to_lowercase();
                let is_error = lower.contains("fail") || lower.contains("error");
                let in_progress = lower.starts_with("copying") || lower.contains("recognising");
                let (bg, fg) = if is_error {
                    (
                        Color::from_rgba(0.85, 0.32, 0.32, 0.22),
                        Color::from_rgba(1.0, 0.78, 0.78, 1.0),
                    )
                } else if in_progress {
                    (
                        Color::from_rgba(1.0, 1.0, 1.0, 0.10),
                        Color::from_rgba(1.0, 1.0, 1.0, 0.85),
                    )
                } else {
                    (
                        Color::from_rgba(0.30, 0.65, 0.45, 0.24),
                        Color::from_rgba(0.78, 1.0, 0.88, 1.0),
                    )
                };
                container(
                    text(s)
                        .size(11)
                        .color(fg)
                        .wrapping(iced::widget::text::Wrapping::Word),
                )
                .padding([3, 8])
                .style(move |_| iced::widget::container::Style {
                    background: Some(bg.into()),
                    border: iced::Border {
                        radius: 9.0.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                })
                .into()
            }
            None => IcedSpace::new().height(Length::Fixed(0.0)).into(),
        };
        let status_area = if compact {
            container(column![dims, hint, toast].spacing(3))
                .width(Length::Fill)
                .clip(true)
        } else {
            let bullet = text("·")
                .size(11)
                .color(Color::from_rgba(1.0, 1.0, 1.0, 0.35));
            container(
                row![
                    dims,
                    IcedSpace::new().width(Length::Fixed(8.0)),
                    bullet,
                    IcedSpace::new().width(Length::Fixed(8.0)),
                    hint,
                    IcedSpace::new().width(Length::Fixed(8.0)),
                    toast,
                ]
                .spacing(0)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .clip(true)
        };

        let discard = editor_action_button(
            "Discard",
            Message::EditorDiscardRequested,
            ActionKind::Danger,
            busy,
        );
        let pin = editor_action_button(
            "Pin",
            Message::EditorPinRequested,
            ActionKind::Secondary,
            busy,
        );
        let copy_text = editor_action_button(
            "Copy Text",
            Message::EditorCopyTextRequested,
            ActionKind::Secondary,
            busy,
        );
        let copy_image = editor_action_button(
            "Copy Image",
            Message::EditorCopyImageRequested,
            ActionKind::Secondary,
            busy,
        );
        let save = editor_action_button(
            "Save",
            Message::EditorSaveRequested,
            ActionKind::Primary,
            busy,
        );

        match layout {
            EditorBottomLayout::Wide => row![
                status_area,
                row![
                    discard,
                    IcedSpace::new().width(Length::Fixed(8.0)),
                    pin,
                    copy_text,
                    copy_image,
                    save,
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into(),
            EditorBottomLayout::Stacked => column![
                status_area,
                row![discard, pin, copy_text, copy_image, save]
                    .spacing(6)
                    .align_y(Alignment::Center)
            ]
            .spacing(8)
            .into(),
            EditorBottomLayout::Compact => column![
                status_area,
                row![discard, pin, save]
                    .spacing(6)
                    .align_y(Alignment::Center),
                row![copy_text, copy_image]
                    .spacing(6)
                    .align_y(Alignment::Center),
            ]
            .spacing(8)
            .into(),
        }
    })
    .height(Length::Shrink)
    .into();

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
        let icon = container(crate::editor_icons::editor_icon(
            crate::editor_icons::EditorIcon::Tool(readshot_ui::editor::ToolState::Text),
            true,
        ))
        .width(Length::Fixed(22.0))
        .height(Length::Fixed(22.0));
        container(
            row![icon, input, commit, cancel]
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
    let Some(pin) = state.pins.get(&id) else {
        return container(text("(no pin)"))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
    };
    let handle = pin.handle.clone();
    let opacity = pin.opacity;
    let locked = pin.locked;
    let img = iced::widget::image(handle)
        .width(Length::Fill)
        .height(Length::Fill)
        .content_fit(iced::ContentFit::Contain)
        .opacity(opacity);
    let drag_area = mouse_area(img).on_double_click(Message::PinClosePressed(id));
    let drag_layer: Element<'_, Message> = if locked {
        drag_area.into()
    } else {
        drag_area
            .on_press(Message::PinDragRequested(id))
            .interaction(iced::mouse::Interaction::Grab)
            .into()
    };

    let lock = button(text(if locked { "Unlock" } else { "Lock" }).size(11))
        .padding([4, 8])
        .style(|_, status| {
            let bg = match status {
                button::Status::Hovered => Color::from_rgba(0.0, 0.0, 0.0, 0.72),
                _ => Color::from_rgba(0.0, 0.0, 0.0, 0.55),
            };
            button::Style {
                background: Some(bg.into()),
                text_color: Color::WHITE,
                border: iced::Border {
                    color: Color::from_rgba(1.0, 1.0, 1.0, 0.28),
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            }
        })
        .on_press(Message::PinLockToggled(id));
    let opacity_label = text(format!("{:.0}%", opacity * 100.0))
        .size(11)
        .color(Color::WHITE)
        .width(Length::Shrink);
    let opacity_slider = iced::widget::slider(0.2..=1.0, opacity, move |value| {
        Message::PinOpacityChanged(id, value)
    })
    .step(0.05)
    .width(Length::Fixed(92.0));
    let controls = row![lock, opacity_slider, opacity_label]
        .spacing(6)
        .align_y(Alignment::Center);
    let controls_layer = container(controls)
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(6)
        .align_x(Alignment::Start)
        .align_y(Alignment::End);

    // Close button — sits in the top-right corner with subtle
    // styling so it's discoverable without dominating the pin.
    let close = button(
        text("×")
            .size(20)
            .color(Color::WHITE)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(iced::alignment::Vertical::Center)
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .padding(0)
    .width(Length::Fixed(28.0))
    .height(Length::Fixed(28.0))
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
                radius: 14.0.into(),
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
    container(stack![drag_layer, close_layer, controls_layer])
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
    let (long, key) = tool_label_and_key(t);
    let is_active = t == active;
    let icon = crate::editor_icons::editor_icon(crate::editor_icons::EditorIcon::Tool(t), !busy);
    let mut b = button(icon)
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
    icon: crate::editor_icons::EditorIcon,
    tip: impl Into<String>,
    enabled: bool,
    gen: F,
) -> Element<'a, Message>
where
    F: Fn() -> Message + 'a,
{
    let tip = tip.into();
    use iced::widget::tooltip;
    let mut b = button(crate::editor_icons::editor_icon(icon, enabled))
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

fn zoom_button<'a>(
    label: &'static str,
    tip: &'static str,
    msg: Message,
    enabled: bool,
    selected: bool,
    busy: bool,
) -> Element<'a, Message> {
    use iced::widget::tooltip;

    let mut b = button(text(label).size(11).color(Color::WHITE))
        .padding([5, 9])
        .style(move |theme, status| zoom_button_style(theme, status, enabled, selected));
    if !busy && enabled {
        b = b.on_press(msg);
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

fn zoom_button_style(
    _theme: &Theme,
    status: button::Status,
    enabled: bool,
    selected: bool,
) -> button::Style {
    let background = if selected {
        Color::from_rgba(0.16, 0.44, 0.92, 1.0)
    } else if !enabled {
        Color::from_rgba(1.0, 1.0, 1.0, 0.03)
    } else if matches!(status, button::Status::Hovered) {
        Color::from_rgba(1.0, 1.0, 1.0, 0.14)
    } else {
        Color::from_rgba(1.0, 1.0, 1.0, 0.07)
    };
    button::Style {
        background: Some(background.into()),
        text_color: if enabled {
            Color::WHITE
        } else {
            Color::from_rgba(1.0, 1.0, 1.0, 0.35)
        },
        border: iced::Border {
            radius: 6.0.into(),
            width: if selected { 1.0 } else { 0.0 },
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.20),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorBottomLayout {
    Wide,
    Stacked,
    Compact,
}

fn editor_bottom_layout(width: f32) -> EditorBottomLayout {
    if width < 520.0 {
        EditorBottomLayout::Compact
    } else if width < 860.0 {
        EditorBottomLayout::Stacked
    } else {
        EditorBottomLayout::Wide
    }
}

fn editor_action_button<'a>(
    label: &'static str,
    msg: Message,
    kind: ActionKind,
    busy: bool,
) -> Element<'a, Message> {
    let lbl = text(label).size(13).color(Color::WHITE);
    let mut b = button(lbl)
        .padding([8, 16])
        .style(move |theme, status| action_button_style(theme, status, kind));
    if !busy {
        b = b.on_press(msg);
    }
    b.into()
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
    // Disabled state: dim the background to ~40% of its base alpha and
    // mute the text. Without this the button looks identical whether
    // its `on_press` is wired or not — confusing for the user when
    // there's nothing to copy / nothing to clear.
    let (bg, fg) = match status {
        button::Status::Hovered => (hover, text_color),
        button::Status::Disabled => {
            let dim = Color {
                a: base.a * 0.4,
                ..base
            };
            let muted = Color {
                a: 0.5,
                ..text_color
            };
            (dim, muted)
        }
        _ => (base, text_color),
    };
    button::Style {
        background: Some(bg.into()),
        text_color: fg,
        border: iced::Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
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

fn editor_compact_hint(tool: readshot_ui::editor::ToolState) -> &'static str {
    use readshot_ui::editor::ToolState as T;
    match tool {
        T::Select => "Tools: R/O/L/A/P/H/T/B/X/N/C · Undo: ⌘Z",
        T::Rectangle => "Drag to draw a rectangle.",
        T::Ellipse => "Drag to draw an ellipse.",
        T::Line => "Drag start to end.",
        T::Arrow => "Drag base to target.",
        T::Pen => "Drag to free-draw.",
        T::Highlighter => "Drag over text.",
        T::Text => "Click, type, Enter.",
        T::Blur => "Drag a region to blur.",
        T::Pixelate => "Drag a region to pixelate.",
        T::NumberedPin => "Click to drop a number.",
        T::Crop => "Drag the region to keep.",
    }
}

fn swatch_eq(a: readshot_core::Rgba, b: readshot_core::Rgba) -> bool {
    (a.r - b.r).abs() < 1e-3
        && (a.g - b.g).abs() < 1e-3
        && (a.b - b.b).abs() < 1e-3
        && (a.a - b.a).abs() < 1e-3
}

fn editor_fit_scale(available: iced::Size, image_w: u32, image_h: u32) -> f32 {
    let iw = image_w as f32;
    let ih = image_h as f32;
    if iw <= 0.0 || ih <= 0.0 {
        return 1.0;
    }
    (available.width / iw)
        .min(available.height / ih)
        .max(f32::EPSILON)
}

fn editor_image_filter(scale: f32, display_scale: f32) -> iced::widget::image::FilterMethod {
    let physical_scale = scale * display_scale.max(f32::EPSILON);
    if (physical_scale - 1.0).abs() < 0.001 {
        iced::widget::image::FilterMethod::Nearest
    } else {
        iced::widget::image::FilterMethod::Linear
    }
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
    let auto_confirm_intent = overlay_auto_confirm_intent(state);
    let cli_interactive = auto_confirm_intent == Some(crate::app::CaptureIntent::CliInteractive);

    let canvas = Canvas::new(crate::overlay::OverlayProgram {
        display_id,
        // Multiply the runtime tick to advance the dash pattern
        // smoothly (each dash period is ~10 logical px).
        dash_offset: state.overlay_tick as usize,
        scale,
        auto_confirm_intent,
        shift_held: state.overlay_shift_held,
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
    let hint_text = if cli_interactive {
        "Drag to capture · hold Shift for square · Enter for full screen · Esc to cancel"
    } else {
        "Drag to select · hold Shift for square · Enter for full screen · Esc to cancel"
    };
    let hint = container(text(hint_text).size(13).color(Color::WHITE))
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
    let toolbar_layer = if cli_interactive {
        None
    } else {
        overlay_record
            .and_then(|d| {
                state
                    .overlay_selections
                    .get(&d.display_id)
                    .map(|rect| (d, rect))
            })
            .map(|(d, rect)| overlay_toolbar_layer(&d.display_id, rect, d.width, d.height))
    };

    // Once the toolbar is up the introductory hint is just noise.
    if let Some(toolbar) = toolbar_layer {
        stack![canvas_layer, toolbar].into()
    } else {
        stack![canvas_layer, hint_layer].into()
    }
}

fn overlay_auto_confirm_intent(state: &App) -> Option<crate::app::CaptureIntent> {
    if matches!(
        state.pending_intent,
        Some(crate::app::CaptureIntent::CliInteractive)
    ) || state.cli_interactive_output.is_some()
    {
        Some(crate::app::CaptureIntent::CliInteractive)
    } else {
        None
    }
}

/// Estimated visual size of the floating overlay toolbar. Used for
/// edge-aware reflow without measuring real layout (which iced
/// doesn't expose mid-build).
const OVERLAY_TOOLBAR_HEIGHT: f32 = 44.0;
const OVERLAY_TOOLBAR_GAP: f32 = 8.0;
/// Conservative estimate of the floating toolbar's rendered width.
/// iced doesn't expose mid-layout widget measurement, so the value is
/// hand-tuned against the actual button row (6 buttons, ~78px each
/// after padding + the row's internal spacing). Slight over-estimate
/// is fine — it just means the clamp engages a few px earlier.
const OVERLAY_TOOLBAR_WIDTH: f32 = 560.0;

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

    use iced::widget::tooltip;
    let make_btn = |label: &'static str, tip: &'static str, msg: Message| -> Element<'a, Message> {
        let btn = button(text(label).size(13).color(Color::WHITE))
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
            .on_press(msg);
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
        tooltip::Tooltip::new(btn, pop, tooltip::Position::Bottom)
            .gap(4)
            .into()
    };

    let buttons = row![
        make_btn(
            "✓ Capture",
            "Open in editor (Enter)",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::Editor,
            },
        ),
        make_btn(
            "Copy Image",
            "Copy selection to clipboard",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::CopyToClipboard,
            },
        ),
        make_btn(
            "Copy Text",
            "Run OCR and copy recognized text",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::CopyTextDirect,
            },
        ),
        make_btn(
            "Save",
            "Save to default folder",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::SaveDirect,
            },
        ),
        make_btn(
            "Pin",
            "Pin as always-on-top window",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::Pin,
            },
        ),
        make_btn(
            "↕ Scroll",
            "Scrolling capture — capture this region repeatedly as you scroll, then stitch into one tall image",
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect: *rect,
                intent: CaptureIntent::ScrollCapture,
            },
        ),
        make_btn("✕", "Cancel (Esc)", Message::OverlayCancelled),
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

    // Anchor the toolbar's right edge at the selection's right edge,
    // but clamp so the toolbar never extends past the left edge of the
    // overlay when the selection sits near the left of the screen.
    let desired_right = (sel_x + sel_w).clamp(OVERLAY_TOOLBAR_WIDTH.min(bounds_w), bounds_w);
    let right_pad = (bounds_w - desired_right).max(0.0);

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

/// Surface the welcome window: if it's already open, focus it;
/// otherwise spawn a fresh one and register it under
/// [`WindowKind::Welcome`]. Used when a tray / hotkey click happens
/// before the permission gate has cleared so the user is never left
/// staring at a silent no-op.
fn show_or_focus_welcome(state: &mut App) -> Task<Message> {
    let existing = state
        .windows
        .iter()
        .find(|(_, k)| matches!(k, WindowKind::Welcome))
        .map(|(id, _)| *id);
    if let Some(id) = existing {
        return window::gain_focus(id);
    }
    let (id, open_task) = window::open(welcome_window_settings());
    state.windows.register(id, WindowKind::Welcome);
    open_task.map(|_id| Message::WelcomeWindowReady)
}

/// Preferences window. v1 = a single General tab with the controls
/// users need regularly. The other `SettingsTab` variants stay on the
/// enum but don't render until they have content worth showing.
fn settings_view(state: &App) -> Element<'_, Message> {
    use iced::widget::{button, pick_list, text_input, toggler};
    use readshot_core::HistoryRetention;

    // `pick_list` borrows its options for the duration of the
    // returned `Element`, so a `'static` slice keeps the lifetime
    // story simple.
    const RETENTION_OPTIONS: [HistoryRetention; 4] = [
        HistoryRetention::Off,
        HistoryRetention::Last50,
        HistoryRetention::Last30Days,
        HistoryRetention::Unlimited,
    ];

    let header = row![
        column![
            text("Settings").size(30),
            text("Capture, files, permissions, and startup")
                .size(13)
                .color(settings_muted_text()),
        ]
        .spacing(3),
        Space::new().width(Length::Fill),
    ]
    .align_y(Alignment::Center);

    let pretty = pretty_hotkey(&state.preferences.capture_hotkey);
    let hotkey_hint = if state.settings_recording_hotkey {
        state
            .settings_hotkey_error
            .clone()
            .unwrap_or_else(|| "Press a modifier shortcut now. Escape cancels.".to_string())
    } else if let Some(status) = &state.settings_hotkey_status {
        status.clone()
    } else if pretty.is_empty() {
        format!(
            "Couldn't read `{}` — try `cmd+shift+x` style.",
            state.preferences.capture_hotkey
        )
    } else if state.hotkey_manager.is_none() {
        // Parsed-OK but registration failed, e.g. another app already
        // owns the chord. Tell the user so they pick another one.
        format!("{pretty} — couldn't grab globally; try a different chord.")
    } else {
        format!("Currently bound to {pretty}.")
    };
    let hotkey_label = if state.settings_recording_hotkey {
        "Press shortcut…".to_string()
    } else {
        pretty_hotkey(&state.preferences.capture_hotkey)
    };

    let startup_row: Element<'_, Message> = toggler(state.preferences.launch_at_login)
        .label("Open Readshot at login")
        .on_toggle(|v| Message::Settings(SettingsMessage::SetLaunchAtLogin(v)))
        .into();

    let save_folder_value = if state.preferences.save_folder.as_os_str().is_empty() {
        "Platform default".to_string()
    } else {
        state.preferences.save_folder.display().to_string()
    };
    let filename_template = state.preferences.filename_template.clone();
    let permission_status = state.coordinator.pre_capture_gate();
    let (permission_title, permission_hint) = permission_settings_summary(permission_status);

    let history_retention = state.preferences.history_retention;
    let settings_recording_hotkey = state.settings_recording_hotkey;
    let permission_control: Element<'_, Message> = responsive(move |available| {
        let status = column![
            text(permission_title).size(13),
            text(permission_hint).size(11).color(settings_muted_text()),
        ]
        .spacing(6)
        .width(Length::Fill);

        if matches!(permission_status, PermissionStatus::NotApplicable) {
            return status.into();
        }

        let open_button =
            button(text("Open Settings")).on_press(Message::OpenPermissionSettingsRequested);
        if available.width < 460.0 {
            column![status, open_button].spacing(10).into()
        } else {
            row![status, open_button]
                .spacing(10)
                .align_y(Alignment::Center)
                .into()
        }
    })
    .height(Length::Shrink)
    .into();
    let permission_section = settings_section("Permissions", "Capture access", permission_control);
    let capture_section = settings_section(
        "Capture",
        "Shortcut and history",
        responsive(move |available| {
            let hotkey_control: Element<'_, Message> = responsive({
                let hotkey_label = hotkey_label.clone();
                move |available| {
                    let value = setting_value_box(hotkey_label.clone(), settings_recording_hotkey);
                    let actions = row![
                        button(text(if settings_recording_hotkey {
                            "Recording"
                        } else {
                            "Record"
                        }))
                        .on_press(Message::SettingsStartHotkeyRecording),
                        button(text("Reset")).on_press(Message::Settings(
                            SettingsMessage::SetCaptureHotkey(default_capture_hotkey().into()),
                        )),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center);

                    let main: Element<'_, Message> = if available.width < 360.0 {
                        column![value, actions].spacing(8).into()
                    } else {
                        row![value, actions]
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .into()
                    };
                    if settings_recording_hotkey {
                        column![
                            main,
                            text("Press the new shortcut, or Esc to cancel.")
                                .size(11)
                                .color(Color::from_rgba(0.78, 0.85, 1.0, 0.85)),
                        ]
                        .spacing(6)
                        .into()
                    } else {
                        main
                    }
                }
            })
            .height(Length::Shrink)
            .into();
            let hotkey_row = settings_field("Capture hotkey", hotkey_hint.clone(), hotkey_control);
            let retention_control: Element<'_, Message> =
                pick_list(&RETENTION_OPTIONS[..], Some(history_retention), |r| {
                    Message::Settings(SettingsMessage::SetHistoryRetention(r))
                })
                .into();
            let retention_row = settings_field(
                "History retention",
                "Searchable archive of captures. Off keeps everything in-memory only.",
                retention_control,
            );

            if available.width < 560.0 {
                column![hotkey_row, retention_row].spacing(14).into()
            } else {
                row![
                    column![hotkey_row].width(Length::FillPortion(3)),
                    column![retention_row].width(Length::FillPortion(2)),
                ]
                .spacing(18)
                .align_y(Alignment::Start)
                .into()
            }
        })
        .height(Length::Shrink)
        .into(),
    );
    let files_section = settings_section(
        "Files",
        "Save location and naming",
        responsive(move |available| {
            let save_folder_control: Element<'_, Message> = column![
                setting_value_box(save_folder_value.clone(), false),
                row![
                    button(text("Choose")).on_press(Message::SettingsChooseSaveFolderRequested),
                    button(text("Open")).on_press(Message::SettingsOpenSaveFolderRequested),
                    button(text("Reset")).on_press(Message::Settings(
                        SettingsMessage::SetSaveFolder(PathBuf::new()),
                    )),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(8)
            .into();
            let save_folder_row = settings_field(
                "Default save folder",
                "Starting folder for Save. Platform default uses your system screenshots location.",
                save_folder_control,
            );
            let filename_input =
                text_input("Screenshot {YYYY-MM-DD at HH.mm.ss}", &filename_template)
                    .on_input(|s| Message::Settings(SettingsMessage::SetFilenameTemplate(s)))
                    .padding([8, 10]);
            let preview_template = if filename_template.trim().is_empty() {
                "Screenshot {YYYY-MM-DD at HH.mm.ss}".to_string()
            } else {
                filename_template.clone()
            };
            let preview_name = default_save_filename(&preview_template, chrono::Utc::now());
            let preview_line = text(format!("Saves as: {preview_name}"))
                .size(11)
                .color(settings_muted_text());
            let filename_control: Element<'_, Message> =
                column![filename_input, preview_line].spacing(4).into();
            let filename_row = settings_field(
                "Filename template",
                "Supports date, time, year, and timestamp tokens.",
                filename_control,
            );

            if available.width < 620.0 {
                column![save_folder_row, filename_row].spacing(14).into()
            } else {
                row![
                    column![save_folder_row].width(Length::FillPortion(3)),
                    column![filename_row].width(Length::FillPortion(2)),
                ]
                .spacing(18)
                .align_y(Alignment::Start)
                .into()
            }
        })
        .height(Length::Shrink)
        .into(),
    );
    let reset_pending = state.settings_reset_all_pending;
    let reset_status = state
        .settings_status
        .as_deref()
        .unwrap_or("Return all preferences to the shipped defaults.")
        .to_string();
    let reset_controls: Element<'_, Message> = responsive(move |available| {
        let copy = if reset_pending {
            "Reset every setting to the app defaults?".to_string()
        } else {
            reset_status.clone()
        };
        let label = text(copy).size(12).color(settings_muted_text());

        if reset_pending {
            let actions = row![
                button(text("Reset all"))
                    .padding([8, 14])
                    .style(|t, s| action_button_style(t, s, ActionKind::Danger))
                    .on_press(Message::SettingsResetAllConfirmed),
                button(text("Cancel"))
                    .padding([8, 14])
                    .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                    .on_press(Message::SettingsResetAllCancelled),
            ]
            .spacing(8)
            .align_y(Alignment::Center);

            if available.width < 460.0 {
                column![label, actions].spacing(8).into()
            } else {
                row![label, Space::new().width(Length::Fill), actions]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            }
        } else {
            let action =
                button(text("Reset all settings")).on_press(Message::SettingsResetAllRequested);
            if available.width < 460.0 {
                column![label, action].spacing(8).into()
            } else {
                row![label, Space::new().width(Length::Fill), action]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            }
        }
    })
    .height(Length::Shrink)
    .into();
    let app_section = settings_section(
        "App",
        "Startup behavior",
        column![startup_row, reset_controls].spacing(12).into(),
    );

    let body = column![
        header,
        permission_section,
        capture_section,
        files_section,
        app_section,
    ]
    .spacing(16)
    .max_width(660);

    container(
        scrollable(body)
            .direction(iced::widget::scrollable::Direction::Vertical(
                slim_scrollbar(),
            ))
            .spacing(10.0)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .padding([24, 30])
    .into()
}

fn settings_section<'a>(
    title: &'a str,
    subtitle: &'a str,
    content: Element<'a, Message>,
) -> Element<'a, Message> {
    container(
        column![
            responsive(move |available| {
                if available.width < 420.0 {
                    column![
                        text(title).size(16),
                        text(subtitle).size(11).color(settings_muted_text()),
                    ]
                    .spacing(3)
                    .into()
                } else {
                    row![
                        text(title).size(16),
                        Space::new().width(Length::Fill),
                        text(subtitle).size(11).color(settings_muted_text()),
                    ]
                    .align_y(Alignment::Center)
                    .into()
                }
            }),
            content,
        ]
        .spacing(14),
    )
    .width(Length::Fill)
    .padding(16)
    .style(settings_section_style)
    .into()
}

fn settings_field<'a>(
    label: &'a str,
    hint: impl Into<String>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    column![
        text(label).size(13),
        control,
        text(hint.into()).size(11).color(settings_muted_text()),
    ]
    .spacing(7)
    .into()
}

fn setting_value_box(value: String, active: bool) -> Element<'static, Message> {
    container(
        text(value)
            .size(14)
            .color(if active {
                Color::from_rgb(0.83, 0.86, 1.0)
            } else {
                Color::from_rgba(1.0, 1.0, 1.0, 0.82)
            })
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding([9, 12])
    .style(move |theme: &Theme| {
        let palette = theme.extended_palette();
        let border_color = if active {
            palette.primary.base.color
        } else {
            palette.background.strong.color
        };
        iced::widget::container::Style {
            background: Some(
                if active {
                    Color::from_rgba(0.45, 0.55, 1.0, 0.10)
                } else {
                    Color::from_rgba(1.0, 1.0, 1.0, 0.045)
                }
                .into(),
            ),
            border: iced::Border {
                color: border_color,
                width: if active { 2.0 } else { 1.0 },
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn empty_state_card<'a>(
    title: &'static str,
    body: &'static str,
    action: Option<(&'static str, Message)>,
) -> Element<'a, Message> {
    let mut content = column![
        text(title).size(18),
        text(body)
            .size(12)
            .color(settings_muted_text())
            .width(Length::Fill),
    ]
    .spacing(8)
    .max_width(420);

    if let Some((label, msg)) = action {
        content = content.push(
            button(text(label).size(13).color(Color::WHITE))
                .padding([8, 16])
                .style(|theme, status| action_button_style(theme, status, ActionKind::Primary))
                .on_press(msg),
        );
    }

    container(content)
        .padding(18)
        .width(Length::Fill)
        .style(settings_section_style)
        .into()
}

fn settings_section_style(theme: &Theme) -> iced::widget::container::Style {
    let palette = theme.extended_palette();
    iced::widget::container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.035).into()),
        border: iced::Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

fn settings_muted_text() -> Color {
    Color::from_rgba(1.0, 1.0, 1.0, 0.56)
}

fn permission_settings_summary(status: PermissionStatus) -> (&'static str, &'static str) {
    match status {
        PermissionStatus::Granted => (
            "Screen Recording is allowed",
            "Readshot can open the capture overlay and recognise text from screenshots.",
        ),
        PermissionStatus::Denied => (
            "Screen Recording needs attention",
            "Enable Readshot in macOS System Settings. If it is already enabled, restart Readshot so macOS refreshes the grant.",
        ),
        PermissionStatus::NotApplicable => (
            "No extra capture permission required",
            "This platform does not need a separate Screen Recording grant.",
        ),
    }
}

fn slim_scrollbar() -> iced::widget::scrollable::Scrollbar {
    iced::widget::scrollable::Scrollbar::new()
        .width(7.0)
        .scroller_width(4.0)
        .margin(2.0)
}

fn welcome_view(state: &App) -> Element<'_, Message> {
    let hero = column![
        text("Readshot").size(34),
        text("Capture, search, find again.")
            .size(14)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55)),
    ]
    .spacing(4)
    .align_x(Alignment::Center);

    let card: Element<'_, Message> = match state.welcome {
        WelcomeState::Pending => welcome_pending_card(),
        WelcomeState::AwaitingGrant | WelcomeState::Denied => welcome_awaiting_card(state.welcome),
        WelcomeState::Granted => welcome_granted_card(state),
    };

    let toast: Element<'_, Message> = match &state.last_capture_status {
        Some(s) => text(s)
            .size(11)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.55))
            .into(),
        None => Space::new().height(Length::Fixed(0.0)).into(),
    };

    let inner = column![
        hero,
        Space::new().height(Length::Fixed(24.0)),
        card,
        Space::new().height(Length::Fixed(16.0)),
        toast,
    ]
    .max_width(420)
    .align_x(Alignment::Center);

    container(inner)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .padding(28)
        .into()
}

/// Shared chrome for every welcome card — soft semi-transparent
/// background, subtle border, generous padding so the action button
/// has room to breathe.
fn welcome_card<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .padding(20)
        .width(Length::Fill)
        .style(|_| iced::widget::container::Style {
            background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.04).into()),
            border: iced::Border {
                radius: 10.0.into(),
                color: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
                width: 1.0,
            },
            ..Default::default()
        })
        .into()
}

fn welcome_pending_card<'a>() -> Element<'a, Message> {
    let body = column![
        text("Allow Screen Recording").size(18),
        text(
            "macOS will pop a permission prompt. Click \"Open System Settings\" \
             inside it and toggle Readshot on — that's all we need."
        )
        .size(13)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
        Space::new().height(Length::Fixed(6.0)),
        button(text("Allow Screen Recording").size(14))
            .padding([10, 18])
            .style(|t, s| action_button_style(t, s, ActionKind::Primary))
            .on_press(Message::GrantPermissionRequested),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    welcome_card(body.into())
}

fn welcome_awaiting_card<'a>(state: WelcomeState) -> Element<'a, Message> {
    let (title, body_copy) = welcome_permission_guidance(state);
    let body = column![
        text(title).size(18),
        text(body_copy)
            .size(13)
            .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
        Space::new().height(Length::Fixed(6.0)),
        row![
            button(text("Open System Settings").size(14))
                .padding([10, 18])
                .style(|t, s| action_button_style(t, s, ActionKind::Primary))
                .on_press(Message::OpenPermissionSettingsRequested),
            button(text("Restart Readshot").size(14))
                .padding([10, 16])
                .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
                .on_press(Message::RestartRequested),
        ]
        .spacing(8),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    welcome_card(body.into())
}

fn welcome_permission_guidance(state: WelcomeState) -> (&'static str, &'static str) {
    match state {
        WelcomeState::Denied => (
            "Permission still blocked",
            "If Readshot is already toggled on in System Settings, restart now so macOS gives this process the new Screen Recording grant. Otherwise open System Settings and enable Readshot first.",
        ),
        _ => (
            "Waiting for permission",
            "Enable Readshot in System Settings. If macOS offers Quit & Reopen, accept it; otherwise use Restart Readshot after toggling the permission on.",
        ),
    }
}

fn welcome_granted_card(state: &App) -> Element<'_, Message> {
    let mut capture_btn = button(text("Capture Screen").size(14))
        .padding([10, 20])
        .style(|t, s| action_button_style(t, s, ActionKind::Primary));
    if !state.capture_in_flight {
        capture_btn = capture_btn.on_press(Message::OpenOverlayRequested);
    }
    let history_btn = button(text("Show History").size(13))
        .padding([8, 14])
        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
        .on_press(Message::OpenHistoryRequested);
    let settings_btn = button(text("Open Settings").size(13))
        .padding([8, 14])
        .style(|t, s| action_button_style(t, s, ActionKind::Secondary))
        .on_press(Message::OpenSettingsRequested);
    let hotkey = pretty_hotkey(&state.preferences.capture_hotkey);
    let body = column![
        text("You're all set.").size(18),
        text(format!(
            "Press {hotkey} or click the menu-bar icon to capture. Every \
             capture lands in History — searchable by anything visible \
             in the image."
        ))
        .size(13)
        .color(Color::from_rgba(1.0, 1.0, 1.0, 0.7)),
        Space::new().height(Length::Fixed(6.0)),
        capture_btn,
        Space::new().height(Length::Fixed(4.0)),
        row![history_btn, settings_btn]
            .spacing(8)
            .align_y(Alignment::Center),
    ]
    .spacing(10)
    .align_x(Alignment::Center);
    welcome_card(body.into())
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

#[cfg(test)]
async fn capture_request_to_dir(
    coord: CaptureCoordinator,
    req: CaptureRequest,
    dir: &Path,
) -> Result<PathBuf, CaptureRunError> {
    let img = coord.capture_region(req).await?;
    save_to_dir(&img, dir)
}

/// Returns the captured image instead of saving — the editor flow
/// uses this so the user can choose what to do with the bytes.
///
/// Looks up the named display so the request carries its real HiDPI
/// scale; without that, the macOS backend would render at half
/// resolution on Retina monitors.
/// Hard limits for a single scrolling-capture session. Tuned so a
/// runaway loop bounded by these caps still produces a sane stitched
/// output and doesn't gobble gigabytes of RAM. The limits below cover
/// roughly 30 s of continuous scrolling at the configured tick rate.
const SCROLL_MAX_FRAMES: usize = 120;
/// Per-pixel SAD threshold (0-255 per channel) above which two frames
/// are considered different. Used by [`frames_differ`] to drop near-
/// duplicate captures and detect "user stopped scrolling".
const SCROLL_MOTION_THRESHOLD: u64 = 1500;
/// Tick interval (ms) driving the per-frame capture loop. 180 ms ≈
/// 5.5 fps — fast enough to keep up with a typical trackpad scroll
/// while leaving headroom for the actual `capture_region` future to
/// complete before the next tick fires.
const SCROLL_FRAME_INTERVAL_MS: u64 = 180;

/// "Did the page move?" check between two adjacent capture frames.
///
/// Computes a subsampled sum-of-absolute-differences over a single
/// horizontal strip in the middle of the frame. SAD beats the
/// per-row mean check it replaced because a small one-line scroll
/// changes lots of individual pixels but barely shifts the row mean.
fn frames_differ(a: &image::RgbaImage, b: &image::RgbaImage) -> bool {
    if a.dimensions() != b.dimensions() {
        return true;
    }
    let (w, h) = a.dimensions();
    if w == 0 || h == 0 {
        return false;
    }
    // Strip in the middle third of the frame — likely to contain
    // content motion regardless of sticky header/footer noise.
    let strip_h = (h / 8).clamp(8, 64);
    let strip_y = h / 2 - strip_h / 2;
    let mut sum: u64 = 0;
    let stride_x: u32 = 4;
    let stride_y: u32 = 2;
    let mut y = 0u32;
    while y < strip_h {
        let mut x = 0u32;
        while x < w {
            let pa = a.get_pixel(x, strip_y + y).0;
            let pb = b.get_pixel(x, strip_y + y).0;
            sum += diff_u8(pa[0], pb[0]) as u64
                + diff_u8(pa[1], pb[1]) as u64
                + diff_u8(pa[2], pb[2]) as u64;
            if sum > SCROLL_MOTION_THRESHOLD {
                return true;
            }
            x += stride_x;
        }
        y += stride_y;
    }
    false
}

fn diff_u8(a: u8, b: u8) -> u32 {
    if a > b {
        (a - b) as u32
    } else {
        (b - a) as u32
    }
}

/// Run the stitching algorithm on a background thread so it doesn't
/// block the iced event loop. Stitching a long scroll can take a
/// hundred ms or two — fine in a worker, not fine on the UI thread.
async fn stitch_frames_async(frames: Vec<image::RgbaImage>) -> Result<image::RgbaImage, String> {
    tokio::task::spawn_blocking(move || {
        readshot_core::scroll_stitch::stitch_scrolling(
            &frames,
            readshot_core::scroll_stitch::StitchConfig::default(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

async fn capture_region_to_image(
    coord: CaptureCoordinator,
    display_id: readshot_capture::DisplayId,
    rect: readshot_core::geom::Rect,
    hide_cursor: bool,
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
        hide_cursor,
    };
    Ok(coord.capture_region(req).await?)
}

async fn write_cli_interactive_capture(
    image: image::RgbaImage,
    output: PathBuf,
) -> Result<(), CaptureRunError> {
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = output.with_extension("png.tmp");
    let _ = std::fs::remove_file(&tmp);
    let write_result = image
        .save_with_format(&tmp, image::ImageFormat::Png)
        .map_err(CaptureRunError::from)
        .and_then(|_| std::fs::rename(&tmp, &output).map_err(CaptureRunError::from));
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&output);
    }
    write_result?;
    Ok(())
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

/// Default per-user history root —
/// `~/Library/Application Support/np.com.pawanpaudel.Readshot/history/`
/// on macOS, the equivalent `data_local_dir` on other platforms.
/// `None` when the system can't supply a project dir (rare; tests fall
/// back to no history).
pub fn default_history_root() -> Option<PathBuf> {
    directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")
        .map(|d| d.data_local_dir().join("history"))
}

/// Default per-user preferences file —
/// `<config_dir>/np.com.pawanpaudel.Readshot/preferences.toml`. `None`
/// in the same edge cases [`default_history_root`] returns `None`;
/// settings changes in that mode stay in-memory only.
pub fn default_preferences_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("np.com", "pawanpaudel", "Readshot")
        .map(|d| d.config_dir().join("preferences.toml"))
}

/// Load `Preferences` from `path`, falling back to first-launch
/// defaults when the file is missing, unreadable, or corrupt.
///
/// The first-launch fallback overrides two fields from
/// [`Preferences::default`]: `history_retention` is bumped to `Last50`
/// (the wedge needs history on, see `project_history_default.md`) and
/// `capture_hotkey` is set to the platform-native chord. Any value
/// that round-trips through the on-disk file wins over these
/// overrides — once the user has a `preferences.toml`, their choices
/// stick.
pub fn load_preferences(path: Option<&Path>) -> Preferences {
    let Some(path) = path else {
        return first_launch_preferences();
    };
    if !path.exists() {
        return first_launch_preferences();
    }
    match Preferences::load(path) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                target: "readshot::preferences",
                "load failed for {}: {e} — using first-launch defaults",
                path.display(),
            );
            first_launch_preferences()
        }
    }
}

/// First-launch preference set: the `Default` shape with the two
/// runtime overrides that keep the wedge alive on a fresh install.
fn first_launch_preferences() -> Preferences {
    Preferences {
        history_retention: readshot_core::HistoryRetention::Last50,
        capture_hotkey: default_capture_hotkey().into(),
        ..Preferences::default()
    }
}

fn reset_settings_to_defaults(state: &mut App) {
    let defaults = first_launch_preferences();
    state.update_sync(Message::Settings(SettingsMessage::SetCaptureHotkey(
        defaults.capture_hotkey,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetSaveFolder(
        defaults.save_folder,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetFilenameTemplate(
        defaults.filename_template,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetDefaultFormat(
        defaults.default_format,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetHistoryRetention(
        defaults.history_retention,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetOcrLanguages(
        defaults.ocr_languages,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetOcrEngine(
        defaults.ocr_engine_choice,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetLaunchAtLogin(
        defaults.launch_at_login,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetUpdateChannel(
        defaults.update_channel,
    )));
    state.update_sync(Message::Settings(SettingsMessage::SetDebugLogging(
        defaults.debug_logging,
    )));
}

/// Spawn a fire-and-forget history-persistence task. The chain is:
/// encode PNG → save record (empty OCR) → run Apple Vision OCR →
/// update sidecar with the recognised text. Each stage logs its own
/// failures and the user-visible flow never blocks on this work.
///
/// Returns `Ok(true)` when the OCR-and-update step completed (the
/// browser should reload to pick up the new text), `Ok(false)` when
/// history was disabled or OCR failed.
fn persist_history_task(
    coord: CaptureCoordinator,
    img: image::RgbaImage,
    record: Option<readshot_core::CaptureRecord>,
    policy: readshot_core::HistoryRetention,
    preferences: Preferences,
) -> Task<Message> {
    Task::perform(
        async move {
            if matches!(policy, readshot_core::HistoryRetention::Off) {
                return Ok::<bool, String>(false);
            }
            let Some(mut record) = record else {
                return Ok(false);
            };
            let now = record.captured_at;
            let buf = readshot_core::encode_png(&img).map_err(|e| format!("png encode: {e}"))?;
            // Save with empty OCR first so the capture is visible in
            // the browser immediately (OCR is the slow bit).
            coord.record_history(record.clone(), buf, policy, now).await;

            // Background OCR — fills `ocr_text` so the search index
            // has something to match. Failure is logged but
            // non-fatal: the record is still useful without text.
            let ocr_req = ocr_request_for_image(img, &preferences);
            match coord.recognise(ocr_req).await {
                Ok(text) => {
                    if let Ok(records) = coord.history_list() {
                        if let Some(latest) = records.into_iter().find(|r| r.id == record.id) {
                            record.annotation_model = latest.annotation_model;
                        }
                    }
                    record.ocr_text = Some(text);
                    if let Err(e) = coord.update_history(&record) {
                        tracing::warn!(target: "readshot::history", "ocr update failed: {e}");
                        return Ok(false);
                    }
                    Ok(true)
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::history", "ocr failed: {e}");
                    Ok(false)
                }
            }
        },
        Message::HistoryRecordPersisted,
    )
}

fn history_record_for_capture(
    img: &image::RgbaImage,
    display_id: &readshot_capture::DisplayId,
    policy: readshot_core::HistoryRetention,
) -> Option<readshot_core::CaptureRecord> {
    if matches!(policy, readshot_core::HistoryRetention::Off) {
        return None;
    }
    Some(readshot_core::CaptureRecord::new(
        chrono::Utc::now(),
        img.width(),
        img.height(),
        display_id.to_string(),
    ))
}

fn save_to_desktop(img: &image::RgbaImage) -> Result<PathBuf, CaptureRunError> {
    let dir = directories::UserDirs::new()
        .and_then(|d| d.desktop_dir().map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    save_to_dir(img, &dir)
}

fn save_to_dir(img: &image::RgbaImage, dir: &Path) -> Result<PathBuf, CaptureRunError> {
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let path = dir.join(format!("Readshot-{stamp}.png"));
    readshot_core::save_png(img, &path)?;
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
    filename_template: String,
) -> Result<Option<PathBuf>, CaptureRunError> {
    let default_name = default_save_filename(&filename_template, chrono::Utc::now());
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
    readshot_core::save_png(&img, &path)?;
    Ok(Some(path))
}

async fn pick_settings_save_folder() -> Result<Option<PathBuf>, CaptureRunError> {
    let handle = rfd::AsyncFileDialog::new().pick_folder().await;
    Ok(handle.map(|folder| folder.path().to_path_buf()))
}

fn settings_save_folder_to_open(prefs: &Preferences) -> PathBuf {
    if prefs.save_folder.as_os_str().is_empty() {
        default_platform_save_folder()
    } else {
        prefs.save_folder.clone()
    }
}

fn default_platform_save_folder() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        directories::UserDirs::new()
            .and_then(|d| d.desktop_dir().map(PathBuf::from))
            .unwrap_or_else(std::env::temp_dir)
    }
    #[cfg(not(target_os = "macos"))]
    {
        directories::UserDirs::new()
            .and_then(|d| d.picture_dir().map(|p| p.join("Screenshots")))
            .unwrap_or_else(std::env::temp_dir)
    }
}

fn preferred_save_seed_dir(
    prefs: &Preferences,
    last_save_dir: &Option<PathBuf>,
) -> Option<PathBuf> {
    last_save_dir
        .clone()
        .or_else(|| (!prefs.save_folder.as_os_str().is_empty()).then(|| prefs.save_folder.clone()))
}

fn default_save_filename(template: &str, when: chrono::DateTime<chrono::Utc>) -> String {
    let mut filename = readshot_core::expand_filename_template(template, when);
    filename.push_str(".png");
    filename
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

async fn copy_text_to_clipboard(text: String) -> Result<(), ClipboardError> {
    tokio::task::spawn_blocking(move || {
        let mut ctx = arboard::Clipboard::new()?;
        ctx.set_text(text)?;
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
    preferences: Preferences,
) -> Result<String, OcrCopyError> {
    let result = coord
        .recognise(ocr_request_for_image(img, &preferences))
        .await?;
    let text = result;
    if !text.is_empty() {
        copy_text_to_clipboard(text.clone()).await?;
    }
    Ok(text)
}

fn ocr_request_for_image(
    image: image::RgbaImage,
    preferences: &Preferences,
) -> readshot_ocr::OCRRequest {
    readshot_ocr::OCRRequest {
        image,
        languages: preferences.ocr_languages.clone(),
        use_language_correction: true,
    }
}

fn shortcut_string_from_keypress(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<String> {
    if !(modifiers.command() || modifiers.control() || modifiers.alt() || modifiers.shift()) {
        return None;
    }
    let key = hotkey_token_for_key(key)?;
    let mut parts: Vec<&str> = Vec::new();
    if modifiers.command() {
        parts.push("cmd");
    }
    if modifiers.control() {
        parts.push("ctrl");
    }
    if modifiers.alt() {
        parts.push("alt");
    }
    if modifiers.shift() {
        parts.push("shift");
    }
    parts.push(key);
    Some(parts.join("+"))
}

fn shortcut_cancelled_by_keypress(
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> bool {
    matches!(
        key,
        iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape)
    ) && !modifiers.command()
        && !modifiers.control()
        && !modifiers.alt()
        && !modifiers.shift()
}

fn hotkey_token_for_key(key: &iced::keyboard::Key) -> Option<&'static str> {
    use iced::keyboard::key::Named;
    use iced::keyboard::Key;

    match key {
        Key::Character(c) => {
            let mut chars = c.chars();
            let ch = chars.next()?;
            if chars.next().is_none() && ch.is_ascii_alphanumeric() {
                Some(match ch.to_ascii_lowercase() {
                    'a' => "a",
                    'b' => "b",
                    'c' => "c",
                    'd' => "d",
                    'e' => "e",
                    'f' => "f",
                    'g' => "g",
                    'h' => "h",
                    'i' => "i",
                    'j' => "j",
                    'k' => "k",
                    'l' => "l",
                    'm' => "m",
                    'n' => "n",
                    'o' => "o",
                    'p' => "p",
                    'q' => "q",
                    'r' => "r",
                    's' => "s",
                    't' => "t",
                    'u' => "u",
                    'v' => "v",
                    'w' => "w",
                    'x' => "x",
                    'y' => "y",
                    'z' => "z",
                    '0' => "0",
                    '1' => "1",
                    '2' => "2",
                    '3' => "3",
                    '4' => "4",
                    '5' => "5",
                    '6' => "6",
                    '7' => "7",
                    '8' => "8",
                    '9' => "9",
                    _ => return None,
                })
            } else {
                None
            }
        }
        Key::Named(Named::Enter) => Some("enter"),
        Key::Named(Named::Escape) => Some("escape"),
        Key::Named(Named::Tab) => Some("tab"),
        Key::Named(Named::Space) => Some("space"),
        Key::Named(Named::Backspace) => Some("backspace"),
        Key::Named(Named::F1) => Some("f1"),
        Key::Named(Named::F2) => Some("f2"),
        Key::Named(Named::F3) => Some("f3"),
        Key::Named(Named::F4) => Some("f4"),
        Key::Named(Named::F5) => Some("f5"),
        Key::Named(Named::F6) => Some("f6"),
        Key::Named(Named::F7) => Some("f7"),
        Key::Named(Named::F8) => Some("f8"),
        Key::Named(Named::F9) => Some("f9"),
        Key::Named(Named::F10) => Some("f10"),
        Key::Named(Named::F11) => Some("f11"),
        Key::Named(Named::F12) => Some("f12"),
        _ => None,
    }
}

fn cli_tools_setup_commands(shell: crate::cli_tools::Shell) -> String {
    crate::cli_tools::setup_commands(shell)
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
    #[error(transparent)]
    Io(#[from] std::io::Error),
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
    // .../Readshot.app/Contents/MacOS/readshot → .../Readshot.app
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
        show_macos_notification("Readshot is ready", &body);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = hotkey;
    }
}

fn notify_update_check_started() {
    #[cfg(target_os = "macos")]
    {
        show_macos_notification("Readshot updates", "Checking for updates...");
    }
}

/// Surface manual updater failures to the user. The tray action is a
/// user-initiated command; silently logging here makes the menu item
/// look broken when Sparkle is missing or cannot load.
fn notify_update_check_failed(error: &str) {
    #[cfg(target_os = "macos")]
    {
        let body = format!("Could not check for updates: {error}");
        show_macos_notification("Readshot updates", &body);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = error;
    }
}

#[cfg(target_os = "macos")]
fn show_macos_notification(title: &str, body: &str) {
    let script = macos_notification_script(title, body);
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

#[cfg(target_os = "macos")]
fn macos_notification_script(title: &str, body: &str) -> String {
    format!(
        r#"display notification "{}" with title "{}""#,
        applescript_escape(body),
        applescript_escape(title)
    )
}

#[cfg(target_os = "macos")]
fn applescript_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            // Newlines / carriage returns / NULs would otherwise close
            // the string literal or break osascript parsing entirely.
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            // AppleScript's line-continuation char joins onto the
            // next statement. AppleScript strings have no `\u`
            // escape, so the only safe option is to drop it.
            '\u{00AC}' => {}
            c if (c as u32) < 0x20 => {
                // Drop other control chars; they have no useful
                // representation inside an AppleScript string.
            }
            c => out.push(c),
        }
    }
    out
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

fn sync_editor_history(ed: &mut crate::editor::EditorSession, coord: &CaptureCoordinator) {
    let Some(record) = ed.source_record.as_mut() else {
        return;
    };
    record.annotation_model = ed.model.annotations().to_vec();
    if let Err(e) = coord.update_history(record) {
        tracing::warn!(target: "readshot::history", "annotation update failed: {e}");
        ed.status = Some(format!("History sync failed: {e}"));
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
    use async_trait::async_trait;
    use readshot_capture::fake::FakeCapturer;
    use readshot_capture::{Capturer, DisplayInfo};
    use readshot_core::error::CaptureError;
    use readshot_core::{Annotation, FsHistoryStore, HistoryStore, RectLike, Rgba};
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

    fn build_app_with_history(perms: Arc<FakePermissions>, history: Arc<dyn HistoryStore>) -> App {
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hi")),
            perms.clone(),
            Some(history),
        );
        App::new(coord, perms, Preferences::default())
    }

    fn solid(w: u32, h: u32) -> image::RgbaImage {
        let mut img = image::RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 255, 255, 255]);
        }
        img
    }

    #[test]
    fn overlay_selection_stores_last_region_per_display() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let display_id = "primary".to_string();
        let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();

        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect,
                intent: crate::app::CaptureIntent::Editor,
            },
        );

        assert_eq!(
            app.last_regions.get(&display_id).map(|r| r.rect),
            Some(rect)
        );
    }

    #[test]
    fn overlay_selection_tracks_most_recent_region_display() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let first = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();
        let second = readshot_core::geom::Rect::from_xywh(30.0, 40.0, 160.0, 90.0).unwrap();

        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: "display-a".to_string(),
                rect: first,
                intent: crate::app::CaptureIntent::Editor,
            },
        );
        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: "display-b".to_string(),
                rect: second,
                intent: crate::app::CaptureIntent::Editor,
            },
        );

        assert_eq!(app.last_region_display_id.as_deref(), Some("display-b"));
    }

    #[test]
    fn overlay_auto_confirm_is_only_for_cli_interactive() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        assert_eq!(overlay_auto_confirm_intent(&app), None);

        app.pending_intent = Some(crate::app::CaptureIntent::Editor);
        assert_eq!(overlay_auto_confirm_intent(&app), None);

        app.pending_intent = Some(crate::app::CaptureIntent::CliInteractive);
        assert_eq!(
            overlay_auto_confirm_intent(&app),
            Some(crate::app::CaptureIntent::CliInteractive)
        );

        app.pending_intent = None;
        app.cli_interactive_output = Some(std::env::temp_dir().join("capture.png"));
        assert_eq!(
            overlay_auto_confirm_intent(&app),
            Some(crate::app::CaptureIntent::CliInteractive)
        );
    }

    #[test]
    fn retake_last_region_without_previous_region_sets_status() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));

        let _ = update(&mut app, Message::RetakeLastRegionRequested);

        assert_eq!(
            app.last_capture_status.as_deref(),
            Some("No previous region to retake. Capture a region first, then use Retake Last Region.")
        );
    }

    #[test]
    fn pin_opacity_message_updates_pin_state() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.pins.insert(
            id,
            crate::app::PinState {
                handle: iced::widget::image::Handle::from_rgba(1, 1, vec![255, 255, 255, 255]),
                opacity: 1.0,
                locked: false,
            },
        );

        let _ = update(&mut app, Message::PinOpacityChanged(id, 0.4));

        assert_eq!(app.pins.get(&id).unwrap().opacity, 0.4);
    }

    #[test]
    fn editor_image_filter_is_crisp_at_actual_size_and_smooth_otherwise() {
        assert_eq!(
            editor_image_filter(1.0, 1.0),
            iced::widget::image::FilterMethod::Nearest
        );
        assert_eq!(
            editor_image_filter(0.5, 2.0),
            iced::widget::image::FilterMethod::Nearest
        );
        assert_eq!(
            editor_image_filter(0.75, 1.0),
            iced::widget::image::FilterMethod::Linear
        );
        assert_eq!(
            editor_image_filter(1.25, 1.0),
            iced::widget::image::FilterMethod::Linear
        );
    }

    #[test]
    fn editor_fit_scale_fills_available_viewport() {
        assert_eq!(
            editor_fit_scale(iced::Size::new(800.0, 600.0), 200, 100),
            4.0
        );
        assert_eq!(
            editor_fit_scale(iced::Size::new(500.0, 400.0), 1000, 400),
            0.5
        );
    }

    #[test]
    fn editor_bottom_layout_stacks_before_controls_crowd() {
        assert_eq!(editor_bottom_layout(1000.0), EditorBottomLayout::Wide);
        assert_eq!(editor_bottom_layout(700.0), EditorBottomLayout::Stacked);
        assert_eq!(editor_bottom_layout(420.0), EditorBottomLayout::Compact);
    }

    #[test]
    fn record_matches_finds_substring_in_ocr_text() {
        let mut r =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
        r.ocr_text = Some("Hello, deploy script ready".into());
        // The caller (history_view) lowercases the query first; the
        // matcher's contract is "q is already lowercase".
        assert!(record_matches(&r, "deploy"));
        assert!(record_matches(&r, "hello"));
        assert!(!record_matches(&r, "ship"));
    }

    #[test]
    fn record_matches_falls_back_to_timestamp_when_no_ocr() {
        use chrono::TimeZone;
        let when = chrono::Utc
            .with_ymd_and_hms(2026, 4, 28, 14, 32, 5)
            .unwrap();
        let r = readshot_core::CaptureRecord::new(when, 100, 100, "primary".to_string());
        // Local-formatted timestamp lets users search by date / time.
        let local = when
            .with_timezone(&chrono::Local)
            .format("%Y-%m")
            .to_string();
        assert!(record_matches(&r, &local.to_lowercase()));
    }

    #[test]
    fn record_matches_finds_display_id_and_dimensions() {
        let r = readshot_core::CaptureRecord::new(chrono::Utc::now(), 1440, 900, "built-in-retina");

        assert!(record_matches(&r, "retina"));
        assert!(record_matches(&r, "1440x900"));
        assert!(record_matches(&r, "900"));
        assert!(!record_matches(&r, "external"));
    }

    #[test]
    fn visible_history_text_uses_current_search_filter() {
        let mut first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
        first.ocr_text = Some("alpha receipt".into());
        let mut second =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "secondary");
        second.ocr_text = Some("beta invoice".into());

        let records = vec![first, second];

        assert_eq!(visible_history_text(&records, "invoice"), "beta invoice");
        assert_eq!(visible_history_text_count(&records, "invoice"), 1);
        assert_eq!(
            visible_history_text(&records, ""),
            "alpha receipt\n\nbeta invoice"
        );
    }

    #[test]
    fn history_list_load_selects_first_visible_capture() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
        let second = readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "secondary");

        let _ = update(
            &mut app,
            Message::HistoryListLoaded(Ok(vec![first.clone(), second])),
        );

        assert_eq!(app.history_selected_id, Some(first.id));
    }

    #[test]
    fn history_search_keeps_selection_inside_filtered_results() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
        first.ocr_text = Some("alpha receipt".into());
        let mut second =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "secondary");
        second.ocr_text = Some("beta invoice".into());
        let second_id = second.id;
        app.history_records = vec![first, second];
        app.history_selected_id = app.history_records.first().map(|r| r.id);

        let _ = update(&mut app, Message::HistorySearchChanged("invoice".into()));

        assert_eq!(app.history_selected_id, Some(second_id));
    }

    #[test]
    fn history_keyboard_selection_moves_through_visible_rows() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let first = readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary");
        let second = readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "secondary");
        let second_id = second.id;
        app.history_records = vec![first, second];
        app.history_selected_id = app.history_records.first().map(|r| r.id);

        let _ = update(&mut app, Message::HistorySelectNext);
        assert_eq!(app.history_selected_id, Some(second_id));

        let _ = update(&mut app, Message::HistorySelectNext);
        assert_eq!(app.history_selected_id, Some(second_id));

        let _ = update(&mut app, Message::HistorySelectPrevious);
        assert_eq!(
            app.history_selected_id,
            app.history_records.first().map(|r| r.id)
        );
    }

    #[test]
    fn history_copy_shortcut_requires_selected_capture() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.history_window_id = Some(id);

        let _ = update(
            &mut app,
            Message::HistoryKeyboardShortcut(id, HistoryKeyboardAction::CopyText),
        );

        assert_eq!(
            app.history_status.as_deref(),
            Some("No history capture selected.")
        );
    }

    #[test]
    fn history_copy_completion_updates_history_status() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));

        let _ = update(&mut app, Message::HistoryCopyImageDone(Ok(())));

        assert_eq!(
            app.history_status.as_deref(),
            Some("Copied image to clipboard.")
        );

        let _ = update(&mut app, Message::HistoryCopyTextDone(Ok("hello".into())));

        assert_eq!(
            app.history_status.as_deref(),
            Some("Copied 5 characters of text.")
        );
    }

    #[test]
    fn permission_settings_summary_explains_denied_state() {
        let (title, hint) = permission_settings_summary(PermissionStatus::Denied);

        assert!(title.contains("needs attention"));
        assert!(hint.contains("System Settings"));
        assert!(hint.contains("restart"));
    }

    #[test]
    fn welcome_permission_guidance_explains_restart_after_denied_poll() {
        let (title, hint) = welcome_permission_guidance(WelcomeState::Denied);

        assert!(title.contains("blocked"));
        assert!(hint.contains("restart"));
        assert!(hint.contains("System Settings"));
    }

    #[test]
    fn history_backed_editor_commit_updates_record_annotations() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let mut record =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
        record.ocr_text = Some("before".into());
        store.save(&record, b"png").unwrap();

        let history: Arc<dyn HistoryStore> = store.clone();
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
        app.editor = Some(crate::editor::EditorSession::from_history(
            solid(100, 100),
            record.clone(),
        ));

        let annotation = Annotation::Rectangle {
            rect: RectLike::new(1.0, 2.0, 30.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
        };
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(annotation)),
        );

        let from_disk = store.list().unwrap();
        assert_eq!(from_disk[0].annotation_model.len(), 1);
        assert_eq!(from_disk[0].ocr_text.as_deref(), Some("before"));
    }

    #[test]
    fn history_clear_all_removes_records_and_resets_browser_state() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let older =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
        let newer =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 120, 90, "primary".to_string());
        store.save(&older, b"png").unwrap();
        store.save(&newer, b"png").unwrap();

        let history: Arc<dyn HistoryStore> = store.clone();
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
        app.history_records = vec![newer, older];
        app.history_status = Some("stale".into());

        let _ = update(&mut app, Message::HistoryClearAllRequested);

        assert!(store.list().unwrap().is_empty());
        assert!(app.history_records.is_empty());
        assert_eq!(app.history_status.as_deref(), Some("History cleared."));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reveal_command_uses_finder_selection_on_macos() {
        let command = reveal_command_for_path(std::path::Path::new("/tmp/readshot/capture.png"));
        assert_eq!(command.program, "open");
        assert_eq!(command.args, vec!["-R", "/tmp/readshot/capture.png"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn open_folder_command_uses_plain_open_on_macos() {
        let command = open_folder_command_for_path(std::path::Path::new("/tmp/readshot"));
        assert_eq!(command.program, "open");
        assert_eq!(command.args, vec!["/tmp/readshot"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_notification_script_escapes_title_and_body() {
        let script = macos_notification_script("Readshot \"updates\"", "Path C:\\tmp\\\"x\"");
        assert_eq!(
            script,
            r#"display notification "Path C:\\tmp\\\"x\"" with title "Readshot \"updates\"""#
        );
    }

    #[test]
    fn history_thumbnail_path_matches_core_layout() {
        use chrono::TimeZone;
        let record = readshot_core::CaptureRecord::new(
            chrono::Utc
                .with_ymd_and_hms(2026, 4, 25, 14, 30, 0)
                .unwrap(),
            100,
            100,
            "primary",
        );
        let root = std::path::Path::new("/history");
        assert_eq!(
            history_thumbnail_path(root, &record),
            root.join(readshot_core::FsHistoryStore::thumbnail_path(&record))
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reveal_command_uses_explorer_selection_on_windows() {
        let command = reveal_command_for_path(std::path::Path::new("C:\\tmp\\capture.png"));
        assert_eq!(command.program, "explorer");
        assert_eq!(command.args, vec!["/select,", "C:\\tmp\\capture.png"]);
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn reveal_command_opens_parent_directory_on_unix() {
        let command = reveal_command_for_path(std::path::Path::new("/tmp/readshot/capture.png"));
        assert_eq!(command.program, "xdg-open");
        assert_eq!(command.args, vec!["/tmp/readshot"]);
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
        assert_eq!(
            pick_primary(std::slice::from_ref(&secondary)).unwrap().id,
            "s"
        );
        assert!(pick_primary(&[]).is_none());
    }

    #[tokio::test]
    async fn capture_request_to_dir_writes_png() {
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hi")),
            perms,
            None,
        );
        let dir = tempfile::TempDir::new().unwrap();
        let req = CaptureRequest {
            display_id: "fake-0".into(),
            rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 256.0, 256.0).unwrap(),
            scale: 1.0,
            hide_cursor: true,
        };
        let path = capture_request_to_dir(coord, req, dir.path())
            .await
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert!(path.starts_with(dir.path()));
    }

    #[tokio::test]
    async fn capture_request_to_dir_preserves_physical_pixel_dimensions() {
        struct HiDpiCapturer;

        #[async_trait]
        impl Capturer for HiDpiCapturer {
            async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
                Ok(vec![DisplayInfo {
                    id: "retina".into(),
                    bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 128.0, 128.0).unwrap(),
                    scale: 2.0,
                    name: "Retina".into(),
                    is_primary: true,
                }])
            }

            async fn capture_region(
                &self,
                req: CaptureRequest,
            ) -> Result<image::RgbaImage, CaptureError> {
                let w = (req.rect.width() * req.scale).round() as u32;
                let h = (req.rect.height() * req.scale).round() as u32;
                Ok(solid(w, h))
            }
        }

        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(HiDpiCapturer),
            Arc::new(FakeOcrEngine::with_text("hi")),
            perms,
            None,
        );
        let dir = tempfile::TempDir::new().unwrap();

        let img = capture_region_to_image(
            coord,
            "retina".into(),
            readshot_core::geom::Rect::from_xywh(0.0, 0.0, 128.0, 128.0).unwrap(),
            true,
        )
        .await
        .unwrap();
        let path = save_to_dir(&img, dir.path()).unwrap();

        let saved = image::open(&path).unwrap();
        assert_eq!(saved.width(), 256);
        assert_eq!(saved.height(), 256);
    }

    #[tokio::test]
    async fn capture_region_to_image_forwards_cursor_visibility() {
        struct CursorFlagCapturer {
            seen_hide_cursor: Arc<Mutex<Option<bool>>>,
        }

        #[async_trait]
        impl Capturer for CursorFlagCapturer {
            async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
                Ok(vec![DisplayInfo {
                    id: "primary".into(),
                    bounds: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 64.0, 64.0).unwrap(),
                    scale: 1.0,
                    name: "Primary".into(),
                    is_primary: true,
                }])
            }

            async fn capture_region(
                &self,
                req: CaptureRequest,
            ) -> Result<image::RgbaImage, CaptureError> {
                *self.seen_hide_cursor.lock().unwrap() = Some(req.hide_cursor);
                Ok(solid(64, 64))
            }
        }

        let seen_hide_cursor = Arc::new(Mutex::new(None));
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(CursorFlagCapturer {
                seen_hide_cursor: Arc::clone(&seen_hide_cursor),
            }),
            Arc::new(FakeOcrEngine::with_text("hi")),
            perms,
            None,
        );

        capture_region_to_image(
            coord,
            "primary".into(),
            readshot_core::geom::Rect::from_xywh(0.0, 0.0, 32.0, 32.0).unwrap(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(*seen_hide_cursor.lock().unwrap(), Some(false));
    }

    #[tokio::test]
    async fn cli_interactive_write_removes_existing_tmp_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let output = dir.path().join("capture.png");
        let tmp = dir.path().join("capture.png.tmp");
        std::fs::write(&tmp, b"stale").unwrap();

        write_cli_interactive_capture(solid(8, 8), output.clone())
            .await
            .unwrap();

        assert!(output.exists());
        assert!(!tmp.exists());
        let saved = image::open(output).unwrap();
        assert_eq!(saved.width(), 8);
        assert_eq!(saved.height(), 8);
    }

    #[test]
    fn pretty_hotkey_renders_macos_glyphs() {
        assert_eq!(pretty_hotkey("cmd+shift+x"), "\u{2318}\u{21E7}X");
        assert_eq!(pretty_hotkey("ctrl+alt+y"), "\u{2303}\u{2325}Y");
        assert_eq!(pretty_hotkey("CMD+Shift+Z"), "\u{2318}\u{21E7}Z");
    }

    #[test]
    fn pretty_hotkey_falls_back_to_input_when_unparseable() {
        // Empty stays empty; nonsense single-token is preserved.
        assert_eq!(pretty_hotkey(""), "");
        assert_eq!(pretty_hotkey("???"), "???");
    }

    #[test]
    fn register_default_hotkey_returns_none_for_garbage_string() {
        let prefs = Preferences {
            capture_hotkey: "not a hotkey at all".into(),
            ..Preferences::default()
        };
        assert!(register_default_hotkey(&prefs).is_none());
    }

    #[test]
    fn ocr_request_uses_preferred_languages_from_preferences() {
        let prefs = Preferences {
            ocr_languages: vec!["ja-JP".into(), "en-US".into()],
            ..Preferences::default()
        };
        let img = image::RgbaImage::new(1, 1);

        let req = ocr_request_for_image(img, &prefs);

        assert_eq!(req.languages, vec!["ja-JP", "en-US"]);
        assert!(req.use_language_correction);
    }

    #[test]
    fn ocr_request_keeps_empty_languages_for_automatic_detection() {
        let prefs = Preferences::default();
        let img = image::RgbaImage::new(1, 1);

        let req = ocr_request_for_image(img, &prefs);

        assert!(req.languages.is_empty());
    }

    #[test]
    fn default_save_filename_expands_template_as_png() {
        let when = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();

        let filename = default_save_filename("Capture {YYYY}-{timestamp}", when);

        assert_eq!(filename, "Capture 2023-1700000000.png");
    }

    #[test]
    fn preferred_save_seed_uses_recent_directory_before_saved_preference() {
        let prefs = Preferences {
            save_folder: PathBuf::from("/configured"),
            ..Preferences::default()
        };
        let last = Some(PathBuf::from("/recent"));

        let seed = preferred_save_seed_dir(&prefs, &last);

        assert_eq!(seed, Some(PathBuf::from("/recent")));
    }

    #[test]
    fn preferred_save_seed_falls_back_to_configured_folder() {
        let prefs = Preferences {
            save_folder: PathBuf::from("/configured"),
            ..Preferences::default()
        };

        let seed = preferred_save_seed_dir(&prefs, &None);

        assert_eq!(seed, Some(PathBuf::from("/configured")));
    }

    #[test]
    fn preferred_save_seed_ignores_empty_configured_folder() {
        let prefs = Preferences::default();

        let seed = preferred_save_seed_dir(&prefs, &None);

        assert_eq!(seed, None);
    }

    #[test]
    fn settings_save_folder_to_open_prefers_configured_folder() {
        let prefs = Preferences {
            save_folder: PathBuf::from("/configured"),
            ..Preferences::default()
        };

        assert_eq!(
            settings_save_folder_to_open(&prefs),
            PathBuf::from("/configured")
        );
    }

    #[test]
    fn shortcut_recorder_requires_a_modifier() {
        let shortcut = shortcut_string_from_keypress(
            &iced::keyboard::Key::Character("x".into()),
            iced::keyboard::Modifiers::default(),
        );

        assert_eq!(shortcut, None);
    }

    #[test]
    fn shortcut_recorder_formats_command_shift_character() {
        let mut modifiers = iced::keyboard::Modifiers::default();
        modifiers.insert(iced::keyboard::Modifiers::SHIFT);
        modifiers.insert(iced::keyboard::Modifiers::LOGO);

        let shortcut =
            shortcut_string_from_keypress(&iced::keyboard::Key::Character("X".into()), modifiers);

        assert_eq!(shortcut.as_deref(), Some("cmd+shift+x"));
    }

    #[test]
    fn shortcut_recorder_cancels_on_plain_escape_only() {
        let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
        assert!(shortcut_cancelled_by_keypress(
            &escape,
            iced::keyboard::Modifiers::default()
        ));

        let mut modifiers = iced::keyboard::Modifiers::default();
        modifiers.insert(iced::keyboard::Modifiers::SHIFT);
        assert!(!shortcut_cancelled_by_keypress(&escape, modifiers));
    }

    #[test]
    fn hotkey_tick_no_events_is_noop() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        // No manager registered → tick does nothing meaningful.
        let _ = update(&mut app, Message::HotkeyTick);
        assert!(!app.capture_in_flight);
    }

    #[test]
    fn url_tick_drains_delivered_url_actions() {
        crate::url_events::clear_for_tests();
        crate::url_events::deliver_url_string("readshot://new").unwrap();
        let mut app = build_app(Arc::new(FakePermissions::granted()));

        let _ = update(&mut app, Message::UrlTick);

        assert_eq!(crate::url_events::drain_actions(), Vec::new());
    }

    #[test]
    fn load_preferences_first_launch_when_path_is_none() {
        let prefs = load_preferences(None);
        // First-launch override: history on (Last50) so the wedge
        // works the first time the user takes a screenshot.
        assert_eq!(
            prefs.history_retention,
            readshot_core::HistoryRetention::Last50,
        );
        assert_eq!(prefs.capture_hotkey, default_capture_hotkey());
    }

    #[test]
    fn load_preferences_first_launch_when_file_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("preferences.toml");
        assert!(!path.exists());
        let prefs = load_preferences(Some(&path));
        assert_eq!(
            prefs.history_retention,
            readshot_core::HistoryRetention::Last50,
        );
    }

    #[test]
    fn load_preferences_round_trips_an_existing_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("preferences.toml");
        // User explicitly turned history off — that must win over the
        // first-launch override.
        let on_disk = Preferences {
            history_retention: readshot_core::HistoryRetention::Off,
            capture_hotkey: "ctrl+alt+9".into(),
            ..Preferences::default()
        };
        on_disk.save(&path).unwrap();

        let loaded = load_preferences(Some(&path));
        assert_eq!(
            loaded.history_retention,
            readshot_core::HistoryRetention::Off,
        );
        assert_eq!(loaded.capture_hotkey, "ctrl+alt+9");
    }

    #[test]
    fn load_preferences_falls_back_when_file_is_corrupt() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("preferences.toml");
        std::fs::write(&path, "this is not [valid toml").unwrap();
        let prefs = load_preferences(Some(&path));
        // Corrupt file → first-launch defaults rather than a panic.
        assert_eq!(
            prefs.history_retention,
            readshot_core::HistoryRetention::Last50,
        );
    }

    #[test]
    fn update_settings_hotkey_change_persists_value_when_reregister_fails() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        assert!(app.hotkey_manager.is_none());
        // Garbage input — register_default_hotkey can't parse it,
        // re-registration fails, but the user's preference still gets
        // saved in-memory so they can fix the typo and try again.
        let _ = update(
            &mut app,
            Message::Settings(SettingsMessage::SetCaptureHotkey("not-a-real-chord".into())),
        );
        assert_eq!(app.preferences.capture_hotkey, "not-a-real-chord");
        assert!(app.hotkey_manager.is_none());
    }

    #[test]
    fn update_settings_non_hotkey_does_not_re_register() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        // Non-hotkey settings (e.g. debug logging) must NOT churn the
        // hotkey registration — that path only fires for hotkey
        // changes specifically.
        let _ = update(
            &mut app,
            Message::Settings(SettingsMessage::SetDebugLogging(true)),
        );
        assert!(app.preferences.debug_logging);
        // Manager stays None because we never tried to register, and
        // the test build had nothing registered to begin with.
        assert!(app.hotkey_manager.is_none());
    }

    #[test]
    fn window_closed_clears_settings_window_id() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.windows.register(id, WindowKind::Settings);
        app.settings_window_id = Some(id);
        app.settings_recording_hotkey = true;
        app.settings_hotkey_error = Some("bad shortcut".into());
        let _ = update(&mut app, Message::WindowClosed(id));
        assert!(app.settings_window_id.is_none());
        assert!(!app.settings_recording_hotkey);
        assert!(app.settings_hotkey_error.is_none());
        assert!(app.windows.kind(id).is_none());
    }
}
