//! iced daemon runtime.
//!
//! Wires [`crate::app::App`] to the iced 0.14 daemon API. This module
//! owns:
//!
//! * `start` — boot fn that builds the initial `App` and opens the
//!   welcome window when onboarding is needed.
//! * `update` — async-aware version of `App::update_sync` that
//!   produces `Task<Message>`s for capture, save, OCR, History and
//!   window work.
//! * `view` / `title` / `theme` — per-window rendering hooks (welcome,
//!   region overlay, scroll capture, editor, pins, History, Settings).
//! * `subscription` — the permission poll, the tray, global hotkeys, URL
//!   scheme events and the editor's timers and key/pointer events.
//!
//! Its submodules hold the larger areas: the annotation editor's
//! message handling (`editor_ops`), off-thread document composes
//! (`compose_jobs`), the Image menu (resize, rotate, flip, crop presets)
//! and combine (`image_menu`, `combine`), the side panel's Text and
//! Beautify tabs (`text_panel`, `beautify_panel`), scroll capture,
//! History, Settings and hotkeys.
//!
//! Everything testable without a display (state transitions, capture
//! orchestration, editor behaviour through `update`) is covered by the
//! unit tests at the bottom of this file and in the sibling modules.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use global_hotkey::GlobalHotKeyEvent;
use iced::window;
use iced::{Color, Element, Subscription, Task, Theme};

use readshot_capture::{display_local_bounds, CaptureRequest};
use readshot_core::Preferences;
use readshot_ui::SettingsMessage;

use crate::editor::EditorSession;
use crate::url_scheme::UrlAction;

mod hotkeys;
pub(crate) use hotkeys::*;
mod window_settings;
pub(crate) use window_settings::*;
mod tokens;
pub(crate) use tokens::*;
mod ui_kit;
pub(crate) use ui_kit::*;
mod view;
pub(crate) use view::*;
mod scroll;
pub(crate) use scroll::*;
mod platform;
pub(crate) use platform::*;
mod editor_ops;
pub(crate) use editor_ops::*;
mod editor_options;
pub(crate) use editor_options::*;
mod text_panel;
pub(crate) use text_panel::*;
mod side_panel;
pub(crate) use side_panel::*;
mod history_ops;
mod settings_ops;

mod beautify_panel;
mod image_menu;
pub(crate) use image_menu::*;
mod combine;
mod compose_jobs;
pub(crate) use combine::*;
mod beautify_preview;
pub(crate) use beautify_preview::*;

/// Initial URL action set from `main.rs` before `iced::daemon` starts.
/// `start()` consumes this and feeds an extra `Message::UrlActionReceived`
/// task on boot, before the welcome window even has time to paint.
static INITIAL_URL_ACTION: OnceLock<Mutex<Option<UrlAction>>> = OnceLock::new();
static CLI_INTERACTIVE_REQUEST: OnceLock<Mutex<Option<CliInteractiveRequest>>> = OnceLock::new();
/// Set by `main.rs` when the binary was started by the login
/// LaunchAgent (which passes `--launched-at-login`). A login launch
/// settles silently into the menu bar instead of opening the ready
/// window — see `needs_welcome_window_on_boot`.
static LAUNCHED_AT_LOGIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

const READSHOT_THEME_NAME: &str = "readshot";

pub(crate) const LIGHT_THEME_NAME: &str = "readshot-light";
static READSHOT_DARK: OnceLock<Theme> = OnceLock::new();
static READSHOT_LIGHT: OnceLock<Theme> = OnceLock::new();

pub(crate) fn readshot_theme_for(mode: ThemeMode) -> Theme {
    let (cell, name, p) = match mode {
        ThemeMode::Dark => (&READSHOT_DARK, READSHOT_THEME_NAME, &DARK),
        ThemeMode::Light => (&READSHOT_LIGHT, LIGHT_THEME_NAME, &LIGHT),
    };
    cell.get_or_init(|| {
        Theme::custom(
            name,
            iced::theme::Palette {
                background: p.bg,
                text: p.t1,
                primary: p.acc,
                success: p.ok,
                warning: p.warn,
                danger: p.danger,
            },
        )
    })
    .clone()
}

/// Palette for a style closure. Light only for the light Readshot theme;
/// every other theme (overlay, iced built-ins) reads dark.
pub(crate) fn palette_for(theme: &Theme) -> &'static Palette {
    use iced::theme::Base; // `name()` is a `Base` trait method
    if theme.name() == LIGHT_THEME_NAME {
        &LIGHT
    } else {
        &DARK
    }
}

/// Whether a window kind follows the system appearance. Capture
/// chrome (overlay, scroll window and outline, pins) is always dark.
pub(crate) fn follows_system_theme(kind: WindowKind) -> bool {
    !matches!(
        kind,
        WindowKind::Overlay | WindowKind::ScrollRegion | WindowKind::ScrollHud | WindowKind::Pin
    )
}

pub(crate) fn theme_mode_for(window: Option<WindowKind>, system: ThemeMode) -> ThemeMode {
    match window {
        Some(kind) if !follows_system_theme(kind) => ThemeMode::Dark,
        _ => system,
    }
}

/// The OS appearance poll only matters while some open window follows
/// it; an idle menu-bar app (or one showing only capture chrome)
/// shouldn't wake every second.
pub(crate) fn theme_poll_needed(state: &App) -> bool {
    state.windows.kinds().any(follows_system_theme)
}

/// Register a newly opened window. A system-themed window re-reads the
/// OS appearance here (in `update`, on the main thread) so it opens in
/// the right mode even though the 1 Hz poll was idle until now.
pub(crate) fn register_window(state: &mut App, id: window::Id, kind: WindowKind) {
    state.windows.register(id, kind);
    if follows_system_theme(kind) {
        state.theme_mode = os_theme_mode();
    }
}

/// Scale a colour's alpha by `factor` (clamped 0..=1). Used to dim
/// controls — e.g. colour swatches — while an editor task is in
/// flight and they carry no `on_press`.
fn fade(color: Color, factor: f32) -> Color {
    Color {
        a: (color.a * factor).clamp(0.0, 1.0),
        ..color
    }
}

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

/// Record that the binary was started by the login LaunchAgent. Call
/// from `main.rs` before the daemon takes over.
pub fn set_launched_at_login() {
    LAUNCHED_AT_LOGIN.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn launched_at_login() -> bool {
    LAUNCHED_AT_LOGIN.load(std::sync::atomic::Ordering::Relaxed)
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

#[cfg(test)]
fn debug_editor_qa_session() -> EditorSession {
    let image = debug_editor_qa_image();
    let mut editor = EditorSession::new_with_display_scale(image, 2.0);
    let annotations = debug_editor_qa_annotations();
    editor.next_pin_number = annotations
        .iter()
        .filter_map(|annotation| match annotation {
            readshot_core::Annotation::NumberedPin { number, .. } => Some(*number),
            _ => None,
        })
        .max()
        .and_then(|n| n.checked_add(1))
        .unwrap_or(1);
    for annotation in annotations {
        editor.model.commit_annotation(annotation);
    }
    editor.refresh_image();
    editor.mark_output_clean();
    editor.set_status("Editor QA fixture loaded.");
    editor
}

#[cfg(test)]
fn debug_editor_qa_image() -> image::RgbaImage {
    let mut img = image::RgbaImage::from_pixel(1040, 680, image::Rgba([247, 249, 252, 255]));
    fill_rect_rgba(&mut img, 0, 0, 1040, 56, [18, 24, 38, 255]);
    fill_rect_rgba(&mut img, 0, 56, 240, 624, [234, 238, 245, 255]);
    fill_rect_rgba(&mut img, 280, 112, 680, 150, [255, 255, 255, 255]);
    fill_rect_rgba(&mut img, 280, 296, 680, 248, [22, 26, 34, 255]);
    fill_rect_rgba(&mut img, 304, 326, 632, 1, [58, 66, 82, 255]);
    fill_rect_rgba(&mut img, 304, 372, 632, 1, [58, 66, 82, 255]);
    fill_rect_rgba(&mut img, 304, 418, 420, 1, [58, 66, 82, 255]);
    fill_rect_rgba(&mut img, 316, 468, 190, 38, [20, 184, 166, 255]);
    fill_rect_rgba(&mut img, 528, 468, 146, 38, [71, 85, 105, 255]);
    fill_rect_rgba(&mut img, 40, 112, 160, 16, [148, 163, 184, 255]);
    fill_rect_rgba(&mut img, 40, 158, 168, 12, [203, 213, 225, 255]);
    fill_rect_rgba(&mut img, 40, 188, 128, 12, [203, 213, 225, 255]);
    fill_rect_rgba(&mut img, 40, 218, 184, 12, [203, 213, 225, 255]);
    img
}

#[cfg(test)]
fn fill_rect_rgba(
    img: &mut image::RgbaImage,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    color: [u8; 4],
) {
    let x_end = x.saturating_add(width).min(img.width());
    let y_end = y.saturating_add(height).min(img.height());
    for yy in y..y_end {
        for xx in x..x_end {
            img.put_pixel(xx, yy, image::Rgba(color));
        }
    }
}

#[cfg(test)]
fn debug_editor_qa_annotations() -> Vec<readshot_core::Annotation> {
    use readshot_core::{Annotation, PointLike, RectLike, Rgba};
    vec![
        Annotation::Text {
            content: "Readshot editor QA".into(),
            origin: PointLike::new(38.0, 34.0),
            color: Rgba::OPAQUE_WHITE,
            font_family: "system-ui".into(),
            size: 24.0,
            style: Default::default(),
        },
        Annotation::Rectangle {
            rect: RectLike::new(270.0, 102.0, 700.0, 170.0),
            color: Rgba::new(0.08, 0.72, 0.65, 1.0),
            line_width: 5.0,
            style: Default::default(),
        },
        Annotation::Arrow {
            a: PointLike::new(236.0, 224.0),
            b: PointLike::new(330.0, 142.0),
            color: Rgba::new(0.93, 0.34, 0.34, 1.0),
            line_width: 6.0,
            style: Default::default(),
        },
        Annotation::Highlighter {
            points: vec![
                PointLike::new(300.0, 500.0),
                PointLike::new(420.0, 498.0),
                PointLike::new(560.0, 500.0),
            ],
            color: Rgba::new(1.0, 0.84, 0.22, 0.38),
            line_width: 18.0,
        },
        Annotation::NumberedPin {
            origin: PointLike::new(686.0, 468.0),
            number: 1,
            color: Rgba::new(0.08, 0.72, 0.65, 1.0),
        },
        Annotation::Text {
            content: "Move, resize, edit text, frame, copy, save.".into(),
            origin: PointLike::new(300.0, 586.0),
            color: Rgba::new(0.10, 0.12, 0.16, 1.0),
            font_family: "system-ui".into(),
            size: 19.0,
            style: Default::default(),
        },
    ]
}

use crate::app::{App, GlobalHotkeyAction, HistoryKeyboardAction, Message, WindowKind};
use crate::coordinator::CaptureCoordinator;
use crate::permissions::default_provider;
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
    // Honour the OS reduce-motion setting from the first frame; it is
    // refreshed when an animated surface opens (see the overlay/scroll
    // handlers) in case the user toggles it while Readshot runs.
    app.reduce_motion = os_reduce_motion();
    let cli_interactive_request = take_cli_interactive_request();
    if let Some(request) = cli_interactive_request.as_ref() {
        app.cli_interactive_output = Some(request.output.clone());
        app.pending_intent = Some(crate::app::CaptureIntent::CliInteractive);
        app.pending_hide_cursor = request.hide_cursor;
    }

    // Surface the initial permission state in the log so users
    // (and us, when triaging issues) can see whether macOS TCC is
    // already returning Granted before the welcome window appears.
    // Already probed once in `App::new`; don't probe (and maybe prompt) again.
    let initial_permission = app.permission_status;
    tracing::info!(
        target: "readshot::permissions",
        "initial status: {:?}",
        initial_permission,
    );
    arm_relaunch_on_system_quit(matches!(
        initial_permission,
        crate::permissions::PermissionStatus::Denied
    ));

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
    if let Some(registration) = register_default_hotkey(&app.preferences) {
        app.hotkey_manager = Some(registration.manager);
        app.hotkey_actions = registration.actions;
        app.capture_hotkey_registered = registration.capture_registered;
        app.fixed_hotkey_failures = registration.failed_fixed;
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
    if let Err(e) = crate::app_events::install_platform_handler() {
        tracing::warn!(target: "readshot::app-events", "app reopen handler unavailable: {e}");
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = crate::updater::install() {
        tracing::warn!(target: "readshot::updater", "Sparkle updater unavailable: {e}");
    }
    // Same fail-soft contract for the tray. Linux without an
    // appindicator daemon, or a Windows session without a Shell_Notify
    // surface, will simply not see the tray entry.
    let hotkey_label = pretty_hotkey(&app.preferences.capture_hotkey);
    app.tray = crate::tray::install(Some(&app.preferences.capture_hotkey), Some(&hotkey_label));

    // The welcome window is a *first-run permission gate*, not the
    // app's main UI. Once Screen Recording is granted the user lives
    // inside the menu-bar tray icon + global hotkey, which is what
    // `LSUIElement=true` apps are supposed to look like. A *manual*
    // relaunch still opens the window once so macOS's "Quit & Reopen"
    // flow has an obvious result; a *login* launch deliberately does
    // not, so the app just appears in the menu bar at every boot
    // instead of popping a window in the user's face.
    let initial_url_action = take_initial_url_action();
    let mut tasks: Vec<Task<Message>> =
        vec![iced::system::theme().map(Message::SystemThemeChanged)];
    let needs_welcome_window = needs_welcome_window_on_boot(
        app.welcome,
        initial_url_action.is_some(),
        launched_at_login(),
    );
    if needs_welcome_window {
        let (id, open_task) = window::open(welcome_window_settings());
        app.windows.register(id, WindowKind::Welcome);
        tasks.push(open_task.map(Message::WelcomeWindowReady));
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
    if let Some(action) = initial_url_action {
        tasks.push(Task::done(Message::UrlActionReceived(action)));
    }
    (app, Task::batch(tasks))
}

fn needs_welcome_window_on_boot(
    welcome: WelcomeState,
    has_initial_url_action: bool,
    launched_at_login: bool,
) -> bool {
    // The permission gate always shows until Screen Recording is granted.
    if welcome.should_show() {
        return true;
    }
    // Granted: a URL action drives the boot (no window), and a login
    // launch settles silently into the menu bar (tray icon + the
    // "running in the menu bar" notification are the feedback). Only a
    // manual relaunch opens the ready window.
    !has_initial_url_action && !launched_at_login
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
        Some(WindowKind::ScrollRegion) => "Readshot — Capture Region".into(),
    }
}

/// Sentinel name for the transparent overlay theme — used by [`style`]
/// to detect overlay windows and return a fully see-through palette.
const OVERLAY_THEME_NAME: &str = "readshot-overlay-transparent";

/// Theme: Readshot-branded dark by default. Overlay windows get a custom theme whose
/// `name()` is `OVERLAY_THEME_NAME` so `style` can identify them
/// and return a transparent base style. The palette colors don't
/// matter for the canvas-only overlay view, so we copy `Theme::Dark`
/// to avoid widget surprises if iced ever consults them.
pub fn theme(state: &App, id: window::Id) -> Theme {
    let kind = state.windows.kind(id);
    if matches!(
        kind,
        Some(WindowKind::Overlay) | Some(WindowKind::ScrollRegion) | Some(WindowKind::ScrollHud)
    ) {
        return Theme::custom(OVERLAY_THEME_NAME.to_string(), iced::theme::Palette::DARK);
    }
    readshot_theme_for(theme_mode_for(kind, state.theme_mode))
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
            text_color: DARK.t1,
        }
    } else {
        theme.base()
    }
}

/// Bring the running scroll capture's window forward (no-op when it
/// has none yet). Used by capture entry points that must not start a
/// second session over a live one.
fn focus_scroll_hud(state: &App) -> Task<Message> {
    match state.scroll_session.as_ref().and_then(|s| s.hud_window_id) {
        Some(id) => window::gain_focus(id),
        None => Task::none(),
    }
}

/// Background subscriptions — permission poll + global-hotkey drain
/// + app/window keyboard shortcuts.
pub fn subscription(state: &App) -> Subscription<Message> {
    let mut subs = Vec::new();
    subs.push(iced::system::theme_changes().map(Message::SystemThemeChanged));
    // iced pins each open window's appearance to its theme, so winit
    // suppresses theme-change events for those windows; poll the OS too.
    // The handler is idempotent, so identical repeats are free.
    // The map closure runs on the tokio executor, where `os_theme_mode`
    // (main-thread only) would always read Dark, so emit a bare tick and
    // read the OS appearance in `update()`.
    // Only while a system-themed window is open (see `theme_poll_needed`).
    if theme_poll_needed(state) {
        subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::ThemePollTick));
    }
    if state.welcome.should_show() {
        // Polling at 500 ms keeps the UI responsive without burning
        // CPU. The TCC db is already cached in-process so each poll
        // is essentially a no-op syscall.
        subs.push(
            iced::time::every(Duration::from_millis(500)).map(|_| Message::PermissionPollTick),
        );
    }
    // These four drains poll OS-event sources (crossbeam channels /
    // AppKit-fed queues) that have no async wakeup, so they run on a
    // timer for the whole app lifetime. Every tick is an iced message,
    // and iced redraws *every* open window on any message — so while an
    // editor window is open these timers repaint it at their combined
    // rate even when nothing changed. They're kept deliberately slow:
    // the canvas geometry cache already makes those redraws cheap (no
    // re-tessellation), and a few hundred ms of latency on a hotkey /
    // tray click is imperceptible, so the win is far fewer idle wake-ups
    // rather than snappier polling.
    if state.hotkey_manager.is_some() {
        // 250 ms drain — `try_recv` is a non-blocking peek at the
        // crossbeam channel `global-hotkey` writes to from its OS event
        // handler. Latency from keypress to capture stays well under the
        // threshold where a global hotkey feels laggy.
        subs.push(iced::time::every(Duration::from_millis(250)).map(|_| Message::HotkeyTick));
    }
    if state.tray.is_some() {
        // 250 ms is fine for tray clicks — humans can't tell the
        // difference between a 100 ms and 250 ms tray menu response.
        subs.push(iced::time::every(Duration::from_millis(250)).map(|_| Message::TrayTick));
    }
    #[cfg(target_os = "macos")]
    {
        // macOS delivers URL + reopen AppleEvents to AppKit callbacks
        // while the app is already running. Those callbacks queue work;
        // these ticks drain them back into iced. Both are rare, external,
        // user-initiated events, so a slower poll costs nothing.
        subs.push(iced::time::every(Duration::from_millis(500)).map(|_| Message::UrlTick));
        subs.push(iced::time::every(Duration::from_millis(500)).map(|_| Message::AppReopenTick));
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
        // Esc anywhere ends the session (same as the Stop button);
        // Shift+Esc / ⌘. discards it (same as Cancel, which arms then
        // confirms). See `scroll_key_message`.
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                return scroll_key_message(&key, modifiers);
            }
            None
        }));
    }
    if !state.overlay_displays.is_empty() || state.scroll_session.is_some() {
        // Global Shift watcher — iced 0.14 does not reliably route
        // modifier changes into canvas widgets, so the overlay's "hold Shift =
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
    if state
        .editor
        .as_ref()
        .map(|e| e.status.is_some() && !e.status_is_in_progress())
        .unwrap_or(false)
    {
        // Slow tick drives the auto-dismiss of stale status pills.
        // Only runs while there's actually something to dismiss so
        // the editor doesn't burn CPU on idle windows.
        subs.push(
            iced::time::every(Duration::from_millis(1000)).map(|_| Message::EditorStatusTick),
        );
    }
    if !state.reduce_motion
        && state.editor.as_ref().is_some_and(|e| {
            matches!(
                editor_panel_mode(state, e),
                PanelMode::Docked | PanelMode::Overlay
            ) && matches!(
                e.text_panel.recognition,
                crate::editor::TextRecognition::Recognizing { .. }
            )
        })
    {
        // Pulses the recognizing skeleton. Never under Reduce Motion,
        // where the skeleton is static.
        subs.push(iced::time::every(Duration::from_millis(80)).map(|_| Message::EditorPanelTick));
    }
    if state.editor.is_some() {
        // Keyboard sub: ⌘Z / Ctrl+Z = Undo, ⇧⌘Z / Ctrl+Shift+Z = Redo,
        // ⌘S / Ctrl+S = Save. Iced 0.14's `event::listen_with` is the
        // window-agnostic event tap; we filter to KeyPressed events
        // and only react when the editor window is the focused one
        // (the canvas gets first chance to handle Escape; unhandled
        // Escape falls through here as the advertised Discard shortcut).
        subs.push(iced::event::listen_with(|event, status, window| {
            use iced::event::Status;
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Window(iced::window::Event::Rescaled(scale)) = event {
                return Some(Message::WindowRescaled(window, scale));
            }
            if let iced::Event::Window(iced::window::Event::Resized(size)) = event {
                return Some(Message::EditorWindowResized(window, size.width));
            }
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                return Some(Message::EditorKeyPressed {
                    window,
                    key,
                    modifiers,
                    status_ignored: status == Status::Ignored,
                });
            }
            None
        }));
        // Global Shift watcher for the editor — iced 0.14 doesn't reliably route
        // modifier changes into canvas widgets, so the editor canvas's
        // "hold Shift = constrain" relies on this forwarding modifier
        // state into App.editor_shift_held (mirrors the overlay watcher).
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            match event {
                iced::Event::Keyboard(KbEvent::KeyPressed { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::KeyReleased { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::ModifiersChanged(modifiers)) => {
                    Some(Message::EditorShiftChanged(modifiers.shift()))
                }
                _ => None,
            }
        }));
        // Left-button state: a slider cancelled while held ignores its
        // ticks until release; one previewed by keys or Ctrl+wheel does not.
        // Focus loss of the window a press began in clears it (its
        // release may go elsewhere or never come).
        subs.push(iced::event::listen_with(|event, _status, window| {
            use iced::mouse::{Button, Event as MouseEvent};
            match event {
                iced::Event::Mouse(MouseEvent::ButtonPressed(Button::Left)) => {
                    Some(Message::EditorPointerButton(true, Some(window)))
                }
                iced::Event::Mouse(MouseEvent::ButtonReleased(Button::Left)) => {
                    Some(Message::EditorPointerButton(false, Some(window)))
                }
                iced::Event::Window(iced::window::Event::Unfocused) => {
                    Some(Message::EditorPointerWindowUnfocused(window))
                }
                _ => None,
            }
        }));
        // Command state for ⌘-click multi-select in the Text panel.
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            match event {
                iced::Event::Keyboard(KbEvent::KeyPressed { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::KeyReleased { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::ModifiersChanged(modifiers)) => {
                    Some(Message::EditorCommandChanged(modifiers.command()))
                }
                _ => None,
            }
        }));
    }
    if state.settings_recording_hotkey {
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                if shortcut_cancelled_by_keypress(&key, modifiers) {
                    return Some(Message::SettingsHotkeyRecordingCancelled);
                }
                // Ignore bare modifier presses: the user is mid-chord,
                // holding Cmd/Ctrl/Shift before the real key. Reporting
                // those as invalid is what made recording feel stuck.
                if is_modifier_key(&key) {
                    return None;
                }
                return Some(
                    shortcut_string_from_keypress(&key, modifiers)
                        .map(Message::SettingsHotkeyRecorded)
                        .unwrap_or(Message::SettingsHotkeyRecordingInvalid),
                );
            }
            None
        }));
    } else {
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event else {
                return None;
            };
            app_window_shortcut_message(key, modifiers)
        }));
    }
    if state.history_window_id.is_some() {
        subs.push(iced::event::listen_with(|event, status, window| {
            use iced::event::Status;
            use iced::keyboard::Event as KbEvent;
            let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event else {
                return None;
            };
            history_key_message(window, &key, modifiers, status == Status::Ignored)
        }));
    }
    if state.settings_window_id.is_some() && !state.settings_recording_hotkey {
        // Settings is pointer-only (togglers, pick-lists, buttons), none
        // of which iced 0.14 can focus. Tap keys and route them to the
        // settings handler, which validates the window id. The filename
        // text_input keeps plain typing because every shortcut is
        // ⌘-prefixed (Esc aside). See `settings_key_action`.
        subs.push(iced::event::listen_with(|event, _status, window| {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                return Some(Message::SettingsKeyPressed {
                    window,
                    key,
                    modifiers,
                });
            }
            None
        }));
    }
    if !state.pins.is_empty() {
        // Pins are borderless, always-on-top thumbnails whose only
        // chrome is pointer-driven; give the focused pin a keyboard map
        // (close / lock / opacity / move). See `pin_key_action`.
        subs.push(iced::event::listen_with(|event, _status, window| {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                return Some(Message::PinKeyPressed {
                    window,
                    key,
                    modifiers,
                });
            }
            None
        }));
    }
    if state.windows.kinds().any(|k| k == WindowKind::Welcome) {
        // Bind Enter to the welcome card's primary action so the
        // permission / capture flow is operable without the mouse.
        // See `welcome_key_message`.
        subs.push(iced::event::listen_with(|event, _status, window| {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                return Some(Message::WelcomeKeyPressed {
                    window,
                    key,
                    modifiers,
                });
            }
            None
        }));
    }
    // Watch for the OS X-button closing any tracked window. Without
    // this, `state.windows` accumulates stale ids, and helpers like
    // `show_or_focus_welcome` end up calling `gain_focus` on dead
    // windows (silent no-op) instead of opening a fresh one.
    subs.push(window::close_events().map(Message::WindowClosed));
    // The editor window opts out of iced's auto-close
    // (`exit_on_close_request: false`) so an OS close (red X / ⌘W) can
    // be routed through the unsaved-work confirmation. Every other
    // window auto-closes and only needs `close_events`; this handler
    // ignores them.
    subs.push(window::close_requests().map(Message::WindowCloseRequested));
    Subscription::batch(subs)
}

/// Map a key press in the History window to a message. `status_ignored`
/// is false when a widget (the search field) already handled the event;
/// the copy / delete chords then stay with the field.
pub(crate) fn history_key_message(
    window: iced::window::Id,
    key: &iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
    status_ignored: bool,
) -> Option<Message> {
    use iced::keyboard::{key::Named, Key};
    let action = |a| Some(Message::HistoryKeyboardShortcut(window, a));
    // A focused search field captures ⌘C and ⌫/⌦ even when it has nothing
    // to copy or delete. Subscriptions cannot see app state, so captured
    // chords go out as `HistoryCapturedShortcut` and the handler applies
    // them only while the search query is empty.
    let chord = |a| {
        Some(if status_ignored {
            Message::HistoryKeyboardShortcut(window, a)
        } else {
            Message::HistoryCapturedShortcut(window, a)
        })
    };
    // ⌘F focuses the search field, wherever focus is.
    if modifiers.command()
        && !modifiers.shift()
        && !modifiers.alt()
        && !modifiers.control()
        && matches!(key, Key::Character(c) if c.eq_ignore_ascii_case("f"))
    {
        return action(HistoryKeyboardAction::FocusSearch);
    }
    // ⌘C copies the image, ⇧⌘C the text.
    if modifiers.command() && !modifiers.alt() && !modifiers.control() {
        if let Key::Character(c) = key {
            if c.eq_ignore_ascii_case("c") {
                return chord(if modifiers.shift() {
                    HistoryKeyboardAction::CopyText
                } else {
                    HistoryKeyboardAction::CopyImage
                });
            }
        }
    }
    // Delete: ⌦ or ⌘⌫ (Move to Trash). Plain ⌫ is never bound.
    if !modifiers.alt() && !modifiers.control() && !modifiers.shift() {
        match key {
            Key::Named(Named::Delete) => return chord(HistoryKeyboardAction::Delete),
            Key::Named(Named::Backspace) if modifiers.command() => {
                return chord(HistoryKeyboardAction::Delete)
            }
            _ => {}
        }
    }
    if modifiers.command() || modifiers.alt() || modifiers.control() || modifiers.shift() {
        return None;
    }
    // Arrow / Enter always navigate the list; the search input is
    // single-line so they don't conflict.
    match key {
        Key::Named(Named::ArrowUp) => action(HistoryKeyboardAction::Previous),
        Key::Named(Named::ArrowDown) => action(HistoryKeyboardAction::Next),
        Key::Named(Named::Enter) => action(HistoryKeyboardAction::Open),
        Key::Named(Named::Escape) => Some(Message::HistorySearchChanged(String::new())),
        _ => None,
    }
}

fn app_window_shortcut_message(
    key: iced::keyboard::Key,
    modifiers: iced::keyboard::Modifiers,
) -> Option<Message> {
    use iced::keyboard::Key;

    if !modifiers.command()
        || modifiers.shift()
        || modifiers.alt()
        || extra_command_modifier(modifiers)
    {
        return None;
    }
    match key {
        Key::Character(c) if c.eq_ignore_ascii_case("y") => Some(Message::OpenHistoryRequested),
        Key::Character(c) if c == "," => Some(Message::OpenSettingsRequested),
        _ => None,
    }
}

/// Top-level update fn. Delegates testable transitions to
/// [`App::update_sync`] and adds the iced-only async branches. After
/// every message the editor's off-thread document work is started
/// ([`compose_jobs::after_update`]).
pub fn update(state: &mut App, message: Message) -> Task<Message> {
    let task = update_message(state, message);
    Task::batch([task, compose_jobs::after_update(state)])
}

fn update_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::WelcomeWindowReady(id) => {
            if !state.welcome.should_show() {
                state.mark_onboarding_completed();
            }
            window::gain_focus(id)
        }

        Message::WelcomeKeyPressed {
            window,
            key,
            modifiers,
        } => {
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

        // Explicit checks (Recheck, after Allow) may probe; the periodic
        // tick must not, or macOS re-raises its permission alert each time.
        tick @ (Message::PermissionTick | Message::PermissionPollTick) => {
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

        Message::OverlayShiftChanged(held) => {
            // No-op when state didn't actually flip — keeps the redraw
            // path from firing on every key event.
            if state.overlay_shift_held == held {
                return Task::none();
            }
            state.overlay_shift_held = held;
            // The overlay rebuilds OverlayProgram from `state` on every
            // view(); the state update itself schedules the next view,
            // so this does not need the animation tick to be active.
            Task::none()
        }

        Message::HotkeyTick => {
            // Drain anything the OS-side handler queued. We collapse
            // repeated presses into one action per tick so a mashed
            // hotkey doesn't stack pending windows/captures.
            let mut capture = false;
            let mut history = false;
            let mut settings = false;
            let receiver = GlobalHotKeyEvent::receiver();
            while let Ok(event) = receiver.try_recv() {
                match global_hotkey_action(&event, &state.hotkey_actions) {
                    Some(GlobalHotkeyAction::Capture) => capture = true,
                    Some(GlobalHotkeyAction::History) => history = true,
                    Some(GlobalHotkeyAction::Settings) => settings = true,
                    None => {}
                }
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

        Message::AppReopenTick => {
            if crate::app_events::take_reopen_requests() == 0 {
                Task::none()
            } else {
                Task::done(Message::AppReopenRequested)
            }
        }

        Message::AppReopenRequested => show_or_focus_welcome(state),

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
                    tracing::warn!(target: "readshot::updater", "manual update check failed: {e}");
                    notify_update_check_failed(&e.to_string());
                }
                Task::none()
            }
            crate::tray::TrayAction::Quit => quit_readshot(&state.coordinator),
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
            // An Add Capture whose overlay can't open gives its hidden
            // editor back at once: whenever this request opens nothing and
            // no overlay is up or on its way.
            let overlay_up =
                state.overlay_opening || state.windows.kinds().any(|k| k == WindowKind::Overlay);
            let refused = state.welcome.should_show()
                || state.capture_in_flight
                || state.scroll_session.is_some()
                || overlay_up;
            // Never once the combine's own capture is in flight (its
            // selection was made): see `abort_combine_capture`.
            let restore = if refused && !overlay_up && !combine_selection_made(state) {
                let why = if state.capture_in_flight || state.scroll_session.is_some() {
                    "Couldn't add the capture — another capture is in progress."
                } else {
                    "Couldn't add the capture. Try again."
                };
                abort_combine_capture(state, Some(why))
            } else {
                Task::none()
            };
            if state.welcome.should_show() || state.capture_in_flight || state.overlay_opening {
                return restore;
            }
            // A scroll capture is running or stitching: starting another
            // would overwrite its session and orphan its windows.
            if state.scroll_session.is_some() {
                return Task::batch([restore, focus_scroll_hud(state)]);
            }
            // Don't stack overlays.
            if state.windows.kinds().any(|k| k == WindowKind::Overlay) {
                return Task::none();
            }
            // List displays asynchronously; the result drives the
            // actual window-open work in `OverlayDisplaysListed` so we
            // can spawn one transparent overlay per monitor.
            let coord = state.coordinator.clone();
            // Cheap refresh so a scroll session started from this
            // overlay honours a reduce-motion toggle the user flipped
            // since launch.
            state.reduce_motion = os_reduce_motion();
            state.overlay_opening = true;
            Task::perform(async move { coord.list_displays().await }, |result| {
                Message::OverlayDisplaysListed(result.map_err(|e| e.to_string()))
            })
        }

        Message::RetakeLastRegionRequested => {
            if state.welcome.should_show() {
                return show_or_focus_welcome(state);
            }
            // An Add Capture is choosing its region: a retake would land
            // in that editor instead.
            if state.capture_in_flight || state.combine_capture.is_some() {
                return Task::none();
            }
            if state.scroll_session.is_some() {
                return focus_scroll_hud(state);
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
            state.pending_display_bounds = last.display_bounds;
            update(
                state,
                Message::CaptureRegionRequested {
                    display_id,
                    rect: last.rect,
                },
            )
        }

        Message::OverlayDisplaysListed(Err(e)) => {
            state.overlay_opening = false;
            clear_pending_capture_state(state);
            tracing::warn!(target: "readshot::capture", "could not list displays: {e}");
            state.last_capture_status = Some(history_ops::friendly_capture_error(&e));
            let restore = abort_combine_capture(state, Some(&combine_capture_failed_status(&e)));
            if state.cli_interactive_output.is_some() {
                Task::batch([
                    restore,
                    Task::done(Message::CliInteractiveWritten(Err(format!(
                        "could not list displays: {e}"
                    )))),
                ])
            } else {
                restore
            }
        }
        Message::OverlayDisplaysListed(Ok(displays)) => {
            state.overlay_opening = false;
            if displays.is_empty() {
                clear_pending_capture_state(state);
                tracing::warn!(target: "readshot::capture", "no displays detected");
                state.last_capture_status =
                    Some("Couldn't capture — no display was detected.".into());
                let restore = abort_combine_capture(
                    state,
                    Some("Couldn't add the capture — no display was detected."),
                );
                return if state.cli_interactive_output.is_some() {
                    Task::batch([
                        restore,
                        Task::done(Message::CliInteractiveWritten(Err(
                            "no displays detected".into()
                        ))),
                    ])
                } else {
                    restore
                };
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
                        origin_x: d.bounds.x(),
                        origin_y: d.bounds.y(),
                        width: d.bounds.width(),
                        height: d.bounds.height(),
                    },
                );
                tasks.push(open_task.map(Message::OverlayWindowReady));
            }
            Task::batch(tasks)
        }

        Message::OverlayWindowReady(id) => configure_overlay_window_after_open(id),

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
            let display_bounds =
                overlay_record.map(|d| (d.origin_x, d.origin_y, d.width, d.height));
            state.last_regions.insert(
                display_id.clone(),
                crate::app::LastRegion {
                    rect,
                    display_scale,
                    display_bounds,
                },
            );
            state.last_region_display_id = Some(display_id.clone());
            if let Some(tray) = state.tray.as_ref() {
                tray.set_retake_last_region_enabled(true);
            }
            // An Add Capture's overlay always returns to its editor, even
            // if another flow overwrote the pending intent meanwhile: the
            // hidden editor must never be stranded.
            let intent = if let Some(cc) = state.combine_capture.as_mut() {
                cc.selected = true;
                crate::app::CaptureIntent::CombineIntoEditor
            } else {
                intent
            };
            state.pending_intent = Some(intent);
            state.pending_display_id = Some(display_id.clone());
            state.pending_display_scale = Some(display_scale);
            state.pending_display_bounds = display_bounds;
            state.overlay_selections.clear();
            let mut tasks = close_all_overlays(state);
            // Scroll capture: skip the synchronous first-frame capture
            // and open the HUD + region indicator immediately. The
            // timer tick captures frame 1 on its next fire, ~120 ms
            // later, so the HUD shows up instantly instead of waiting
            // for the ~100-200 ms ScreenCaptureKit round-trip.
            if matches!(intent, crate::app::CaptureIntent::ScrollCapture) {
                let Some(bounds) = display_bounds else {
                    // We set pending_intent/display_id/display_scale/
                    // display_bounds above; a scroll capture with no known
                    // display bounds can't proceed, so clear that state
                    // instead of leaving it dangling for the next flow.
                    clear_pending_capture_state(state);
                    return Task::batch(tasks);
                };
                let session = crate::app::ScrollSession {
                    display_id: display_id.clone(),
                    rect,
                    scale: display_scale,
                    frames: Vec::new(),
                    no_motion_count: 0,
                    hud_window_id: None,
                    region_window_id: None,
                    display_size: Some(bounds),
                    capture_in_flight: 0,
                    next_capture_seq: 0,
                    next_frame_seq_to_process: 1,
                    pending_frames: std::collections::BTreeMap::new(),
                    judge_in_flight: None,
                    stopping: false,
                    started_at: std::time::Instant::now(),
                    last_frame_at: None,
                    frame_tick: 0,
                    preview_strip: None,
                    preview_handle: None,
                    stitched_rows: 0,
                    last_frame_rows_scaled: 0,
                    stitch_frame_count: 0,
                    cancel_armed_at: None,
                    recent_join_heights: std::collections::VecDeque::new(),
                };
                state.reduce_motion = os_reduce_motion();
                state.scroll_session = Some(session);
                // The session carries its own display; the pending
                // fields are for a one-shot region capture and would be
                // stale for whatever capture comes next.
                clear_pending_capture_state(state);
                let (region_id, region_open) = window::open(scroll_region_window_settings(bounds));
                state.windows.register(region_id, WindowKind::ScrollRegion);
                if let Some(s) = state.scroll_session.as_mut() {
                    s.region_window_id = Some(region_id);
                }
                tasks.push(region_open.map(Message::ScrollRegionWindowReady));
                let (hud_pos, hud_size) = scroll_window_frame(rect, Some(bounds));
                let (hud_id, hud_open) =
                    window::open(scroll_hud_window_settings(hud_pos, hud_size));
                state.windows.register(hud_id, WindowKind::ScrollHud);
                tasks.push(hud_open.map(Message::ScrollHudWindowReady));
                return Task::batch(tasks);
            }
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
            state.overlay_opening = false;
            // A stray cancel after a combine's selection must leave its
            // in-flight capture's display scale and cursor choice alone.
            if !combine_selection_made(state) {
                clear_pending_capture_state(state);
            }
            let restore = abort_combine_capture(state, None);
            if state.cli_interactive_output.is_some() {
                state.cli_interactive_output = None;
                return Task::batch(
                    close_all_overlays(state)
                        .into_iter()
                        .chain([restore, iced::exit()]),
                );
            }
            Task::batch(
                close_all_overlays(state)
                    .into_iter()
                    .chain(std::iter::once(restore)),
            )
        }

        Message::CaptureRegionRequested { display_id, rect } => {
            let coord = state.coordinator.clone();
            let hide_cursor = state.pending_hide_cursor;
            let display_scale = state.pending_display_scale;
            state.capture_in_flight = true;
            state.last_capture_status = None;
            // Region capture lands in the editor instead of saving
            // directly — the editor decides what to do with it.
            Task::perform(
                async move {
                    match display_scale {
                        Some(scale) => {
                            capture_region_to_image_with_scale(
                                coord,
                                display_id,
                                rect,
                                scale,
                                hide_cursor,
                            )
                            .await
                        }
                        None => capture_region_to_image(coord, display_id, rect, hide_cursor).await,
                    }
                },
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
            let display_bounds = state.pending_display_bounds.take();
            state.pending_hide_cursor = true;
            // Add Capture: back to the same editor, no History record.
            if intent == crate::app::CaptureIntent::CombineIntoEditor
                || state.combine_capture.is_some()
            {
                return match result {
                    Ok(image) => finish_combine_capture(state, image),
                    Err(e) => {
                        tracing::warn!(target: "readshot::capture", "region capture failed: {e}");
                        let status = combine_capture_failed_status(&e);
                        fail_combine_capture(state, Some(&status))
                    }
                };
            }
            match result {
                Ok(image) => {
                    let history_record = history_record_for_capture(
                        &image,
                        &display_id,
                        state.preferences.history_retention,
                    );
                    // One OCR per capture: when history is on (a record
                    // exists) its task OCRs the same base image, so an
                    // editor opened for this capture shares that result.
                    // Its session id is allocated up front so the history
                    // task can tag the result before the editor exists.
                    let shared_session = (intent == crate::app::CaptureIntent::Editor
                        && history_record.is_some())
                    .then(crate::editor::next_session_id);
                    // Persist a history record in parallel with the
                    // user-visible intent action. The history task is
                    // gated on `preferences.history_retention` and
                    // logs internally on failure, so it can never
                    // block or fail the user-visible flow.
                    let history_task = if intent == crate::app::CaptureIntent::CliInteractive
                        || history_record.is_none()
                    {
                        Task::none()
                    } else {
                        persist_history_task(
                            state.coordinator.clone(),
                            image.clone(),
                            history_record.clone(),
                            state.preferences.history_retention,
                            state.preferences.clone(),
                            shared_session,
                        )
                    };
                    let intent_task = match intent {
                        crate::app::CaptureIntent::Editor => {
                            let mut editor = match history_record {
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
                            };
                            // Mark the panel Recognizing so
                            // `request_text_recognition` doesn't start a
                            // second OCR.
                            if let Some(session) = shared_session {
                                editor.session_id = session;
                                editor.text_panel.recognition =
                                    crate::editor::TextRecognition::Recognizing {
                                        since: std::time::Instant::now(),
                                    };
                                editor.text_panel.recognizing_part =
                                    Some(readshot_core::document::ORIGINAL_PART);
                            }
                            open_editor_window_replacing(state, editor, display_bounds)
                        }
                        crate::app::CaptureIntent::CopyToClipboard => {
                            Task::perform(copy_image_to_clipboard(image), |r| {
                                Message::OverlayCopyDone(r.map_err(|e| e.to_string()))
                            })
                        }
                        crate::app::CaptureIntent::CopyTextDirect => {
                            let coord = state.coordinator.clone();
                            let prefs = state.preferences.clone();
                            Task::perform(ocr_then_copy(coord, image, prefs, false), |r| {
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
                            let handle = rgba_handle(image);
                            let (id, open_task) =
                                window::open(pin_window_settings(size, display_bounds));
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
                        crate::app::CaptureIntent::CombineIntoEditor => {
                            unreachable!("handled above")
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
                            let mut session = crate::app::ScrollSession::new(
                                display_id.clone(),
                                last.rect,
                                display_scale,
                            );
                            let rows = preview_rows(&image, None);
                            accept_scroll_frame(&mut session, image, Some(rows), None);
                            session.display_size = display_bounds;
                            state.reduce_motion = os_reduce_motion();
                            state.scroll_session = Some(session);
                            // Open the click-through region indicator
                            // first so the user sees the rect outline
                            // anchored to the page they're scrolling.
                            // The HUD opens beside it for stop/cancel.
                            let mut tasks: Vec<Task<Message>> = Vec::new();
                            if let Some(bounds) = display_bounds {
                                let (region_id, region_open) =
                                    window::open(scroll_region_window_settings(bounds));
                                state.windows.register(region_id, WindowKind::ScrollRegion);
                                if let Some(s) = state.scroll_session.as_mut() {
                                    s.region_window_id = Some(region_id);
                                }
                                tasks.push(region_open.map(Message::ScrollRegionWindowReady));
                            }
                            let (hud_pos, hud_size) =
                                scroll_window_frame(last.rect, display_bounds);
                            let (hud_id, hud_open) =
                                window::open(scroll_hud_window_settings(hud_pos, hud_size));
                            state.windows.register(hud_id, WindowKind::ScrollHud);
                            tasks.push(hud_open.map(Message::ScrollHudWindowReady));
                            Task::batch(tasks)
                        }
                    };
                    Task::batch([history_task, intent_task])
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::capture", "region capture failed: {e}");
                    state.last_capture_status = Some(history_ops::friendly_capture_error(&e));
                    if state.cli_interactive_output.is_some() {
                        Task::done(Message::CliInteractiveWritten(Err(e)))
                    } else {
                        Task::none()
                    }
                }
            }
        }

        msg @ (Message::HistoryRecordPersisted(..)
        | Message::OpenHistoryRequested
        | Message::HistoryWindowReady(..)
        | Message::HistoryListLoaded(..)
        | Message::HistoryClosed
        | Message::HistorySearchChanged(..)
        | Message::HistoryStatusDismissed
        | Message::HistorySelect(..)
        | Message::HistorySelectPrevious
        | Message::HistorySelectNext
        | Message::HistoryOpenSelected
        | Message::HistoryDeleteSelected
        | Message::HistoryKeyboardShortcut(..)
        | Message::HistoryCapturedShortcut(..)
        | Message::HistoryClearAllRequested
        | Message::HistoryClearAllCancelled
        | Message::HistoryClearAllConfirmed
        | Message::HistoryOpenInEditor(..)
        | Message::HistoryOpenInEditorReady(..)
        | Message::HistoryReveal(..)
        | Message::HistoryCopyImage(..)
        | Message::HistoryCopyImageDone(..)
        | Message::HistoryCopyText(..)
        | Message::HistoryCopyTextDone(..)
        | Message::HistoryCopyVisibleTextRequested
        | Message::HistoryCopyVisibleTextDone(..)
        | Message::HistoryPin(..)
        | Message::HistoryPinReady(..)
        | Message::HistoryDelete(..)
        | Message::HistoryDeleteConfirmed(..)
        | Message::HistoryDeleteCancelled
        | Message::HistoryRecordSaved
        | Message::HistoryShowMore) => {
            let task = history_ops::handle_history_message(state, msg);
            Task::batch([task, editor_ops::history_write_followups(state)])
        }
        Message::WindowCloseRequested(id) => {
            // Only the editor window opts out of auto-close. If the
            // close request is for it, route through the same two-stage
            // unsaved-work confirmation as the in-app Discard button:
            // clean editors (and the confirmed second request) close
            // immediately; a dirty editor arms the confirmation and
            // stays open. Every other window uses iced's default
            // auto-close, so there is nothing to do here.
            let is_editor = state
                .editor
                .as_ref()
                .and_then(|ed| ed.window_id)
                .is_some_and(|editor_id| editor_id == id);
            if is_editor {
                return update(state, Message::EditorDiscardRequested);
            }
            Task::none()
        }
        Message::WindowClosed(id) => {
            // A press that began in the closed window never gets its
            // release.
            if let Some(ed) = state.editor.as_mut() {
                ed.pointer_window_gone(id);
            }
            // Always drop the id from `Windows` so helpers that
            // probe the live-window map (e.g. `show_or_focus_welcome`)
            // don't try to focus a dead window.
            state.windows.forget(id);
            state.overlay_displays.remove(&id);
            // The last overlay went away without a selection or a cancel
            // (closed by the OS): an Add Capture waiting on it gives its
            // hidden editor back. A confirmed selection marked the combine
            // (it ends when its capture completes), so that capture is left
            // to finish.
            let restore = if state.combine_capture.is_some()
                && !state.overlay_opening
                && !state.capture_in_flight
                && !combine_selection_made(state)
                && !state.windows.kinds().any(|k| k == WindowKind::Overlay)
            {
                state.overlay_selections.clear();
                abort_combine_capture(state, None)
            } else {
                Task::none()
            };
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
            if state
                .editor
                .as_ref()
                .and_then(|ed| ed.window_id)
                .is_some_and(|editor_id| editor_id == id)
            {
                if let Some(ed) = state.editor.as_mut() {
                    compose_jobs::settle_all(ed, &state.coordinator);
                    cancel_editor_previews(ed);
                    commit_pending_editor_text(ed, &state.coordinator);
                    editor_ops::sync_owed_history(ed, &state.coordinator);
                }
                state.editor = None;
                state.release_editor_modifiers();
            }
            // A scroll-capture HUD or region window closing out-of-band
            // (OS, crash, future close affordance) must tear the whole
            // session down. Otherwise `scroll_session` stays `Some` and
            // the frame-capture tick — gated on `scroll_session.is_some()`
            // — keeps firing forever against dead window ids.
            if state
                .scroll_session
                .as_ref()
                .is_some_and(|s| s.hud_window_id == Some(id) || s.region_window_id == Some(id))
            {
                if let Some(session) = state.scroll_session.take() {
                    let mut tasks: Vec<Task<Message>> = Vec::new();
                    for sibling in [session.hud_window_id, session.region_window_id]
                        .into_iter()
                        .flatten()
                        .filter(|&w| w != id)
                    {
                        state.windows.forget(sibling);
                        tasks.push(window::close(sibling));
                    }
                    state.last_capture_status = Some("Scroll capture cancelled.".into());
                    tasks.push(restore);
                    return Task::batch(tasks);
                }
            }
            restore
        }
        Message::WindowRescaled(id, scale) => {
            if matches!(state.windows.kind(id), Some(WindowKind::Editor)) {
                if let Some(ed) = state.editor.as_mut() {
                    let was_actual = ed.zoom_is_actual_size();
                    ed.display_scale = scale.max(f32::EPSILON);
                    if was_actual {
                        ed.zoom = ed.actual_size_zoom();
                    }
                    // The stage relaid out without a scroll event.
                    return view::pin_stage_offset(ed);
                }
            }
            Task::none()
        }
        Message::EditorPointerButton(pressed, window) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.set_pointer_held(pressed.then_some(window));
            }
            Task::none()
        }
        Message::EditorPointerWindowUnfocused(id) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.pointer_window_gone(id);
            }
            Task::none()
        }
        Message::EditorShiftChanged(held) => {
            // Only react on an actual change so the per-keystroke modifier
            // stream doesn't force a canvas re-tessellation each key.
            if state.editor_shift_held != held {
                state.editor_shift_held = held;
                if let Some(ed) = state.editor.as_ref() {
                    // Re-tessellate so a live drag preview reflects the new
                    // constraint on the next redraw.
                    ed.canvas_cache.clear();
                }
            }
            Task::none()
        }
        // High-frequency editor events: handled here, not in the editor
        // arm, so they never clear the canvas geometry cache.
        Message::EditorCommandChanged(held) => {
            // Only the Text panel's ⌘-click reads it; nothing to redraw.
            if state.editor_command_held != held {
                state.editor_command_held = held;
            }
            Task::none()
        }
        // Text-panel hover fires per pointer move; its outline is drawn in
        // the canvas's uncached marks layer, so the annotation geometry
        // cache stays valid.
        Message::EditorTextLineHovered(index) => {
            if let Some(ed) = state.editor.as_mut() {
                ed.text_panel.hovered = Some(index);
            }
            Task::none()
        }
        Message::EditorTextLineUnhovered(index) => {
            if let Some(ed) = state.editor.as_mut() {
                if ed.text_panel.hovered == Some(index) {
                    ed.text_panel.hovered = None;
                }
            }
            Task::none()
        }
        Message::EditorWindowResized(window, width) => {
            let mut docked_overlay = false;
            if let Some(ed) = state.editor.as_mut() {
                if ed.window_id == Some(window) {
                    ed.window_width = width;
                    // An overlay widened into the dock is now the docked
                    // panel: fold it into the preference (as ⌥⌘P does)
                    // so no stale overlay flag is left to eat an Esc.
                    if width >= PANEL_DOCK_MIN_WINDOW && ed.panel_overlay_open {
                        ed.panel_overlay_open = false;
                        docked_overlay = true;
                    }
                }
            }
            if docked_overlay {
                state.set_editor_panel_open(true);
            }
            // The stage relaid out without a scroll event.
            match state.editor.as_ref() {
                Some(ed) if ed.window_id == Some(window) => view::pin_stage_offset(ed),
                _ => Task::none(),
            }
        }
        // Only redraws the recognizing skeleton.
        Message::EditorPanelTick => Task::none(),
        Message::WindowDragRequested(id) => window::drag(id),
        msg @ (Message::EditorKeyPressed { .. }
        | Message::EditorStatusTick
        | Message::EditorWindowReady(..)
        | Message::EditorSaveRequested
        | Message::EditorSaved(..)
        | Message::EditorCopyImageRequested
        | Message::EditorCopyImageDone(..)
        | Message::EditorBeautifyPreset(..)
        | Message::EditorBeautifySavedPresetApplied(..)
        | Message::EditorBeautifyEdit(..)
        | Message::EditorBeautifySliderPreview(..)
        | Message::EditorSnapshotSliderCommit
        | Message::EditorBeautifySavePreset
        | Message::EditorBeautifyDeletePreset(..)
        | Message::EditorBeautifyHexChanged(..)
        | Message::EditorBeautifyHexSubmitted
        | Message::EditorBeautifyPickBackground
        | Message::EditorCopyTextRequested
        | Message::EditorCopyTextDone(..)
        | Message::EditorDiscardRequested
        | Message::EditorDiscardDisarmed
        | Message::EditorStatusDismissed
        | Message::EditorPopoverToggled(..)
        | Message::EditorPopoverClosed
        | Message::EditorToolbar(..)
        | Message::EditorLineWidthPreview(..)
        | Message::EditorLineWidthCommit
        | Message::EditorStyle(..)
        | Message::EditorStylePreview(..)
        | Message::EditorStyleCommit
        | Message::EditorHexChanged(..)
        | Message::EditorHexSubmitted
        | Message::EditorEyedropperToggled
        | Message::EditorCanvas(..)
        | Message::EditorEditSelectedText
        | Message::EditorDeleteSelected
        | Message::EditorCropApply
        | Message::EditorCropCancel
        | Message::EditorNudgeSelected(..)
        | Message::EditorTextChanged(..)
        | Message::EditorTextAction(..)
        | Message::EditorTextCommit
        | Message::EditorTextCancel
        | Message::EditorWidthBump(..)
        | Message::EditorRotate { .. }
        | Message::EditorFlip(..)
        | Message::EditorResizeOpen
        | Message::EditorResizeWidthChanged(..)
        | Message::EditorResizeHeightChanged(..)
        | Message::EditorResizeUnitChanged(..)
        | Message::EditorResizeLockToggled(..)
        | Message::EditorResizeApply
        | Message::EditorCombineCaptureRequested(..)
        | Message::EditorAddImageFromFile
        | Message::EditorPartLoaded { .. }
        | Message::EditorLayoutGapPreview(..)
        | Message::EditorLayoutGapFlush { .. }
        | Message::EditorLayoutAlign(..)
        | Message::EditorComposed { .. }
        | Message::EditorColorCycle(..)
        | Message::EditorZoomIn
        | Message::EditorZoomOut
        | Message::EditorZoomActual
        | Message::EditorZoomFit
        | Message::EditorPinRequested
        | Message::PinWindowReady(..)
        | Message::PinClosePressed(..)
        | Message::PinDragRequested(..)
        | Message::PinOpacityChanged(..)
        | Message::PinLockToggled(..)
        | Message::PinHovered(..)
        | Message::PinKeyPressed { .. }
        | Message::EditorTextRecognize
        | Message::EditorTextRecognized { .. }
        | Message::EditorPanelToggled
        | Message::EditorPanelTabSelected(..)
        | Message::EditorPanelOverlayDismissed
        | Message::EditorOpenTextSettings
        | Message::EditorTextLineClicked(..)
        | Message::EditorCopyLines
        | Message::EditorCopyLinesDone(..)
        | Message::EditorRemoveLineBreaksToggled(..)
        | Message::EditorRedactStyleChanged(..)
        | Message::EditorRedactAll
        | Message::EditorRedactLine(..)
        | Message::EditorHistoryWritten { .. }) => {
            // Images the message lets go of are freed off the UI thread.
            let held = state.editor.as_ref().map(compose_jobs::hold);
            // The frame the canvas computed this message's points in: the
            // one the last view showed (a step settled now, or a change
            // handled earlier in the same batch, may have moved it).
            let shown = state.editor.as_ref().map(|ed| ed.shown_frame());
            compose_jobs::settle_before(state, &msg);
            // A canvas point computed against another frame than the
            // present one is mapped into it, or the message is dropped.
            let msg = match (state.editor.as_mut(), shown) {
                (Some(ed), Some(shown))
                    if compose_jobs::points_into_the_shown_image(&msg)
                        && !crate::editor::same_frame(&shown, ed.model.geometry()) =>
                {
                    compose_jobs::remap_stale_press(ed, &shown, msg)
                }
                (Some(ed), _) => compose_jobs::remap_same_batch_drag(ed, msg),
                (None, _) => Some(msg),
            };
            let task = match msg {
                Some(msg) => editor_ops::handle_editor_message(state, msg),
                None => Task::none(),
            };
            editor_ops::sync_owed_editor_history(state);
            if let Some(held) = held {
                held.release(state.editor.as_ref());
            }
            Task::batch([task, editor_ops::history_write_followups(state)])
        }
        // Bookkeeping only (no history, no settle): what the stage shows.
        Message::EditorStageScrolled(x, y) => {
            if let Some(ed) = state.editor.as_ref() {
                ed.stage_offset.set((x, y));
            }
            Task::none()
        }
        Message::OverlayCopyDone(result) => {
            state.last_capture_status = Some(match result {
                Ok(()) => "Copied image to clipboard.".into(),
                Err(e) => {
                    tracing::warn!(target: "readshot::overlay", "copy image failed: {e}");
                    history_ops::friendly_string_error_with(
                        "copy to the clipboard",
                        &e,
                        Some(history_ops::RETRY_HINT),
                    )
                }
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
                Ok(Some(p)) => format!("Saved to {}.", p.display()),
                Ok(None) => "Save cancelled.".into(),
                Err(e) => {
                    tracing::warn!(target: "readshot::overlay", "save failed: {e}");
                    history_ops::friendly_string_error_with(
                        "save the image",
                        &e,
                        Some(history_ops::SAVE_HINT),
                    )
                }
            });
            Task::none()
        }
        Message::OverlayCopyTextDone(result) => {
            state.last_capture_status = Some(match result {
                Ok(text) if text.is_empty() => "No text recognized.".into(),
                Ok(text) => {
                    let n = text.chars().count();
                    format!(
                        "Copied {n} character{} of text.",
                        if n == 1 { "" } else { "s" }
                    )
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::overlay", "copy text failed: {e}");
                    history_ops::friendly_string_error_with(
                        "copy the text",
                        &e,
                        Some(history_ops::RETRY_HINT),
                    )
                }
            });
            Task::none()
        }
        Message::CliInteractiveWritten(result) => {
            if let Err(e) = result {
                tracing::warn!(target: "readshot::cli", "interactive capture failed: {e}");
            }
            state.cli_interactive_output = None;
            state.overlay_opening = false;
            clear_pending_capture_state(state);
            iced::exit()
        }

        Message::NoOp => Task::none(),

        msg @ (Message::ScrollHudWindowReady(..)
        | Message::ScrollRegionWindowReady(..)
        | Message::ScrollHudDragRequested
        | Message::ScrollCaptureTick
        | Message::ScrollCaptureFrame { .. }
        | Message::ScrollFrameJudged { .. }
        | Message::ScrollCaptureCancelRequested
        | Message::ScrollCaptureKeepGoing
        | Message::ScrollCaptureStopRequested
        | Message::ScrollCaptureStitched(..)) => scroll::handle_scroll_message(state, msg),

        Message::WelcomeStatusDismissed => {
            state.last_capture_status = None;
            Task::none()
        }

        Message::CaptureSaved(result) => {
            state.capture_in_flight = false;
            state.last_capture_status = Some(match &result {
                Ok(path) => format!("Saved to {}.", path.display()),
                Err(e) => {
                    tracing::warn!(target: "readshot::capture", "capture failed: {e}");
                    history_ops::friendly_capture_error(e)
                }
            });
            Task::none()
        }

        Message::ThemePollTick => {
            let os = os_theme_mode();
            if os != state.theme_mode {
                state.theme_mode = os;
            }
            Task::none()
        }
        Message::SystemThemeChanged(mode) => {
            state.theme_mode = match mode {
                iced::theme::Mode::Light => ThemeMode::Light,
                _ => ThemeMode::Dark,
            };
            Task::none()
        }
        Message::UrlActionReceived(action) => match action {
            UrlAction::NewCapture => {
                tracing::info!(target: "readshot::url", "readshot:// → opening overlay");
                if state.welcome.should_show() {
                    state.pending_url_after_permission = Some(UrlAction::NewCapture);
                    return show_or_focus_welcome(state);
                }
                update(state, Message::OpenOverlayRequested)
            }
            UrlAction::Unknown(path) => {
                tracing::warn!(target: "readshot::url", "ignoring unknown readshot:// path: {path}");
                Task::none()
            }
        },

        Message::QuitRequested => quit_readshot(&state.coordinator),

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
                disarm_relaunch_on_system_quit();
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

        // Clear the stale row, re-register this signature with macOS (the
        // SCK probe inside `list_displays` does that), and open the pane
        // so the user can switch Readshot on for this build.
        Message::ResetPermissionRequested => {
            state.update_sync(Message::ResetPermissionRequested);
            let coord = state.coordinator.clone();
            Task::perform(
                async move {
                    let _ = coord.list_displays().await;
                },
                |()| Message::OpenPermissionSettingsRequested,
            )
        }

        // Synchronous transitions — reuse the existing handler.
        msg @ (Message::PermissionPoll(_) | Message::OpenPermissionSettingsRequested) => {
            state.update_sync(msg);
            Task::none()
        }

        msg @ (Message::Settings(..)
        | Message::OpenSettingsRequested
        | Message::SettingsChooseSaveFolderRequested
        | Message::SettingsSaveFolderPicked(..)
        | Message::SettingsOpenSaveFolderRequested
        | Message::SettingsOpenSaveFolderDone(..)
        | Message::SettingsStartHotkeyRecording
        | Message::SettingsHotkeyRecorded(..)
        | Message::SettingsHotkeyRecordingInvalid
        | Message::SettingsHotkeyRecordingCancelled
        | Message::SettingsResetAllRequested
        | Message::SettingsResetAllCancelled
        | Message::SettingsResetAllConfirmed
        | Message::SettingsUninstallRequested
        | Message::SettingsUninstallCancelled
        | Message::SettingsUninstallPurgeToggled(..)
        | Message::SettingsUninstallConfirmed
        | Message::SettingsUninstallFinished
        | Message::SettingsWindowReady(..)
        | Message::SettingsPaneSelected(..)
        | Message::SettingsKeyPressed { .. }
        | Message::OpenCliToolsRequested
        | Message::CliToolsWindowReady(..)
        | Message::CliToolsCopyRequested(..)
        | Message::CliToolsCopyDone(..)) => settings_ops::handle_settings_message(state, msg),
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
        Some(WindowKind::ScrollRegion) => scroll_region_view(state),
    }
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
    register_window(state, id, WindowKind::Welcome);
    open_task.map(Message::WelcomeWindowReady)
}

/// Async helper: capture the primary display's full bounds and write
/// the PNG to `~/Desktop/Readshot-<timestamp>.png`. Returns the saved
/// path on success.
async fn capture_primary_to_desktop(coord: CaptureCoordinator) -> Result<PathBuf, CaptureRunError> {
    let displays = coord.list_displays().await?;
    let primary = pick_primary(&displays).ok_or(CaptureRunError::NoDisplays)?;
    let req = CaptureRequest {
        display_id: primary.id.clone(),
        rect: display_local_bounds(primary),
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
    capture_region_to_image_with_scale(coord, display.id.clone(), rect, display.scale, hide_cursor)
        .await
}

async fn capture_region_to_image_with_scale(
    coord: CaptureCoordinator,
    display_id: readshot_capture::DisplayId,
    rect: readshot_core::geom::Rect,
    scale: f32,
    hide_cursor: bool,
) -> Result<image::RgbaImage, CaptureRunError> {
    let req = CaptureRequest {
        display_id,
        rect,
        scale,
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
    // On failure clean up only the temp file. The destination is
    // either untouched (write/rename never happened) or was replaced
    // atomically by the rename — never delete a pre-existing file the
    // capture failed to overwrite.
    if write_result.is_err() {
        let _ = std::fs::remove_file(&tmp);
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

/// Spawn a fire-and-forget history-persistence task: encode the PNG and
/// save the record (empty OCR) while OCR runs **concurrently**, then
/// update the sidecar with the recognised text. Each stage logs its own
/// failures and the user-visible flow never blocks on this work. See
/// [`persist_history`] for the messages it emits.
fn persist_history_task(
    coord: CaptureCoordinator,
    img: image::RgbaImage,
    record: Option<readshot_core::CaptureRecord>,
    policy: readshot_core::HistoryRetention,
    preferences: Preferences,
    editor_session: Option<u64>,
) -> Task<Message> {
    Task::stream(iced::stream::channel(
        4,
        move |output: iced::futures::channel::mpsc::Sender<Message>| {
            persist_history(
                coord,
                img,
                record,
                policy,
                preferences,
                editor_session,
                output,
            )
        },
    ))
}

/// Body of [`persist_history_task`], separate so tests can drive it.
///
/// Messages:
/// * [`Message::EditorTextRecognized`] when `editor_session` is set, the
///   moment OCR resolves (not after the slower PNG encode / save): the
///   same OCR feeds that editor's Text panel (one OCR per capture),
///   errors included so the editor never stays Recognizing. Engines map
///   a panicked OCR worker to an error, so this is always sent;
/// * [`Message::HistoryRecordSaved`] the moment the record hits disk —
///   the new row shows up without waiting on OCR;
/// * [`Message::HistoryRecordPersisted`] once both resolve — `Ok(true)`
///   when text landed, `Ok(false)` when OCR failed or the update did.
///   Not sent when the PNG encode failed (no record was saved).
async fn persist_history(
    coord: CaptureCoordinator,
    img: image::RgbaImage,
    record: Option<readshot_core::CaptureRecord>,
    policy: readshot_core::HistoryRetention,
    preferences: Preferences,
    editor_session: Option<u64>,
    mut output: iced::futures::channel::mpsc::Sender<Message>,
) {
    use iced::futures::SinkExt;
    if matches!(policy, readshot_core::HistoryRetention::Off) {
        return;
    }
    let Some(record) = record else {
        return;
    };
    let now = record.captured_at;
    let ocr = {
        let coord = coord.clone();
        let req = ocr_request_for_image(img.clone(), &preferences);
        let mut output = output.clone();
        async move {
            let doc = coord.recognise_document(req).await;
            if let Some(session) = editor_session {
                let result = match &doc {
                    Ok(d) => Ok(d.clone()),
                    Err(e) => Err(e.to_string()),
                };
                let _ = output
                    .send(Message::EditorTextRecognized {
                        session,
                        part: readshot_core::document::ORIGINAL_PART,
                        result,
                    })
                    .await;
            }
            doc
        }
    };
    let save = {
        let coord = coord.clone();
        let record = record.clone();
        let mut output = output.clone();
        async move {
            // Best-compression encode is CPU-heavy on large captures —
            // keep it off the async workers.
            let encoded =
                tokio::task::spawn_blocking(move || readshot_core::encode_png(&img)).await;
            let buf = match encoded {
                Ok(Ok(buf)) => buf,
                Ok(Err(e)) => {
                    tracing::warn!(target: "readshot::history", "png encode: {e}");
                    return false;
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::history", "png encode worker: {e}");
                    return false;
                }
            };
            coord.record_history(record, buf, policy, now).await;
            let _ = output.send(Message::HistoryRecordSaved).await;
            true
        }
    };
    let (doc, saved) = iced::futures::future::join(ocr, save).await;
    if !saved {
        return;
    }
    let persisted = match doc {
        Ok(doc) => {
            // The same string `recognise` returned before, stored next to
            // whatever annotations the editor synced meanwhile. If the
            // editor scrubbed the record, the text is masked with its
            // redactions instead (never stored raw).
            let write_coord = coord.clone();
            let written = tokio::task::spawn_blocking(move || {
                write_coord.record_recognized_text(&record, &doc, true)
            })
            .await;
            match written {
                Ok(Ok(())) => true,
                Ok(Err(e)) => {
                    tracing::warn!(target: "readshot::history", "ocr update failed: {e}");
                    false
                }
                Err(e) => {
                    tracing::warn!(target: "readshot::history", "ocr update worker: {e}");
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: "readshot::history", "ocr failed: {e}");
            false
        }
    };
    let _ = output
        .send(Message::HistoryRecordPersisted(Ok(persisted)))
        .await;
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

/// Move an owned image's buffer into an iced handle without cloning
/// the (multi-megabyte) pixel buffer.
fn rgba_handle(img: image::RgbaImage) -> iced::widget::image::Handle {
    let (w, h) = (img.width(), img.height());
    iced::widget::image::Handle::from_rgba(w, h, img.into_raw())
}

/// Load the history list on a blocking thread. `FsHistoryStore::list`
/// is synchronous fs work that can decode full PNGs to backfill
/// missing thumbnails; running it inline on the async executor would
/// stall a tokio worker for seconds on a large archive.
fn history_list_task(coord: CaptureCoordinator) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || coord.history_list())
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        },
        Message::HistoryListLoaded,
    )
}

fn editor_output_image(ed: &mut EditorSession) -> image::RgbaImage {
    // The model's flattened image of the present (rendered here if it
    // has none), never a display.
    let img = ed.model.flatten();
    let beautify = ed.model.beautify();
    if beautify.clamped().is_identity() {
        return img;
    }
    readshot_core::beautify::render(&img, &beautify)
}

fn can_preview_drag_annotation(annotations: &[readshot_core::Annotation], index: usize) -> bool {
    annotations
        .get(index)
        .is_some_and(|annotation| !matches!(annotation, readshot_core::Annotation::Crop { .. }))
}

fn refresh_image_during_select_drag(
    annotations: &[readshot_core::Annotation],
    index: usize,
) -> bool {
    annotations
        .get(index)
        .is_some_and(|annotation| matches!(annotation, readshot_core::Annotation::Crop { .. }))
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
    remove_breaks: bool,
) -> Result<String, OcrCopyError> {
    let result = coord
        .recognise(ocr_request_for_image(img, &preferences))
        .await?;
    let text = if remove_breaks {
        readshot_core::text_lines::remove_line_breaks(&result)
    } else {
        result
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::SettingsPane;
    use crate::permissions::fake::FakePermissions;
    use crate::permissions::PermissionStatus;
    use async_trait::async_trait;
    use global_hotkey::HotKeyState;
    use iced::widget::button;
    use readshot_capture::fake::FakeCapturer;
    use readshot_capture::{Capturer, DisplayInfo};
    use readshot_core::beautify::BuiltinPreset;
    use readshot_core::error::CaptureError;
    use readshot_core::{Annotation, FsHistoryStore, HistoryStore, PointLike, RectLike, Rgba};
    use readshot_ocr::fake::FakeOcrEngine;
    use std::collections::HashMap;

    #[test]
    fn overlay_like_windows_are_always_dark() {
        for kind in [
            WindowKind::Overlay,
            WindowKind::ScrollRegion,
            WindowKind::ScrollHud,
            WindowKind::Pin,
        ] {
            assert_eq!(
                theme_mode_for(Some(kind), ThemeMode::Light),
                ThemeMode::Dark
            );
        }
        assert_eq!(
            theme_mode_for(Some(WindowKind::Editor), ThemeMode::Light),
            ThemeMode::Light
        );
        assert_eq!(theme_mode_for(None, ThemeMode::Dark), ThemeMode::Dark);
    }

    #[test]
    fn inline_text_editor_styles_follow_the_palette() {
        for (mode, p) in [(ThemeMode::Light, &LIGHT), (ThemeMode::Dark, &DARK)] {
            let theme = readshot_theme_for(mode);
            let chrome = editor_chrome_style(&theme);
            assert_eq!(chrome.background, Some(p.float.into()), "{mode:?}");
            assert_eq!(chrome.border.color, p.border, "{mode:?}");
            let boxed = inline_text_box_style(&theme);
            assert_eq!(boxed.background, Some(p.float.into()), "{mode:?}");
            assert_eq!(boxed.border.color, p.acc, "{mode:?}");
            let ed = inline_text_editor_style(&theme, iced::widget::text_editor::Status::Active);
            assert_eq!(ed.value, p.t1, "{mode:?}");
            assert_eq!(ed.placeholder, p.t3, "{mode:?}");
            assert_eq!(ed.selection, p.acc_tint, "{mode:?}");
            let disabled = disabled_text_input_style(&theme);
            assert_eq!(disabled.background, Some(p.hover.into()), "{mode:?}");
            assert_eq!(disabled.border.color, p.border, "{mode:?}");
            let track = line_width_track_style(&theme);
            assert_eq!(track.background, Some(p.hover.into()), "{mode:?}");
        }
    }

    #[test]
    fn palette_for_reads_the_theme_name() {
        assert_eq!(
            palette_for(&readshot_theme_for(ThemeMode::Light)).bg,
            LIGHT.bg
        );
        assert_eq!(
            palette_for(&readshot_theme_for(ThemeMode::Dark)).bg,
            DARK.bg
        );
        assert_eq!(palette_for(&Theme::Dark).bg, DARK.bg);
    }

    #[test]
    fn system_theme_message_switches_mode() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let _ = update(
            &mut app,
            Message::SystemThemeChanged(iced::theme::Mode::Light),
        );
        assert_eq!(app.theme_mode, ThemeMode::Light);
        let _ = update(
            &mut app,
            Message::SystemThemeChanged(iced::theme::Mode::None),
        );
        assert_eq!(app.theme_mode, ThemeMode::Dark);
    }

    #[test]
    fn theme_poll_tick_reads_os_mode_on_update_thread() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.theme_mode = ThemeMode::Light;
        let _ = update(&mut app, Message::ThemePollTick);
        // Off the main thread (tests) `os_theme_mode` reports Dark.
        assert_eq!(app.theme_mode, os_theme_mode());
    }

    #[test]
    fn theme_poll_runs_only_while_a_system_themed_window_is_open() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        assert!(!theme_poll_needed(&app));
        app.windows.register(window::Id::unique(), WindowKind::Pin);
        app.windows
            .register(window::Id::unique(), WindowKind::ScrollHud);
        assert!(!theme_poll_needed(&app));
        for kind in [
            WindowKind::Welcome,
            WindowKind::Editor,
            WindowKind::History,
            WindowKind::Settings,
            WindowKind::CliTools,
        ] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            app.windows.register(window::Id::unique(), kind);
            assert!(theme_poll_needed(&app), "{kind:?}");
        }
    }

    #[test]
    fn opening_a_system_themed_window_reads_the_os_mode() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.theme_mode = ThemeMode::Light;
        let _ = update(&mut app, Message::OpenSettingsRequested);
        // Off the main thread (tests) `os_theme_mode` reports Dark.
        assert_eq!(app.theme_mode, os_theme_mode());
        assert_eq!(app.theme_mode, ThemeMode::Dark);
    }

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

    #[cfg(debug_assertions)]
    #[test]
    fn debug_editor_qa_session_is_clean_and_pin_ready() {
        let ed = debug_editor_qa_session();

        assert_eq!(ed.image_size(), (1040, 680));
        assert_eq!(ed.next_pin_number, 2);
        assert!(ed.model.undo_depth() > 0);
        assert!(!editor_has_unsaved_work(&ed));
        assert_eq!(ed.status.as_deref(), Some("Editor QA fixture loaded."));
    }

    #[test]
    fn popover_toggles_and_esc_closes_it_before_cancelling_text() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(debug_editor_qa_session());
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().popover,
            Some(crate::editor::EditorPopover::Color)
        );
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        assert_eq!(app.editor.as_ref().unwrap().popover, None);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Width),
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        assert_eq!(app.editor.as_ref().unwrap().popover, None);
    }

    #[test]
    fn plain_esc_closes_popover_instead_of_arming_discard() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = debug_editor_qa_session();
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: true,
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert!(ed.discard_pending_at.is_none());
    }

    #[test]
    fn opening_the_color_popover_seeds_the_hex_field() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.hex_draft, ed.model.effective_color().to_hex());
    }

    #[test]
    fn hex_submit_applies_the_color_and_remembers_it() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let _ = update(&mut app, Message::EditorHexChanged("#4f46e5".into()));
        let _ = update(&mut app, Message::EditorHexSubmitted);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.current_color().to_hex(), "#4F46E5");
        assert_eq!(ed.popover, None);
        assert_eq!(
            app.preferences.editor_recent_colors,
            vec!["#4F46E5".to_string()]
        );
    }

    #[test]
    fn invalid_hex_keeps_the_popover_open_with_an_error() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let before = app.editor.as_ref().unwrap().model.current_color();
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let _ = update(&mut app, Message::EditorHexChanged("#12".into()));
        let _ = update(&mut app, Message::EditorHexSubmitted);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, Some(crate::editor::EditorPopover::Color));
        assert_eq!(ed.model.current_color(), before);
        assert!(ed.status_is_error());
    }

    #[test]
    fn preset_colors_are_not_added_to_recent() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let _ = update(
            &mut app,
            Message::EditorHexChanged(readshot_ui::editor::toolbar::PALETTE[4].to_hex()),
        );
        let _ = update(&mut app, Message::EditorHexSubmitted);
        assert!(app.preferences.editor_recent_colors.is_empty());
    }

    #[test]
    fn clicking_a_recent_swatch_moves_it_to_the_front() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        app.remember_custom_color(readshot_core::Rgba::from_hex("#111111").unwrap());
        app.remember_custom_color(readshot_core::Rgba::from_hex("#222222").unwrap());
        assert_eq!(
            app.preferences.editor_recent_colors,
            vec!["#222222".to_string(), "#111111".to_string()]
        );
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectColor(
                readshot_core::Rgba::from_hex("#111111").unwrap(),
            )),
        );
        assert_eq!(
            app.preferences.editor_recent_colors,
            vec!["#111111".to_string(), "#222222".to_string()]
        );
    }

    #[test]
    fn hex_field_filters_to_hex_characters() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let _ = update(&mut app, Message::EditorHexChanged("#4z f46e5xx99".into()));
        assert_eq!(app.editor.as_ref().unwrap().hex_draft, "#4f46e5");
    }

    #[test]
    fn esc_while_typing_in_the_hex_field_closes_the_popover() {
        // The focused text input captures Esc, so it arrives with
        // status_ignored == false and maps to EditorTextCancel.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(32, 32));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: false,
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert!(ed.discard_pending_at.is_none());
    }

    #[test]
    fn tool_letters_and_brackets_are_ignored_while_typing_in_the_hex_field() {
        // The focused text input captures printable keys, so they arrive
        // with status_ignored == false and must not switch tools or widths.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(32, 32));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let tool = app.editor.as_ref().unwrap().model.active_tool();
        let width = app.editor.as_ref().unwrap().model.current_line_width();
        for ch in ["i", "s", "e", "t", "1", "[", "]"] {
            let _ = update(
                &mut app,
                Message::EditorKeyPressed {
                    window: id,
                    key: iced::keyboard::Key::Character(ch.into()),
                    modifiers: iced::keyboard::Modifiers::empty(),
                    status_ignored: false,
                },
            );
        }
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.active_tool(), tool);
        assert_eq!(ed.model.current_line_width(), width);
        assert_eq!(ed.popover, Some(crate::editor::EditorPopover::Color));
    }

    #[test]
    fn cmd_z_is_ignored_while_the_color_popover_is_open() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(32, 32));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.model
            .commit_annotation(readshot_core::Annotation::Rectangle {
                rect: readshot_core::RectLike::new(2.0, 2.0, 10.0, 10.0),
                color: readshot_core::Rgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: Default::default(),
            });
        app.editor = Some(ed);
        let cmd = iced::keyboard::Modifiers::COMMAND;
        let press = |app: &mut App, mods| {
            let _ = update(
                app,
                Message::EditorKeyPressed {
                    window: id,
                    key: iced::keyboard::Key::Character("z".into()),
                    modifiers: mods,
                    status_ignored: false,
                },
            );
        };
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        press(&mut app, cmd);
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
        press(&mut app, cmd | iced::keyboard::Modifiers::SHIFT);
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
        let _ = update(&mut app, Message::EditorPopoverClosed);
        press(&mut app, cmd);
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 0);
    }

    #[test]
    fn hex_field_allows_hash_only_first() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let _ = update(&mut app, Message::EditorHexChanged("#ab#cd#".into()));
        assert_eq!(app.editor.as_ref().unwrap().hex_draft, "#abcd");
        let _ = update(&mut app, Message::EditorHexChanged("ab#cd".into()));
        assert_eq!(app.editor.as_ref().unwrap().hex_draft, "abcd");
    }

    #[test]
    fn picking_a_color_closes_the_color_popover() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(debug_editor_qa_session());
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectColor(
                readshot_ui::editor::toolbar::PALETTE[3],
            )),
        );
        assert_eq!(app.editor.as_ref().unwrap().popover, None);
    }

    #[test]
    fn the_beautify_hex_field_follows_the_solid_background_unless_mid_edit() {
        use readshot_core::beautify::{Background, BeautifyEdit, BuiltinPreset};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let hex = |app: &App| app.editor.as_ref().unwrap().beautify_hex_shown();
        // Opening the tab: seeded from the current solid background.
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));
        let _ = update(
            &mut app,
            Message::EditorPanelTabSelected(crate::editor::PanelTab::Beautify),
        );
        assert_eq!(hex(&app), "#E5EAF0");
        // Changed elsewhere (a swatch, a preset): the field follows.
        let solid_bg = |rgba| BeautifyEdit::Background(Background::Solid { rgba });
        let _ = update(
            &mut app,
            Message::EditorBeautifyEdit(solid_bg([0x4f, 0x46, 0xe5, 255])),
        );
        assert_eq!(hex(&app), "#4F46E5");
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::None));
        assert_eq!(hex(&app), "", "no solid background");
        // Mid-edit: a background change doesn't clobber what is typed.
        let _ = update(&mut app, Message::EditorBeautifyHexChanged("#12".into()));
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));
        assert_eq!(hex(&app), "#12");
        // Applied: follows the background again.
        let _ = update(
            &mut app,
            Message::EditorBeautifyHexChanged("#123456".into()),
        );
        let _ = update(&mut app, Message::EditorBeautifyHexSubmitted);
        assert_eq!(hex(&app), "#123456");
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));
        assert_eq!(hex(&app), "#E5EAF0");
        // Reopening the tab drops an abandoned draft.
        let _ = update(&mut app, Message::EditorBeautifyHexChanged("#9".into()));
        let _ = update(
            &mut app,
            Message::EditorPanelTabSelected(crate::editor::PanelTab::Beautify),
        );
        assert_eq!(hex(&app), "#E5EAF0");
    }

    #[test]
    fn beautify_slider_drag_is_one_undo_step_and_esc_cancels_it() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
        );
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        for p in [30, 60, 90] {
            let _ = update(
                &mut app,
                Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(p)),
            );
        }
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.beautify().padding, 90);
        assert_eq!(ed.model.undo_depth(), depth + 1);

        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.model.beautify().padding,
            90,
            "esc restores the value before the drag"
        );
        assert!(ed.snapshot_drag_baseline.is_none());
    }

    #[test]
    fn a_preset_click_mid_drag_keeps_the_pre_drag_state_in_undo() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
        );
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
        );
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Dark));
        let ed = app.editor.as_mut().unwrap();
        assert!(ed.snapshot_drag_baseline.is_none());
        assert_eq!(ed.model.beautify(), BuiltinPreset::Dark.settings());
        assert!(ed.model.undo());
        assert_eq!(
            ed.model.beautify(),
            BuiltinPreset::Minimal.settings(),
            "no drag value leaked into history"
        );
    }

    #[test]
    fn undo_ends_a_slider_drag() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
        );
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
        );
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.snapshot_drag_baseline.is_none());
        assert_ne!(ed.model.beautify().padding, 150);
    }

    #[test]
    fn undo_during_a_held_drag_cancels_only_the_drag_and_loses_nothing() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Dark));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
        );
        // The mouse is down: this is a held slider drag.
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        // The slider is still held: more ticks arrive, then release.
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(160)),
        );
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.beautify(), BuiltinPreset::Dark.settings());
        assert_eq!(ed.model.undo_depth(), depth, "one undo, no history step");
        assert!(!ed.snapshot_drag_cancelled);
        // The next drag works again.
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(30)),
        );
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        assert_eq!(app.editor.as_ref().unwrap().model.beautify().padding, 30);
    }

    #[test]
    fn esc_during_a_held_drag_ignores_the_remaining_ticks() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
        );
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
        );
        // The mouse is down: this is a held slider drag.
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let _ = update(&mut app, Message::EditorTextCancel);
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(151)),
        );
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.beautify(), BuiltinPreset::Minimal.settings());
        assert_eq!(ed.model.undo_depth(), depth);
    }

    #[test]
    fn output_commits_a_key_driven_slider_preview() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
        );
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        // Arrow keys preview but never send a release.
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(25)),
        );
        let _ = update(&mut app, Message::EditorCopyImageRequested);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.model.beautify().padding,
            25,
            "the copied output keeps the previewed value"
        );
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(ed.snapshot_drag_baseline.is_none());
    }

    /// A mouse drag on a Beautify slider: the press tick (iced sends it
    /// before the button event reaches the subscription), the button,
    /// more ticks, then release.
    fn drag_padding(app: &mut App, values: &[u32]) {
        use readshot_core::beautify::BeautifyEdit;
        for (i, v) in values.iter().enumerate() {
            let _ = update(
                app,
                Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(*v)),
            );
            if i == 0 {
                assert_eq!(
                    app.editor.as_ref().unwrap().model.beautify().padding,
                    *v,
                    "the press tick previews"
                );
                let _ = update(app, Message::EditorPointerButton(true, None));
            }
        }
        let _ = update(app, Message::EditorSnapshotSliderCommit);
        let _ = update(app, Message::EditorPointerButton(false, None));
    }

    #[test]
    fn a_cancelled_key_or_wheel_slider_preview_never_swallows_the_next_drag() {
        use readshot_core::beautify::{BeautifyEdit, BuiltinPreset};
        // Each way a preview with no held slider can be cancelled.
        type Cancel = fn(&mut App);
        let cancels: [(&str, Cancel); 3] = [
            ("esc", |app| {
                let _ = update(app, Message::EditorTextCancel);
            }),
            ("undo", |app| {
                let _ = update(
                    app,
                    Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
                );
            }),
            ("preset", |app| {
                let _ = update(app, Message::EditorBeautifyPreset(BuiltinPreset::Minimal));
            }),
        ];
        for (name, cancel) in cancels {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
            let _ = update(
                &mut app,
                Message::EditorBeautifyPreset(BuiltinPreset::Minimal),
            );
            // Arrow keys (one tick) or a Ctrl+wheel burst (several): no
            // button press, no release.
            for v in [20, 21, 22] {
                let _ = update(
                    &mut app,
                    Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(v)),
                );
            }
            cancel(&mut app);
            let ed = app.editor.as_ref().unwrap();
            assert!(!ed.snapshot_drag_cancelled, "{name}: no held slider");
            let depth = ed.model.undo_depth();
            drag_padding(&mut app, &[40, 44]);
            let ed = app.editor.as_ref().unwrap();
            assert_eq!(ed.model.beautify().padding, 44, "{name}: the drag applies");
            assert_eq!(ed.model.undo_depth(), depth + 1, "{name}: one undo step");
        }
    }

    #[test]
    fn a_cancelled_key_style_preview_never_swallows_the_next_drag() {
        use readshot_ui::editor::StyleMessage;
        let mut app = two_rects_first_selected();
        // An arrow key over the corner slider, then Esc.
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(6.0)),
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.style_drag_cancelled);
        let depth = ed.model.undo_depth();
        let radius = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
            Annotation::Rectangle { style, .. } => style.corner_radius,
            other => panic!("expected rectangle, got {other:?}"),
        };
        assert_eq!(radius(&app), 0.0);
        // Now a mouse drag on the same slider.
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(9.0)),
        );
        assert_eq!(radius(&app), 9.0, "the press tick previews");
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(11.0)),
        );
        let _ = update(&mut app, Message::EditorStyleCommit);
        let _ = update(&mut app, Message::EditorPointerButton(false, None));
        assert_eq!(radius(&app), 11.0);
        assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
    }

    #[test]
    fn a_press_whose_window_loses_focus_or_closes_is_no_longer_held() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        let (a, b) = (iced::window::Id::unique(), iced::window::Id::unique());
        let held = |app: &App| app.editor.as_ref().unwrap().pointer_held;
        // Focus loss of another window leaves the press held.
        let _ = update(&mut app, Message::EditorPointerButton(true, Some(a)));
        let _ = update(&mut app, Message::EditorPointerWindowUnfocused(b));
        let _ = update(&mut app, Message::WindowClosed(b));
        assert!(held(&app));
        // The press's own window losing focus: its release may never come.
        app.editor.as_mut().unwrap().snapshot_drag_cancelled = true;
        let _ = update(&mut app, Message::EditorPointerWindowUnfocused(a));
        assert!(!held(&app));
        assert!(!app.editor.as_ref().unwrap().snapshot_drag_cancelled);
        // …or closing before the release reaches the app.
        let _ = update(&mut app, Message::EditorPointerButton(true, Some(b)));
        assert!(held(&app));
        let _ = update(&mut app, Message::WindowClosed(b));
        assert!(!held(&app));
        // A new editor session starts with nothing held.
        let _ = update(&mut app, Message::EditorPointerButton(true, Some(a)));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        assert!(!held(&app));
    }

    #[test]
    fn a_new_press_clears_a_stale_cancelled_drag_flag() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(16, 16));
        ed.snapshot_drag_cancelled = true;
        ed.style_drag_cancelled = true;
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pointer_held);
        assert!(!ed.snapshot_drag_cancelled && !ed.style_drag_cancelled);
    }

    #[test]
    fn picking_a_background_closes_the_narrow_panel_overlay() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_width = 800.0;
        ed.panel_overlay_open = true;
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorBeautifyPickBackground);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.panel_overlay_open);
        assert!(ed.eyedropper.is_some());
    }

    #[test]
    fn saved_presets_persist_apply_and_cap_at_twelve() {
        use readshot_core::beautify::{BuiltinPreset, MAX_SAVED_PRESETS};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Dark));
        let _ = update(&mut app, Message::EditorBeautifySavePreset);
        assert_eq!(app.preferences.editor_beautify_presets.len(), 1);
        assert_eq!(app.preferences.editor_beautify_presets[0].name, "Preset 1");
        assert_eq!(
            app.preferences.editor_beautify_presets[0].settings,
            BuiltinPreset::Dark.settings()
        );
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::None));
        let _ = update(&mut app, Message::EditorBeautifySavedPresetApplied(0));
        assert_eq!(
            app.editor.as_ref().unwrap().model.beautify(),
            BuiltinPreset::Dark.settings()
        );
        for _ in 0..20 {
            let _ = update(&mut app, Message::EditorBeautifySavePreset);
        }
        assert_eq!(
            app.preferences.editor_beautify_presets.len(),
            MAX_SAVED_PRESETS
        );
        let _ = update(&mut app, Message::EditorBeautifyDeletePreset(0));
        assert_eq!(
            app.preferences.editor_beautify_presets.len(),
            MAX_SAVED_PRESETS - 1
        );
    }

    #[test]
    fn an_out_of_range_saved_preset_applies_clamped_and_never_panics() {
        use readshot_core::beautify::{SavedBeautifyPreset, MAX_PADDING};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(32, 32)));
        let mut bad = SavedBeautifyPreset::default();
        bad.settings.padding = u32::MAX;
        app.preferences.editor_beautify_presets.push(bad);
        let _ = update(&mut app, Message::EditorBeautifySavedPresetApplied(0));
        assert_eq!(
            app.editor.as_ref().unwrap().model.beautify().padding,
            MAX_PADDING
        );
        let _ = update(&mut app, Message::EditorBeautifySavedPresetApplied(99));
        let _ = update(&mut app, Message::EditorBeautifyDeletePreset(99));
    }

    #[test]
    fn background_hex_and_eyedropper_set_a_solid_background() {
        use readshot_core::beautify::Background;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        let _ = update(
            &mut app,
            Message::EditorBeautifyHexChanged("#102030".into()),
        );
        let _ = update(&mut app, Message::EditorBeautifyHexSubmitted);
        assert_eq!(
            app.editor.as_ref().unwrap().model.beautify().background,
            Background::Solid {
                rgba: [0x10, 0x20, 0x30, 255]
            }
        );
        let _ = update(&mut app, Message::EditorBeautifyPickBackground);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.eyedropper.as_ref().unwrap().target,
            crate::editor::EyedropperTarget::BeautifyBackground
        );
        let tool_color = ed.model.current_color();
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(
                readshot_core::PointLike::new(2.0, 2.0),
            )),
        );
        let ed = app.editor.as_ref().unwrap();
        let px = solid(16, 16).get_pixel(2, 2).0;
        assert_eq!(
            ed.model.beautify().background,
            Background::Solid { rgba: px }
        );
        assert_eq!(
            ed.model.current_color(),
            tool_color,
            "the tool color is untouched"
        );
    }

    #[test]
    fn title_shows_the_output_size_when_beautified() {
        use readshot_core::beautify::BuiltinPreset;
        let mut ed = crate::editor::EditorSession::new(solid(100, 50));
        assert_eq!(editor_size_label(&ed), "100 × 50 px");
        ed.model.set_beautify(BuiltinPreset::Minimal.settings());
        assert_eq!(editor_size_label(&ed), "100 × 50 px → 148 × 98 px");
    }

    #[test]
    fn preview_metrics_fall_back_to_the_bare_image_when_output_overflows() {
        use readshot_core::beautify::BuiltinPreset;
        let w = u32::MAX - 1;
        let m = preview_metrics(
            w,
            20,
            &BuiltinPreset::Minimal.settings(),
            iced::Size::new(800.0, 600.0),
            None,
        );
        assert_eq!(m.pad, iced::Padding::ZERO);
        assert_eq!(m.output_w, w as f32 * m.scale);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn editor_window_draws_under_a_transparent_title_bar() {
        let ps = editor_window_settings(None).platform_specific;
        assert!(ps.title_hidden && ps.titlebar_transparent && ps.fullsize_content_view);
        assert_eq!(TITLEBAR_HEIGHT, 44.0);
        assert_eq!(TITLEBAR_INSET_LEFT, 78.0);
    }

    #[test]
    fn line_width_label_drops_needless_decimals() {
        assert_eq!(line_width_label(3.0), "3");
        assert_eq!(line_width_label(2.5), "2.5");
        assert_eq!(line_width_label(0.5), "0.5");
    }

    fn editor_with_draft_and_color_popover(app: &mut App) -> iced::window::Id {
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.begin_text_edit(crate::editor::PendingText {
            origin: PointLike::new(10.0, 10.0),
            content: "hello".into(),
            edit_index: None,
        });
        app.editor = Some(ed);
        let _ = update(
            app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        id
    }

    /// One Esc press as the running editor delivers it. The inline text
    /// editor lets Esc through (`editor_text_key_binding` → None) and the
    /// annotation canvas captures every Esc (publishing `Cancelled`), so
    /// the keyboard subscription always sees `status_ignored: false`.
    fn press_escape(app: &mut App, id: iced::window::Id) {
        assert!(editor_text_key_binding(escape_key_press()).is_none());
        let _ = update(
            app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::Cancelled),
        );
        let _ = update(
            app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: false,
            },
        );
    }

    fn escape_key_press() -> iced::widget::text_editor::KeyPress {
        iced::widget::text_editor::KeyPress {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Escape),
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            status: iced::widget::text_editor::Status::Focused { is_hovered: false },
        }
    }

    #[test]
    fn text_editor_lets_escape_through_to_the_single_cancel_path() {
        // If the text editor also emitted EditorTextCancel, one press would
        // cancel twice (close the popover *and* drop the draft).
        assert!(editor_text_key_binding(escape_key_press()).is_none());
    }

    #[test]
    fn one_esc_closes_the_popover_but_keeps_the_draft() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = editor_with_draft_and_color_popover(&mut app);
        press_escape(&mut app, id);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert_eq!(
            ed.pending_text.as_ref().map(|t| t.content.as_str()),
            Some("hello")
        );

        // With no popover open, the next Esc cancels the draft.
        press_escape(&mut app, id);
        let ed = app.editor.as_ref().expect("editor stays open");
        assert!(ed.pending_text.is_none());
        assert!(ed.discard_pending_at.is_none());
    }

    #[test]
    fn one_esc_closes_the_popover_but_keeps_the_selection() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );

        press_escape(&mut app, id);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert_eq!(ed.model.selected_annotation(), Some(0));

        // Next Esc clears the selection; the editor stays open.
        press_escape(&mut app, id);
        let ed = app.editor.as_ref().expect("editor stays open");
        assert_eq!(ed.model.selected_annotation(), None);
        assert!(ed.discard_pending_at.is_none());
    }

    #[test]
    fn esc_closing_the_width_popover_cancels_a_live_width_preview() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Width),
        );
        let _ = update(&mut app, Message::EditorLineWidthPreview(10.0));
        assert!(app.editor.as_ref().unwrap().width_drag_baseline.is_some());
        press_escape(&mut app, id);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert!(ed.width_drag_baseline.is_none());
        assert_eq!(ed.model.undo_depth(), undo_depth);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn zoom_and_relayout_pin_the_stage_to_the_recorded_offset() {
        // No scroll event follows a zoom or a window resize: each pins the
        // stage scrollable to the offset the editor records, so where a
        // new magnifier bubble goes matches what is on screen.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let window = iced::window::Id::unique();
        ed.window_id = Some(window);
        app.editor = Some(ed);
        for msg in [
            Message::EditorZoomIn,
            Message::EditorZoomOut,
            Message::EditorZoomActual,
            Message::EditorZoomFit,
            Message::EditorWindowResized(window, 900.0),
        ] {
            let task = update(&mut app, msg.clone());
            assert!(task.units() > 0, "{msg:?} pins the stage offset");
        }
        // Another window's resize does not touch this stage.
        let task = update(
            &mut app,
            Message::EditorWindowResized(iced::window::Id::unique(), 900.0),
        );
        assert_eq!(task.units(), 0);
    }

    #[test]
    fn zoom_steps_from_fit_use_the_scale_rendered_at_update_time() {
        // The view can be stale after a resize (iced relayouts without a
        // rebuild), so the step resolves the Fit scale when handled.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let ed = crate::editor::EditorSession::new_with_display_scale(solid(64, 64), 2.0);
        ed.render_scale.set(0.4);
        app.editor = Some(ed);
        // Window grew after the last view build: fit is now 0.8.
        app.editor.as_ref().unwrap().render_scale.set(0.8);
        let _ = update(&mut app, Message::EditorZoomIn);
        assert_eq!(
            app.editor.as_ref().unwrap().zoom,
            crate::editor::EditorZoom::Percent(0.8 * crate::editor::EditorZoom::STEP)
        );
        let _ = update(&mut app, Message::EditorZoomFit);
        let _ = update(&mut app, Message::EditorZoomOut);
        assert_eq!(
            app.editor.as_ref().unwrap().zoom,
            crate::editor::EditorZoom::Percent(0.8 / crate::editor::EditorZoom::STEP)
        );
    }

    #[test]
    fn editor_chrome_layers_clear_the_stage() {
        // Hint (one caption line) ends above the image; the toast (one
        // body line + padding) ends below the image's bottom inset.
        const HINT_LINE: f32 = 18.0;
        const TOAST_HEIGHT: f32 = 34.0;
        const { assert!(EDITOR_HINT_TOP + HINT_LINE <= EDITOR_STAGE_PAD_TOP) };
        const { assert!(EDITOR_TOAST_BOTTOM + TOAST_HEIGHT <= EDITOR_STAGE_PAD_BOTTOM) };
        const { assert!(EDITOR_OPTIONS_TOP + EDITOR_OPTIONS_HEIGHT <= EDITOR_STAGE_PAD_TOP) };
        const { assert!(EDITOR_OPTIONS_TOP + EDITOR_OPTIONS_HEIGHT <= EDITOR_POPOVER_TOP) };
        assert_eq!(EDITOR_STAGE_PAD_TOP, 138.0);
    }

    #[test]
    fn editor_toast_kind_classifies_status() {
        assert_eq!(
            editor_toast_kind("Couldn't save — the folder is read-only.", false, false),
            ToastKind::Error
        );
        assert_eq!(
            editor_toast_kind("Copied image", false, false),
            ToastKind::Ok
        );
        assert_eq!(editor_toast_kind("Saving…", true, false), ToastKind::Info);
        assert_eq!(editor_toast_kind("anything", false, true), ToastKind::Warn);
    }

    #[test]
    fn status_toast_kind_separates_notices_from_successes() {
        for s in [
            "Capture not found.",
            "No history capture selected.",
            "No OCR text on this capture yet.",
            "No OCR text in the visible captures.",
            "Scroll capture cancelled.",
        ] {
            assert_eq!(status_toast_kind(s), ToastKind::Info, "{s:?}");
        }
        for s in ["Couldn't read history: denied", "Scroll capture failed: x"] {
            assert_eq!(status_toast_kind(s), ToastKind::Error, "{s:?}");
        }
        for s in ["Saved to /tmp/x.png.", "Copied text."] {
            assert_eq!(status_toast_kind(s), ToastKind::Ok, "{s:?}");
        }
        // The editor shares the notice rule for its idle statuses.
        assert_eq!(
            editor_toast_kind("No text recognized.", false, false),
            ToastKind::Info
        );
    }

    #[test]
    fn history_status_dismissed_clears_history_status() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.history_status = Some("Couldn't read history: denied".into());
        let _ = update(&mut app, Message::HistoryStatusDismissed);
        assert!(app.history_status.is_none());
    }

    #[test]
    fn other_status_after_arming_disarms_discard() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = debug_editor_qa_session();
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorDiscardRequested);
        let ed = app.editor.as_mut().unwrap();
        let armed = ed.discard_pending_at.is_some();
        assert!(armed, "dirty editor should arm");
        ed.set_status("Copying…");
        assert!(ed.discard_pending_at.is_none());
        let s = ed.status.clone().unwrap();
        assert_ne!(
            editor_toast_kind(
                &s,
                crate::editor::status_str_is_in_progress(&s),
                ed.discard_pending_at.is_some()
            ),
            ToastKind::Warn
        );
        let _ = update(&mut app, Message::EditorDiscardRequested);
        let ed = app.editor.as_mut().unwrap();
        ed.set_status("Couldn't save — disk full.");
        let s = ed.status.clone().unwrap();
        assert_eq!(
            editor_toast_kind(&s, false, ed.discard_pending_at.is_some()),
            ToastKind::Error
        );
    }

    #[test]
    fn keep_editing_disarms_discard() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = debug_editor_qa_session();
        ed.discard_pending_at = Some(std::time::Instant::now());
        ed.status = Some("Unsaved edits".into());
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorDiscardDisarmed);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.discard_pending_at.is_none());
        assert!(ed.status.is_none());
    }

    #[test]
    fn dismissing_status_clears_it_and_tick_keeps_errors() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = debug_editor_qa_session();
        ed.status = Some("Couldn't save — the folder is read-only.".into());
        ed.status_set_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(60));
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorStatusTick);
        assert!(app.editor.as_ref().unwrap().status.is_some());
        let _ = update(&mut app, Message::EditorStatusDismissed);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.status.is_none());
        assert!(ed.status_set_at.is_none());
    }

    #[test]
    fn popover_closed_message_clears_any_open_popover() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(debug_editor_qa_session());
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Width),
        );
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().popover,
            Some(crate::editor::EditorPopover::Color)
        );
        let _ = update(&mut app, Message::EditorPopoverClosed);
        assert_eq!(app.editor.as_ref().unwrap().popover, None);
    }

    fn overlay_display(id: &str) -> crate::app::OverlayDisplay {
        crate::app::OverlayDisplay {
            display_id: id.to_string(),
            scale: 1.0,
            origin_x: 0.0,
            origin_y: 0.0,
            width: 800.0,
            height: 600.0,
        }
    }

    #[test]
    fn no_animation_tick_runs_for_static_overlays() {
        let src = include_str!("runtime.rs");
        assert!(!src.contains(concat!("Message::", "OverlayTick")));
    }

    fn solid(w: u32, h: u32) -> image::RgbaImage {
        let mut img = image::RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = image::Rgba([255, 255, 255, 255]);
        }
        img
    }

    fn command_modifiers() -> iced::keyboard::Modifiers {
        let mut modifiers = iced::keyboard::Modifiers::default();
        #[cfg(target_os = "macos")]
        modifiers.insert(iced::keyboard::Modifiers::LOGO);
        #[cfg(not(target_os = "macos"))]
        modifiers.insert(iced::keyboard::Modifiers::CTRL);
        modifiers
    }

    fn scrolling_texture(w: u32, h: u32, offset: u32) -> image::RgbaImage {
        let mut img = image::RgbaImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let content_y = y + offset;
                let r = ((content_y * 3 + x * 5) % 251) as u8;
                let g = ((content_y * 7 + x * 11) % 253) as u8;
                let b = ((content_y * 13 + x * 17) % 247) as u8;
                img.put_pixel(x, y, image::Rgba([r, g, b, 255]));
            }
        }
        img
    }

    #[test]
    fn scroll_motion_accepts_real_vertical_motion() {
        let a = scrolling_texture(80, 120, 0);
        let b = scrolling_texture(80, 120, 14);

        assert!(frame_has_scroll_motion(&a, &b));
        let dy = scroll_motion_offset(&a, &b).unwrap();
        assert!((12..=16).contains(&dy), "expected dy near 14, got {dy}");
    }

    #[test]
    fn scroll_motion_rejects_non_scroll_pixel_change() {
        let a = scrolling_texture(80, 120, 0);
        let mut b = a.clone();
        for y in 45..55 {
            for x in 20..30 {
                b.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }

        assert!(!frame_has_scroll_motion(&a, &b));
        assert_eq!(scroll_motion_offset(&a, &b), Some(0));
    }

    #[test]
    fn scroll_capture_drains_out_of_order_frames_in_sequence() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap();
        let mut session = crate::app::ScrollSession::new("display-a".into(), rect, 1.0);
        session.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
        app.scroll_session = Some(session);

        // Out-of-order arrival: seq 2 buffers until seq 1 lands.
        let _ = update(
            &mut app,
            Message::ScrollCaptureFrame {
                seq: 2,
                result: Ok(scrolling_texture(80, 120, 28)),
            },
        );
        let session = app.scroll_session.as_ref().unwrap();
        assert_eq!(session.frames.len(), 1);
        assert_eq!(session.pending_frames.len(), 1);
        assert_eq!(session.judge_in_flight, None);

        // Seq 1 arrives: draining dispatches a motion judge for it and
        // pauses (the verdict task isn't executed in tests).
        let _ = update(
            &mut app,
            Message::ScrollCaptureFrame {
                seq: 1,
                result: Ok(scrolling_texture(80, 120, 14)),
            },
        );
        let session = app.scroll_session.as_ref().unwrap();
        assert_eq!(session.frames.len(), 1);
        assert_eq!(session.judge_in_flight, Some(1));
        assert_eq!(session.pending_frames.len(), 1);

        // Verdict for seq 1 accepts the frame and dispatches the judge
        // for the buffered seq 2.
        let _ = update(
            &mut app,
            Message::ScrollFrameJudged {
                seq: 1,
                image: Some(scrolling_texture(80, 120, 14)),
                moved: true,
                offset: None,
                rows: None,
            },
        );
        let session = app.scroll_session.as_ref().unwrap();
        assert_eq!(session.frames.len(), 2);
        assert_eq!(session.judge_in_flight, Some(2));
        assert!(session.pending_frames.is_empty());

        let _ = update(
            &mut app,
            Message::ScrollFrameJudged {
                seq: 2,
                image: Some(scrolling_texture(80, 120, 28)),
                moved: true,
                offset: None,
                rows: None,
            },
        );
        let session = app.scroll_session.as_ref().unwrap();
        assert_eq!(session.frames.len(), 3);
        assert_eq!(session.judge_in_flight, None);
        assert_eq!(session.next_frame_seq_to_process, 3);
    }

    #[test]
    fn scroll_judge_rejection_counts_no_motion_and_stale_verdicts_drop() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap();
        let mut session = crate::app::ScrollSession::new("display-a".into(), rect, 1.0);
        session.frames.push(Arc::new(scrolling_texture(80, 120, 0)));
        session.judge_in_flight = Some(7);
        app.scroll_session = Some(session);

        // Stale verdict (wrong seq) is dropped entirely.
        let _ = update(
            &mut app,
            Message::ScrollFrameJudged {
                seq: 3,
                image: Some(scrolling_texture(80, 120, 14)),
                moved: true,
                offset: None,
                rows: None,
            },
        );
        let session = app.scroll_session.as_ref().unwrap();
        assert_eq!(session.frames.len(), 1);
        assert_eq!(session.judge_in_flight, Some(7));

        // A "no motion" verdict bumps the counter without appending.
        let _ = update(
            &mut app,
            Message::ScrollFrameJudged {
                seq: 7,
                image: Some(scrolling_texture(80, 120, 0)),
                moved: false,
                offset: None,
                rows: None,
            },
        );
        let session = app.scroll_session.as_ref().unwrap();
        assert_eq!(session.frames.len(), 1);
        assert_eq!(session.no_motion_count, 1);
        assert_eq!(session.judge_in_flight, None);
    }

    fn test_session() -> crate::app::ScrollSession {
        crate::app::ScrollSession::new(
            "display-a".into(),
            readshot_core::geom::Rect::from_xywh(0.0, 0.0, 80.0, 120.0).unwrap(),
            1.0,
        )
    }

    #[test]
    fn scroll_hud_state_covers_every_phase() {
        let mut s = test_session();
        assert_eq!(scroll_hud_state(&s), ScrollHudState::Waiting);
        s.preview_handle = Some(iced::widget::image::Handle::from_rgba(1, 1, vec![0; 4]));
        assert_eq!(scroll_hud_state(&s), ScrollHudState::Recording);
        s.cancel_armed_at = Some(std::time::Instant::now());
        assert_eq!(scroll_hud_state(&s), ScrollHudState::Armed);
        s.stopping = true;
        s.stitch_frame_count = SCROLL_MAX_FRAMES;
        assert_eq!(scroll_hud_state(&s), ScrollHudState::Limit);
        s.stitch_frame_count = 5;
        assert_eq!(scroll_hud_state(&s), ScrollHudState::Stitching);
    }

    #[test]
    fn accepted_frames_record_join_heights_capped_at_64() {
        let mut s = test_session();
        let first = scrolling_texture(80, 120, 0);
        let rows = preview_rows(&first, None);
        accept_scroll_frame(&mut s, first, Some(rows), None);
        // The first frame has nothing above it to join to.
        assert!(s.recent_join_heights.is_empty());
        accept_scroll_frame(
            &mut s,
            scrolling_texture(80, 120, 14),
            Some(image::RgbaImage::new(PREVIEW_STRIP_W, 60)),
            Some(14),
        );
        assert_eq!(
            s.recent_join_heights.iter().copied().collect::<Vec<_>>(),
            [60]
        );
        for _ in 0..80 {
            accept_scroll_frame(
                &mut s,
                scrolling_texture(80, 120, 14),
                Some(image::RgbaImage::new(PREVIEW_STRIP_W, 7)),
                Some(14),
            );
        }
        assert_eq!(s.recent_join_heights.len(), 64);
        assert_eq!(s.recent_join_heights.back(), Some(&7));
    }

    #[test]
    fn join_ticks_sit_above_each_newer_slice_scaled_to_the_stage() {
        // Slices appended oldest→newest: 60 then 40 strip px. From the
        // bottom, the newest slice (40) ends at the first join, the
        // older one (60) at the second. Stage twice the strip width.
        let joins: std::collections::VecDeque<u32> = [60, 40].into_iter().collect();
        let ticks = join_tick_offsets(&joins, PREVIEW_STRIP_W as f32 * 2.0);
        assert_eq!(ticks, vec![80.0, 200.0]);
    }

    #[test]
    fn scroll_window_footer_goes_compact_below_300px() {
        assert!(scroll_hud_compact(240.0));
        assert!(scroll_hud_compact(299.0));
        assert!(!scroll_hud_compact(SCROLL_WINDOW_W));
    }

    #[test]
    fn discard_label_drops_the_count_below_340px() {
        assert_eq!(scroll_discard_label(299.0, 120), "Discard");
        assert_eq!(scroll_discard_label(330.0, 120), "Discard");
        assert_eq!(scroll_discard_label(340.0, 120), "Discard 120 Frames");
        assert_eq!(scroll_discard_label(SCROLL_WINDOW_W, 1), "Discard 1 Frame");
    }

    #[test]
    fn stop_button_shows_esc_chip_only_while_recording_and_roomy() {
        assert_eq!(scroll_stop_key_chip(ScrollHudState::Waiting, false), None);
        assert_eq!(scroll_stop_key_chip(ScrollHudState::Recording, true), None);
        assert_eq!(
            scroll_stop_key_chip(ScrollHudState::Recording, false),
            Some("esc")
        );
    }

    #[test]
    fn keep_going_disarms_cancel() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut s = test_session();
        s.cancel_armed_at = Some(std::time::Instant::now());
        app.scroll_session = Some(s);
        let _ = update(&mut app, Message::ScrollCaptureKeepGoing);
        assert!(app
            .scroll_session
            .as_ref()
            .unwrap()
            .cancel_armed_at
            .is_none());
    }

    #[test]
    fn cancel_arm_expires_after_the_window() {
        let mut s = test_session();
        s.cancel_armed_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(3));
        assert!(!cancel_armed(&s));
        s.cancel_armed_at = Some(std::time::Instant::now());
        assert!(cancel_armed(&s));
    }

    #[test]
    fn frames_arriving_after_stop_do_not_touch_the_session() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut s = test_session();
        let first = scrolling_texture(80, 120, 0);
        let rows = preview_rows(&first, None);
        accept_scroll_frame(&mut s, first, Some(rows), None);
        // A capture and a judge verdict are outstanding at Stop.
        s.judge_in_flight = Some(1);
        s.pending_frames
            .insert(2, Ok(scrolling_texture(80, 120, 28)));
        s.capture_in_flight = 1;
        app.scroll_session = Some(s);
        let _ = update(&mut app, Message::ScrollCaptureStopRequested);
        let before = app.scroll_session.as_ref().unwrap();
        let strip_h = before.preview_strip.as_ref().unwrap().height();
        let stitched = before.stitched_rows;
        assert!(before.pending_frames.is_empty());
        assert_eq!(before.judge_in_flight, None);

        let _ = update(
            &mut app,
            Message::ScrollFrameJudged {
                seq: 1,
                image: Some(scrolling_texture(80, 120, 14)),
                moved: true,
                offset: Some(14),
                rows: Some(image::RgbaImage::new(PREVIEW_STRIP_W, 60)),
            },
        );
        let _ = update(
            &mut app,
            Message::ScrollCaptureFrame {
                seq: 3,
                result: Ok(scrolling_texture(80, 120, 42)),
            },
        );
        let after = app.scroll_session.as_ref().unwrap();
        assert!(after.frames.is_empty());
        assert!(after.pending_frames.is_empty());
        assert_eq!(after.judge_in_flight, None);
        assert_eq!(after.preview_strip.as_ref().unwrap().height(), strip_h);
        assert_eq!(after.stitched_rows, stitched);
    }

    #[test]
    fn stitch_result_without_a_session_is_dropped() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        assert!(app.scroll_session.is_none());
        let _ = update(
            &mut app,
            Message::ScrollCaptureStitched(Ok(image::RgbaImage::new(10, 10))),
        );
        assert!(app.editor.is_none());
    }

    #[test]
    fn cancel_is_ignored_while_stitching() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut s = test_session();
        s.stopping = true;
        app.scroll_session = Some(s);
        let _ = update(&mut app, Message::ScrollCaptureCancelRequested);
        assert!(app.scroll_session.is_some());
    }

    /// A scroll session with live HUD + region windows registered.
    fn app_with_scroll_windows(stopping: bool) -> (App, window::Id, window::Id) {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let hud = window::Id::unique();
        let region = window::Id::unique();
        app.windows.register(hud, WindowKind::ScrollHud);
        app.windows.register(region, WindowKind::ScrollRegion);
        let mut s = test_session();
        s.hud_window_id = Some(hud);
        s.region_window_id = Some(region);
        s.stopping = stopping;
        app.scroll_session = Some(s);
        (app, hud, region)
    }

    #[test]
    fn open_overlay_leaves_a_running_scroll_session_alone() {
        let (mut app, hud, _) = app_with_scroll_windows(true);
        let _ = update(&mut app, Message::OpenOverlayRequested);
        assert!(!app.overlay_opening);
        let s = app.scroll_session.as_ref().unwrap();
        assert!(s.stopping);
        assert_eq!(s.hud_window_id, Some(hud));
    }

    #[test]
    fn retake_leaves_a_running_scroll_session_alone() {
        let (mut app, hud, _) = app_with_scroll_windows(true);
        let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();
        app.last_regions.insert(
            "display-a".into(),
            crate::app::LastRegion {
                rect,
                display_scale: 2.0,
                display_bounds: None,
            },
        );
        app.last_region_display_id = Some("display-a".into());
        let _ = update(&mut app, Message::RetakeLastRegionRequested);
        assert_eq!(app.pending_display_id, None);
        let s = app.scroll_session.as_ref().unwrap();
        assert!(s.stopping);
        assert_eq!(s.hud_window_id, Some(hud));
    }

    #[test]
    fn stitch_result_closes_both_scroll_windows() {
        let (mut app, hud, region) = app_with_scroll_windows(true);
        let _ = update(&mut app, Message::ScrollCaptureStitched(Err("boom".into())));
        assert!(app.scroll_session.is_none());
        assert_eq!(app.windows.kind(hud), None);
        assert_eq!(app.windows.kind(region), None);
    }

    #[test]
    fn stop_with_no_frames_cancels_instead_of_failing() {
        let (mut app, hud, region) = app_with_scroll_windows(false);
        let _ = update(&mut app, Message::ScrollCaptureStopRequested);
        assert!(app.scroll_session.is_none());
        assert_eq!(app.windows.kind(hud), None);
        assert_eq!(app.windows.kind(region), None);
        let status = app.last_capture_status.as_deref().unwrap_or("");
        assert!(!status.contains("failed"), "status: {status}");
    }

    #[test]
    fn editor_window_centers_on_capture_display() {
        let settings = editor_window_settings(Some((1440.0, 0.0, 1920.0, 1080.0)));

        match settings.position {
            window::Position::Specific(point) => {
                assert_eq!(point.x, 1850.0);
                assert_eq!(point.y, 160.0);
            }
            other => panic!("expected specific editor position, got {other:?}"),
        }
    }

    #[test]
    fn pin_window_centers_on_capture_display() {
        let settings = pin_window_settings((400, 300), Some((-1280.0, 120.0, 1280.0, 720.0)));

        match settings.position {
            window::Position::Specific(point) => {
                assert_eq!(point.x, -840.0);
                assert_eq!(point.y, 330.0);
            }
            other => panic!("expected specific pin position, got {other:?}"),
        }
    }

    #[test]
    fn editor_session_remembers_capture_display_bounds() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let bounds = Some((1440.0, 0.0, 1920.0, 1080.0));
        let editor = crate::editor::EditorSession::new(solid(32, 32));

        let _ = open_editor_window_replacing(&mut app, editor, bounds);

        assert_eq!(
            app.editor.as_ref().and_then(|ed| ed.source_display_bounds),
            bounds
        );
    }

    #[test]
    fn editor_pin_window_centers_on_capture_display() {
        let mut editor = crate::editor::EditorSession::new(solid(32, 32));
        editor.source_display_bounds = Some((-1280.0, 120.0, 1280.0, 720.0));

        let settings = editor_pin_window_settings(&editor, (400, 300));

        match settings.position {
            window::Position::Specific(point) => {
                assert_eq!(point.x, -840.0);
                assert_eq!(point.y, 330.0);
            }
            other => panic!("expected specific editor pin position, got {other:?}"),
        }
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
    fn overlay_selection_preserves_display_context_for_fast_capture() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let window = iced::window::Id::unique();
        app.overlay_displays.insert(
            window,
            crate::app::OverlayDisplay {
                display_id: "display-a".to_string(),
                scale: 2.0,
                origin_x: 1440.0,
                origin_y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
        );
        let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();

        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: "display-a".to_string(),
                rect,
                intent: crate::app::CaptureIntent::Editor,
            },
        );

        assert_eq!(app.pending_display_scale, Some(2.0));
        assert_eq!(
            app.pending_display_bounds,
            Some((1440.0, 0.0, 1920.0, 1080.0))
        );
        assert_eq!(
            app.last_regions
                .get("display-a")
                .and_then(|r| r.display_bounds),
            Some((1440.0, 0.0, 1920.0, 1080.0))
        );
    }

    #[test]
    fn retake_last_region_preserves_display_context() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let display_id = "display-a".to_string();
        let rect = readshot_core::geom::Rect::from_xywh(10.0, 20.0, 120.0, 80.0).unwrap();
        app.last_regions.insert(
            display_id.clone(),
            crate::app::LastRegion {
                rect,
                display_scale: 2.0,
                display_bounds: Some((1440.0, 0.0, 1920.0, 1080.0)),
            },
        );
        app.last_region_display_id = Some(display_id.clone());

        let _ = update(&mut app, Message::RetakeLastRegionRequested);

        assert_eq!(app.pending_display_id.as_deref(), Some(display_id.as_str()));
        assert_eq!(app.pending_display_scale, Some(2.0));
        assert_eq!(
            app.pending_display_bounds,
            Some((1440.0, 0.0, 1920.0, 1080.0))
        );
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
    fn open_overlay_sets_opening_guard_until_display_list_completes() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));

        let _ = update(&mut app, Message::OpenOverlayRequested);

        assert!(app.overlay_opening);

        let _ = update(&mut app, Message::OverlayDisplaysListed(Ok(Vec::new())));

        assert!(!app.overlay_opening);
        assert_eq!(
            app.last_capture_status.as_deref(),
            Some("Couldn't capture — no display was detected.")
        );
    }

    #[test]
    fn overlay_display_list_error_clears_pending_capture_state() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.overlay_opening = true;
        app.pending_intent = Some(crate::app::CaptureIntent::Editor);
        app.pending_display_id = Some("display-a".to_string());
        app.pending_display_scale = Some(2.0);
        app.pending_display_bounds = Some((10.0, 20.0, 800.0, 600.0));
        app.pending_hide_cursor = false;

        let _ = update(
            &mut app,
            Message::OverlayDisplaysListed(Err("backend unavailable".into())),
        );

        assert!(!app.overlay_opening);
        assert_eq!(app.pending_intent, None);
        assert_eq!(app.pending_display_id, None);
        assert_eq!(app.pending_display_scale, None);
        assert_eq!(app.pending_display_bounds, None);
        assert!(app.pending_hide_cursor);
        // The raw backend error is logged, not leaked into the status —
        // the user sees friendly recovery text instead.
        let status = app.last_capture_status.as_deref().unwrap();
        assert!(status.starts_with("Couldn't capture"), "{status}");
        assert!(!status.contains("backend unavailable"), "{status}");
    }

    #[test]
    fn periodic_poll_notices_grant_without_probing() {
        let perms = Arc::new(FakePermissions::denied());
        let mut app = build_app(perms.clone());
        assert!(app.welcome.should_show());
        perms.flip_to_granted();
        let _ = update(&mut app, Message::PermissionPollTick);
        assert_eq!(app.permission_status, PermissionStatus::Granted);
    }

    #[test]
    fn url_capture_waits_for_permission_then_replays() {
        let perms = Arc::new(FakePermissions::denied());
        let mut app = build_app(perms.clone());

        let _ = update(&mut app, Message::UrlActionReceived(UrlAction::NewCapture));

        assert_eq!(
            app.pending_url_after_permission,
            Some(UrlAction::NewCapture)
        );

        perms.flip_to_granted();
        let _ = update(&mut app, Message::PermissionTick);

        assert_eq!(app.pending_url_after_permission, None);
    }

    #[test]
    fn closing_overlay_window_removes_display_mapping() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.windows.register(id, WindowKind::Overlay);
        app.overlay_displays
            .insert(id, overlay_display("display-a"));

        let _ = update(&mut app, Message::WindowClosed(id));

        assert!(app.overlay_displays.is_empty());
        assert!(app.windows.kind(id).is_none());
    }

    #[test]
    fn opening_replacement_editor_forgets_previous_editor_window() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let old_id = iced::window::Id::unique();
        let mut old = crate::editor::EditorSession::new(solid(16, 16));
        old.window_id = Some(old_id);
        app.windows.register(old_id, WindowKind::Editor);
        app.editor = Some(old);

        let new = crate::editor::EditorSession::new(solid(32, 32));
        let _ = open_editor_window_replacing(&mut app, new, None);

        assert!(app.windows.kind(old_id).is_none());
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (32, 32));
    }

    #[test]
    fn opening_replacement_editor_commits_old_pending_history_text() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let record =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
        store.save(&record, b"png").unwrap();

        let history: Arc<dyn HistoryStore> = store.clone();
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
        let old_id = iced::window::Id::unique();
        let mut old = crate::editor::EditorSession::from_history(solid(100, 100), record);
        old.window_id = Some(old_id);
        old.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(12.0, 18.0),
            content: "replacement draft".into(),
            edit_index: None,
        });
        app.windows.register(old_id, WindowKind::Editor);
        app.editor = Some(old);

        let new = crate::editor::EditorSession::new(solid(32, 32));
        let _ = open_editor_window_replacing(&mut app, new, None);

        assert!(app.windows.kind(old_id).is_none());
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (32, 32));
        // The sidecar write runs on the coordinator's background
        // writer; wait for it before reading the store.
        app.coordinator.flush_history_updates();
        let from_disk = store.list().unwrap();
        assert_eq!(from_disk[0].annotation_model.len(), 1);
        match &from_disk[0].annotation_model[0] {
            Annotation::Text {
                content, origin, ..
            } => {
                assert_eq!(content, "replacement draft");
                assert_eq!(*origin, PointLike::new(12.0, 18.0));
            }
            other => panic!("expected text annotation, got {other:?}"),
        }
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
    fn pin_hover_tracks_the_window() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        let _ = update(&mut app, Message::PinHovered(id, true));
        assert_eq!(app.hovered_pin, Some(id));
        let _ = update(&mut app, Message::PinHovered(id, false));
        assert_eq!(app.hovered_pin, None);
    }

    #[test]
    fn pin_hover_exit_of_another_pin_keeps_current() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (a, b) = (iced::window::Id::unique(), iced::window::Id::unique());
        let _ = update(&mut app, Message::PinHovered(a, true));
        let _ = update(&mut app, Message::PinHovered(b, false));
        assert_eq!(app.hovered_pin, Some(a));
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

    #[cfg(target_os = "macos")]
    #[test]
    fn relaunch_on_exit_only_for_system_quit_while_awaiting_permission() {
        // macOS "Quit & Reopen" after a grant: relaunch ourselves.
        assert!(should_relaunch_on_exit(true, false));
        // Our own Quit / Restart: never.
        assert!(!should_relaunch_on_exit(true, true));
        // Started with permission already granted: never.
        assert!(!should_relaunch_on_exit(false, false));
        let reuse = relaunch_command_for_bundle(
            std::path::Path::new("/Applications/Readshot.app"),
            RelaunchMode::ReuseRunning,
        );
        // Plain `open` so a copy macOS already reopened is reused, not doubled.
        assert!(reuse.args[1].contains("exec /usr/bin/open \"$1\""));
        assert!(!reuse.args[1].contains("-n"));
    }

    #[test]
    fn history_timestamp_label_is_relative_to_today() {
        use chrono::TimeZone;
        let now = chrono::Local
            .with_ymd_and_hms(2026, 9, 27, 18, 0, 0)
            .unwrap();
        let at = |y, m, d, h, min| chrono::Local.with_ymd_and_hms(y, m, d, h, min, 0).unwrap();
        assert_eq!(
            view::history_timestamp_label(at(2026, 9, 27, 14, 3), now),
            "Today 14:03"
        );
        assert_eq!(
            view::history_timestamp_label(at(2026, 9, 26, 9, 10), now),
            "Yesterday 09:10"
        );
        assert_eq!(
            view::history_timestamp_label(at(2026, 3, 5, 7, 45), now),
            "Mar 5, 07:45"
        );
        assert_eq!(
            view::history_timestamp_label(at(2025, 12, 31, 23, 0), now),
            "Dec 31, 2025"
        );
    }

    #[test]
    fn line_width_fraction_clamps_to_toolbar_range() {
        assert_eq!(line_width_fraction(-10.0), 0.0);
        assert_eq!(line_width_fraction(99.0), 1.0);
        assert!(
            (line_width_fraction(readshot_ui::editor::toolbar::MIN_LINE_WIDTH) - 0.0).abs()
                < f32::EPSILON
        );
        assert!(
            (line_width_fraction(readshot_ui::editor::toolbar::MAX_LINE_WIDTH) - 1.0).abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn disabled_action_button_does_not_keep_active_border_strength() {
        let active =
            action_button_style(&Theme::Dark, button::Status::Active, ActionKind::Secondary);
        let disabled = action_button_style(
            &Theme::Dark,
            button::Status::Disabled,
            ActionKind::Secondary,
        );

        assert!(disabled.text_color.a < active.text_color.a);
        assert!(disabled.border.color.a < active.border.color.a);
    }

    #[test]
    fn select_drag_preview_refreshes_image_only_for_crop_geometry() {
        let annotations = vec![
            Annotation::Rectangle {
                rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
                color: Rgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: Default::default(),
            },
            Annotation::Crop {
                rect: RectLike::new(4.0, 4.0, 18.0, 18.0),
            },
        ];

        assert!(can_preview_drag_annotation(&annotations, 0));
        assert!(!refresh_image_during_select_drag(&annotations, 0));
        assert!(!can_preview_drag_annotation(&annotations, 1));
        assert!(refresh_image_during_select_drag(&annotations, 1));
        assert!(!refresh_image_during_select_drag(&annotations, 99));
    }

    #[test]
    fn editor_selection_chrome_follows_selection_presence() {
        // Chrome shows iff something is selected — independent of the
        // active tool, so a selection stays editable after switching to a
        // drawing tool.
        assert!(editor_show_selection_chrome(true));
        assert!(!editor_show_selection_chrome(false));
    }

    #[test]
    fn inline_text_editor_geometry_anchors_to_clicked_image_point() {
        let geom =
            inline_text_editor_geometry(PointLike::new(60.0, 40.0), (200, 100), (10.0, 20.0), 2.0);

        assert_eq!(geom.x, 100.0);
        assert_eq!(geom.y, 40.0);
    }

    #[test]
    fn inline_text_editor_geometry_clamps_near_image_edges() {
        let geom =
            inline_text_editor_geometry(PointLike::new(198.0, 98.0), (200, 100), (0.0, 0.0), 2.0);

        assert!(geom.x + geom.width <= 400.0);
        assert!(geom.y + geom.height <= 200.0);
    }

    #[test]
    fn m_selects_the_magnifier_only_when_not_typing() {
        use iced::keyboard::{Key, Modifiers};
        assert!(matches!(
            editor_key_message(Key::Character("m".into()), Modifiers::empty(), true),
            Some(Message::EditorToolbar(
                readshot_ui::ToolbarMessage::SelectTool(readshot_ui::editor::ToolState::Magnifier)
            ))
        ));
        assert!(
            editor_key_message(Key::Character("m".into()), Modifiers::empty(), false).is_none()
        );
    }

    #[test]
    fn editor_selected_hint_explains_the_magnifier_handles() {
        assert_eq!(
            editor_selected_hint("Magnifier", false),
            "Selected Magnifier — drag the bubble to move it · drag the centre to change what it shows · drag the ring to resize · ⌫ removes"
        );
    }

    #[test]
    fn editor_selected_hint_special_cases_crop_and_text() {
        assert_eq!(
            editor_selected_hint("Crop", false),
            "Selected Crop — drag frame or handles · arrows nudge · Delete restores full image"
        );
        assert_eq!(
            editor_selected_hint("Text", true),
            "Selected Text — edit inline · color/size apply here · arrows nudge · ⌫ removes"
        );
        assert_eq!(
            editor_selected_hint("Rectangle", false),
            "Selected Rectangle — drag to move · arrows nudge · handles resize · drag elsewhere to draw"
        );
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
    fn history_focus_search_shortcut_leaves_state_alone() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.history_window_id = Some(id);
        app.history_search = "invoice".into();

        let _ = update(
            &mut app,
            Message::HistoryKeyboardShortcut(id, HistoryKeyboardAction::FocusSearch),
        );

        assert_eq!(app.history_status, None);
        assert_eq!(app.history_search, "invoice");
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
            style: Default::default(),
        };
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(annotation)),
        );

        // The sidecar write runs on the coordinator's background
        // writer; wait for it before reading the store.
        app.coordinator.flush_history_updates();
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

        // First click arms the destructive prompt; records stay intact.
        let _ = update(&mut app, Message::HistoryClearAllRequested);
        assert!(app.history_clear_all_pending);
        assert_eq!(app.history_records.len(), 2);
        // Cancel exits the armed state without touching anything.
        let _ = update(&mut app, Message::HistoryClearAllCancelled);
        assert!(!app.history_clear_all_pending);
        assert_eq!(app.history_records.len(), 2);
        // Re-arm and confirm; now the records actually go.
        let _ = update(&mut app, Message::HistoryClearAllRequested);
        let _ = update(&mut app, Message::HistoryClearAllConfirmed);

        assert!(!app.history_clear_all_pending);
        assert!(store.list().unwrap().is_empty());
        assert!(app.history_records.is_empty());
        assert!(app
            .history_status
            .as_deref()
            .unwrap_or("")
            .starts_with("Cleared "));
    }

    // ---- #6: single-capture delete needs a confirm ----

    fn app_with_two_saved_records() -> (
        tempfile::TempDir,
        Arc<FsHistoryStore>,
        App,
        readshot_core::CaptureRecord,
        readshot_core::CaptureRecord,
    ) {
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
        app.history_records = vec![newer.clone(), older.clone()];
        app.history_selected_id = Some(newer.id);
        (dir, store, app, newer, older)
    }

    #[test]
    fn single_delete_arms_before_removing_anything() {
        let (_dir, store, mut app, newer, _older) = app_with_two_saved_records();

        // First press only arms the prompt; nothing is removed.
        let _ = update(&mut app, Message::HistoryDelete(newer.id));
        assert_eq!(app.history_delete_pending, Some(newer.id));
        assert_eq!(store.list().unwrap().len(), 2);

        // Explicit confirm removes exactly the armed record.
        let _ = update(&mut app, Message::HistoryDeleteConfirmed(newer.id));
        assert_eq!(app.history_delete_pending, None);
        let remaining = store.list().unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining.iter().all(|r| r.id != newer.id));
    }

    #[test]
    fn single_delete_second_press_confirms() {
        // Delete-key path: a repeat press for the same id confirms.
        let (_dir, store, mut app, newer, _older) = app_with_two_saved_records();
        let _ = update(&mut app, Message::HistoryDelete(newer.id));
        let _ = update(&mut app, Message::HistoryDelete(newer.id));
        assert_eq!(app.history_delete_pending, None);
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn single_delete_cancelled_by_other_actions() {
        let (_dir, store, mut app, newer, older) = app_with_two_saved_records();

        // Selecting another row cancels the armed delete.
        let _ = update(&mut app, Message::HistoryDelete(newer.id));
        let _ = update(&mut app, Message::HistorySelect(older.id));
        assert_eq!(app.history_delete_pending, None);

        // A search change (Escape maps here too) cancels it.
        let _ = update(&mut app, Message::HistoryDelete(newer.id));
        let _ = update(&mut app, Message::HistorySearchChanged("x".into()));
        assert_eq!(app.history_delete_pending, None);

        // Explicit cancel clears it as well.
        let _ = update(&mut app, Message::HistoryDelete(newer.id));
        let _ = update(&mut app, Message::HistoryDeleteCancelled);
        assert_eq!(app.history_delete_pending, None);

        // Nothing was ever removed while only arming/cancelling.
        assert_eq!(store.list().unwrap().len(), 2);
    }

    // ---- #7: open browser refreshes on save and on OCR result ----

    #[test]
    fn history_record_saved_refreshes_only_when_browser_open() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));

        // Closed browser: no reload work is scheduled.
        app.history_window_id = None;
        assert_eq!(update(&mut app, Message::HistoryRecordSaved).units(), 0);

        // Open browser: the fresh (text-less) row triggers a reload.
        app.history_window_id = Some(iced::window::Id::unique());
        assert_eq!(update(&mut app, Message::HistoryRecordSaved).units(), 1);
    }

    #[test]
    fn history_persisted_refreshes_on_text_but_not_on_ocr_failure() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.history_window_id = Some(iced::window::Id::unique());

        // OCR landed text → reload to surface it.
        assert_eq!(
            update(&mut app, Message::HistoryRecordPersisted(Ok(true))).units(),
            1
        );
        // OCR failed/empty → row already shown via HistoryRecordSaved,
        // text won't change, so no extra reload.
        assert_eq!(
            update(&mut app, Message::HistoryRecordPersisted(Ok(false))).units(),
            0
        );
    }

    // ---- #10: pagination bounds the rendered list ----

    #[test]
    fn history_show_more_grows_page_and_search_resets_it() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        assert_eq!(app.history_page_limit, crate::app::HISTORY_PAGE_SIZE);

        let _ = update(&mut app, Message::HistoryShowMore);
        assert_eq!(app.history_page_limit, crate::app::HISTORY_PAGE_SIZE * 2);

        // A search change resets paging to the first page so the new
        // result set starts from the top.
        let _ = update(&mut app, Message::HistorySearchChanged("q".into()));
        assert_eq!(app.history_page_limit, crate::app::HISTORY_PAGE_SIZE);
    }

    // ---- #32: user-facing statuses are human-readable ----

    #[test]
    fn friendly_errors_hide_raw_internals_and_keep_hints() {
        use readshot_core::HistoryError;

        // Permission-denied io errors keep a useful, plain hint and
        // never leak the raw Display string into the status line.
        let denied = HistoryError::Io("Permission denied (os error 13)".into());
        let msg = history_ops::friendly_history_error("delete this capture", &denied);
        assert!(msg.contains("permission denied"), "{msg}");
        assert!(!msg.contains("os error 13"), "{msg}");

        // Unclassified io errors fall back to a generic message.
        let other = HistoryError::Io("broken pipe".into());
        let msg = history_ops::friendly_history_error("clear the history", &other);
        assert!(msg.starts_with("Couldn't clear the history"), "{msg}");
        assert!(!msg.contains("broken pipe"), "{msg}");

        // String-typed (clipboard / file-manager) errors: not-found
        // hint, and no raw text leaked otherwise.
        let nf = history_ops::friendly_string_error("open the save folder", "No such file");
        assert!(nf.contains("already gone"), "{nf}");
        let generic =
            history_ops::friendly_string_error("copy the image to the clipboard", "weird backend");
        assert!(!generic.contains("weird backend"), "{generic}");

        // A recovery next-step is appended as a second sentence; an
        // unclassified error drops the "See the log" tail in its favour.
        let save = history_ops::friendly_string_error_with(
            "save the image",
            "No space left on device",
            Some(history_ops::SAVE_HINT),
        );
        assert!(save.contains("the disk is full"), "{save}");
        assert!(
            save.ends_with("Pick a different folder in Settings and try again."),
            "{save}"
        );
        let retry = history_ops::friendly_string_error_with(
            "copy the text",
            "weird backend",
            Some("Try again."),
        );
        assert_eq!(retry, "Couldn't copy the text. Try again.", "{retry}");

        // Capture errors suggest the Screen Recording permission only
        // when the error looks permission-related, never otherwise.
        let perm = history_ops::friendly_capture_error("permission denied (os error 1)");
        assert!(perm.contains("permission denied"), "{perm}");
        assert!(perm.contains("Screen Recording"), "{perm}");
        let other = history_ops::friendly_capture_error("backend offline");
        assert!(!other.contains("backend offline"), "{other}");
        assert!(other.ends_with("Try again."), "{other}");
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

    #[cfg(target_os = "macos")]
    #[test]
    fn relaunch_command_waits_then_forces_a_fresh_instance() {
        let command = relaunch_command_for_bundle(
            std::path::Path::new("/Applications/Readshot.app"),
            RelaunchMode::NewInstance,
        );

        assert_eq!(command.program, "/bin/sh");
        assert!(command.args[1].contains("sleep 0.35"));
        assert!(command.args[1].contains("/usr/bin/open -n \"$1\""));
        assert_eq!(command.args[3], "/Applications/Readshot.app");
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
        // explorer.exe needs `/select,` and the path in ONE argv token,
        // otherwise it ignores the selection.
        assert_eq!(command.args, vec!["/select,C:\\tmp\\capture.png"]);
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
    fn welcome_ready_marks_onboarding_completed_after_grant() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.onboarding_completed = false;
        app.welcome = WelcomeState::Granted;

        let _ = update(
            &mut app,
            Message::WelcomeWindowReady(iced::window::Id::unique()),
        );

        assert!(app.preferences.onboarding_completed);
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
    fn welcome_status_dismissed_clears_capture_status() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.last_capture_status = Some("Couldn't save.".into());
        let _ = update(&mut app, Message::WelcomeStatusDismissed);
        assert!(app.last_capture_status.is_none());
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
    async fn capture_region_with_known_scale_skips_display_listing() {
        struct KnownScaleCapturer {
            seen_scale: Arc<Mutex<Option<f32>>>,
        }

        #[async_trait]
        impl Capturer for KnownScaleCapturer {
            async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
                panic!("known-scale capture should not list displays");
            }

            async fn capture_region(
                &self,
                req: CaptureRequest,
            ) -> Result<image::RgbaImage, CaptureError> {
                *self.seen_scale.lock().unwrap() = Some(req.scale);
                Ok(solid(64, 64))
            }
        }

        let seen_scale = Arc::new(Mutex::new(None));
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(KnownScaleCapturer {
                seen_scale: Arc::clone(&seen_scale),
            }),
            Arc::new(FakeOcrEngine::with_text("hi")),
            perms,
            None,
        );

        capture_region_to_image_with_scale(
            coord,
            "secondary".into(),
            readshot_core::geom::Rect::from_xywh(0.0, 0.0, 32.0, 32.0).unwrap(),
            2.0,
            true,
        )
        .await
        .unwrap();

        assert_eq!(*seen_scale.lock().unwrap(), Some(2.0));
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
        assert_eq!(pretty_hotkey("cmd+shift+x"), "\u{21E7}\u{2318}X");
        // Apple order ⌃⌥⇧⌘: control then alt here.
        assert_eq!(pretty_hotkey("ctrl+alt+y"), "\u{2303}\u{2325}Y");
        assert_eq!(pretty_hotkey("CMD+Shift+Z"), "\u{21E7}\u{2318}Z");
    }

    #[test]
    fn pretty_hotkey_falls_back_to_input_when_unparseable() {
        // Empty stays empty; nonsense single-token is preserved.
        assert_eq!(pretty_hotkey(""), "");
        assert_eq!(pretty_hotkey("???"), "???");
    }

    #[test]
    fn register_hotkey_actions_keeps_fixed_hotkeys_when_capture_hotkey_is_invalid() {
        let prefs = Preferences {
            capture_hotkey: "not a hotkey at all".into(),
            ..Preferences::default()
        };
        let HotkeyActions {
            actions,
            capture_registered,
            ..
        } = register_hotkey_actions(&prefs, |_| Ok(()));

        assert!(!capture_registered);
        assert!(actions.values().any(|a| *a == GlobalHotkeyAction::History));
        assert!(actions.values().any(|a| *a == GlobalHotkeyAction::Settings));
        assert!(!actions.values().any(|a| *a == GlobalHotkeyAction::Capture));
    }

    #[test]
    fn register_hotkey_actions_keeps_fixed_hotkeys_when_capture_registration_fails() {
        let calls = std::cell::Cell::new(0);
        let prefs = Preferences {
            capture_hotkey: "ctrl+shift+x".into(),
            ..Preferences::default()
        };
        // Capture is registered first; the first call fails so capture is
        // dropped, and the two later fixed registrations succeed.
        let HotkeyActions {
            actions,
            capture_registered,
            failed_fixed,
        } = register_hotkey_actions(&prefs, |_| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err("already owned".into())
            } else {
                Ok(())
            }
        });

        assert!(!capture_registered);
        assert!(failed_fixed.is_empty());
        assert!(actions.values().any(|a| *a == GlobalHotkeyAction::History));
        assert!(actions.values().any(|a| *a == GlobalHotkeyAction::Settings));
        assert!(!actions.values().any(|a| *a == GlobalHotkeyAction::Capture));
    }

    #[test]
    fn register_hotkey_actions_rejects_capture_chord_that_conflicts_with_fixed() {
        // A capture chord equal to the reserved Settings hotkey must not
        // be registered (and must not clobber the fixed one).
        let prefs = Preferences {
            capture_hotkey: settings_hotkey().into(),
            ..Preferences::default()
        };
        let attempts = std::cell::Cell::new(0);
        let HotkeyActions {
            actions,
            capture_registered,
            failed_fixed,
        } = register_hotkey_actions(&prefs, |_| {
            attempts.set(attempts.get() + 1);
            Ok(())
        });

        assert!(!capture_registered, "conflicting capture must not register");
        assert!(!actions.values().any(|a| *a == GlobalHotkeyAction::Capture));
        assert!(actions.values().any(|a| *a == GlobalHotkeyAction::Settings));
        assert!(actions.values().any(|a| *a == GlobalHotkeyAction::History));
        assert!(failed_fixed.is_empty());
        // Only the two fixed hotkeys were ever handed to the OS.
        assert_eq!(attempts.get(), 2);
        assert_eq!(capture_hotkey_conflict(settings_hotkey()), Some("Settings"));
    }

    #[test]
    fn register_hotkey_actions_reports_failed_fixed_hotkeys() {
        let prefs = Preferences {
            capture_hotkey: "ctrl+shift+x".into(),
            ..Preferences::default()
        };
        // Fail every registration: capture drops, and both fixed hotkeys
        // are reported as failures.
        let HotkeyActions {
            capture_registered,
            failed_fixed,
            ..
        } = register_hotkey_actions(&prefs, |_| Err("denied".into()));

        assert!(!capture_registered);
        assert_eq!(failed_fixed, vec!["History", "Settings"]);
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
    fn editor_output_image_bakes_annotations_and_selected_frame() {
        let mut ed = crate::editor::EditorSession::new(solid(32, 32));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(6.0, 6.0, 20.0, 20.0),
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            line_width: 4.0,
            style: Default::default(),
        });
        ed.model
            .set_beautify(readshot_core::beautify::BuiltinPreset::Minimal.settings());

        let output = editor_output_image(&mut ed);

        assert_eq!((output.width(), output.height()), (80, 80));
        assert_eq!(*output.get_pixel(0, 0), image::Rgba([255, 255, 255, 255]));
        let annotated = *output.get_pixel(24 + 6, 24 + 6);
        assert!(
            annotated[0] > 180 && annotated[1] < 80 && annotated[2] < 80,
            "expected visible red annotation pixel, got {annotated:?}"
        );
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
    fn app_window_shortcuts_open_history_and_settings() {
        let mut modifiers = iced::keyboard::Modifiers::default();
        modifiers.insert(iced::keyboard::Modifiers::COMMAND);

        assert!(matches!(
            app_window_shortcut_message(iced::keyboard::Key::Character("y".into()), modifiers),
            Some(Message::OpenHistoryRequested)
        ));
        assert!(matches!(
            app_window_shortcut_message(iced::keyboard::Key::Character(",".into()), modifiers),
            Some(Message::OpenSettingsRequested)
        ));
    }

    #[test]
    fn app_window_shortcuts_ignore_extra_modifiers() {
        let mut modifiers = iced::keyboard::Modifiers::default();
        modifiers.insert(iced::keyboard::Modifiers::COMMAND);
        modifiers.insert(iced::keyboard::Modifiers::SHIFT);

        assert!(
            app_window_shortcut_message(iced::keyboard::Key::Character("y".into()), modifiers)
                .is_none()
        );
    }

    #[test]
    fn global_hotkey_action_ignores_release_events() {
        let mut actions = HashMap::new();
        actions.insert(7, GlobalHotkeyAction::History);
        let event = GlobalHotKeyEvent {
            id: 7,
            state: HotKeyState::Released,
        };

        assert_eq!(global_hotkey_action(&event, &actions), None);
    }

    #[test]
    fn global_hotkey_action_maps_pressed_ids() {
        let mut actions = HashMap::new();
        actions.insert(7, GlobalHotkeyAction::Settings);
        let event = GlobalHotKeyEvent {
            id: 7,
            state: HotKeyState::Pressed,
        };

        assert_eq!(
            global_hotkey_action(&event, &actions),
            Some(GlobalHotkeyAction::Settings)
        );
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
        let _lock = crate::url_events::lock_for_tests();
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
        assert!(!prefs.onboarding_completed);
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
        assert!(!prefs.onboarding_completed);
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
    fn load_preferences_migrates_existing_users_as_onboarded() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("preferences.toml");
        std::fs::write(
            &path,
            "schema_version = 2\ncapture_hotkey = \"ctrl+alt+9\"\n",
        )
        .unwrap();

        let loaded = load_preferences(Some(&path));

        assert!(loaded.onboarding_completed);
        assert_eq!(loaded.capture_hotkey, "ctrl+alt+9");
    }

    #[test]
    fn boot_opens_ready_window_for_manual_granted_launch() {
        assert!(needs_welcome_window_on_boot(
            WelcomeState::Granted,
            false,
            false
        ));
    }

    #[test]
    fn boot_skips_ready_window_for_login_launch_when_granted() {
        // The whole point of the fix: a login launch settles into the
        // menu bar instead of popping the ready window every boot.
        assert!(!needs_welcome_window_on_boot(
            WelcomeState::Granted,
            false,
            true
        ));
    }

    #[test]
    fn boot_still_opens_permission_window_for_login_launch_when_blocked() {
        // Permission still missing → the gate must show even at login,
        // otherwise the user has no way to grant access.
        assert!(needs_welcome_window_on_boot(
            WelcomeState::Pending,
            false,
            true
        ));
    }

    #[test]
    fn boot_skips_ready_window_for_url_launch_when_granted() {
        assert!(!needs_welcome_window_on_boot(
            WelcomeState::Granted,
            true,
            false
        ));
    }

    #[test]
    fn boot_still_opens_permission_window_for_url_launch_when_blocked() {
        assert!(needs_welcome_window_on_boot(
            WelcomeState::Pending,
            true,
            false
        ));
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
        // capture re-registration fails, but the user's preference
        // still gets saved in-memory so they can fix the typo and try
        // again. Fixed app hotkeys may still be registered.
        let _ = update(
            &mut app,
            Message::Settings(SettingsMessage::SetCaptureHotkey("not-a-real-chord".into())),
        );
        assert_eq!(app.preferences.capture_hotkey, "not-a-real-chord");
        assert!(!app.capture_hotkey_registered);
        assert_eq!(
            app.settings_hotkey_error.as_deref(),
            Some("That shortcut could not be read."),
        );
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

    #[test]
    fn window_closed_clears_editor_session() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_id = Some(id);
        app.windows.register(id, WindowKind::Editor);
        app.editor = Some(ed);

        let _ = update(&mut app, Message::WindowClosed(id));

        assert!(app.editor.is_none());
        assert!(app.windows.kind(id).is_none());
    }

    #[test]
    fn window_closed_commits_pending_history_text_before_dropping_editor() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let record =
            readshot_core::CaptureRecord::new(chrono::Utc::now(), 100, 100, "primary".to_string());
        store.save(&record, b"png").unwrap();

        let history: Arc<dyn HistoryStore> = store.clone();
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), history);
        let id = iced::window::Id::unique();
        let mut ed = crate::editor::EditorSession::from_history(solid(100, 100), record);
        ed.window_id = Some(id);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(12.0, 18.0),
            content: "window close draft".into(),
            edit_index: None,
        });
        app.windows.register(id, WindowKind::Editor);
        app.editor = Some(ed);

        let _ = update(&mut app, Message::WindowClosed(id));

        assert!(app.editor.is_none());
        assert!(app.windows.kind(id).is_none());
        // The sidecar write runs on the coordinator's background
        // writer; wait for it before reading the store.
        app.coordinator.flush_history_updates();
        let from_disk = store.list().unwrap();
        assert_eq!(from_disk[0].annotation_model.len(), 1);
        match &from_disk[0].annotation_model[0] {
            Annotation::Text {
                content, origin, ..
            } => {
                assert_eq!(content, "window close draft");
                assert_eq!(*origin, PointLike::new(12.0, 18.0));
            }
            other => panic!("expected text annotation, got {other:?}"),
        }
    }

    #[test]
    fn keyboard_width_bump_updates_selected_annotation() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorWidthBump(3.0));

        // Steps from the selection's own width (2), which the options
        // bar's Width control shows, not the tool width (3).
        let ed = app.editor.as_ref().unwrap();
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 5.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn dragging_inside_an_unselected_bubble_moves_only_the_bubble() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(200, 100));
        ed.model.commit_annotation(Annotation::Magnifier {
            source: PointLike::new(20.0, 20.0),
            radius: 10.0,
            bubble: PointLike::new(80.0, 40.0),
            zoom: 4.0,
            shape: readshot_core::MagnifierShape::Circle,
            color: Rgba::OPAQUE_BLACK,
        });
        ed.model.clear_selection();
        app.editor = Some(ed);

        // 30 px from the bubble centre: inside the bubble, off its handle.
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                110.0, 40.0,
            ))),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
                100.0, 50.0,
            ))),
        );
        let ed = app.editor.as_ref().unwrap();
        match &ed.model.annotations()[0] {
            Annotation::Magnifier { source, bubble, .. } => {
                assert_eq!(*source, PointLike::new(20.0, 20.0));
                assert_eq!(*bubble, PointLike::new(70.0, 50.0));
            }
            other => panic!("expected magnifier, got {other:?}"),
        }
    }

    fn magnifier_app(selected: bool) -> App {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(300, 200));
        // Source (40, 40) r 20, bubble (200, 40) r 40: the leader runs
        // from x 60 to x 160 along y 40.
        ed.model.commit_annotation(Annotation::Magnifier {
            source: PointLike::new(40.0, 40.0),
            radius: 20.0,
            bubble: PointLike::new(200.0, 40.0),
            zoom: 2.0,
            shape: readshot_core::MagnifierShape::Circle,
            color: Rgba::OPAQUE_BLACK,
        });
        if !selected {
            ed.model.clear_selection();
        }
        app.editor = Some(ed);
        app
    }

    fn select_drag(app: &mut App, from: (f32, f32), to: (f32, f32)) {
        for msg in [
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(from.0, from.1)),
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(to.0, to.1)),
            readshot_ui::CanvasMessage::SelectReleased,
        ] {
            let _ = update(app, Message::EditorCanvas(msg));
        }
    }

    fn magnifier_parts(app: &App) -> (PointLike, f32, PointLike) {
        match &app.editor.as_ref().unwrap().model.annotations()[0] {
            Annotation::Magnifier {
                source,
                radius,
                bubble,
                ..
            } => (*source, *radius, *bubble),
            other => panic!("expected magnifier, got {other:?}"),
        }
    }

    #[test]
    fn a_first_drag_on_an_unselected_magnifiers_handles_matches_the_second() {
        for selected in [false, true] {
            // Near the source centre (the Start handle): only the source.
            let mut app = magnifier_app(selected);
            select_drag(&mut app, (43.0, 42.0), (53.0, 47.0));
            assert_eq!(
                magnifier_parts(&app),
                (
                    PointLike::new(50.0, 45.0),
                    20.0,
                    PointLike::new(200.0, 40.0)
                ),
                "selected: {selected}"
            );
            // Inside the disc near the rim (the East handle): the radius.
            let mut app = magnifier_app(selected);
            select_drag(&mut app, (55.0, 40.0), (65.0, 40.0));
            assert_eq!(
                magnifier_parts(&app),
                (
                    PointLike::new(40.0, 40.0),
                    30.0,
                    PointLike::new(200.0, 40.0)
                ),
                "selected: {selected}"
            );
        }
    }

    #[test]
    fn a_selected_magnifiers_leader_moves_the_whole_magnifier() {
        let mut app = magnifier_app(true);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        select_drag(&mut app, (110.0, 41.0), (120.0, 51.0));
        assert_eq!(
            magnifier_parts(&app),
            (
                PointLike::new(50.0, 50.0),
                20.0,
                PointLike::new(210.0, 50.0)
            )
        );
        assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
        // An unselected leader stays click-through.
        let mut app = magnifier_app(false);
        select_drag(&mut app, (110.0, 41.0), (120.0, 51.0));
        assert_eq!(
            magnifier_parts(&app),
            (
                PointLike::new(40.0, 40.0),
                20.0,
                PointLike::new(200.0, 40.0)
            )
        );
        assert_eq!(
            app.editor.as_ref().unwrap().model.selected_annotation(),
            None
        );
    }

    #[test]
    fn editor_select_drag_cancel_restores_baseline_without_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                5.0, 5.0,
            ))),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
                12.0, 14.0,
            ))),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::Cancelled),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.move_drag.is_none());
        assert_eq!(ed.model.undo_depth(), undo_depth);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.x, 0.0);
                assert_eq!(rect.y, 0.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn editor_select_subpixel_drag_release_does_not_mutate_annotation() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                5.0, 5.0,
            ))),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
                5.25, 5.25,
            ))),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectReleased),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.move_drag.is_none());
        assert_eq!(ed.model.undo_depth(), undo_depth);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(rect.x, 0.0);
                assert_eq!(rect.y, 0.0);
            }
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn editor_tool_change_commits_pending_text_when_leaving_text_tool() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.set_tool(readshot_ui::editor::ToolState::Text);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "draft".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Rectangle,
            )),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert_eq!(ed.model.annotations().len(), 1);
        match &ed.model.annotations()[0] {
            Annotation::Text {
                content, origin, ..
            } => {
                assert_eq!(content, "draft");
                assert_eq!(*origin, PointLike::new(8.0, 9.0));
            }
            other => panic!("expected text annotation, got {other:?}"),
        }
        assert_eq!(
            ed.model.active_tool(),
            readshot_ui::editor::ToolState::Rectangle
        );
    }

    #[test]
    fn editor_tool_change_dismisses_empty_pending_text_without_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let undo_depth = ed.model.undo_depth();
        ed.model.set_tool(readshot_ui::editor::ToolState::Text);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "   ".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Rectangle,
            )),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert!(ed.model.annotations().is_empty());
        assert_eq!(ed.model.undo_depth(), undo_depth);
        assert_eq!(
            ed.model.active_tool(),
            readshot_ui::editor::ToolState::Rectangle
        );
    }

    #[test]
    fn editor_requesting_new_text_commits_existing_non_empty_draft() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.set_tool(readshot_ui::editor::ToolState::Text);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "first note".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::RequestText(PointLike::new(
                24.0, 25.0,
            ))),
        );

        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 1);
        match &ed.model.annotations()[0] {
            Annotation::Text {
                content, origin, ..
            } => {
                assert_eq!(content, "first note");
                assert_eq!(*origin, PointLike::new(8.0, 9.0));
            }
            other => panic!("expected text annotation, got {other:?}"),
        }
        let pending = ed.pending_text.as_ref().unwrap();
        assert_eq!(pending.origin, PointLike::new(24.0, 25.0));
        assert!(pending.content.is_empty());
    }

    #[test]
    fn editor_requesting_new_text_dismisses_empty_draft_without_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let undo_depth = ed.model.undo_depth();
        ed.model.set_tool(readshot_ui::editor::ToolState::Text);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "   ".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::RequestText(PointLike::new(
                24.0, 25.0,
            ))),
        );

        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), undo_depth);
        assert!(ed.model.annotations().is_empty());
        let pending = ed.pending_text.as_ref().unwrap();
        assert_eq!(pending.origin, PointLike::new(24.0, 25.0));
        assert!(pending.content.is_empty());
    }

    #[test]
    fn editor_text_commit_updates_original_edit_index_even_if_selection_changes() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.model.commit_annotation(Annotation::Text {
            content: "old".into(),
            origin: PointLike::new(10.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(40.0, 40.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(10.0, 20.0),
            content: "new".into(),
            edit_index: Some(0),
        });
        assert_eq!(ed.model.select_at(PointLike::new(45.0, 45.0)), Some(1));
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorTextCommit);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        match &ed.model.annotations()[0] {
            Annotation::Text { content, .. } => assert_eq!(content, "new"),
            other => panic!("expected text annotation, got {other:?}"),
        }
        assert!(matches!(
            &ed.model.annotations()[1],
            Annotation::Rectangle { .. }
        ));
    }

    #[test]
    fn editor_delete_selected_dismisses_pending_text_edit() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.model.commit_annotation(Annotation::Text {
            content: "old".into(),
            origin: PointLike::new(10.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(10.0, 20.0),
            content: "draft replacement".into(),
            edit_index: Some(0),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDeleteSelected);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert!(ed.model.annotations().is_empty());
        assert_eq!(ed.model.selected_annotation(), None);
        assert_eq!(
            ed.status.as_deref(),
            Some("Deleted annotation. ⌘Z to undo.")
        );
    }

    #[test]
    fn editor_nudge_is_suppressed_while_editing_text() {
        // iced's single-line text input ignores Up/Down, so those arrows
        // reach the global key tap while a text draft is open. The nudge
        // handler must drop them rather than move the annotation being
        // typed (which would also desync the inline editor box).
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.model.commit_annotation(Annotation::Text {
            content: "hi".into(),
            origin: PointLike::new(10.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
        let undo_before = ed.model.undo_depth();
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(10.0, 20.0),
            content: "hi".into(),
            edit_index: Some(0),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorNudgeSelected(0.0, 5.0));

        let ed = app.editor.as_ref().unwrap();
        match &ed.model.annotations()[0] {
            Annotation::Text { origin, .. } => {
                assert_eq!(origin.x, 10.0);
                assert_eq!(origin.y, 20.0);
            }
            other => panic!("expected text, got {other:?}"),
        }
        assert_eq!(ed.model.undo_depth(), undo_before);
    }

    #[test]
    fn editor_delete_without_selection_keeps_new_pending_text() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(10.0, 20.0),
            content: "draft".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDeleteSelected);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_some());
        assert!(ed.model.annotations().is_empty());
        assert_eq!(ed.status, None);
    }

    #[test]
    fn editor_cancel_existing_text_edit_keeps_annotation_selected() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.model.commit_annotation(Annotation::Text {
            content: "old".into(),
            origin: PointLike::new(10.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(12.0, 8.0)), Some(0));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(10.0, 20.0),
            content: "draft replacement".into(),
            edit_index: Some(0),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorTextCancel);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert_eq!(ed.model.selected_annotation(), Some(0));
        match &ed.model.annotations()[0] {
            Annotation::Text { content, .. } => assert_eq!(content, "old"),
            other => panic!("expected text annotation, got {other:?}"),
        }
    }

    #[test]
    fn editor_cancel_new_text_draft_clears_selection() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(10.0, 20.0),
            content: "new label".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorTextCancel);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert_eq!(ed.model.selected_annotation(), None);
        assert_eq!(ed.model.annotations().len(), 1);
    }

    #[test]
    fn editor_undo_commits_then_undoes_new_pending_text() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(30.0, 30.0),
            content: "draft label".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert_eq!(ed.model.annotations().len(), 1);
        assert!(matches!(
            ed.model.annotations()[0],
            Annotation::Rectangle { .. }
        ));
        assert!(ed.model.can_redo());
        assert_eq!(ed.status.as_deref(), Some("Undid edit. ⇧⌘Z to redo."));
    }

    #[test]
    fn cmd_z_with_a_text_draft_open_keeps_the_draft() {
        // The draft's text_input has no undo and doesn't capture ⌘Z; the
        // shortcut must not commit-then-undo the typed text.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(30.0, 30.0),
            content: "hello".into(),
            edit_index: None,
        });
        app.editor = Some(ed);
        let undo_before = app.editor.as_ref().unwrap().model.undo_depth();

        for modifiers in [
            iced::keyboard::Modifiers::COMMAND,
            iced::keyboard::Modifiers::COMMAND | iced::keyboard::Modifiers::SHIFT,
        ] {
            for status_ignored in [true, false] {
                let _ = update(
                    &mut app,
                    Message::EditorKeyPressed {
                        window: id,
                        key: iced::keyboard::Key::Character("z".into()),
                        modifiers,
                        status_ignored,
                    },
                );
            }
        }

        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.pending_text.as_ref().map(|p| p.content.as_str()),
            Some("hello")
        );
        assert_eq!(ed.model.annotations().len(), 1);
        assert_eq!(ed.model.undo_depth(), undo_before);
        assert!(!ed.model.can_redo());
    }

    #[test]
    fn editor_redo_with_pending_text_commits_new_branch_and_clears_redo() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(96, 96));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        ed.model.commit_annotation(Annotation::Ellipse {
            rect: RectLike::new(40.0, 40.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert!(ed.model.undo());
        assert!(ed.model.can_redo());
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(30.0, 30.0),
            content: "new branch".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert_eq!(ed.model.annotations().len(), 2);
        assert!(matches!(
            ed.model.annotations()[0],
            Annotation::Rectangle { .. }
        ));
        match &ed.model.annotations()[1] {
            Annotation::Text {
                content, origin, ..
            } => {
                assert_eq!(content, "new branch");
                assert_eq!(*origin, PointLike::new(30.0, 30.0));
            }
            other => panic!("expected text annotation, got {other:?}"),
        }
        assert!(!ed.model.can_redo());
        assert_eq!(ed.status.as_deref(), Some("Added text. ⌘Z to undo."));
    }

    #[test]
    fn editor_select_press_commits_non_empty_pending_text() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "label".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                30.0, 30.0,
            ))),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert_eq!(ed.model.annotations().len(), 1);
        match &ed.model.annotations()[0] {
            Annotation::Text {
                content, origin, ..
            } => {
                assert_eq!(content, "label");
                assert_eq!(*origin, PointLike::new(8.0, 9.0));
            }
            other => panic!("expected text annotation, got {other:?}"),
        }
    }

    #[test]
    fn editor_select_press_dismisses_empty_pending_text_without_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let undo_depth = ed.model.undo_depth();
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "   ".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                30.0, 30.0,
            ))),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert!(ed.model.annotations().is_empty());
        assert_eq!(ed.model.undo_depth(), undo_depth);
    }

    #[test]
    fn editor_tool_change_cancels_line_width_preview_without_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorLineWidthPreview(10.0));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Arrow,
            )),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.width_drag_baseline.is_none());
        assert_eq!(ed.model.undo_depth(), undo_depth);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn editor_select_press_cancels_line_width_preview_without_committing() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorLineWidthPreview(10.0));
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                30.0, 30.0,
            ))),
        );

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.width_drag_baseline.is_none());
        assert_eq!(ed.model.undo_depth(), undo_depth);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn editor_frame_change_cancels_line_width_preview_without_committing() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorLineWidthPreview(10.0));
        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));

        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.beautify(), BuiltinPreset::Soft.settings());
        assert!(ed.width_drag_baseline.is_none());
        assert_eq!(ed.model.undo_depth(), undo_depth + 1);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn editor_save_cancels_line_width_preview_before_output() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorLineWidthPreview(10.0));
        let _ = update(&mut app, Message::EditorSaveRequested);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.busy);
        assert!(ed.width_drag_baseline.is_none());
        assert_eq!(ed.model.undo_depth(), undo_depth);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn editor_save_commits_pending_text_before_output() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: " ship this ".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorSaveRequested);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert_eq!(ed.model.annotations().len(), 1);
        match &ed.model.annotations()[0] {
            Annotation::Text {
                content, origin, ..
            } => {
                assert_eq!(content, "ship this");
                assert_eq!(*origin, PointLike::new(8.0, 9.0));
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn editor_save_dismisses_empty_pending_text_without_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let undo_depth = ed.model.undo_depth();
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "   ".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorSaveRequested);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert!(ed.model.annotations().is_empty());
        assert_eq!(ed.model.undo_depth(), undo_depth);
    }

    #[test]
    fn editor_successful_save_marks_current_output_clean_but_keeps_undo() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        ed.model.set_beautify(BuiltinPreset::Soft.settings());
        let undo_depth = ed.model.undo_depth();
        app.editor = Some(ed);

        let dir = tempfile::tempdir().unwrap();
        let saved_path = dir.path().join("saved.png");
        let _ = update(&mut app, Message::EditorSaved(Ok(Some(saved_path))));

        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), undo_depth);
        assert!(ed.model.can_undo());
        assert!(!editor_has_unsaved_work(ed));

        let _ = update(&mut app, Message::EditorDiscardRequested);

        assert!(app.editor.is_none());
    }

    #[test]
    fn editor_successful_copy_image_marks_current_output_clean() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        ed.model.set_beautify(BuiltinPreset::Dark.settings());
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorCopyImageDone(Ok(())));

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.model.can_undo());
        assert!(!editor_has_unsaved_work(ed));
    }

    #[test]
    fn editor_cancelled_or_failed_output_keeps_unsaved_work_dirty() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorSaved(Ok(None)));
        assert!(editor_has_unsaved_work(app.editor.as_ref().unwrap()));

        let _ = update(
            &mut app,
            Message::EditorCopyImageDone(Err("clipboard unavailable".into())),
        );
        assert!(editor_has_unsaved_work(app.editor.as_ref().unwrap()));
    }

    #[test]
    fn editor_discard_requires_confirmation_for_non_empty_pending_text() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "draft".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDiscardRequested);

        let ed = app.editor.as_ref().expect("first discard should only arm");
        assert!(ed.discard_pending_at.is_some());
        assert_eq!(
            ed.status.as_deref(),
            Some("Unsaved edits. Press Discard again to close without saving.")
        );

        let _ = update(&mut app, Message::EditorDiscardRequested);

        assert!(app.editor.is_none());
    }

    #[test]
    fn editor_text_action_supports_multi_line_and_commits_newline() {
        use iced::widget::text_editor::{Action, Edit};

        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.begin_text_edit(crate::editor::PendingText {
            origin: PointLike::new(8.0, 20.0),
            content: String::new(),
            edit_index: None,
        });
        assert!(ed.text_edit_content.is_some());
        app.editor = Some(ed);

        // Type "ab", press Enter (newline), type "cd".
        for action in [
            Action::Edit(Edit::Insert('a')),
            Action::Edit(Edit::Insert('b')),
            Action::Edit(Edit::Enter),
            Action::Edit(Edit::Insert('c')),
            Action::Edit(Edit::Insert('d')),
        ] {
            let _ = update(&mut app, Message::EditorTextAction(action));
        }

        let pending = app
            .editor
            .as_ref()
            .unwrap()
            .pending_text
            .as_ref()
            .expect("draft still open");
        assert_eq!(pending.content, "ab\ncd", "Enter should insert a newline");

        // Commit builds a multi-line Annotation::Text and tears down the
        // inline editor's backing store.
        let _ = update(&mut app, Message::EditorTextCommit);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.pending_text.is_none());
        assert!(ed.text_edit_content.is_none());
        match ed.model.annotations().last().expect("text committed") {
            Annotation::Text { content, .. } => assert_eq!(content, "ab\ncd"),
            other => panic!("expected multi-line text, got {other:?}"),
        }
    }

    #[test]
    fn editor_os_close_request_confirms_unsaved_work() {
        // OS close (red X / ⌘W) on a dirty editor must route through the
        // two-stage confirmation, not silently drop the session.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let editor_id = iced::window::Id::unique();
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_id = Some(editor_id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);

        // First request arms the confirmation and keeps the editor open.
        let _ = update(&mut app, Message::WindowCloseRequested(editor_id));
        let ed = app
            .editor
            .as_ref()
            .expect("first OS close should only arm, not drop the editor");
        assert!(ed.discard_pending_at.is_some());
        assert_eq!(
            ed.status.as_deref(),
            Some("Unsaved edits. Press Discard again to close without saving.")
        );

        // Second request within the confirm window closes it.
        let _ = update(&mut app, Message::WindowCloseRequested(editor_id));
        assert!(app.editor.is_none());
    }

    #[test]
    fn editor_os_close_request_closes_clean_editor_immediately() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let editor_id = iced::window::Id::unique();
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_id = Some(editor_id);
        app.editor = Some(ed);

        let _ = update(&mut app, Message::WindowCloseRequested(editor_id));
        assert!(app.editor.is_none());
    }

    #[test]
    fn os_close_request_for_non_editor_window_leaves_editor_untouched() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let editor_id = iced::window::Id::unique();
        let other_id = iced::window::Id::unique();
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_id = Some(editor_id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);

        // A close request for some other window must not touch the
        // editor session (those windows auto-close on their own).
        let _ = update(&mut app, Message::WindowCloseRequested(other_id));
        assert!(app.editor.is_some());
        assert!(app.editor.as_ref().unwrap().discard_pending_at.is_none());
    }

    #[test]
    fn editor_discard_treats_empty_pending_text_as_clean() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "   ".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDiscardRequested);

        assert!(app.editor.is_none());
    }

    #[test]
    fn editor_discard_treats_unchanged_text_edit_as_clean() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Text {
            content: "same text".into(),
            origin: PointLike::new(8.0, 9.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        ed.mark_output_clean();
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: " same text ".into(),
            edit_index: Some(0),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDiscardRequested);

        assert!(app.editor.is_none());
    }

    #[test]
    fn editor_discard_requires_confirmation_for_changed_text_edit() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Text {
            content: "old text".into(),
            origin: PointLike::new(8.0, 9.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        });
        ed.mark_output_clean();
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "new text".into(),
            edit_index: Some(0),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDiscardRequested);

        let ed = app.editor.as_ref().expect("first discard should only arm");
        assert!(ed.discard_pending_at.is_some());
        assert_eq!(
            ed.status.as_deref(),
            Some("Unsaved edits. Press Discard again to close without saving.")
        );
    }

    #[test]
    fn editor_discard_requires_confirmation_for_selected_frame() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.set_beautify(BuiltinPreset::Soft.settings());
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDiscardRequested);

        let ed = app.editor.as_ref().expect("first discard should only arm");
        assert!(ed.discard_pending_at.is_some());
        assert_eq!(
            ed.status.as_deref(),
            Some("Unsaved edits. Press Discard again to close without saving.")
        );
    }

    #[test]
    fn editor_discard_confirmation_resets_after_frame_change() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDiscardRequested);
        assert!(app.editor.as_ref().unwrap().discard_pending_at.is_some());

        let _ = update(&mut app, Message::EditorBeautifyPreset(BuiltinPreset::Soft));
        assert!(app.editor.as_ref().unwrap().discard_pending_at.is_none());

        let _ = update(&mut app, Message::EditorDiscardRequested);

        let ed = app.editor.as_ref().expect("discard should re-arm");
        assert!(ed.discard_pending_at.is_some());
    }

    #[test]
    fn editor_discard_confirmation_resets_after_annotation_change() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorDiscardRequested);
        assert!(app.editor.as_ref().unwrap().discard_pending_at.is_some());

        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                Annotation::Rectangle {
                    rect: RectLike::new(30.0, 30.0, 12.0, 12.0),
                    color: Rgba::OPAQUE_BLACK,
                    line_width: 2.0,
                    style: Default::default(),
                },
            )),
        );
        assert!(app.editor.as_ref().unwrap().discard_pending_at.is_none());

        let _ = update(&mut app, Message::EditorDiscardRequested);

        let ed = app.editor.as_ref().expect("discard should re-arm");
        assert!(ed.discard_pending_at.is_some());
        assert_eq!(ed.model.annotations().len(), 2);
    }

    #[test]
    fn editor_text_commit_label_matches_add_or_edit_state() {
        assert_eq!(editor_text_commit_label(false), "Add");
        assert_eq!(editor_text_commit_label(true), "Update");
    }

    #[test]
    fn editor_busy_ignores_export_reentry_without_committing_pending_text() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.busy = true;
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "draft".into(),
            edit_index: None,
        });
        app.editor = Some(ed);

        let _ = update(&mut app, Message::EditorSaveRequested);
        let _ = update(&mut app, Message::EditorCopyImageRequested);
        let _ = update(&mut app, Message::EditorCopyTextRequested);

        let ed = app.editor.as_ref().unwrap();
        assert!(ed.busy);
        assert!(ed.pending_text.is_some());
        assert!(ed.model.annotations().is_empty());
    }

    #[test]
    fn editor_busy_ignores_canvas_toolbar_and_zoom_mutations() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        ed.busy = true;
        app.editor = Some(ed);

        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Arrow,
            )),
        );
        let _ = update(&mut app, Message::EditorWidthBump(3.0));
        let _ = update(&mut app, Message::EditorZoomIn);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                Annotation::Rectangle {
                    rect: RectLike::new(30.0, 30.0, 10.0, 10.0),
                    color: Rgba::OPAQUE_BLACK,
                    line_width: 4.0,
                    style: Default::default(),
                },
            )),
        );

        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.model.active_tool(),
            readshot_ui::editor::ToolState::Select
        );
        assert_eq!(ed.model.annotations().len(), 1);
        assert_eq!(ed.zoom, crate::editor::EditorZoom::Fit);
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
            other => panic!("expected rectangle, got {other:?}"),
        }
    }

    #[test]
    fn editor_keyboard_zoom_is_display_scale_aware() {
        // ⌘+/⌘- must follow the same display-scale-aware ladder as the
        // toolbar zoom buttons; on a HiDPI (2.0) display the hardcoded
        // 1.0 path used to diverge from the buttons.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let ed = crate::editor::EditorSession::new_with_display_scale(solid(64, 64), 2.0);
        app.editor = Some(ed);

        // In Fit mode both start from the scale the stage rendered at.
        app.editor.as_ref().unwrap().render_scale.set(0.5);
        let _ = update(&mut app, Message::EditorZoomIn);
        let after_in = app.editor.as_ref().unwrap().zoom;
        assert_eq!(
            after_in,
            crate::editor::EditorZoom::Fit.zoom_in_from_display_scale(0.5),
            "keyboard zoom-in should match the display-scale-aware button path"
        );
        assert_ne!(
            after_in,
            crate::editor::EditorZoom::Fit.zoom_in(),
            "keyboard zoom-in must not use the hardcoded 1.0 ladder on HiDPI"
        );

        let _ = update(&mut app, Message::EditorZoomActual);
        let _ = update(&mut app, Message::EditorZoomOut);
        let after_out = app.editor.as_ref().unwrap().zoom;
        let expected_out =
            crate::editor::EditorZoom::actual_size(2.0).zoom_out_from_display_scale(2.0);
        assert_eq!(
            after_out, expected_out,
            "keyboard zoom-out should match the display-scale-aware button path"
        );
    }

    #[test]
    fn editor_key_events_ignore_non_editor_windows() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let editor_id = iced::window::Id::unique();
        let other_id = iced::window::Id::unique();
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_id = Some(editor_id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        app.editor = Some(ed);

        let delete = iced::keyboard::Key::Named(iced::keyboard::key::Named::Delete);
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: other_id,
                key: delete.clone(),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: true,
            },
        );
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);

        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: editor_id,
                key: delete,
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: true,
            },
        );
        assert!(app.editor.as_ref().unwrap().model.annotations().is_empty());
    }

    #[test]
    fn escape_cancels_live_canvas_drag_before_discarding() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let editor_id = iced::window::Id::unique();
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_id = Some(editor_id);
        // A committed annotation makes the editor "dirty" so the eventual
        // discard is the two-stage confirm (keeps the window open) rather
        // than an immediate clean close — lets us assert on it afterward.
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        // A shape drag is in flight (canvas published DragStarted).
        ed.canvas_drag_active = true;
        let seq_before = ed.drag_cancel_seq;
        app.editor = Some(ed);

        let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: editor_id,
                key: escape.clone(),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: true,
            },
        );

        // Escape cancelled the drag (seq bumped, flag cleared) and did NOT
        // discard the editor.
        let ed = app.editor.as_ref().expect("editor still open after cancel");
        assert!(!ed.canvas_drag_active);
        assert_eq!(ed.drag_cancel_seq, seq_before.wrapping_add(1));

        // A second Escape with no drag in progress falls through to the
        // discard shortcut (two-stage: first arms the confirmation).
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: editor_id,
                key: escape,
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: true,
            },
        );
        let ed = app
            .editor
            .as_ref()
            .expect("dirty editor arms discard, stays open");
        // The discard path doesn't touch the drag-cancel sequence.
        assert_eq!(ed.drag_cancel_seq, seq_before.wrapping_add(1));
        assert!(
            ed.discard_pending_at.is_some(),
            "second Escape should arm the discard confirmation"
        );
    }

    #[test]
    fn editor_advertised_copy_shortcuts_map_to_copy_messages() {
        let cmd = command_modifiers();
        let copy_image = editor_key_message(iced::keyboard::Key::Character("c".into()), cmd, true);

        assert!(matches!(
            copy_image,
            Some(Message::EditorCopyImageRequested)
        ));

        let mut cmd_shift = cmd;
        cmd_shift.insert(iced::keyboard::Modifiers::SHIFT);
        let copy_text =
            editor_key_message(iced::keyboard::Key::Character("c".into()), cmd_shift, true);

        assert!(matches!(copy_text, Some(Message::EditorCopyTextRequested)));
    }

    #[test]
    fn editor_copy_shortcuts_do_not_hijack_captured_text_input_events() {
        let cmd = command_modifiers();
        let copy_image = editor_key_message(iced::keyboard::Key::Character("c".into()), cmd, false);

        assert!(copy_image.is_none());
    }

    #[test]
    fn editor_advertised_pin_shortcut_maps_to_pin_message() {
        let pin = editor_key_message(
            iced::keyboard::Key::Character("p".into()),
            command_modifiers(),
            true,
        );

        assert!(matches!(pin, Some(Message::EditorPinRequested)));
    }

    #[test]
    fn editor_advertised_tool_shortcuts_map_to_their_tools() {
        for tool in readshot_ui::editor::toolbar::TOOL_ORDER {
            let (_label, key) = tool_label_and_key(tool);
            assert_eq!(tool_for_key(key), Some(tool), "shortcut drift for {tool:?}");
            let message = editor_key_message(
                iced::keyboard::Key::Character(key.to_string().into()),
                iced::keyboard::Modifiers::empty(),
                true,
            );
            assert!(matches!(
                message,
                Some(Message::EditorToolbar(
                    readshot_ui::ToolbarMessage::SelectTool(mapped)
                )) if mapped == tool
            ));
        }
    }

    #[test]
    fn e_selects_the_stamp_tool() {
        let msg = editor_key_message(
            iced::keyboard::Key::Character("e".into()),
            iced::keyboard::Modifiers::empty(),
            true,
        );
        assert!(matches!(
            msg,
            Some(Message::EditorToolbar(
                readshot_ui::ToolbarMessage::SelectTool(readshot_ui::editor::ToolState::Stamp)
            ))
        ));
    }

    #[test]
    fn selecting_a_new_tool_marks_it_seen() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Spotlight,
            )),
        );
        assert_eq!(
            app.preferences.editor_tools_seen,
            vec!["spotlight".to_string()]
        );
    }

    #[test]
    fn selecting_an_old_tool_records_nothing() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        for tool in [
            readshot_ui::editor::ToolState::Rectangle,
            readshot_ui::editor::ToolState::Arrow,
            readshot_ui::editor::ToolState::Text,
        ] {
            let _ = update(
                &mut app,
                Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(tool)),
            );
        }
        assert!(app.preferences.editor_tools_seen.is_empty());
    }

    #[test]
    fn spotlight_drag_commits_an_undoable_spotlight() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                Annotation::Spotlight {
                    rect: RectLike::new(8.0, 8.0, 20.0, 20.0),
                    shape: readshot_core::SpotlightShape::Rectangle,
                    dim: 0.55,
                },
            )),
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.model.can_undo());
        assert_eq!(ed.model.selected_kind_label(), Some("Spotlight"));
    }

    #[test]
    fn editor_existing_command_shortcuts_stay_mapped() {
        let cmd = command_modifiers();
        let undo = editor_key_message(iced::keyboard::Key::Character("z".into()), cmd, false);
        assert!(matches!(
            undo,
            Some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo))
        ));

        let mut cmd_shift = cmd;
        cmd_shift.insert(iced::keyboard::Modifiers::SHIFT);
        let redo = editor_key_message(iced::keyboard::Key::Character("z".into()), cmd_shift, false);
        assert!(matches!(
            redo,
            Some(Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo))
        ));

        let save = editor_key_message(iced::keyboard::Key::Character("s".into()), cmd, false);
        assert!(matches!(save, Some(Message::EditorSaveRequested)));

        let mut cmd_shift = cmd;
        cmd_shift.insert(iced::keyboard::Modifiers::SHIFT);
        let zoom_in =
            editor_key_message(iced::keyboard::Key::Character("=".into()), cmd_shift, false);
        assert!(matches!(zoom_in, Some(Message::EditorZoomIn)));

        let close = editor_key_message(iced::keyboard::Key::Character("w".into()), cmd, false);
        assert!(matches!(close, Some(Message::EditorDiscardRequested)));
    }

    #[test]
    fn editor_arrow_keys_nudge_selection_with_shift_coarsening() {
        use iced::keyboard::{key::Named, Key, Modifiers};

        fn nudge(key: Named, mods: Modifiers, status_ignored: bool) -> (f32, f32) {
            match editor_key_message(Key::Named(key), mods, status_ignored) {
                Some(Message::EditorNudgeSelected(dx, dy)) => (dx, dy),
                other => panic!("expected nudge, got {other:?}"),
            }
        }

        let none = Modifiers::empty();
        assert_eq!(nudge(Named::ArrowLeft, none, true), (-1.0, 0.0));
        assert_eq!(nudge(Named::ArrowRight, none, true), (1.0, 0.0));
        assert_eq!(nudge(Named::ArrowUp, none, true), (0.0, -1.0));
        assert_eq!(nudge(Named::ArrowDown, none, true), (0.0, 1.0));
        // Shift coarsens the step to 10px for fast positioning.
        assert_eq!(nudge(Named::ArrowDown, Modifiers::SHIFT, true), (0.0, 10.0));
        // While typing in a text field (status not ignored) arrows do nothing.
        assert!(editor_key_message(Key::Named(Named::ArrowLeft), none, false).is_none());
    }

    #[test]
    fn editor_escape_maps_to_discard_when_not_handled_by_canvas() {
        let discard = editor_key_message(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            iced::keyboard::Modifiers::empty(),
            true,
        );

        assert!(matches!(discard, Some(Message::EditorDiscardRequested)));
    }

    #[test]
    fn editor_escape_keeps_canvas_cancellation_when_already_handled() {
        // The annotation canvas captures every Esc, so this is the path a
        // real Esc press takes: one EditorTextCancel per press.
        let cancel = editor_key_message(
            iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
            iced::keyboard::Modifiers::empty(),
            false,
        );

        assert!(matches!(cancel, Some(Message::EditorTextCancel)));
    }

    #[test]
    fn scroll_keys_map_stop_and_discard() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        let esc = Key::Named(Named::Escape);
        // Esc alone stops & stitches (same as the Stop button).
        assert!(matches!(
            scroll_key_message(&esc, Modifiers::empty()),
            Some(Message::ScrollCaptureStopRequested)
        ));
        // Shift+Esc and ⌘. discard (same as the Cancel button, which
        // arms then confirms on repeat).
        assert!(matches!(
            scroll_key_message(&esc, Modifiers::SHIFT),
            Some(Message::ScrollCaptureCancelRequested)
        ));
        let dot = Key::Character(".".into());
        assert!(matches!(
            scroll_key_message(&dot, command_modifiers()),
            Some(Message::ScrollCaptureCancelRequested)
        ));
        assert!(scroll_key_message(&dot, Modifiers::empty()).is_none());
    }

    #[test]
    fn welcome_enter_triggers_card_primary_action() {
        use crate::welcome::WelcomeState;
        use iced::keyboard::{key::Named, Key, Modifiers};
        let enter = Key::Named(Named::Enter);
        assert!(matches!(
            welcome_key_message(WelcomeState::Pending, false, &enter, Modifiers::empty()),
            Some(Message::GrantPermissionRequested)
        ));
        assert!(matches!(
            welcome_key_message(WelcomeState::Denied, false, &enter, Modifiers::empty()),
            Some(Message::OpenPermissionSettingsRequested)
        ));
        assert!(matches!(
            welcome_key_message(WelcomeState::Granted, false, &enter, Modifiers::empty()),
            Some(Message::OpenOverlayRequested)
        ));
        // Capture is disabled (so Enter is a no-op) while one is running.
        assert!(
            welcome_key_message(WelcomeState::Granted, true, &enter, Modifiers::empty()).is_none()
        );
        // A chord is not the bare-Enter primary activation.
        assert!(
            welcome_key_message(WelcomeState::Pending, false, &enter, command_modifiers())
                .is_none()
        );
    }

    #[test]
    fn editor_backspace_and_forward_delete_both_remove_selection() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        for named in [Named::Backspace, Named::Delete] {
            let m = editor_key_message(Key::Named(named), Modifiers::empty(), true);
            assert!(
                matches!(m, Some(Message::EditorDeleteSelected)),
                "{named:?}"
            );
            // While a text field is editing (event captured) neither fires.
            assert!(editor_key_message(Key::Named(named), Modifiers::empty(), false).is_none());
        }
    }

    #[test]
    fn history_copy_keys_match_overlay_and_editor() {
        use iced::keyboard::{Key, Modifiers};
        let w = iced::window::Id::unique();
        let c = Key::Character("c".into());
        let cmd_shift = command_modifiers() | Modifiers::SHIFT;
        assert!(matches!(
            history_key_message(w, &c, command_modifiers(), true),
            Some(Message::HistoryKeyboardShortcut(
                _,
                HistoryKeyboardAction::CopyImage
            ))
        ));
        assert!(matches!(
            history_key_message(w, &c, cmd_shift, true),
            Some(Message::HistoryKeyboardShortcut(
                _,
                HistoryKeyboardAction::CopyText
            ))
        ));
        // A focused search field captured it: deferred to the handler,
        // which only applies it while the query is empty.
        assert!(matches!(
            history_key_message(w, &c, command_modifiers(), false),
            Some(Message::HistoryCapturedShortcut(
                _,
                HistoryKeyboardAction::CopyImage
            ))
        ));
        assert!(matches!(
            history_key_message(w, &c, cmd_shift, false),
            Some(Message::HistoryCapturedShortcut(
                _,
                HistoryKeyboardAction::CopyText
            ))
        ));
    }

    #[test]
    fn history_delete_keys() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        let w = iced::window::Id::unique();
        let is_delete = |m: Option<Message>| {
            matches!(
                m,
                Some(Message::HistoryKeyboardShortcut(
                    _,
                    HistoryKeyboardAction::Delete
                ))
            )
        };
        let bs = Key::Named(Named::Backspace);
        let del = Key::Named(Named::Delete);
        // ⌘⌫ and ⌦ delete.
        assert!(is_delete(history_key_message(
            w,
            &bs,
            command_modifiers(),
            true
        )));
        assert!(is_delete(history_key_message(
            w,
            &del,
            Modifiers::empty(),
            true
        )));
        // Plain ⌫ never deletes a capture.
        assert!(history_key_message(w, &bs, Modifiers::empty(), true).is_none());
        assert!(history_key_message(w, &bs, Modifiers::empty(), false).is_none());
        // Captured by the search field: deferred, never immediate.
        assert!(matches!(
            history_key_message(w, &bs, command_modifiers(), false),
            Some(Message::HistoryCapturedShortcut(
                _,
                HistoryKeyboardAction::Delete
            ))
        ));
        assert!(matches!(
            history_key_message(w, &del, Modifiers::empty(), false),
            Some(Message::HistoryCapturedShortcut(
                _,
                HistoryKeyboardAction::Delete
            ))
        ));
    }

    #[test]
    fn history_captured_shortcut_applies_only_for_empty_search() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.history_window_id = Some(id);
        let captured = |a| Message::HistoryCapturedShortcut(id, a);

        // Non-empty query: the field owns the chord, nothing happens.
        app.history_search = "invoice".into();
        let _ = update(&mut app, captured(HistoryKeyboardAction::CopyImage));
        assert_eq!(app.history_status, None);

        // Empty query: it reaches History (no selection -> status).
        app.history_search.clear();
        let _ = update(&mut app, captured(HistoryKeyboardAction::CopyImage));
        assert_eq!(
            app.history_status.as_deref(),
            Some("No history capture selected.")
        );
    }

    #[test]
    fn history_second_delete_key_confirms() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.history_window_id = Some(id);
        let rec = readshot_core::Uuid::new_v4();
        app.history_selected_id = Some(rec);
        let del = Message::HistoryKeyboardShortcut(id, HistoryKeyboardAction::Delete);
        let _ = update(&mut app, del.clone());
        assert_eq!(app.history_delete_pending, Some(rec));
        // Second press goes down the confirm path (pending is consumed).
        let _ = update(&mut app, del);
        assert_ne!(app.history_delete_pending, Some(rec));
    }

    #[test]
    fn history_arrow_while_search_focused_returns_nav_message() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        let w = iced::window::Id::unique();
        assert!(matches!(
            history_key_message(w, &Key::Named(Named::ArrowDown), Modifiers::empty(), false),
            Some(Message::HistoryKeyboardShortcut(
                _,
                HistoryKeyboardAction::Next
            ))
        ));
        // The handler pairs the move with an unfocus task for the field.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.history_window_id = Some(w);
        let task = update(
            &mut app,
            Message::HistoryKeyboardShortcut(w, HistoryKeyboardAction::Next),
        );
        assert!(task.units() >= 1);
    }

    #[test]
    fn pin_keys_map_to_actions() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        assert_eq!(
            pin_key_action(&Key::Named(Named::Escape), Modifiers::empty()),
            Some(PinKeyAction::Close)
        );
        assert_eq!(
            pin_key_action(&Key::Character("w".into()), command_modifiers()),
            Some(PinKeyAction::Close)
        );
        assert_eq!(
            pin_key_action(&Key::Character("l".into()), Modifiers::empty()),
            Some(PinKeyAction::ToggleLock)
        );
        assert_eq!(
            pin_key_action(&Key::Character("=".into()), Modifiers::empty()),
            Some(PinKeyAction::OpacityUp)
        );
        assert_eq!(
            pin_key_action(&Key::Character("-".into()), Modifiers::empty()),
            Some(PinKeyAction::OpacityDown)
        );
        // Arrows move the window: 1 px, Shift → 10 px (same as the
        // overlay and editor nudges).
        assert_eq!(
            pin_key_action(&Key::Named(Named::ArrowLeft), Modifiers::empty()),
            Some(PinKeyAction::Nudge(-1.0, 0.0))
        );
        assert_eq!(
            pin_key_action(&Key::Named(Named::ArrowDown), Modifiers::SHIFT),
            Some(PinKeyAction::Nudge(0.0, 10.0))
        );
        // An unmapped plain key is ignored.
        assert_eq!(
            pin_key_action(&Key::Character("q".into()), Modifiers::empty()),
            None
        );
    }

    #[test]
    fn hud_recording_pulse_is_constant_under_reduce_motion() {
        // Reduce-motion: constant full alpha at any elapsed time.
        for t in [0.0_f32, 0.35, 0.7, 1.4, 3.3] {
            assert_eq!(hud_recording_pulse(t, true), 1.0);
        }
        // Motion allowed: breathes inside ~0.9..=1.0, never unreadable.
        for t in [0.0_f32, 0.35, 0.7, 1.4, 3.3] {
            let a = hud_recording_pulse(t, false);
            assert!(
                (0.89..=1.01).contains(&a),
                "pulse {a} out of range at t={t}"
            );
        }
    }

    #[test]
    fn settings_keys_map_to_actions() {
        use iced::keyboard::{key::Named, Key, Modifiers};
        let cmd = command_modifiers();
        assert_eq!(
            settings_key_action(&Key::Named(Named::Escape), Modifiers::empty()),
            Some(SettingsKeyAction::Close)
        );
        assert_eq!(
            settings_key_action(&Key::Character("w".into()), cmd),
            Some(SettingsKeyAction::Close)
        );
        assert_eq!(
            settings_key_action(&Key::Character("l".into()), cmd),
            Some(SettingsKeyAction::ToggleLaunchAtLogin)
        );
        assert_eq!(
            settings_key_action(&Key::Character("d".into()), cmd),
            Some(SettingsKeyAction::ToggleDebugLogging)
        );
        assert_eq!(
            settings_key_action(&Key::Character("r".into()), cmd),
            Some(SettingsKeyAction::CycleRetention)
        );
        assert_eq!(
            settings_key_action(&Key::Character("e".into()), cmd),
            Some(SettingsKeyAction::CycleEngine)
        );
        assert_eq!(
            settings_key_action(&Key::Character("f".into()), cmd),
            Some(SettingsKeyAction::CycleFormat)
        );
        // Destructive commands need ⇧⌘ and only arm; the second press
        // (resolved in the runtime) confirms, so a single press can't wipe.
        let mut cmd_shift = cmd;
        cmd_shift.insert(Modifiers::SHIFT);
        assert_eq!(
            settings_key_action(&Key::Character("r".into()), cmd_shift),
            Some(SettingsKeyAction::ResetAll)
        );
        assert_eq!(
            settings_key_action(&Key::Character("u".into()), cmd_shift),
            Some(SettingsKeyAction::Uninstall)
        );
        // A bare letter (no ⌘) never changes a setting — it must reach
        // the filename text_input instead.
        assert_eq!(
            settings_key_action(&Key::Character("l".into()), Modifiers::empty()),
            None
        );
    }

    #[test]
    fn command_digits_select_settings_panes() {
        use iced::keyboard::{Key, Modifiers};
        let k = |c: &str| Key::Character(c.into());
        assert!(matches!(
            settings_key_action(&k("1"), Modifiers::COMMAND),
            Some(SettingsKeyAction::SelectPane(SettingsPane::Permissions))
        ));
        assert!(matches!(
            settings_key_action(&k("7"), Modifiers::COMMAND),
            Some(SettingsKeyAction::SelectPane(SettingsPane::App))
        ));
        assert!(settings_key_action(&k("8"), Modifiers::COMMAND).is_none());
        // A bare digit is typing (e.g. in the filename field), not a pane switch.
        assert!(settings_key_action(&k("3"), Modifiers::empty()).is_none());
    }

    #[test]
    fn pane_labels_follow_the_spec_order() {
        let labels: Vec<_> = SettingsPane::ALL.iter().map(|p| p.label()).collect();
        assert_eq!(
            labels,
            [
                "Permissions",
                "Capture",
                "Files",
                "Text Recognition",
                "Export",
                "Diagnostics",
                "App"
            ]
        );
    }

    #[test]
    fn selecting_a_pane_is_remembered() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let _ = update(
            &mut app,
            Message::SettingsPaneSelected(SettingsPane::Export),
        );
        assert_eq!(app.settings_pane, SettingsPane::Export);
    }

    /// Open-settings app plus a helper that presses `key` with ⌘ (and ⇧
    /// when `shift`) in the settings window.
    fn settings_app() -> (App, iced::window::Id) {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.settings_window_id = Some(id);
        (app, id)
    }

    fn press_settings_key(app: &mut App, id: iced::window::Id, key: &str, shift: bool) {
        let mut modifiers = command_modifiers();
        if shift {
            modifiers.insert(iced::keyboard::Modifiers::SHIFT);
        }
        let _ = update(
            app,
            Message::SettingsKeyPressed {
                window: id,
                key: iced::keyboard::Key::Character(key.into()),
                modifiers,
            },
        );
    }

    #[test]
    fn destructive_shortcut_from_another_pane_arms_and_shows_app() {
        for (key, armed) in [
            (
                "r",
                (|a: &App| a.settings_reset_all_pending) as fn(&App) -> bool,
            ),
            ("u", |a: &App| a.settings_uninstall_pending),
        ] {
            let (mut app, id) = settings_app();
            app.settings_pane = SettingsPane::Permissions;
            press_settings_key(&mut app, id, key, true);
            assert_eq!(app.settings_pane, SettingsPane::App, "⇧⌘{key}");
            assert!(armed(&app), "⇧⌘{key} arms");
        }
    }

    #[test]
    fn reset_confirm_from_another_pane_only_shows_the_prompt() {
        let (mut app, id) = settings_app();
        app.preferences.debug_logging = true;
        app.settings_reset_all_pending = true;
        app.settings_pane = SettingsPane::Capture;
        press_settings_key(&mut app, id, "r", true);
        assert_eq!(app.settings_pane, SettingsPane::App);
        assert!(app.settings_reset_all_pending, "still armed, not confirmed");
        assert!(app.preferences.debug_logging, "preferences not wiped");
    }

    #[test]
    fn reset_confirm_on_app_pane_still_resets() {
        let (mut app, id) = settings_app();
        app.preferences.debug_logging = true;
        app.settings_reset_all_pending = true;
        app.settings_pane = SettingsPane::App;
        press_settings_key(&mut app, id, "r", true);
        assert!(!app.settings_reset_all_pending);
        assert_eq!(
            app.preferences.debug_logging,
            Preferences::default().debug_logging
        );
    }

    #[test]
    fn uninstall_confirm_from_another_pane_only_shows_the_prompt() {
        let (mut app, id) = settings_app();
        app.settings_uninstall_pending = true;
        app.settings_pane = SettingsPane::Files;
        press_settings_key(&mut app, id, "u", true);
        assert_eq!(app.settings_pane, SettingsPane::App);
        assert!(app.settings_uninstall_pending, "still armed, not confirmed");
    }

    #[test]
    fn setting_shortcut_switches_to_its_pane() {
        let (mut app, id) = settings_app();
        app.settings_pane = SettingsPane::Permissions;
        let before = app.preferences.debug_logging;
        press_settings_key(&mut app, id, "d", false);
        assert_eq!(app.settings_pane, SettingsPane::Diagnostics);
        assert_ne!(app.preferences.debug_logging, before);
    }

    #[test]
    fn leaving_capture_pane_cancels_hotkey_recording() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.settings_pane = SettingsPane::Capture;
        app.settings_recording_hotkey = true;
        let _ = update(
            &mut app,
            Message::SettingsPaneSelected(SettingsPane::Capture),
        );
        assert!(app.settings_recording_hotkey, "staying on Capture keeps it");
        let _ = update(&mut app, Message::SettingsPaneSelected(SettingsPane::Files));
        assert!(!app.settings_recording_hotkey);
        assert_eq!(app.settings_pane, SettingsPane::Files);
    }

    #[test]
    fn command_digit_in_settings_window_switches_pane() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let id = iced::window::Id::unique();
        app.settings_window_id = Some(id);
        let _ = update(
            &mut app,
            Message::SettingsKeyPressed {
                window: id,
                key: iced::keyboard::Key::Character("3".into()),
                modifiers: command_modifiers(),
            },
        );
        assert_eq!(app.settings_pane, SettingsPane::Files);
        // Another window's key press is ignored.
        let _ = update(
            &mut app,
            Message::SettingsKeyPressed {
                window: iced::window::Id::unique(),
                key: iced::keyboard::Key::Character("7".into()),
                modifiers: command_modifiers(),
            },
        );
        assert_eq!(app.settings_pane, SettingsPane::Files);
    }

    #[test]
    fn style_message_restyles_the_selected_rectangle_as_one_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        let depth = ed.model.undo_depth();
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorStyle(readshot_ui::editor::StyleMessage::Fill(
                readshot_core::FillMode::Filled,
            )),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(matches!(
            &ed.model.annotations()[0],
            Annotation::Rectangle { style, .. } if style.fill == readshot_core::FillMode::Filled
        ));
        assert_eq!(ed.model.tool_style().fill, readshot_core::FillMode::Filled);
    }

    fn editor_with_selected_rect() -> crate::editor::EditorSession {
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        ed
    }

    #[test]
    fn output_commits_the_text_draft_after_dropping_a_lingering_width_preview() {
        for request in [
            Message::EditorCopyImageRequested,
            Message::EditorSaveRequested,
            Message::EditorCopyTextRequested,
        ] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            app.editor = Some(editor_with_selected_rect());
            let _ = update(&mut app, Message::EditorLineWidthPreview(10.0));
            app.editor.as_mut().unwrap().pending_text = Some(crate::editor::PendingText {
                origin: PointLike::new(50.0, 50.0),
                content: "typed".into(),
                edit_index: None,
            });
            let _ = update(&mut app, request.clone());
            let ed = app.editor.as_ref().unwrap();
            assert!(
                ed.model
                    .annotations()
                    .iter()
                    .any(|a| matches!(a, Annotation::Text { content, .. } if content == "typed")),
                "{request:?} dropped the committed text"
            );
            assert!(ed.width_drag_baseline.is_none());
        }
    }

    #[test]
    fn output_keeps_a_lingering_style_preview_as_one_undo_step() {
        use readshot_ui::editor::StyleMessage;
        for request in [
            Message::EditorCopyImageRequested,
            Message::EditorSaveRequested,
        ] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            app.editor = Some(editor_with_selected_rect());
            let depth = app.editor.as_ref().unwrap().model.undo_depth();
            // Arrow keys on the slider: previews, never a release.
            let _ = update(
                &mut app,
                Message::EditorStylePreview(StyleMessage::CornerRadius(12.0)),
            );
            let _ = update(&mut app, request.clone());
            let ed = app.editor.as_ref().unwrap();
            assert!(ed.style_drag_baseline.is_none());
            assert_eq!(ed.model.undo_depth(), depth + 1, "{request:?}");
            assert!(
                matches!(
                    &ed.model.annotations()[0],
                    Annotation::Rectangle { style, .. } if style.corner_radius == 12.0
                ),
                "{request:?}: the output must match the preview"
            );
        }
    }

    #[test]
    fn style_slider_preview_commits_once_and_esc_cancels_it() {
        use readshot_ui::editor::StyleMessage;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        let depth = ed.model.undo_depth();
        app.editor = Some(ed);
        for r in [4.0, 8.0, 12.0] {
            let _ = update(
                &mut app,
                Message::EditorStylePreview(StyleMessage::CornerRadius(r)),
            );
        }
        let _ = update(&mut app, Message::EditorStyleCommit);
        assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);

        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(20.0)),
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.style_drag_baseline.is_none());
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(matches!(
            &ed.model.annotations()[0],
            Annotation::Rectangle { style, .. } if style.corner_radius == 12.0
        ));
    }

    fn two_rects_first_selected() -> App {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        for x in [0.0, 30.0] {
            ed.model.commit_annotation(Annotation::Rectangle {
                rect: RectLike::new(x, x, 20.0, 20.0),
                color: Rgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: Default::default(),
            });
        }
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        app.editor = Some(ed);
        app
    }

    #[test]
    fn undo_during_a_held_style_drag_cancels_only_the_drag() {
        use readshot_ui::editor::StyleMessage;
        let mut app = two_rects_first_selected();
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let tick = |app: &mut App, r: f32| {
            let _ = update(
                app,
                Message::EditorStylePreview(StyleMessage::CornerRadius(r)),
            );
        };
        tick(&mut app, 6.0);
        // The mouse is down: this is a held slider drag.
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        tick(&mut app, 8.0);
        let _ = update(&mut app, Message::EditorStyleCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.model.annotations().len(),
            2,
            "the second rect is not undone"
        );
        assert_eq!(ed.model.undo_depth(), depth, "no history step");
        assert!(matches!(
            &ed.model.annotations()[0],
            Annotation::Rectangle { style, .. } if style.corner_radius == 0.0
        ));
        assert!(!ed.style_drag_cancelled);
        // The next drag works again.
        tick(&mut app, 10.0);
        let _ = update(&mut app, Message::EditorStyleCommit);
        assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
    }

    #[test]
    fn esc_during_a_held_style_drag_ignores_the_remaining_ticks() {
        use readshot_ui::editor::StyleMessage;
        let mut app = two_rects_first_selected();
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(6.0)),
        );
        // The mouse is down: this is a held slider drag.
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let _ = update(&mut app, Message::EditorTextCancel);
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(9.0)),
        );
        let _ = update(&mut app, Message::EditorStyleCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth);
        assert!(matches!(
            &ed.model.annotations()[0],
            Annotation::Rectangle { style, .. } if style.corner_radius == 0.0
        ));
    }

    #[test]
    fn a_hex_background_clears_a_stale_cancelled_drag_flag() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(16, 16));
        ed.snapshot_drag_cancelled = true;
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorBeautifyHexChanged("#102030".into()),
        );
        let _ = update(&mut app, Message::EditorBeautifyHexSubmitted);
        assert!(!app.editor.as_ref().unwrap().snapshot_drag_cancelled);
        app.editor.as_mut().unwrap().snapshot_drag_cancelled = true;
        let _ = update(&mut app, Message::EditorBeautifyPickBackground);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(
                2.0, 2.0,
            ))),
        );
        assert!(!app.editor.as_ref().unwrap().snapshot_drag_cancelled);
    }

    #[test]
    fn cancelled_slider_restores_the_tool_style_without_a_selection() {
        use readshot_ui::editor::StyleMessage;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(20.0)),
        );
        assert_eq!(
            app.editor
                .as_ref()
                .unwrap()
                .model
                .tool_style()
                .corner_radius,
            20.0
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.tool_style().corner_radius, 0.0);
        assert!(ed.style_drag_baseline.is_none());
        // The cancelled drag's release ends it; the next drag keeps its value.
        let _ = update(&mut app, Message::EditorStyleCommit);
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(8.0)),
        );
        let _ = update(&mut app, Message::EditorStyleCommit);
        assert_eq!(
            app.editor
                .as_ref()
                .unwrap()
                .model
                .tool_style()
                .corner_radius,
            8.0
        );
    }

    #[test]
    fn bracket_keys_step_selected_text_through_sizes() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(200, 200));
        ed.model.commit_annotation(Annotation::Text {
            content: "hi".into(),
            origin: PointLike::new(20.0, 60.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 24.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let size = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
            Annotation::Text { size, .. } => *size,
            other => panic!("{other:?}"),
        };
        let _ = update(&mut app, Message::EditorWidthBump(1.0));
        assert_eq!(size(&app), 36.0);
        let _ = update(&mut app, Message::EditorWidthBump(-1.0));
        let _ = update(&mut app, Message::EditorWidthBump(-1.0));
        assert_eq!(size(&app), 16.0);
    }

    #[test]
    fn new_text_uses_the_text_style() {
        use readshot_ui::editor::{StyleMessage, TextSize};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(200, 200)));
        let _ = update(
            &mut app,
            Message::EditorStyle(StyleMessage::TextSize(TextSize::L)),
        );
        let _ = update(&mut app, Message::EditorStyle(StyleMessage::TextPill(true)));
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::RequestText(PointLike::new(
                20.0, 60.0,
            ))),
        );
        let _ = update(&mut app, Message::EditorTextChanged("Hi".into()));
        let _ = update(&mut app, Message::EditorTextCommit);
        let ed = app.editor.as_ref().unwrap();
        assert!(matches!(
            ed.model.annotations().last(),
            Some(Annotation::Text { size, style, .. }) if *size == 36.0 && style.pill && !style.bold
        ));
    }

    fn app_with_text(size: f32) -> App {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(200, 200));
        ed.model.commit_annotation(Annotation::Text {
            content: "hi".into(),
            origin: PointLike::new(20.0, 60.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size,
            style: Default::default(),
        });
        app.editor = Some(ed);
        app
    }

    #[test]
    fn bracket_keys_step_legacy_text_relative_to_its_size() {
        let size = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
            Annotation::Text { size, .. } => *size,
            other => panic!("{other:?}"),
        };
        let depth = |app: &App| app.editor.as_ref().unwrap().model.undo_depth();
        for (start, delta, want) in [
            (96.0, 1.0, 96.0),
            (12.0, -1.0, 12.0),
            (20.0, 1.0, 24.0),
            (20.0, -1.0, 16.0),
            (30.0, -1.0, 24.0),
        ] {
            let mut app = app_with_text(start);
            let before = depth(&app);
            let _ = update(&mut app, Message::EditorWidthBump(delta));
            assert_eq!(size(&app), want, "{start} {delta}");
            let pushed = if want == start { 0 } else { 1 };
            assert_eq!(depth(&app), before + pushed, "{start} {delta}");
        }
    }

    #[test]
    fn bracket_keys_with_the_text_tool_step_the_text_size() {
        use readshot_ui::editor::{TextSize, ToolState};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.set_tool(ToolState::Text);
        let width = ed.model.current_line_width();
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorWidthBump(1.0));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.tool_style().text_size, TextSize::L);
        assert_eq!(ed.model.current_line_width(), width);
        let _ = update(&mut app, Message::EditorWidthBump(-1.0));
        let _ = update(&mut app, Message::EditorWidthBump(-1.0));
        let _ = update(&mut app, Message::EditorWidthBump(-1.0));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.tool_style().text_size, TextSize::S);
        assert_eq!(ed.model.current_line_width(), width);
    }

    #[test]
    fn bracket_keys_bump_a_selected_shape_from_its_own_width() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.set_line_width(3.0);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 8.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorWidthBump(1.0));
        assert!(matches!(
            &app.editor.as_ref().unwrap().model.annotations()[0],
            Annotation::Rectangle { line_width, .. } if *line_width == 9.0
        ));
    }

    #[test]
    fn esc_during_a_slider_drag_only_cancels_the_drag() {
        use readshot_ui::editor::StyleMessage;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 40.0, 40.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorStylePreview(StyleMessage::CornerRadius(20.0)),
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.style_drag_baseline.is_none());
        assert_eq!(ed.model.selected_annotation(), Some(0));
        assert!(matches!(
            &ed.model.annotations()[0],
            Annotation::Rectangle { style, .. } if style.corner_radius == 0.0
        ));
        // The next Esc clears the selection as before.
        let _ = update(&mut app, Message::EditorTextCancel);
        assert_eq!(
            app.editor.as_ref().unwrap().model.selected_annotation(),
            None
        );
    }

    #[test]
    fn i_toggles_the_eyedropper_but_not_while_typing() {
        let toggled = editor_key_message(
            iced::keyboard::Key::Character("i".into()),
            iced::keyboard::Modifiers::empty(),
            true,
        );
        assert!(matches!(toggled, Some(Message::EditorEyedropperToggled)));
        let upper = editor_key_message(
            iced::keyboard::Key::Character("I".into()),
            iced::keyboard::Modifiers::SHIFT,
            true,
        );
        assert!(matches!(upper, Some(Message::EditorEyedropperToggled)));
        // A focused text field captures the key (status not ignored):
        // tool letters and I do nothing while typing.
        for key in ["i", "s", "e", "v", "r"] {
            let typed = editor_key_message(
                iced::keyboard::Key::Character(key.into()),
                iced::keyboard::Modifiers::empty(),
                false,
            );
            assert!(typed.is_none(), "{key} leaked while typing");
        }
    }

    #[test]
    fn eyedropper_pick_samples_the_source_image_and_exits() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut img = solid(16, 16);
        img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
        let mut ed = crate::editor::EditorSession::new(img);
        // A red rectangle over the pixel must not affect the pick.
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 16.0, 16.0),
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            line_width: 2.0,
            style: readshot_core::ShapeStyle {
                fill: readshot_core::FillMode::Filled,
                ..Default::default()
            },
        });
        ed.model.clear_selection();
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(
                3.5, 4.5,
            ))),
        );
        let loupe = app
            .editor
            .as_ref()
            .unwrap()
            .eyedropper
            .as_ref()
            .unwrap()
            .loupe
            .clone()
            .expect("hover fills the loupe");
        let n = (readshot_ui::editor::LOUPE_RADIUS * 2 + 1) as usize;
        assert_eq!(loupe.pixels.len(), n * n);
        assert_eq!(
            loupe.pixels[n * n / 2].map(|c| c.to_hex()),
            Some("#4F46E5".to_string())
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(
                3.5, 4.5,
            ))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.eyedropper.is_none());
        assert_eq!(ed.model.current_color().to_hex(), "#4F46E5");
        assert_eq!(
            app.preferences.editor_recent_colors,
            vec!["#4F46E5".to_string()]
        );
    }

    #[test]
    fn eyedropper_pick_recolors_the_selection_as_one_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut img = solid(16, 16);
        img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
        let mut ed = crate::editor::EditorSession::new(img);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 16.0, 16.0),
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(
                3.5, 4.5,
            ))),
        );
        let color_of = |app: &App| match &app.editor.as_ref().unwrap().model.annotations()[0] {
            Annotation::Rectangle { color, .. } => color.to_hex(),
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(color_of(&app), "#4F46E5");
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        assert_eq!(color_of(&app), "#FF0000");
    }

    #[test]
    fn eyedropper_pick_of_a_preset_does_not_join_recent() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let preset = readshot_ui::editor::toolbar::PALETTE[0];
        let mut img = solid(16, 16);
        let to8 = |v: f32| (v * 255.0).round() as u8;
        img.put_pixel(
            3,
            4,
            image::Rgba([to8(preset.r), to8(preset.g), to8(preset.b), 255]),
        );
        app.editor = Some(crate::editor::EditorSession::new(img));
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(
                3.5, 4.5,
            ))),
        );
        assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
        assert!(app.preferences.editor_recent_colors.is_empty());
    }

    #[test]
    fn esc_closes_the_popover_then_the_eyedropper() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Width),
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert!(ed.eyedropper.is_some(), "first esc only closes the popover");
        let _ = update(&mut app, Message::EditorTextCancel);
        assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
    }

    #[test]
    fn esc_exits_the_eyedropper_before_clearing_the_selection() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(16, 16));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 8.0, 8.0),
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.eyedropper.is_none());
        assert_eq!(ed.model.selected_annotation(), Some(0));
    }

    #[test]
    fn eyedropper_button_closes_the_color_popover() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert!(ed.eyedropper.is_some());
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
    }

    #[test]
    fn choosing_a_tool_exits_the_eyedropper() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Rectangle,
            )),
        );
        assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
    }

    fn pick(app: &mut App, x: f32, y: f32) {
        let _ = update(app, Message::EditorEyedropperToggled);
        let _ = update(
            app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(
                x, y,
            ))),
        );
    }

    #[test]
    fn eyedropper_pick_matches_the_pixel_shown_after_a_fractional_crop() {
        let mut img = solid(32, 16);
        img.put_pixel(10, 3, image::Rgba([79, 70, 229, 255]));
        img.put_pixel(11, 4, image::Rgba([200, 10, 10, 255]));
        let crop = |x: f32, y: f32| Annotation::Crop {
            rect: RectLike::new(x, y, 12.0, 8.0),
        };
        // x0 = 10.6, y0 = 3.6: local (0.5, 0.5) → base (11.1, 4.1),
        // but blit_base shows base (10, 3) at displayed (0, 0).
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(img.clone());
        ed.model.commit_annotation(crop(10.6, 3.6));
        ed.model.clear_selection();
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(
                11.1, 4.1,
            ))),
        );
        let loupe = app
            .editor
            .as_ref()
            .unwrap()
            .eyedropper
            .as_ref()
            .unwrap()
            .loupe
            .clone()
            .unwrap();
        let n = (readshot_ui::editor::LOUPE_RADIUS * 2 + 1) as usize;
        assert_eq!(
            loupe.pixels[n * n / 2].map(|c| c.to_hex()),
            Some("#4F46E5".to_string())
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperPick(PointLike::new(
                11.1, 4.1,
            ))),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().model.current_color().to_hex(),
            "#4F46E5"
        );

        // Whole-pixel crop: no shift.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(img);
        ed.model.commit_annotation(crop(10.0, 3.0));
        ed.model.clear_selection();
        app.editor = Some(ed);
        pick(&mut app, 11.5, 4.5);
        assert_eq!(
            app.editor.as_ref().unwrap().model.current_color().to_hex(),
            "#C80A0A"
        );
    }

    #[test]
    fn zoom_clears_the_eyedropper_loupe() {
        for zoom in [
            Message::EditorZoomIn,
            Message::EditorZoomOut,
            Message::EditorZoomActual,
            Message::EditorZoomFit,
        ] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
            let _ = update(&mut app, Message::EditorEyedropperToggled);
            let _ = update(
                &mut app,
                Message::EditorCanvas(readshot_ui::CanvasMessage::EyedropperHover(PointLike::new(
                    3.5, 4.5,
                ))),
            );
            let label = format!("{zoom:?}");
            let _ = update(&mut app, zoom);
            let eye = app
                .editor
                .as_ref()
                .unwrap()
                .eyedropper
                .as_ref()
                .expect("still on");
            assert!(eye.loupe.is_none(), "{label} kept a stale loupe");
        }
    }

    #[test]
    fn eyedropper_pick_on_a_selection_keeps_the_undo_hint() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut img = solid(16, 16);
        img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
        let mut ed = crate::editor::EditorSession::new(img);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 16.0, 16.0),
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        pick(&mut app, 3.5, 4.5);
        assert_eq!(
            app.editor.as_ref().unwrap().status.as_deref(),
            Some("Picked #4F46E5. ⌘Z to undo.")
        );
        // Without a selection there is nothing to undo.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut img = solid(16, 16);
        img.put_pixel(3, 4, image::Rgba([79, 70, 229, 255]));
        app.editor = Some(crate::editor::EditorSession::new(img));
        pick(&mut app, 3.5, 4.5);
        assert_eq!(
            app.editor.as_ref().unwrap().status.as_deref(),
            Some("Picked #4F46E5.")
        );
    }

    #[test]
    fn rotate_flip_and_resize_wait_for_a_held_canvas_drag() {
        for held_move in [false, true] {
            for msg in [
                Message::EditorRotate { clockwise: true },
                Message::EditorFlip(readshot_core::document::FlipAxis::Horizontal),
                Message::EditorResizeOpen,
            ] {
                let mut app = build_app(Arc::new(FakePermissions::granted()));
                let mut ed = crate::editor::EditorSession::new(solid(16, 8));
                if held_move {
                    ed.move_drag = Some(crate::editor::MoveDrag {
                        baseline: Vec::new(),
                        selected_index: 0,
                        start: PointLike::new(2.0, 2.0),
                        moved: true,
                        kind: crate::editor::MoveDragKind::Move,
                    });
                } else {
                    ed.canvas_drag_active = true;
                }
                let depth = ed.model.undo_depth();
                app.editor = Some(ed);
                let _ = update(&mut app, msg.clone());
                let ed = app.editor.as_ref().unwrap();
                assert!(ed.model.geometry().is_original(), "{msg:?}");
                assert_eq!(ed.model.undo_depth(), depth, "{msg:?}");
                assert!(ed.popover.is_none(), "{msg:?}");
                assert!(ed.resize_draft.is_none(), "{msg:?}");
            }
        }
        // Released: the rotation applies.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 8)));
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        assert!(!app.editor.as_ref().unwrap().model.geometry().is_original());
    }

    #[test]
    fn i_is_ignored_during_a_canvas_drag() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(16, 16));
        ed.canvas_drag_active = true;
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
    }

    #[test]
    fn i_is_ignored_during_a_select_move_drag() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(16, 16));
        ed.move_drag = Some(crate::editor::MoveDrag {
            baseline: Vec::new(),
            selected_index: 0,
            start: PointLike::new(2.0, 2.0),
            moved: true,
            kind: crate::editor::MoveDragKind::Move,
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorEyedropperToggled);
        assert!(app.editor.as_ref().unwrap().eyedropper.is_none());
    }

    /// 64 × 64 capture: "Name Priya" / "Email priya@example.com" (two
    /// word boxes) / "Phone +1 (415) 555-0142". Two sensitive items.
    fn sample_doc() -> readshot_core::text_lines::TextDocument {
        use readshot_core::ocr_layout::{RecognizedLine, WordBox};
        let line = |text: &str, y: f32, words: Vec<WordBox>| RecognizedLine {
            text: text.into(),
            x: 0.1,
            y,
            w: 0.8,
            h: 0.1,
            words,
        };
        readshot_core::text_lines::TextDocument::from_recognized(
            &[
                line("Name Priya", 0.1, Vec::new()),
                line(
                    "Email priya@example.com",
                    0.3,
                    vec![
                        WordBox {
                            start: 0,
                            end: 5,
                            x: 0.1,
                            y: 0.3,
                            w: 0.2,
                            h: 0.1,
                        },
                        WordBox {
                            start: 6,
                            end: 23,
                            x: 0.35,
                            y: 0.3,
                            w: 0.55,
                            h: 0.1,
                        },
                    ],
                ),
                line("Phone +1 (415) 555-0142", 0.5, Vec::new()),
            ],
            64,
            64,
        )
    }

    /// An editor whose Text panel finished recognizing `sample_doc()`.
    fn ready_editor(app: &mut App) {
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(app, Message::EditorTextRecognize);
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            app,
            Message::EditorTextRecognized {
                session,
                part: 0,
                result: Ok(sample_doc()),
            },
        );
    }

    fn highlight_stroke() -> Annotation {
        // Over "priya@example.com" (line 1: y 19.2–25.6, word x 22.4–57.6).
        Annotation::Highlighter {
            points: vec![PointLike::new(25.0, 22.0), PointLike::new(40.0, 23.0)],
            color: Rgba::new(1.0, 1.0, 0.0, 0.45),
            line_width: 12.0,
        }
    }

    #[test]
    fn snap_to_text_turns_a_stroke_into_the_line_box() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Highlighter,
            )),
        );
        let _ = update(
            &mut app,
            Message::EditorStyle(readshot_ui::editor::StyleMessage::SnapToText(true)),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                highlight_stroke(),
            )),
        );
        match &app.editor.as_ref().unwrap().model.annotations()[0] {
            Annotation::Highlighter {
                points, line_width, ..
            } => {
                assert_eq!(points.len(), 2);
                assert!((points[0].x - 22.4).abs() < 0.01, "{points:?}");
                assert!((points[1].x - 57.6).abs() < 0.01, "{points:?}");
                assert!((points[0].y - 22.4).abs() < 0.01 && (points[1].y - 22.4).abs() < 0.01);
                assert!((line_width - 6.4).abs() < 0.01);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn snap_off_keeps_the_freehand_stroke() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                highlight_stroke(),
            )),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().model.annotations()[0],
            highlight_stroke()
        );
    }

    fn build_app_with_positions(perms: Arc<FakePermissions>) -> App {
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_lines(Vec::new())),
            perms.clone(),
            None,
        );
        App::new(coord, perms, Preferences::default())
    }

    fn select_highlighter(app: &mut App) {
        let _ = update(
            app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Highlighter,
            )),
        );
    }

    #[test]
    fn choosing_the_highlighter_starts_recognition() {
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = false;
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        select_highlighter(&mut app);
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
    }

    #[test]
    fn choosing_the_highlighter_without_line_positions_does_not_recognize() {
        // Snap to text needs line boxes; an engine without them would
        // copy and OCR the whole image for nothing.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = false;
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        select_highlighter(&mut app);
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Idle
        ));
    }

    #[test]
    fn text_recognition_result_for_the_current_session_is_kept() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(&mut app, Message::EditorTextRecognize);
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
        ready_editor(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.text_panel.document().unwrap().lines.len(), 3);
        assert_eq!(ed.text_panel.matches.len(), 2);
        assert!(!ed.busy, "recognition never blocks editing");
    }

    #[test]
    fn stale_text_recognition_result_is_dropped() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(&mut app, Message::EditorTextRecognize);
        let session = app.editor.as_ref().unwrap().session_id + 1000;
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: 0,
                result: Ok(sample_doc()),
            },
        );
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
    }

    #[test]
    fn failed_recognition_toasts_retry_and_retry_restarts() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(&mut app, Message::EditorTextRecognize);
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: 0,
                result: Err("Vision OCR timed out".into()),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(matches!(
            ed.text_panel.recognition,
            crate::editor::TextRecognition::Failed(_)
        ));
        assert!(ed.status_is_error(), "{:?}", ed.status);
        assert_eq!(
            ed.toast_action,
            Some(crate::editor::EditorToastAction::RetryRecognition)
        );

        let _ = update(&mut app, Message::EditorTextRecognize);
        let ed = app.editor.as_ref().unwrap();
        assert!(matches!(
            ed.text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
        assert!(ed.status.is_none() && ed.toast_action.is_none());
    }

    #[test]
    fn recognition_is_not_restarted_once_ready() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorTextRecognize);
        assert!(app.editor.as_ref().unwrap().text_panel.document().is_some());
    }

    #[test]
    fn opening_an_editor_starts_recognition_only_with_the_panel_open() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let _ = open_editor_window_replacing(
            &mut app,
            crate::editor::EditorSession::new(solid(64, 64)),
            None,
        );
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
        app.preferences.editor_panel_open = false;
        let _ = open_editor_window_replacing(
            &mut app,
            crate::editor::EditorSession::new(solid(64, 64)),
            None,
        );
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Idle
        ));
    }

    #[test]
    fn a_fresh_capture_with_history_shares_its_ocr_with_the_text_panel() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        // Panel closed, so only the history OCR could mark it Recognizing.
        app.preferences.editor_panel_open = false;
        app.preferences.history_retention = readshot_core::HistoryRetention::Last50;
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 64))));
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
        app.preferences.history_retention = readshot_core::HistoryRetention::Off;
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 64))));
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Idle
        ));
    }

    #[test]
    fn reopening_from_history_without_positions_reuses_the_stored_text() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "fake-0");
        record.ocr_text = Some("one\ntwo".into());
        let _ = update(
            &mut app,
            Message::HistoryOpenInEditorReady(Ok((solid(64, 64), record.clone()))),
        );
        let doc = app
            .editor
            .as_ref()
            .unwrap()
            .text_panel
            .document()
            .expect("seeded, no OCR");
        assert_eq!(doc.lines.len(), 2);
        assert!(!doc.positioned);

        // A positioned engine (Apple Vision) recognizes again for boxes.
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_lines(Vec::new())),
            perms.clone(),
            None,
        );
        let mut app = App::new(coord, perms, Preferences::default());
        let _ = update(
            &mut app,
            Message::HistoryOpenInEditorReady(Ok((solid(64, 64), record))),
        );
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
    }

    #[test]
    fn background_recognition_failure_is_quiet_with_the_panel_closed() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = false;
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let session = app.editor.as_ref().unwrap().session_id;
        // A history-shared OCR nobody asked for fails: no toast.
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: 0,
                result: Err("no models".into()),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(matches!(
            ed.text_panel.recognition,
            crate::editor::TextRecognition::Failed(_)
        ));
        assert!(ed.status.is_none() && ed.toast_action.is_none());

        // Asked for explicitly: the failure toasts with Retry.
        let _ = update(&mut app, Message::EditorTextRecognize);
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: 0,
                result: Err("no models".into()),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.toast_action,
            Some(crate::editor::EditorToastAction::RetryRecognition)
        );
    }

    /// Run `persist_history` to completion and collect what it sent.
    async fn run_persist_history(
        ocr: FakeOcrEngine,
        img: image::RgbaImage,
        session: Option<u64>,
    ) -> (
        Vec<Message>,
        CaptureCoordinator,
        readshot_core::CaptureRecord,
        tempfile::TempDir,
    ) {
        use iced::futures::StreamExt;
        let dir = tempfile::TempDir::new().unwrap();
        let store: Arc<dyn HistoryStore> =
            Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(ocr),
            perms,
            Some(store),
        );
        let record = readshot_core::CaptureRecord::new(
            chrono::Utc::now(),
            img.width(),
            img.height(),
            "fake-0",
        );
        let (tx, rx) = iced::futures::channel::mpsc::channel(4);
        persist_history(
            coord.clone(),
            img,
            Some(record.clone()),
            readshot_core::HistoryRetention::Last50,
            Preferences::default(),
            session,
            tx,
        )
        .await;
        let msgs: Vec<Message> = rx.collect().await;
        (msgs, coord, record, dir)
    }

    #[tokio::test]
    async fn history_ocr_feeds_the_editor_first_and_stores_the_layout_text() {
        let (msgs, coord, record, _dir) = run_persist_history(
            FakeOcrEngine::with_text("one\n\ntwo"),
            solid(16, 16),
            Some(7),
        )
        .await;
        // OCR resolves before the PNG encode / save, so the panel isn't
        // held back by them.
        match &msgs[0] {
            Message::EditorTextRecognized {
                session: 7,
                part: 0,
                result: Ok(doc),
            } => {
                assert_eq!(doc.layout_text, "one\n\ntwo");
            }
            other => panic!("expected EditorTextRecognized first, got {other:?}"),
        }
        assert!(msgs
            .iter()
            .any(|m| matches!(m, Message::HistoryRecordSaved)));
        assert!(matches!(
            msgs.last(),
            Some(Message::HistoryRecordPersisted(Ok(true)))
        ));
        let stored = coord.history_list().unwrap();
        let stored = stored.iter().find(|r| r.id == record.id).unwrap();
        assert_eq!(stored.ocr_text.as_deref(), Some("one\n\ntwo"));

        // No editor session: nothing is sent to an editor.
        let (msgs, _, _, _dir) =
            run_persist_history(FakeOcrEngine::with_text("hi"), solid(16, 16), None).await;
        assert!(!msgs
            .iter()
            .any(|m| matches!(m, Message::EditorTextRecognized { .. })));
    }

    #[tokio::test]
    async fn history_ocr_errors_reach_the_editor_even_when_the_save_fails() {
        let (msgs, _, _, _dir) =
            run_persist_history(FakeOcrEngine::failing("boom"), solid(16, 16), Some(3)).await;
        assert!(matches!(
            &msgs[0],
            Message::EditorTextRecognized {
                session: 3,
                part: 0,
                result: Err(_)
            }
        ));
        assert!(matches!(
            msgs.last(),
            Some(Message::HistoryRecordPersisted(Ok(false)))
        ));

        // A 0 × 0 image fails the PNG encode: no record is saved, but the
        // editor still gets its OCR result (it never stays Recognizing).
        let (msgs, _, _, _dir) = run_persist_history(
            FakeOcrEngine::with_text("hi"),
            image::RgbaImage::new(0, 0),
            Some(4),
        )
        .await;
        assert!(
            matches!(
                &msgs[..],
                [Message::EditorTextRecognized {
                    session: 4,
                    part: 0,
                    result: Ok(_)
                }]
            ),
            "{msgs:?}"
        );
    }

    #[test]
    fn option_command_p_toggles_the_panel_and_command_p_still_pins() {
        use iced::keyboard::{Key, Modifiers};
        let mut alt_cmd = command_modifiers();
        alt_cmd.insert(Modifiers::ALT);
        for ignored in [true, false] {
            assert!(matches!(
                editor_key_message(Key::Character("p".into()), alt_cmd, ignored),
                Some(Message::EditorPanelToggled)
            ));
        }
        assert!(matches!(
            editor_key_message(Key::Character("p".into()), command_modifiers(), true),
            Some(Message::EditorPinRequested)
        ));
    }

    #[test]
    fn toggling_the_panel_is_remembered_and_starts_recognition() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = false;
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(&mut app, Message::EditorPanelToggled);
        assert!(app.preferences.editor_panel_open);
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
        let _ = update(&mut app, Message::EditorPanelToggled);
        assert!(!app.preferences.editor_panel_open);
    }

    #[test]
    fn narrow_window_opens_a_temporary_overlay_that_closes_three_ways() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_width = 800.0;
        app.editor = Some(ed);
        let overlay = |app: &App| app.editor.as_ref().unwrap().panel_overlay_open;
        assert_eq!(
            editor_panel_mode(&app, app.editor.as_ref().unwrap()),
            PanelMode::Collapsed
        );
        let _ = update(&mut app, Message::EditorPanelToggled);
        assert!(overlay(&app));
        assert!(
            app.preferences.editor_panel_open,
            "the remembered preference is untouched"
        );
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
        // esc (popover level of the single Esc chain)
        let _ = update(&mut app, Message::EditorTextCancel);
        assert!(!overlay(&app));
        // click outside
        let _ = update(&mut app, Message::EditorPanelToggled);
        let _ = update(&mut app, Message::EditorPanelOverlayDismissed);
        assert!(!overlay(&app));
        // ⌥⌘P again
        let _ = update(&mut app, Message::EditorPanelToggled);
        let _ = update(&mut app, Message::EditorPanelToggled);
        assert!(!overlay(&app));
        assert!(app.preferences.editor_panel_open);
    }

    #[test]
    fn editor_window_resize_updates_only_the_editor_width() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        app.editor = Some(ed);
        assert_eq!(
            app.editor.as_ref().unwrap().window_width,
            crate::editor::DEFAULT_WINDOW_WIDTH
        );
        let _ = update(
            &mut app,
            Message::EditorWindowResized(iced::window::Id::unique(), 800.0),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().window_width,
            crate::editor::DEFAULT_WINDOW_WIDTH
        );
        let _ = update(&mut app, Message::EditorWindowResized(id, 800.0));
        assert_eq!(app.editor.as_ref().unwrap().window_width, 800.0);
    }

    #[test]
    fn panel_tab_is_remembered_and_no_text_links_to_text_recognition_settings() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        app.preferences.editor_panel_tab = "beautify".into();
        let _ = update(
            &mut app,
            Message::EditorPanelTabSelected(crate::editor::PanelTab::Text),
        );
        assert_eq!(app.preferences.editor_panel_tab, "text");
        let _ = update(&mut app, Message::EditorOpenTextSettings);
        assert_eq!(app.settings_pane, crate::app::SettingsPane::TextRecognition);
        assert!(app.settings_window_id.is_some());
    }

    #[test]
    fn esc_closes_a_popover_then_the_overlay_panel_and_never_arms_discard() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.window_width = 800.0;
        ed.panel_overlay_open = true;
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        // Captured esc: the popover goes first, the overlay stays.
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert!(ed.panel_overlay_open);
        // An esc no widget captured closes the overlay instead of
        // falling through to Discard.
        let _ = update(
            &mut app,
            Message::EditorKeyPressed {
                window: id,
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                modifiers: iced::keyboard::Modifiers::empty(),
                status_ignored: true,
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.panel_overlay_open);
        assert!(ed.discard_pending_at.is_none());
    }

    #[test]
    fn an_overlay_widened_into_the_dock_hides_with_one_toggle() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = false;
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_width = 800.0;
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorPanelToggled);
        app.editor.as_mut().unwrap().window_width = 1200.0;
        assert_eq!(
            editor_panel_mode(&app, app.editor.as_ref().unwrap()),
            PanelMode::Docked
        );
        // "Hide panel" on what is shown hides it, not re-opens it.
        let _ = update(&mut app, Message::EditorPanelToggled);
        assert_eq!(
            editor_panel_mode(&app, app.editor.as_ref().unwrap()),
            PanelMode::Hidden
        );
        assert!(!app.preferences.editor_panel_open);
    }

    #[test]
    fn widening_the_window_folds_an_open_overlay_into_the_docked_preference() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = false;
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.window_width = 800.0;
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorPanelToggled);
        assert!(app.editor.as_ref().unwrap().panel_overlay_open);
        let _ = update(&mut app, Message::EditorWindowResized(id, 1200.0));
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.panel_overlay_open, "no stale overlay flag");
        assert!(app.preferences.editor_panel_open, "what is shown is kept");
        assert_eq!(editor_panel_mode(&app, ed), PanelMode::Docked);
        // Narrowing again collapses; nothing reopens as an overlay.
        let _ = update(&mut app, Message::EditorWindowResized(id, 800.0));
        assert_eq!(
            editor_panel_mode(&app, app.editor.as_ref().unwrap()),
            PanelMode::Collapsed
        );
    }

    #[test]
    fn a_stale_overlay_flag_in_a_wide_window_does_not_eat_esc() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.window_width = 1200.0;
        ed.panel_overlay_open = true;
        ed.model.commit_annotation(Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
            radius: 4.0,
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorTextCancel);
        assert!(
            app.editor
                .as_ref()
                .unwrap()
                .model
                .selected_annotation()
                .is_none(),
            "esc reached the annotation selection"
        );
    }

    #[test]
    fn opening_the_panel_closes_an_open_popover() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = false;
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        assert!(app.editor.as_ref().unwrap().popover.is_some());
        let _ = update(&mut app, Message::EditorPanelToggled);
        assert!(app.preferences.editor_panel_open);
        assert_eq!(app.editor.as_ref().unwrap().popover, None);
        // Narrow-window overlay too.
        app.preferences.editor_panel_open = false;
        app.editor.as_mut().unwrap().window_width = 800.0;
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Color),
        );
        let _ = update(&mut app, Message::EditorPanelToggled);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.panel_overlay_open);
        assert_eq!(ed.popover, None);
    }

    #[test]
    fn dismissing_no_overlay_keeps_the_hovered_line() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorTextLineHovered(1));
        let _ = update(&mut app, Message::EditorPanelOverlayDismissed);
        assert_eq!(app.editor.as_ref().unwrap().text_panel.hovered, Some(1));
        // An open overlay that closes does drop the hover.
        let ed = app.editor.as_mut().unwrap();
        ed.window_width = 800.0;
        ed.panel_overlay_open = true;
        let _ = update(&mut app, Message::EditorPanelOverlayDismissed);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.panel_overlay_open);
        assert_eq!(ed.text_panel.hovered, None);
    }

    #[test]
    fn command_changes_are_tracked_without_touching_the_editor() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorCommandChanged(true));
        assert!(app.editor_command_held);
        let _ = update(&mut app, Message::EditorCommandChanged(false));
        assert!(!app.editor_command_held);
    }

    #[test]
    fn editor_modifier_flags_reset_when_an_editor_opens_or_closes() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let hold = |app: &mut App| {
            app.editor_command_held = true;
            app.editor_shift_held = true;
        };
        let released = |app: &App| !app.editor_command_held && !app.editor_shift_held;

        // Opening (the old window's key-up never arrives).
        hold(&mut app);
        let _ = open_editor_window_replacing(
            &mut app,
            crate::editor::EditorSession::new(solid(16, 16)),
            None,
        );
        assert!(released(&app), "open");

        // Closing via Discard on a clean editor.
        hold(&mut app);
        let _ = update(&mut app, Message::EditorDiscardRequested);
        assert!(app.editor.is_none());
        assert!(released(&app), "discard");

        // Closing via Pin.
        let _ = open_editor_window_replacing(
            &mut app,
            crate::editor::EditorSession::new(solid(16, 16)),
            None,
        );
        hold(&mut app);
        let _ = update(&mut app, Message::EditorPinRequested);
        assert!(app.editor.is_none());
        assert!(released(&app), "pin");

        // Closing the window from the OS.
        let _ = open_editor_window_replacing(
            &mut app,
            crate::editor::EditorSession::new(solid(16, 16)),
            None,
        );
        let id = app.editor.as_ref().unwrap().window_id;
        let id = id.unwrap_or_else(|| {
            let id = iced::window::Id::unique();
            app.editor.as_mut().unwrap().window_id = Some(id);
            id
        });
        hold(&mut app);
        let _ = update(&mut app, Message::WindowClosed(id));
        assert!(app.editor.is_none());
        assert!(released(&app), "window closed");
    }

    #[test]
    fn copy_all_uses_the_recognized_text_when_annotations_are_text_neutral() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorCopyTextRequested);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.busy);
        assert_eq!(ed.status.as_deref(), Some("Copying text…"));
    }

    #[test]
    fn copy_all_re_recognizes_the_output_when_annotations_add_or_hide_text() {
        let typed = Annotation::Text {
            content: "typed note".into(),
            origin: PointLike::new(4.0, 4.0),
            color: Rgba::OPAQUE_BLACK,
            font_family: "system-ui".into(),
            size: 16.0,
            style: Default::default(),
        };
        let scribble = Annotation::Pen {
            points: vec![PointLike::new(0.0, 20.0), PointLike::new(60.0, 22.0)],
            color: Rgba::OPAQUE_BLACK,
            line_width: 6.0,
        };
        let blur = Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
            radius: 4.0,
        };
        for a in [typed, scribble, blur] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            ready_editor(&mut app);
            app.editor
                .as_mut()
                .unwrap()
                .model
                .commit_annotation(a.clone());
            let _ = update(&mut app, Message::EditorCopyTextRequested);
            assert_eq!(
                app.editor.as_ref().unwrap().status.as_deref(),
                Some("Recognizing text…"),
                "{a:?}"
            );
        }
    }

    #[test]
    fn copy_all_commits_a_pending_text_draft_before_choosing_the_source() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        app.editor
            .as_mut()
            .unwrap()
            .begin_text_edit(crate::editor::PendingText {
                origin: PointLike::new(4.0, 4.0),
                content: "draft".into(),
                edit_index: None,
            });
        let _ = update(&mut app, Message::EditorCopyTextRequested);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 1, "draft committed");
        assert_eq!(ed.status.as_deref(), Some("Recognizing text…"));
    }

    #[test]
    fn copy_all_before_recognition_falls_back_to_ocr() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(&mut app, Message::EditorCopyTextRequested);
        assert_eq!(
            app.editor.as_ref().unwrap().status.as_deref(),
            Some("Recognizing text…")
        );
    }

    #[test]
    fn panel_line_clicks_follow_shift_and_command() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let selected = |app: &App| -> Vec<usize> {
            app.editor
                .as_ref()
                .unwrap()
                .text_panel
                .selected
                .iter()
                .copied()
                .collect()
        };
        let _ = update(&mut app, Message::EditorTextLineClicked(0));
        assert_eq!(selected(&app), [0]);
        let _ = update(&mut app, Message::EditorShiftChanged(true));
        let _ = update(&mut app, Message::EditorTextLineClicked(2));
        assert_eq!(selected(&app), [0, 1, 2]);
        let _ = update(&mut app, Message::EditorShiftChanged(false));
        let _ = update(&mut app, Message::EditorCommandChanged(true));
        let _ = update(&mut app, Message::EditorTextLineClicked(1));
        assert_eq!(selected(&app), [0, 2]);
        let _ = update(&mut app, Message::EditorTextLineClicked(9));
        assert_eq!(selected(&app), [0, 2], "out-of-range clicks are ignored");
    }

    #[test]
    fn hover_tracks_the_line_under_the_pointer() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorTextLineHovered(1));
        let _ = update(&mut app, Message::EditorTextLineHovered(2));
        // A late exit from line 1 must not clear line 2's hover.
        let _ = update(&mut app, Message::EditorTextLineUnhovered(1));
        assert_eq!(app.editor.as_ref().unwrap().text_panel.hovered, Some(2));
        let _ = update(&mut app, Message::EditorTextLineUnhovered(2));
        assert_eq!(app.editor.as_ref().unwrap().text_panel.hovered, None);
    }

    #[test]
    fn copy_lines_is_ignored_while_busy() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorTextLineClicked(0));
        app.editor.as_mut().unwrap().busy = true;
        let _ = update(&mut app, Message::EditorCopyLines);
        assert!(app.editor.as_ref().unwrap().status.is_none());
    }

    #[test]
    fn copy_lines_needs_a_selection_and_reports_the_count() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorCopyLines);
        assert!(app.editor.as_ref().unwrap().status.is_none());
        let _ = update(&mut app, Message::EditorTextLineClicked(0));
        let _ = update(&mut app, Message::EditorCopyLines);
        assert_eq!(
            app.editor.as_ref().unwrap().status.as_deref(),
            Some("Copying text…")
        );
        let _ = update(&mut app, Message::EditorCopyLinesDone(Ok(2)));
        assert_eq!(
            app.editor.as_ref().unwrap().status.as_deref(),
            Some("Copied 2 lines.")
        );
    }

    #[test]
    fn remove_line_breaks_toggle_is_remembered() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorRemoveLineBreaksToggled(true));
        assert!(app.preferences.editor_copy_remove_line_breaks);
    }

    #[test]
    fn esc_clears_the_panel_selection_last() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorTextLineClicked(1));
        // commit_annotation selects the new annotation.
        app.editor
            .as_mut()
            .unwrap()
            .model
            .commit_annotation(Annotation::Blur {
                rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
                radius: 4.0,
            });
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.model.selected_annotation().is_none());
        assert!(
            !ed.text_panel.selected.is_empty(),
            "first Esc clears the canvas selection"
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        assert!(app.editor.as_ref().unwrap().text_panel.selected.is_empty());
    }

    #[test]
    fn text_marks_follow_hover_and_selection_only_with_positions() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorTextLineHovered(1));
        let _ = update(&mut app, Message::EditorTextLineClicked(0));
        let ed = app.editor.as_ref().unwrap();
        let doc = ed.text_panel.document().unwrap();
        let (hover, selected) = text_marks(ed, true);
        assert_eq!(hover, doc.lines[1].rect);
        assert_eq!(selected, vec![doc.lines[0].rect.unwrap()]);
        assert_eq!(text_marks(ed, false), (None, Vec::new()));

        let mut plain = crate::editor::EditorSession::new(solid(64, 64));
        plain.text_panel.recognition = crate::editor::TextRecognition::Ready(std::sync::Arc::new(
            readshot_core::text_lines::TextDocument::from_plain("one\ntwo"),
        ));
        plain.text_panel.hovered = Some(0);
        assert_eq!(text_marks(&plain, true), (None, Vec::new()));
    }

    #[test]
    fn redact_all_adds_one_block_per_item_as_one_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        let anns = ed.model.annotations();
        assert_eq!(anns.len(), 2);
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert_eq!(ed.status.as_deref(), Some("Redacted 2 items."));
        assert!(matches!(
            ed.visible_toast_action(),
            Some(crate::editor::EditorToastAction::UndoRedaction { .. })
        ));
        // Span-precise: the email block starts at the address's word box
        // (x 22.4, padded and snapped to 21), not at "Email" (x 6.4).
        match &anns[0] {
            Annotation::Rectangle {
                rect, style, color, ..
            } => {
                assert_eq!(style.fill, readshot_core::FillMode::Filled);
                assert_eq!(*color, Rgba::OPAQUE_BLACK);
                assert_eq!(rect.x, 21.0);
            }
            other => panic!("{other:?}"),
        }

        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.model.annotations().len(),
            2,
            "covered items are not redacted twice"
        );
        assert_eq!(
            ed.status.as_deref(),
            Some("No sensitive items left to redact.")
        );

        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        assert!(app.editor.as_ref().unwrap().model.annotations().is_empty());
    }

    #[test]
    fn redact_line_uses_the_chosen_style() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(
            &mut app,
            Message::EditorRedactStyleChanged(readshot_core::redact::RedactStyle::Pixelate),
        );
        let _ = update(&mut app, Message::EditorRedactLine(2));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 1);
        assert!(matches!(
            ed.model.annotations()[0],
            Annotation::Pixelate { .. }
        ));
        assert_eq!(ed.status.as_deref(), Some("Redacted 1 item."));
        let _ = update(&mut app, Message::EditorRedactLine(0));
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
    }

    #[test]
    fn redaction_needs_line_positions() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 64)));
        let _ = update(&mut app, Message::EditorTextRecognize);
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: 0,
                result: Ok(readshot_core::text_lines::TextDocument::from_plain(
                    "Email priya@example.com",
                )),
            },
        );
        assert!(app.editor.as_ref().unwrap().text_panel.matches.is_empty());
        let _ = update(&mut app, Message::EditorRedactAll);
        assert!(app.editor.as_ref().unwrap().model.annotations().is_empty());
    }

    #[test]
    fn unlocatable_items_are_reported_not_dropped() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        // A match whose line no longer exists cannot be placed.
        app.editor
            .as_mut()
            .unwrap()
            .text_panel
            .matches
            .push(readshot_core::redact::RedactMatch {
                line: 99,
                span: 0..5,
                kind: readshot_core::redact::RedactKind::Email,
            });
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(super::text_panel::redact_state(ed).pending.len(), 2);
        assert_eq!(super::text_panel::redact_state(ed).unlocatable.len(), 1);

        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 2);
        assert_eq!(
            ed.status.as_deref(),
            Some("Redacted 2 items. 1 more couldn't be located; cover it by hand.")
        );
        assert!(ed.visible_toast_action().is_some());

        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.status.as_deref(),
            Some("Couldn't locate 1 sensitive item. Cover it by hand.")
        );
        assert!(ed.status_is_error());
    }

    #[test]
    fn redaction_undo_is_dropped_by_any_other_history_change() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorRedactAll);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        assert_eq!(app.editor.as_ref().unwrap().toast_action, None);
        // Redo returns to the redaction's depth; the Undo must not come
        // back (a different edit could sit at that depth too).
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 2);
        assert_eq!(ed.visible_toast_action(), None);

        // Same for history changes that set no status of their own
        // (e.g. a move): the history sync itself drops the Undo.
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        let _ = update(&mut app, Message::EditorRedactAll);
        let coord = app.coordinator.clone();
        let ed = app.editor.as_mut().unwrap();
        assert!(ed.visible_toast_action().is_some());
        assert!(ed.model.undo());
        super::editor_ops::sync_editor_history(ed, &coord);
        assert!(ed.model.redo());
        assert_eq!(ed.visible_toast_action(), None);
    }

    #[test]
    fn arming_discard_drops_the_redaction_undo() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorRedactAll);
        let _ = update(&mut app, Message::EditorDiscardRequested);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.discard_pending_at.is_some());
        assert_eq!(ed.toast_action, None);
    }

    #[test]
    fn a_blur_redaction_does_not_count_as_solid() {
        use readshot_core::redact::RedactStyle;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(
            &mut app,
            Message::EditorRedactStyleChanged(RedactStyle::Blur),
        );
        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 2);
        let rs = super::text_panel::redact_state(ed);
        assert!(rs.pending.is_empty(), "Blur chosen: blurred items are done");
        assert_eq!(rs.weak.len(), 2);
        // Blur again: no duplicates.
        let _ = update(&mut app, Message::EditorRedactAll);
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 2);

        // Switch to Solid: the blurred items are pending again and Redact
        // All puts a solid block on top of each.
        let _ = update(
            &mut app,
            Message::EditorRedactStyleChanged(RedactStyle::Solid),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(super::text_panel::redact_state(ed).pending.len(), 2);
        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.status.as_deref(), Some("Redacted 2 items."));
        let anns = ed.model.annotations();
        assert_eq!(anns.len(), 4);
        assert!(matches!(anns[2], Annotation::Rectangle { .. }));
        let rs = super::text_panel::redact_state(ed);
        assert!(rs.pending.is_empty() && rs.weak.is_empty());
        assert_eq!(rs.covered.len(), 2);
    }

    #[test]
    fn a_weak_manual_blur_is_not_reported_as_redacted() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        app.editor
            .as_mut()
            .unwrap()
            .model
            .commit_annotation(Annotation::Blur {
                rect: RectLike::new(0.0, 0.0, 64.0, 64.0),
                radius: 2.0,
            });
        let ed = app.editor.as_ref().unwrap();
        let rs = super::text_panel::redact_state(ed);
        assert_eq!(rs.pending.len(), 2, "Solid (default) still offers both");
        assert_eq!(rs.weak.len(), 2);
        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 3);
        assert!(super::text_panel::redact_state(ed).pending.is_empty());
    }

    #[test]
    fn copy_all_after_a_redaction_recognizes_the_output() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let _ = update(&mut app, Message::EditorRedactAll);
        let _ = update(&mut app, Message::EditorCopyTextRequested);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.busy);
        assert_eq!(
            ed.status.as_deref(),
            Some("Recognizing text…"),
            "redacted text must not come from the cached panel text"
        );
    }

    // --- History scrub after a redaction (B10) -------------------------

    fn decode_png(bytes: &[u8]) -> image::RgbaImage {
        image::load_from_memory(bytes).unwrap().to_rgba8()
    }

    fn stored(store: &FsHistoryStore) -> (readshot_core::CaptureRecord, image::RgbaImage) {
        let record = store.list().unwrap().remove(0);
        let png = std::fs::read(FsHistoryStore::abs_png_path(store.root(), &record)).unwrap();
        (record, decode_png(&png))
    }

    /// An editor opened on a saved History record whose text is
    /// `sample_doc()`'s, with the panel recognized. `save` false leaves
    /// the record off disk (a fresh capture whose save hasn't landed).
    fn scrub_app(save: bool) -> (tempfile::TempDir, Arc<FsHistoryStore>, App) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let mut app = build_app_with_history(
            Arc::new(FakePermissions::granted()),
            store.clone() as Arc<dyn HistoryStore>,
        );
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "d");
        record.ocr_text = Some(sample_doc().layout_text);
        if save {
            store
                .save(&record, &readshot_core::encode_png(&solid(64, 64)).unwrap())
                .unwrap();
        }
        app.editor = Some(crate::editor::EditorSession::from_history(
            solid(64, 64),
            record,
        ));
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: 0,
                result: Ok(sample_doc()),
            },
        );
        (dir, store, app)
    }

    const EMAIL: &str = "priya@example.com";
    const PHONE: &str = "555-0142";

    #[test]
    fn redact_all_scrubs_the_history_image_and_text() {
        let (_dir, store, mut app) = scrub_app(true);
        let _ = update(&mut app, Message::EditorRedactAll);
        app.coordinator.flush_history_updates();
        let (record, png) = stored(&store);
        let text = record.ocr_text.clone().unwrap();
        assert!(record.redacted);
        assert!(!text.contains(EMAIL) && !text.contains(PHONE), "{text}");
        assert!(text.contains("Name Priya"), "{text}");
        assert!(text.contains(&"█".repeat(EMAIL.chars().count())), "{text}");
        assert!(
            record.annotation_model.is_empty(),
            "the PNG already shows them: reopening must not apply them twice"
        );
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(png, ed.model.flatten(), "the PNG is the exported image");
        assert!(ed.history_scrubbed);
    }

    #[test]
    fn undoing_every_redaction_restores_the_original_record() {
        let (_dir, store, mut app) = scrub_app(true);
        let original = sample_doc().layout_text;
        let _ = update(&mut app, Message::EditorRedactAll);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        app.coordinator.flush_history_updates();
        let (record, png) = stored(&store);
        assert!(!record.redacted);
        assert_eq!(record.ocr_text.as_deref(), Some(original.as_str()));
        assert!(record.annotation_model.is_empty());
        assert_eq!(png, solid(64, 64));
        assert!(!app.editor.as_ref().unwrap().history_scrubbed);
    }

    #[test]
    fn a_non_redaction_edit_keeps_the_plain_sidecar_sync() {
        let (_dir, store, mut app) = scrub_app(true);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                highlight_stroke(),
            )),
        );
        app.coordinator.flush_history_updates();
        let (record, png) = stored(&store);
        assert!(!record.redacted);
        assert_eq!(record.annotation_model.len(), 1);
        assert_eq!(png, solid(64, 64), "the original image is untouched");
    }

    #[test]
    fn without_positions_a_redaction_masks_every_detected_item() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let mut app = build_app_with_history(
            Arc::new(FakePermissions::granted()),
            store.clone() as Arc<dyn HistoryStore>,
        );
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "d");
        record.ocr_text = Some(format!("Contact\nmail {EMAIL} today"));
        store
            .save(&record, &readshot_core::encode_png(&solid(64, 64)).unwrap())
            .unwrap();
        let mut ed = crate::editor::EditorSession::from_history(solid(64, 64), record);
        super::text_panel::seed_text_from_history(&mut ed, false);
        app.editor = Some(ed);
        // A solid block drawn by hand somewhere: positions are unknown,
        // so every detected item is masked.
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                readshot_core::redact::redaction_annotation(
                    RectLike::new(0.0, 0.0, 10.0, 10.0),
                    readshot_core::redact::RedactStyle::Solid,
                ),
            )),
        );
        app.coordinator.flush_history_updates();
        let (record, _) = stored(&store);
        assert_eq!(
            record.ocr_text.as_deref(),
            Some(format!("Contact\nmail {} today", "█".repeat(EMAIL.len())).as_str())
        );
    }

    #[tokio::test]
    async fn a_capture_saved_after_the_editor_closed_is_still_scrubbed() {
        let (_dir, store, mut app) = scrub_app(false);
        let _ = update(&mut app, Message::EditorRedactAll);
        let expected = app.editor.as_mut().unwrap().model.flatten();
        let record = app.editor.as_ref().unwrap().source_record.clone().unwrap();
        // The editor goes away before the capture's save lands.
        app.editor = None;
        app.coordinator.flush_history_updates();
        assert!(store.list().unwrap().is_empty(), "nothing on disk yet");
        let raw = readshot_core::encode_png(&solid(64, 64)).unwrap();
        app.coordinator
            .record_history(
                record.clone(),
                raw,
                readshot_core::HistoryRetention::Unlimited,
                chrono::Utc::now(),
            )
            .await;
        // The late OCR pass can't put the raw text back either.
        app.coordinator
            .record_recognized_text(&record, &sample_doc(), true)
            .unwrap();
        let (stored, png) = stored(&store);
        let text = stored.ocr_text.clone().unwrap();
        assert!(stored.redacted);
        assert!(!text.contains(EMAIL) && !text.contains(PHONE), "{text}");
        assert_eq!(png, expected);
    }

    /// An editor on a History record whose stored text is `text`, with a
    /// positional engine and no recognized document yet.
    fn positional_scrub_app(text: &str) -> (tempfile::TempDir, Arc<FsHistoryStore>, App) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(FsHistoryStore::new(dir.path().join("history")));
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_lines(Vec::new())),
            perms.clone(),
            Some(store.clone() as Arc<dyn HistoryStore>),
        );
        let mut app = App::new(coord, perms, Preferences::default());
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 64, 64, "d");
        record.ocr_text = Some(text.to_string());
        store
            .save(&record, &readshot_core::encode_png(&solid(64, 64)).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            solid(64, 64),
            record,
        ));
        (dir, store, app)
    }

    fn draw_solid_block(app: &mut App) {
        let _ = update(
            app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                readshot_core::redact::redaction_annotation(
                    RectLike::new(6.0, 6.0, 40.0, 8.0),
                    readshot_core::redact::RedactStyle::Solid,
                ),
            )),
        );
    }

    #[test]
    fn with_positions_but_no_word_boxes_yet_the_scrub_stores_no_text() {
        // A name no detector matches, hidden by hand.
        let (_dir, store, mut app) = positional_scrub_app("Name Priya");
        draw_solid_block(&mut app);
        app.coordinator.flush_history_updates();
        let (record, _) = stored(&store);
        assert!(record.redacted);
        assert_eq!(record.ocr_text, None, "fail closed until positions land");
        // …and recognition starts so the precise text can land.
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
    }

    #[test]
    fn a_failed_recognition_is_retried_on_the_next_scrub() {
        let (_dir, _store, mut app) = positional_scrub_app("Name Priya");
        app.editor.as_mut().unwrap().text_panel.recognition =
            crate::editor::TextRecognition::Failed("busy".into());
        draw_solid_block(&mut app);
        assert!(matches!(
            app.editor.as_ref().unwrap().text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
    }

    #[tokio::test]
    async fn recognized_text_reaches_history_even_after_the_editor_closed() {
        let (_dir, store, mut app) = positional_scrub_app("Name Priya");
        // Redact the email line by hand.
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                readshot_core::redact::redaction_annotation(
                    RectLike::new(20.0, 18.0, 40.0, 9.0),
                    readshot_core::redact::RedactStyle::Solid,
                ),
            )),
        );
        let record = app.editor.as_ref().unwrap().source_record.clone().unwrap();
        app.editor = None;
        app.coordinator.flush_history_updates();
        // The recognition task's History write, run with the editor gone.
        let _ = super::text_panel::store_recognized_text(
            &app.coordinator,
            &record,
            None,
            Ok(sample_doc()),
        )
        .await;
        let (stored, _) = stored(&store);
        let text = stored.ocr_text.unwrap();
        assert!(!text.contains(EMAIL), "{text}");
        assert!(text.contains("Name Priya"), "{text}");
    }

    #[test]
    fn a_failed_history_write_shows_an_error_and_forces_a_full_rewrite_next() {
        let (_dir, _store, mut app) = scrub_app(true);
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorHistoryWritten {
                session,
                result: Some(Err("No space left on device".into())),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.status_is_error(), "{:?}", ed.status);
        assert!(
            ed.status
                .as_deref()
                .is_some_and(|s| s.ends_with("Readshot will keep trying.")),
            "{:?}",
            ed.status
        );
        assert!(
            ed.history_scrubbed,
            "disk state unknown: next sync rewrites"
        );
        // A superseded write reports nothing.
        let _ = update(&mut app, Message::EditorStatusDismissed);
        let _ = update(
            &mut app,
            Message::EditorHistoryWritten {
                session,
                result: None,
            },
        );
        assert_eq!(app.editor.as_ref().unwrap().status, None);
    }

    #[test]
    fn a_successful_history_write_reloads_an_open_history_window() {
        let (_dir, _store, mut app) = scrub_app(true);
        let session = app.editor.as_ref().unwrap().session_id;
        let msg = || Message::EditorHistoryWritten {
            session,
            result: Some(Ok(())),
        };
        assert_eq!(update(&mut app, msg()).units(), 0);
        app.history_window_id = Some(iced::window::Id::unique());
        assert_eq!(update(&mut app, msg()).units(), 1);
    }

    #[test]
    fn copy_lines_masks_redacted_words() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        ready_editor(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            super::text_panel::copy_lines_text(ed, &[1], false).as_deref(),
            Some(format!("Email {EMAIL}").as_str())
        );
        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            super::text_panel::copy_lines_text(ed, &[0, 1], false).as_deref(),
            Some(format!("Name Priya\nEmail {}", "█".repeat(EMAIL.len())).as_str())
        );
    }

    fn positioned_doc(text: &str, rect: RectLike) -> readshot_core::text_lines::TextDocument {
        use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
        TextDocument {
            lines: vec![TextLine {
                text: text.into(),
                rect: Some(rect),
                words: vec![WordRect {
                    start: 0,
                    end: text.chars().count(),
                    rect,
                }],
            }],
            positioned: true,
            layout_text: text.into(),
        }
    }

    #[test]
    fn text_boxes_follow_a_rotation() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(30, 20));
        let session = ed.session_id;
        ed.text_panel.recognition = crate::editor::TextRecognition::Recognizing {
            since: std::time::Instant::now(),
        };
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: readshot_core::document::ORIGINAL_PART,
                result: Ok(positioned_doc(
                    "Example",
                    RectLike::new(2.0, 4.0, 10.0, 3.0),
                )),
            },
        );
        let ed = app.editor.as_mut().unwrap();
        ed.model.rotate(true).unwrap();
        sync_editor_history(ed, &app.coordinator);
        let doc = ed.text_panel.positioned_document().expect("still ready");
        // (x, y, w, h) → (H − y − h, x, h, w) for a clockwise turn of a 30×20 image.
        assert_eq!(doc.lines[0].rect, Some(RectLike::new(13.0, 2.0, 3.0, 10.0)));
        // Undo back to the original frame moves the boxes back.
        ed.model.undo();
        sync_editor_history(ed, &app.coordinator);
        let doc = ed.text_panel.positioned_document().expect("still ready");
        assert_eq!(doc.lines[0].rect, Some(RectLike::new(2.0, 4.0, 10.0, 3.0)));
    }

    #[test]
    fn an_added_part_needs_its_own_recognition_and_the_scrub_fails_closed_until_then() {
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(20, 10));
        let session = ed.session_id;
        ed.text_panel.recognition = crate::editor::TextRecognition::Recognizing {
            since: std::time::Instant::now(),
        };
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: readshot_core::document::ORIGINAL_PART,
                result: Ok(positioned_doc("first", RectLike::new(1.0, 1.0, 10.0, 4.0))),
            },
        );
        let ed = app.editor.as_mut().unwrap();
        let id = ed
            .model
            .add_part(solid(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: readshot_core::ShapeStyle {
                fill: readshot_core::FillMode::Filled,
                ..Default::default()
            },
        });
        sync_editor_history(ed, &app.coordinator);
        assert!(
            ed.text_panel.positioned_document().is_none(),
            "the new part isn't recognized yet"
        );
        assert_eq!(
            scrubbed_history_text(ed, true),
            None,
            "no text rather than unmasked text"
        );
        // Without positions from the engine either: still no text while a
        // part has none.
        assert_eq!(scrubbed_history_text(ed, false), None);
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: id,
                result: Ok(positioned_doc("second", RectLike::new(1.0, 1.0, 10.0, 4.0))),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        let doc = ed
            .text_panel
            .positioned_document()
            .expect("both parts recognized");
        assert_eq!(doc.lines.len(), 2);
        assert_eq!(doc.lines[1].rect, Some(RectLike::new(1.0, 11.0, 10.0, 4.0)));
        assert_eq!(
            scrubbed_history_text(ed, true).as_deref(),
            Some("█████\n\n██████")
        );
    }

    #[test]
    fn recognition_runs_once_per_part_starting_with_the_first_missing_one() {
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(20, 10));
        ed.text_panel.part_docs.insert(
            readshot_core::document::ORIGINAL_PART,
            Arc::new(positioned_doc("first", RectLike::new(1.0, 1.0, 10.0, 4.0))),
        );
        let id = ed
            .model
            .add_part(solid(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        app.editor = Some(ed);
        let _ = request_text_recognition(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.text_panel.recognizing_part, Some(id));
        assert!(matches!(
            ed.text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
    }

    #[test]
    fn a_stale_recognition_of_the_original_frame_never_masks_a_rotated_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(8, 4);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
        ed.model.rotate(true).unwrap();
        ed.model.commit_annotation(Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 4.0, 8.0),
            radius: 4.0,
        });
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        assert!(stored.redacted && stored.geometry_id.is_some());
        // The capture's own OCR (original frame) lands late.
        let doc = positioned_doc("secret", RectLike::new(0.0, 0.0, 8.0, 4.0));
        app.coordinator
            .record_recognized_text(&record, &doc, true)
            .unwrap();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        assert_eq!(stored.ocr_text, None, "stale text is dropped, never stored");
    }

    #[tokio::test]
    async fn a_rotation_rewrites_history_as_the_rotated_document_with_editable_annotations() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(8, 4);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(1.0, 1.0, 2.0, 2.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });
        ed.model.rotate(true).unwrap();
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        assert!(stored.geometry_id.is_some());
        assert_eq!((stored.width_px, stored.height_px), (4, 8));
        assert_eq!(
            stored.annotation_model.len(),
            1,
            "annotations stay editable"
        );
        assert!(!stored.redacted);
        // Undo back to the original frame restores the original image.
        ed.model.undo();
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        assert_eq!(stored.geometry_id, None);
        assert_eq!((stored.width_px, stored.height_px), (8, 4));
    }

    fn stored_png_dims(
        store: &readshot_core::FsHistoryStore,
        record: &readshot_core::CaptureRecord,
    ) -> (u32, u32) {
        image::image_dimensions(readshot_core::FsHistoryStore::abs_png_path(
            store.root(),
            record,
        ))
        .unwrap()
    }

    fn arrow() -> Annotation {
        Annotation::Arrow {
            a: PointLike::new(1.0, 1.0),
            b: PointLike::new(3.0, 3.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        }
    }

    #[test]
    fn a_plain_edit_after_a_rotation_keeps_the_stored_size_of_the_rotated_png() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(8, 4);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
        ed.model.rotate(true).unwrap();
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        // An ordinary edit afterwards: the plain sidecar sync.
        ed.model.commit_annotation(arrow());
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = store.list().unwrap().remove(0);
        assert_eq!(stored.annotation_model.len(), 1, "the plain sync landed");
        assert_eq!(stored_png_dims(&store, &record), (4, 8));
        assert_eq!((stored.width_px, stored.height_px), (4, 8));
    }

    #[test]
    fn a_plain_edit_after_a_combine_stores_the_combined_size() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(8, 4);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
        ed.model
            .add_part(solid(8, 6), readshot_core::document::Join::Below)
            .unwrap();
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        ed.model.commit_annotation(arrow());
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = store.list().unwrap().remove(0);
        assert_eq!(stored.annotation_model.len(), 1, "the plain sync landed");
        let png = stored_png_dims(&store, &record);
        assert_eq!(png, (ed.model.base().width(), ed.model.base().height()));
        assert_ne!(png, (8, 4));
        assert_eq!((stored.width_px, stored.height_px), png);
    }

    /// A 40 × 20 capture cropped to x 20–40, with a weak pixelate over a
    /// word at x 30–36 (x 10–16 of the cropped image).
    fn cropped_weak_pixelate_scrub(
        store: &Arc<readshot_core::FsHistoryStore>,
        app: &App,
    ) -> readshot_core::CaptureRecord {
        let img = solid(40, 20);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        let mut ed = crate::editor::EditorSession::from_history(img, record.clone());
        ed.model.commit_annotation(Annotation::Crop {
            rect: RectLike::new(20.0, 0.0, 20.0, 20.0),
        });
        ed.model.commit_annotation(Annotation::Pixelate {
            rect: RectLike::new(30.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        });
        write_editor_history(&mut ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        record
    }

    #[tokio::test]
    async fn a_cropped_scrub_stores_its_redactions_in_the_stored_png_and_masks_reopen_ocr() {
        use readshot_core::ocr_layout::RecognizedLine;
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        // OCR of the stored (cropped) image reads the weakly hidden word
        // where it is in that image: x 10–16 of 20.
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_lines(vec![RecognizedLine {
                text: hidden_word(),
                x: 0.5,
                y: 0.1,
                w: 0.3,
                h: 0.2,
                words: Vec::new(),
            }])),
            perms.clone(),
            Some(store.clone()),
        );
        let app = App::new(coord, perms, Preferences::default());
        let record = cropped_weak_pixelate_scrub(&store, &app);
        let stored = stored_record(&store, record.id);
        assert!(stored.redacted);
        assert_eq!(stored_png_dims(&store, &record), (20, 20));
        assert_eq!((stored.width_px, stored.height_px), (20, 20));
        assert_eq!(
            stored.redactions,
            vec![Annotation::Pixelate {
                rect: RectLike::new(10.0, 2.0, 6.0, 4.0),
                block_size: 4.0,
            }],
            "in the stored PNG's pixels"
        );
        // The capture's own OCR of the uncropped image lands late: its
        // boxes are in another frame than the stored image's.
        app.coordinator
            .record_recognized_text(
                &record,
                &positioned_doc(&hidden_word(), RectLike::new(30.0, 2.0, 6.0, 4.0)),
                true,
            )
            .unwrap();
        let text = stored_record(&store, record.id)
            .ocr_text
            .unwrap_or_default();
        assert!(!text.contains(&hidden_word()), "{text}");
        // Reopen: the editor recognizes the stored image; the task's
        // History write masks it with the stored redactions.
        let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
            store.root(),
            &stored,
        ))
        .unwrap()
        .to_rgba8();
        let ed = crate::editor::EditorSession::from_history(png.clone(), stored.clone());
        let mut framed = stored.clone();
        framed.geometry_id = history_geometry_id(&ed);
        let doc = app
            .coordinator
            .recognise_document(readshot_ocr::OCRRequest {
                image: png,
                languages: Vec::new(),
                use_language_correction: true,
            })
            .await
            .map_err(|e| e.to_string());
        assert!(doc.as_ref().unwrap().layout_text.contains(&hidden_word()));
        let _ =
            super::text_panel::store_recognized_text(&app.coordinator, &framed, None, doc).await;
        let text = stored_record(&store, record.id)
            .ocr_text
            .unwrap_or_default();
        assert!(!text.contains(&hidden_word()), "{text}");
        assert!(text.contains('█'), "{text}");
    }

    #[test]
    fn a_reopened_cropped_scrub_masks_its_text_with_the_inherited_redactions() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let record = cropped_weak_pixelate_scrub(&store, &app);
        let stored = stored_record(&store, record.id);
        let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
            store.root(),
            &stored,
        ))
        .unwrap()
        .to_rgba8();
        app.editor = Some(crate::editor::EditorSession::from_history(png, stored));
        // OCR of the stored image reads the word at x 10–16.
        recognized(
            &mut app,
            readshot_core::document::ORIGINAL_PART,
            positioned_doc(&hidden_word(), RectLike::new(10.0, 2.0, 6.0, 4.0)),
        );
        let ed = app.editor.as_mut().unwrap();
        ed.model.rotate(true).unwrap();
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let text = stored_record(&store, record.id)
            .ocr_text
            .unwrap_or_default();
        assert!(!text.contains(&hidden_word()), "{text}");
        assert_eq!(text, "█".repeat(6));
    }

    /// "outside inside <hidden word>" on a 40 × 20 capture: "outside" at
    /// x 2–12, "inside" at x 22–28, the hidden word at x 30–36.
    fn three_words() -> readshot_core::text_lines::TextDocument {
        use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
        let text = format!("outside inside {}", hidden_word());
        let word = |start, end, x| WordRect {
            start,
            end,
            rect: RectLike::new(x, 2.0, if start == 0 { 10.0 } else { 6.0 }, 4.0),
        };
        TextDocument {
            lines: vec![TextLine {
                text: text.clone(),
                rect: Some(RectLike::new(2.0, 2.0, 34.0, 4.0)),
                words: vec![word(0, 7, 2.0), word(8, 14, 22.0), word(15, 21, 30.0)],
            }],
            positioned: true,
            layout_text: text,
        }
    }

    /// A saved 40 × 20 capture (its text already stored) open in the
    /// editor, cropped to x 20–40 with the hidden word pixelated.
    fn cropped_scrub_session(
        store: &readshot_core::FsHistoryStore,
        app: &mut App,
        doc: Option<readshot_core::text_lines::TextDocument>,
    ) -> readshot_core::CaptureRecord {
        let img = solid(40, 20);
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
        record.ocr_text = Some(three_words().layout_text);
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        if let Some(doc) = doc {
            recognized(app, readshot_core::document::ORIGINAL_PART, doc);
        }
        let ed = app.editor.as_mut().unwrap();
        ed.model.commit_annotation(Annotation::Crop {
            rect: RectLike::new(20.0, 0.0, 20.0, 20.0),
        });
        ed.model.commit_annotation(Annotation::Pixelate {
            rect: RectLike::new(30.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        });
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        record
    }

    #[test]
    fn a_cropped_scrub_stores_only_the_text_inside_the_crop() {
        use readshot_core::history::record_matches_query;
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let record = cropped_scrub_session(&store, &mut app, Some(three_words()));
        let stored = stored_record(&store, record.id);
        assert!(stored.redacted);
        assert_eq!(stored_png_dims(&store, &record), (20, 20));
        // Cut-away "outside" is gone, "inside" kept, the redaction masked.
        assert_eq!(stored.ocr_text.as_deref(), Some("inside ██████"));
        // History search and the MCP server (same sidecar, same matcher).
        assert!(!record_matches_query(&stored, "outside"));
        assert!(!record_matches_query(&stored, &hidden_word()));
        assert!(record_matches_query(&stored, "inside"));
    }

    #[test]
    fn a_cropped_scrub_without_word_boxes_stores_no_text() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        // An engine without positions: only the stored plain text is known.
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let record = cropped_scrub_session(&store, &mut app, None);
        let stored = stored_record(&store, record.id);
        assert!(stored.redacted);
        assert_eq!(
            stored.ocr_text, None,
            "fail closed: nothing tells what the crop keeps"
        );
    }

    #[tokio::test]
    async fn recognition_landing_after_a_cropped_scrub_stores_only_the_text_inside_the_crop() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        // Redacted and cropped before any recognition: no text yet.
        let record = cropped_scrub_session(&store, &mut app, None);
        assert_eq!(stored_record(&store, record.id).ocr_text, None);
        // The recognition task's History write, run with the editor gone.
        let (target, cut) =
            super::text_panel::recognition_history_target(app.editor.as_ref().unwrap());
        app.editor = None;
        let target = target.expect("a History target");
        assert!(cut.is_some());
        let _ = super::text_panel::store_recognized_text(
            &app.coordinator,
            &target,
            cut.as_ref(),
            Ok(three_words()),
        )
        .await;
        let stored = stored_record(&store, record.id);
        assert_eq!(stored.ocr_text.as_deref(), Some("inside ██████"));
    }

    fn build_app_with_positions_and_history(history: Arc<dyn HistoryStore>) -> App {
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_lines(Vec::new())),
            perms.clone(),
            Some(history),
        );
        App::new(coord, perms, Preferences::default())
    }

    fn stored_record(
        store: &readshot_core::FsHistoryStore,
        id: readshot_core::Uuid,
    ) -> readshot_core::CaptureRecord {
        store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap()
    }

    fn recognized(
        app: &mut App,
        part: readshot_core::document::PartId,
        doc: readshot_core::text_lines::TextDocument,
    ) {
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            app,
            Message::EditorTextRecognized {
                session,
                part,
                result: Ok(doc),
            },
        );
    }

    fn word_blur(rect: RectLike) -> Annotation {
        Annotation::Blur { rect, radius: 2.0 }
    }

    /// A History record scrubbed in an earlier session: the stored image
    /// has `word_blur` baked in, its text isn't known yet, and a weak blur
    /// leaves the word readable to OCR of the flattened image.
    fn reopened_scrubbed(
        store: &readshot_core::FsHistoryStore,
    ) -> (image::RgbaImage, readshot_core::CaptureRecord) {
        let img = solid(30, 20);
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
        record.redacted = true;
        record.redactions = vec![word_blur(RectLike::new(2.0, 4.0, 10.0, 3.0))];
        record.redactions_complete = true;
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        (img, record)
    }

    fn hidden_word() -> String {
        ["sec", "ret"].concat()
    }

    fn ocr_line(
        text: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) -> readshot_core::ocr_layout::RecognizedLine {
        readshot_core::ocr_layout::RecognizedLine {
            text: text.into(),
            x,
            y,
            w,
            h,
            words: Vec::new(),
        }
    }

    /// An app whose OCR reads `lines` (normalized boxes) in any image.
    fn build_app_reading(
        history: Arc<dyn HistoryStore>,
        lines: Vec<readshot_core::ocr_layout::RecognizedLine>,
    ) -> App {
        let perms = Arc::new(FakePermissions::granted());
        let coord = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_lines(lines)),
            perms.clone(),
            Some(history),
        );
        App::new(coord, perms, Preferences::default())
    }

    #[test]
    fn a_cropped_record_of_an_older_rule_drops_its_text_on_load_and_stores_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_reading(store.clone(), vec![ocr_line("kept", 0.0, 0.5, 0.4, 0.2)]);
        let record = cropped_weak_pixelate_scrub(&store, &app);
        // Rewrite the sidecar as an older build left it: an unprefixed crop
        // id and text holding a word from the cut-away area.
        let path = store
            .root()
            .join(readshot_core::FsHistoryStore::json_path(&record));
        let mut json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        json["geometry_id"] = "crop:73d1ba24128a984d".into();
        json["ocr_text"] = "cutaway ██████".into();
        std::fs::write(&path, json.to_string()).unwrap();
        // History (and so MCP) never serves the old text.
        let listed = app.coordinator.history_list().unwrap();
        assert_eq!(listed[0].ocr_text, None);
        assert_eq!(
            listed[0].geometry_id.as_deref(),
            Some("crop3:73d1ba24128a984d")
        );
        // Whether that era's stored redactions are complete is unknown: no
        // text is recovered from the stored image (fail closed).
        let _ = update(&mut app, Message::HistoryListLoaded(Ok(listed)));
        app.coordinator.flush_history_updates();
        assert_eq!(stored_record(&store, record.id).ocr_text, None);
    }

    /// A record scrubbed by a build that kept no redactions: its image has
    /// a weak blur over [`hidden_word`] (x 2–12, y 4–7) that nothing
    /// records, and its text is masked.
    fn scrubbed_without_redactions(
        store: &readshot_core::FsHistoryStore,
    ) -> (image::RgbaImage, readshot_core::CaptureRecord) {
        let img = solid(30, 20);
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
        record.redacted = true;
        record.ocr_text = Some("██████ visible".into());
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        (img, record)
    }

    #[tokio::test]
    async fn a_new_redaction_over_unknown_baked_in_redactions_never_recovers_text() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let (img, record) = scrubbed_without_redactions(&store);
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        let ed = app.editor.as_mut().unwrap();
        ed.model
            .commit_annotation(word_blur(RectLike::new(20.0, 10.0, 5.0, 5.0)));
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        assert_eq!(stored_record(&store, record.id).ocr_text, None);
        // The editor's recognition of the opened image reads the word under
        // the unknown blur: it has nowhere to be stored.
        let ed = app.editor.as_ref().unwrap();
        let (target, cut) = super::text_panel::recognition_history_target(ed);
        assert!(target.is_none(), "{target:?}");
        let doc = positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0));
        if let Some(target) = target {
            let _ = super::text_panel::store_recognized_text(
                &app.coordinator,
                &target,
                cut.as_ref(),
                Ok(doc),
            )
            .await;
        }
        // Nor does loading History recover anything.
        let listed = app.coordinator.history_list().unwrap();
        let _ = update(&mut app, Message::HistoryListLoaded(Ok(listed)));
        app.coordinator.flush_history_updates();
        let text = stored_record(&store, record.id).ocr_text;
        assert_eq!(text, None, "fail closed");
        // The new redaction is stored, but the list is never complete.
        let stored = stored_record(&store, record.id);
        assert_eq!(stored.redactions.len(), 1);
        assert!(!stored.redactions_complete);
        // Reopen that record and recognize again: its stored list must not
        // be trusted, in the original frame or another.
        let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
            store.root(),
            &stored,
        ))
        .unwrap()
        .to_rgba8();
        app.editor = Some(crate::editor::EditorSession::from_history(
            png,
            stored.clone(),
        ));
        let ed = app.editor.as_ref().unwrap();
        let (target, _) = super::text_panel::recognition_history_target(ed);
        assert!(target.is_none(), "{target:?}");
        // Even handed the record directly, History stores nothing.
        let doc = positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0));
        app.coordinator
            .record_recognized_text(&stored, &doc, true)
            .unwrap();
        assert_eq!(stored_record(&store, record.id).ocr_text, None);
        // The editor-side write of another frame: no text either.
        recognized(&mut app, readshot_core::document::ORIGINAL_PART, doc);
        let ed = app.editor.as_mut().unwrap();
        ed.model.rotate(true).unwrap();
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = stored_record(&store, record.id);
        assert!(stored.geometry_id.is_some());
        assert_eq!(stored.ocr_text, None);
        assert!(!stored.redactions_complete);
    }

    #[tokio::test]
    async fn a_complete_chain_of_scrubs_still_masks_recognized_text() {
        // A fresh capture scrubbed, then reopened and scrubbed again: every
        // redaction in the image is known, so recognition is stored masked.
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let img = solid(30, 20);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        let first = RectLike::new(2.0, 4.0, 10.0, 3.0);
        let ed = app.editor.as_mut().unwrap();
        ed.model.commit_annotation(word_blur(first));
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = stored_record(&store, record.id);
        assert!(stored.redacted && stored.redactions_complete);
        // Reopen, add a second redaction.
        let png = image::open(readshot_core::FsHistoryStore::abs_png_path(
            store.root(),
            &stored,
        ))
        .unwrap()
        .to_rgba8();
        app.editor = Some(crate::editor::EditorSession::from_history(png, stored));
        let second = RectLike::new(16.0, 12.0, 10.0, 3.0);
        let ed = app.editor.as_mut().unwrap();
        ed.model.commit_annotation(word_blur(second));
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = stored_record(&store, record.id);
        assert!(stored.redactions_complete);
        assert_eq!(stored.redactions.len(), 2);
        let ed = app.editor.as_ref().unwrap();
        let (target, cut) = super::text_panel::recognition_history_target(ed);
        let target = target.expect("a complete list: text may be stored");
        use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
        let line = |text: &str, rect: RectLike| TextLine {
            text: text.into(),
            rect: Some(rect),
            words: vec![WordRect {
                start: 0,
                end: text.chars().count(),
                rect,
            }],
        };
        let doc = TextDocument {
            lines: vec![line(&hidden_word(), first), line("abcdef", second)],
            positioned: true,
            layout_text: format!("{}\nabcdef", hidden_word()),
        };
        let _ = super::text_panel::store_recognized_text(
            &app.coordinator,
            &target,
            cut.as_ref(),
            Ok(doc),
        )
        .await;
        let text = stored_record(&store, record.id).ocr_text.unwrap();
        assert_eq!(text, format!("{}\n{}", "█".repeat(6), "█".repeat(6)));
    }

    #[test]
    fn copy_lines_masks_a_word_hidden_only_by_a_resized_redactions_fringe() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(30, 30));
        ed.model.scale_by(3.0, 3.0).unwrap();
        app.editor = Some(ed);
        // A word at x 20–21 of the capture: x 60–63 once tripled, just
        // right of the blur's exact rect (x 30–60) but under the resize's
        // fringe, which the exported image hides.
        recognized(
            &mut app,
            readshot_core::document::ORIGINAL_PART,
            positioned_doc("Zx9Qw7", RectLike::new(20.0, 10.0, 1.0, 10.0)),
        );
        let ed = app.editor.as_mut().unwrap();
        ed.model
            .commit_annotation(word_blur(RectLike::new(30.0, 30.0, 30.0, 30.0)));
        let drawn = ed.model.drawn_annotations();
        assert!(matches!(
            drawn.as_slice(),
            [Annotation::Blur { rect, .. }] if rect.x + rect.width > 61.5
        ));
        assert_eq!(
            super::text_panel::copy_lines_text(ed, &[0], false).as_deref(),
            Some("██████")
        );
    }

    #[tokio::test]
    async fn known_complete_redactions_still_mask_recognized_text() {
        // A reopened scrub whose stored redactions are known: the editor's
        // recognition is stored, masked with all of them.
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let (img, record) = reopened_scrubbed(&store);
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        let ed = app.editor.as_mut().unwrap();
        ed.model
            .commit_annotation(word_blur(RectLike::new(20.0, 10.0, 5.0, 5.0)));
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let ed = app.editor.as_ref().unwrap();
        let (target, cut) = super::text_panel::recognition_history_target(ed);
        let target = target.expect("redactions known: text may be stored");
        let _ = super::text_panel::store_recognized_text(
            &app.coordinator,
            &target,
            cut.as_ref(),
            Ok(positioned_doc(
                &hidden_word(),
                RectLike::new(2.0, 4.0, 10.0, 3.0),
            )),
        )
        .await;
        let text = stored_record(&store, record.id).ocr_text.unwrap();
        assert_eq!(text, "█".repeat(6));
    }

    #[test]
    fn a_rotated_reopened_scrub_never_stores_the_raw_text_of_its_flattened_image() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let (img, record) = reopened_scrubbed(&store);
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        recognized(
            &mut app,
            readshot_core::document::ORIGINAL_PART,
            positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0)),
        );
        let ed = app.editor.as_mut().unwrap();
        ed.model.rotate(true).unwrap();
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = stored_record(&store, record.id);
        assert!(stored.redacted);
        assert!(stored.geometry_id.is_some());
        let text = stored.ocr_text.unwrap_or_default();
        assert!(!text.contains(&hidden_word()), "{text}");
        assert_eq!(text, "█".repeat(6));
        // The inherited redaction, in the rotated frame.
        assert_eq!(
            stored.redactions,
            vec![word_blur(RectLike::new(13.0, 2.0, 3.0, 10.0))]
        );
    }

    #[test]
    fn redacting_then_undoing_on_a_reopened_scrub_never_stores_the_raw_text() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let (img, record) = reopened_scrubbed(&store);
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        recognized(
            &mut app,
            readshot_core::document::ORIGINAL_PART,
            positioned_doc(&hidden_word(), RectLike::new(2.0, 4.0, 10.0, 3.0)),
        );
        let ed = app.editor.as_mut().unwrap();
        ed.model
            .commit_annotation(word_blur(RectLike::new(20.0, 10.0, 5.0, 5.0)));
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = stored_record(&store, record.id);
        let text = stored.ocr_text.clone().unwrap_or_default();
        assert!(!text.contains(&hidden_word()), "{text}");
        assert_eq!(stored.redactions.len(), 2, "inherited + new");
        ed.model.undo();
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = stored_record(&store, record.id);
        assert!(stored.redacted);
        let text = stored.ocr_text.clone().unwrap_or_default();
        assert!(!text.contains(&hidden_word()), "{text}");
        assert_eq!(stored.redactions, record.redactions);
    }

    #[test]
    fn a_plain_sync_keeps_the_merged_text_of_a_combined_record() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let img = solid(20, 10);
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 20, 10, "d");
        record.ocr_text = Some("first".into());
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        recognized(
            &mut app,
            readshot_core::document::ORIGINAL_PART,
            positioned_doc("first", RectLike::new(1.0, 1.0, 10.0, 4.0)),
        );
        let ed = app.editor.as_mut().unwrap();
        let id = ed
            .model
            .add_part(solid(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        sync_editor_history(ed, &app.coordinator);
        recognized(
            &mut app,
            id,
            positioned_doc("second", RectLike::new(1.0, 1.0, 10.0, 4.0)),
        );
        app.coordinator.flush_history_updates();
        assert_eq!(
            stored_record(&store, record.id).ocr_text.as_deref(),
            Some("first\n\nsecond")
        );
        let ed = app.editor.as_mut().unwrap();
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(1.0, 1.0, 2.0, 2.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: Default::default(),
        });
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let stored = stored_record(&store, record.id);
        assert_eq!(stored.annotation_model.len(), 1);
        assert_eq!(stored.ocr_text.as_deref(), Some("first\n\nsecond"));
    }

    #[test]
    fn redact_after_a_cancelled_geometry_preview_places_rects_in_the_restored_frame() {
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        let email = format!("{}@{}", "priya", "example.com");
        let line = RectLike::new(2.0, 4.0, 20.0, 3.0);
        let mut ed = crate::editor::EditorSession::new(solid(30, 20));
        ed.text_panel.part_docs.insert(
            readshot_core::document::ORIGINAL_PART,
            Arc::new(positioned_doc(&email, line)),
        );
        // A geometry preview is open, and the panel was rebuilt for it.
        let baseline = ed.model.snapshot();
        let g = ed.model.geometry();
        let rotated = g.with_transform(g.transform.rotated_cw());
        ed.model.preview_geometry(&baseline, rotated).unwrap();
        ed.snapshot_drag_baseline = Some(baseline);
        refresh_text_for_geometry(&mut ed);
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorRedactAll);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.model.geometry().is_original(), "preview cancelled");
        let rects: Vec<RectLike> = ed
            .model
            .annotations()
            .iter()
            .filter_map(|a| match a {
                Annotation::Rectangle { rect, .. }
                | Annotation::Blur { rect, .. }
                | Annotation::Pixelate { rect, .. } => Some(*rect),
                _ => None,
            })
            .collect();
        assert_eq!(rects.len(), 1, "{rects:?}");
        let r = rects[0];
        assert!(
            r.x <= line.x + 1.0
                && r.y <= line.y
                && r.x + r.width >= line.x + line.width - 1.0
                && r.y + r.height >= line.y + line.height,
            "{r:?} misses the line {line:?}"
        );
    }

    #[test]
    fn a_late_failure_for_another_part_leaves_the_running_recognition_alone() {
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(20, 10));
        let session = ed.session_id;
        ed.text_panel.recognition = crate::editor::TextRecognition::Recognizing {
            since: std::time::Instant::now(),
        };
        ed.text_panel.recognizing_part = Some(5);
        ed.text_panel.user_requested = true;
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: readshot_core::document::ORIGINAL_PART,
                result: Err("late".into()),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(matches!(
            ed.text_panel.recognition,
            crate::editor::TextRecognition::Recognizing { .. }
        ));
        assert_eq!(ed.text_panel.recognizing_part, Some(5));
        assert!(ed.text_panel.user_requested);
    }

    #[test]
    fn a_redaction_follows_a_rotation_and_its_undo_in_history() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let img = solid(30, 20);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        let word = RectLike::new(2.0, 4.0, 10.0, 3.0);
        recognized(
            &mut app,
            readshot_core::document::ORIGINAL_PART,
            positioned_doc(&hidden_word(), word),
        );
        let ed = app.editor.as_mut().unwrap();
        // A: redact the word.
        ed.model.commit_annotation(word_blur(word));
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let a = stored_record(&store, record.id);
        assert!(a.redacted);
        assert_eq!(a.geometry_id, None);
        assert_eq!(a.ocr_text.as_deref(), Some("██████"));
        assert_eq!(a.redactions, vec![word_blur(word)]);
        // B: rotate; the redaction and the boxes follow.
        ed.model.rotate(true).unwrap();
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let b = stored_record(&store, record.id);
        assert!(b.redacted);
        assert!(b.geometry_id.is_some());
        assert_eq!(b.ocr_text.as_deref(), Some("██████"));
        assert_eq!(
            b.redactions,
            vec![word_blur(RectLike::new(13.0, 2.0, 3.0, 10.0))]
        );
        // Back to A.
        ed.model.undo();
        sync_editor_history(ed, &app.coordinator);
        app.coordinator.flush_history_updates();
        let back = stored_record(&store, record.id);
        assert_eq!(back.geometry_id, None);
        assert_eq!(back.ocr_text.as_deref(), Some("██████"));
        assert_eq!(back.redactions, vec![word_blur(word)]);
    }

    #[test]
    fn without_positions_a_rotated_capture_still_masks_its_own_text() {
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 20, 10, "d");
        let email = format!("{}@{}", "priya", "example.com");
        record.ocr_text = Some(format!("Email {email}"));
        let mut ed = crate::editor::EditorSession::from_history(solid(20, 10), record);
        ed.model
            .commit_annotation(word_blur(RectLike::new(0.0, 0.0, 4.0, 4.0)));
        ed.model.rotate(true).unwrap();
        let text = scrubbed_history_text(&ed, false).expect("the capture's own text, masked");
        assert!(!text.contains(&email), "{text}");
        assert!(text.starts_with("Email "), "{text}");
        // Another part with no text: nothing at all.
        ed.model
            .add_part(solid(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        assert_eq!(scrubbed_history_text(&ed, false), None);
    }

    #[test]
    fn rotate_and_resize_keys_map_only_when_not_typing() {
        use iced::keyboard::{Key, Modifiers};
        let cmd = command_modifiers();
        assert!(matches!(
            editor_key_message(Key::Character("[".into()), cmd, true),
            Some(Message::EditorRotate { clockwise: false })
        ));
        assert!(matches!(
            editor_key_message(Key::Character("]".into()), cmd, true),
            Some(Message::EditorRotate { clockwise: true })
        ));
        assert!(editor_key_message(Key::Character("]".into()), cmd, false).is_none());
        assert!(matches!(
            editor_key_message(Key::Character("i".into()), cmd | Modifiers::ALT, true),
            Some(Message::EditorResizeOpen)
        ));
        // Plain [ / ] still bump the width; plain I is still the eyedropper.
        assert!(matches!(
            editor_key_message(Key::Character("[".into()), Modifiers::empty(), true),
            Some(Message::EditorWidthBump(_))
        ));
        assert!(matches!(
            editor_key_message(Key::Character("i".into()), Modifiers::empty(), true),
            Some(Message::EditorEyedropperToggled)
        ));
    }

    #[test]
    fn rotate_moves_annotations_with_the_image_and_undoes() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(30, 20));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(2.0, 4.0, 10.0, 3.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (20, 30));
        match &ed.model.annotations()[0] {
            Annotation::Rectangle { rect, .. } => {
                assert_eq!(*rect, RectLike::new(13.0, 2.0, 3.0, 10.0))
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(ed.status.as_deref(), Some("Rotated right. ⌘Z to undo."));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (30, 20));
    }

    #[test]
    fn resize_draft_locks_the_ratio_in_px_and_percent() {
        let mut d = crate::editor::ResizeDraft::new((400, 200));
        assert_eq!((d.width.as_str(), d.height.as_str()), ("400", "200"));
        d.set_width("200");
        assert_eq!(d.height, "100");
        assert_eq!(d.target(), Some((200, 100)));
        d.set_unit(crate::editor::ResizeUnit::Percent);
        assert_eq!((d.width.as_str(), d.height.as_str()), ("50", "50"));
        d.lock = false;
        d.set_height("25");
        assert_eq!(d.target(), Some((200, 50)));
        d.set_width("abc");
        assert_eq!(d.target(), None);
        d.set_width("0");
        assert_eq!(d.target(), None, "at least 1 px");
    }

    #[test]
    fn resize_draft_unit_switches_round_trip_exactly() {
        use crate::editor::{ResizeDraft, ResizeUnit};
        let mut d = ResizeDraft::new((1000, 500));
        d.lock = false;
        d.set_width("333");
        d.set_height("167");
        d.set_unit(ResizeUnit::Percent);
        assert_eq!((d.width.as_str(), d.height.as_str()), ("33", "33"));
        assert_eq!(
            d.target(),
            Some((333, 167)),
            "the shown % stands for the px"
        );
        d.set_unit(ResizeUnit::Pixels);
        // Before, this came back as 330 x 165.
        assert_eq!((d.width.as_str(), d.height.as_str()), ("333", "167"));
        // A field typed after a switch means what it says.
        d.set_unit(ResizeUnit::Percent);
        d.set_width("50");
        assert_eq!(d.target(), Some((500, 167)));
    }

    #[test]
    fn resize_draft_lock_follows_the_original_aspect() {
        use crate::editor::{ResizeDraft, ResizeUnit};
        let mut d = ResizeDraft::new((1000, 501));
        d.set_unit(ResizeUnit::Percent);
        d.set_width("33.3");
        assert_eq!(d.height, "33");
        // Before, the height was the rounded "33" %: 165 px, off aspect.
        assert_eq!(d.target(), Some((333, 167)));
        d.set_unit(ResizeUnit::Pixels);
        assert_eq!((d.width.as_str(), d.height.as_str()), ("333", "167"));
        // Locking again re-syncs the height from the width at the
        // original aspect.
        let mut d = ResizeDraft::new((1000, 300));
        d.lock = false;
        d.set_width("333");
        d.set_height("10");
        d.set_lock(true);
        assert_eq!(d.height, "100");
        d.set_unit(ResizeUnit::Percent);
        d.set_unit(ResizeUnit::Pixels);
        assert_eq!((d.width.as_str(), d.height.as_str()), ("333", "100"));
        assert_eq!(d.target(), Some((333, 100)));
    }

    #[test]
    fn resize_apply_scales_the_document_and_refuses_oversize() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
        let _ = update(&mut app, Message::EditorResizeOpen);
        assert_eq!(
            app.editor.as_ref().unwrap().popover,
            Some(crate::editor::EditorPopover::Resize)
        );
        let _ = update(&mut app, Message::EditorResizeWidthChanged("20".into()));
        let _ = update(&mut app, Message::EditorResizeApply);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.effective_image_size(), (20, 10));
        assert_eq!(ed.popover, None);
        let _ = update(&mut app, Message::EditorResizeOpen);
        let _ = update(&mut app, Message::EditorResizeWidthChanged("70000".into()));
        let _ = update(&mut app, Message::EditorResizeApply);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.effective_image_size(), (20, 10), "unchanged");
        assert!(ed.status.as_deref().unwrap().starts_with("Couldn't resize"));
    }

    #[test]
    fn flip_mirrors_and_escape_closes_the_image_menu() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
        let _ = update(
            &mut app,
            Message::EditorPopoverToggled(crate::editor::EditorPopover::Image),
        );
        let _ = update(&mut app, Message::EditorTextCancel);
        assert_eq!(app.editor.as_ref().unwrap().popover, None);
        let _ = update(
            &mut app,
            Message::EditorFlip(readshot_core::document::FlipAxis::Horizontal),
        );
        assert!(
            app.editor
                .as_ref()
                .unwrap()
                .model
                .geometry()
                .transform
                .flip_h
        );
    }

    #[test]
    fn escape_closes_the_resize_dialog_and_drops_its_draft() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
        let _ = update(&mut app, Message::EditorResizeOpen);
        assert!(app.editor.as_ref().unwrap().resize_draft.is_some());
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.popover, None);
        assert!(ed.resize_draft.is_none());
    }

    #[test]
    fn a_geometry_error_reaches_the_user_as_a_status() {
        let s = geometry_error_status(
            "rotate",
            true,
            readshot_core::document::GeometryError::TooLarge {
                width: 40000,
                height: 10,
            },
        );
        assert!(s.starts_with("Couldn't rotate"), "{s}");
        assert!(s.contains("uncropped image would be 40000"), "{s}");
        let s = geometry_error_status(
            "flip",
            false,
            readshot_core::document::GeometryError::TooManyPixels { pixels: 9, max: 8 },
        );
        assert!(
            s.starts_with("Couldn't flip") && s.contains("too large"),
            "{s}"
        );
        assert!(!s.contains("uncropped"), "{s}");
        // With a crop, the pixel limit is the uncropped image's too.
        let s = geometry_error_status(
            "resize",
            true,
            readshot_core::document::GeometryError::TooManyPixels { pixels: 9, max: 8 },
        );
        assert!(s.starts_with("Couldn't resize"), "{s}");
        assert!(s.contains("the uncropped image would be 9 px"), "{s}");
    }

    #[test]
    fn rotating_a_redacted_capture_keeps_boxes_and_history_in_one_frame_through_undo() {
        use readshot_core::text_lines::{TextDocument, TextLine, WordRect};
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_positions_and_history(store.clone());
        let img = solid(30, 20);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 30, 20, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        let hidden = hidden_word();
        let shown = "visible";
        let word = RectLike::new(2.0, 4.0, 10.0, 3.0);
        let other = RectLike::new(2.0, 12.0, 10.0, 3.0);
        let line = |t: &str, r: RectLike| TextLine {
            text: t.into(),
            rect: Some(r),
            words: vec![WordRect {
                start: 0,
                end: t.chars().count(),
                rect: r,
            }],
        };
        recognized(
            &mut app,
            readshot_core::document::ORIGINAL_PART,
            TextDocument {
                lines: vec![line(&hidden, word), line(shown, other)],
                positioned: true,
                layout_text: format!("{hidden}\n{shown}"),
            },
        );
        app.editor
            .as_mut()
            .unwrap()
            .model
            .commit_annotation(word_blur(word));
        // The rotated frame masks exactly the hidden word, not its neighbour.
        let check = |app: &App, rotated: bool| {
            app.coordinator.flush_history_updates();
            let ed = app.editor.as_ref().unwrap();
            let doc = ed.text_panel.positioned_document().expect("boxes follow");
            let (r0, r1, size) = if rotated {
                (
                    RectLike::new(13.0, 2.0, 3.0, 10.0),
                    RectLike::new(5.0, 2.0, 3.0, 10.0),
                    (20, 30),
                )
            } else {
                (word, other, (30, 20))
            };
            assert_eq!(doc.lines[0].rect, Some(r0));
            assert_eq!(doc.lines[1].rect, Some(r1));
            assert_eq!(ed.image_size(), size);
            let stored = stored_record(&store, record.id);
            assert!(stored.redacted, "still a scrub");
            assert_eq!((stored.width_px, stored.height_px), size);
            let text = stored.ocr_text.expect("masked text");
            assert!(!text.contains(&hidden), "{text}");
            assert!(text.contains(shown), "neighbour stays readable: {text}");
            assert_eq!(stored.geometry_id.is_some(), rotated);
        };
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        check(&app, true);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        check(&app, false);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
        );
        check(&app, true);
    }

    #[test]
    fn rotate_drops_an_open_slider_preview_even_with_a_text_draft() {
        use readshot_core::beautify::BeautifyEdit;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.set_tool(readshot_ui::editor::ToolState::Text);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "draft".into(),
            edit_index: None,
        });
        app.editor = Some(ed);
        let start = app.editor.as_ref().unwrap().model.beautify().padding;
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(150)),
        );
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(ed.model.beautify().padding, start, "preview dropped");
        assert!(ed.snapshot_drag_baseline.is_none());
        while ed.model.undo() {
            assert_ne!(ed.model.beautify().padding, 150, "never baked into history");
        }
        assert_eq!(ed.model.beautify().padding, start);
    }

    #[test]
    fn copy_commits_an_open_slider_preview_and_the_text_draft() {
        use readshot_core::beautify::BeautifyEdit;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(64, 64));
        ed.model.set_tool(readshot_ui::editor::ToolState::Text);
        ed.pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(8.0, 9.0),
            content: "draft".into(),
            edit_index: None,
        });
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(
            &mut app,
            Message::EditorBeautifySliderPreview(BeautifyEdit::Padding(25)),
        );
        let _ = update(&mut app, Message::EditorCopyImageRequested);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.beautify().padding, 25, "output keeps the preview");
        assert_eq!(ed.model.undo_depth(), depth + 2, "preview and text");
        assert!(ed.snapshot_drag_baseline.is_none());
        assert_eq!(ed.model.annotations().len(), 1);
    }

    #[test]
    fn cmd_z_is_ignored_while_the_resize_dialog_is_open() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(32, 32));
        let id = iced::window::Id::unique();
        ed.window_id = Some(id);
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(2.0, 2.0, 10.0, 10.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorResizeOpen);
        let cmd = iced::keyboard::Modifiers::COMMAND;
        for mods in [cmd, cmd | iced::keyboard::Modifiers::SHIFT] {
            let _ = update(
                &mut app,
                Message::EditorKeyPressed {
                    window: id,
                    key: iced::keyboard::Key::Character("z".into()),
                    modifiers: mods,
                    status_ignored: false,
                },
            );
            assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
        }
    }

    #[test]
    fn reopening_resize_while_it_is_open_keeps_the_typed_draft() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
        let _ = update(&mut app, Message::EditorResizeOpen);
        let _ = update(&mut app, Message::EditorResizeWidthChanged("12".into()));
        let _ = update(&mut app, Message::EditorResizeOpen);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.resize_draft.as_ref().unwrap().width, "12");
        assert_eq!(ed.popover, Some(crate::editor::EditorPopover::Resize));
    }

    #[test]
    fn resizing_a_cropped_image_reports_the_exact_size() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(200, 150));
        ed.model.commit_annotation(Annotation::Crop {
            rect: RectLike::new(10.0, 10.0, 101.0, 77.0),
        });
        assert_eq!(ed.effective_image_size(), (101, 77));
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorResizeOpen);
        let _ = update(&mut app, Message::EditorResizeWidthChanged("999".into()));
        let _ = update(&mut app, Message::EditorResizeApply);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.effective_image_size(), (999, 762));
    }

    #[test]
    fn crop_drag_makes_a_draft_enter_applies_and_esc_cancels() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(100, 80)));
        app.editor
            .as_mut()
            .unwrap()
            .model
            .set_tool(readshot_ui::editor::ToolState::Crop);
        let crop = |r| {
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                Annotation::Crop { rect: r },
            ))
        };
        let _ = update(&mut app, crop(RectLike::new(10.0, 10.0, 40.0, 30.0)));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, Some(RectLike::new(10.0, 10.0, 40.0, 30.0)));
        assert_eq!(ed.model.undo_depth(), 0, "a draft is not an edit");
        assert_eq!(ed.effective_image_size(), (100, 80));
        // esc cancels only the draft.
        let _ = update(&mut app, Message::EditorTextCancel);
        assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
        let _ = update(&mut app, crop(RectLike::new(0.0, 0.0, 50.0, 40.0)));
        // ↵ arrives as EditorEditSelectedText (status_ignored Enter).
        let _ = update(&mut app, Message::EditorEditSelectedText);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None);
        assert_eq!(ed.effective_image_size(), (50, 40));
        assert_eq!(ed.model.undo_depth(), 1);
    }

    #[test]
    fn choosing_an_aspect_refits_the_draft_and_switching_tools_drops_it() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(100, 80)));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Crop,
            )),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                Annotation::Crop {
                    rect: RectLike::new(0.0, 0.0, 80.0, 40.0),
                },
            )),
        );
        let _ = update(
            &mut app,
            Message::EditorStyle(readshot_ui::editor::style::StyleMessage::CropAspect(
                readshot_ui::editor::style::CropAspect::Square,
            )),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().crop_draft,
            Some(RectLike::new(20.0, 0.0, 40.0, 40.0))
        );
        assert!(visible_option_controls(app.editor.as_ref().unwrap())
            .contains(&readshot_ui::editor::style::OptionControl::CropApply));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Select,
            )),
        );
        assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
    }

    fn crop_app_with_rect_selected() -> App {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(100, 80));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(5.0, 5.0)), Some(0));
        ed.crop_draft = Some(RectLike::new(10.0, 10.0, 40.0, 30.0));
        app.editor = Some(ed);
        app
    }

    #[test]
    fn esc_drops_the_crop_draft_before_the_selection() {
        let mut app = crop_app_with_rect_selected();
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None);
        assert_eq!(ed.model.selected_annotation(), Some(0), "one esc, one step");
        let _ = update(&mut app, Message::EditorTextCancel);
        assert_eq!(
            app.editor.as_ref().unwrap().model.selected_annotation(),
            None
        );
    }

    #[test]
    fn esc_during_a_crop_re_drag_cancels_the_drag_then_the_draft() {
        let mut app = crop_app_with_rect_selected();
        let id = iced::window::Id::unique();
        let ed = app.editor.as_mut().unwrap();
        ed.window_id = Some(id);
        ed.model.set_tool(readshot_ui::editor::ToolState::Crop);
        let draft = ed.crop_draft;
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::DragStarted),
        );
        let esc = |app: &mut App, canvas: readshot_ui::CanvasMessage| {
            // The canvas handles Escape first, then the key subscription
            // sees it captured.
            let _ = update(app, Message::EditorCanvas(canvas));
            let _ = update(
                app,
                Message::EditorKeyPressed {
                    window: id,
                    key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                    modifiers: iced::keyboard::Modifiers::empty(),
                    status_ignored: false,
                },
            );
        };
        esc(&mut app, readshot_ui::CanvasMessage::DragEscaped);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.canvas_drag_active, "the re-drag is cancelled");
        assert_eq!(ed.crop_draft, draft, "the previous draft is back");
        assert_eq!(ed.model.selected_annotation(), Some(0));
        esc(&mut app, readshot_ui::CanvasMessage::Cancelled);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None, "the second Esc cancels the draft");
        assert_eq!(ed.model.selected_annotation(), Some(0), "and only that");
    }

    #[test]
    fn esc_during_a_crop_re_drag_is_one_step_whichever_message_comes_first() {
        let mut app = crop_app_with_rect_selected();
        let id = iced::window::Id::unique();
        let ed = app.editor.as_mut().unwrap();
        ed.window_id = Some(id);
        ed.model.set_tool(readshot_ui::editor::ToolState::Crop);
        let draft = ed.crop_draft;
        let key = |app: &mut App, named| {
            let _ = update(
                app,
                Message::EditorKeyPressed {
                    window: id,
                    key: iced::keyboard::Key::Named(named),
                    modifiers: iced::keyboard::Modifiers::empty(),
                    status_ignored: false,
                },
            );
        };
        let canvas = |app: &mut App, m| {
            let _ = update(app, Message::EditorCanvas(m));
        };
        use iced::keyboard::key::Named;
        use readshot_ui::CanvasMessage;
        // Key first, then the canvas's message for the same Escape.
        canvas(&mut app, CanvasMessage::DragStarted);
        key(&mut app, Named::Escape);
        canvas(&mut app, CanvasMessage::DragEscaped);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.canvas_drag_active, "the re-drag is cancelled");
        assert_eq!(ed.crop_draft, draft, "the previous draft is back");
        assert_eq!(ed.escape_ended_drag, None, "nothing left to swallow");
        // A stale marker (its counterpart never came) can't swallow a
        // later Escape: another key clears it.
        canvas(&mut app, CanvasMessage::DragStarted);
        canvas(&mut app, CanvasMessage::DragEscaped);
        assert!(app.editor.as_ref().unwrap().escape_ended_drag.is_some());
        key(&mut app, Named::Shift);
        assert_eq!(app.editor.as_ref().unwrap().escape_ended_drag, None);
        // …and so does a press.
        canvas(&mut app, CanvasMessage::DragStarted);
        canvas(&mut app, CanvasMessage::DragEscaped);
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let _ = update(&mut app, Message::EditorPointerButton(false, None));
        assert_eq!(app.editor.as_ref().unwrap().escape_ended_drag, None);
        assert_eq!(app.editor.as_ref().unwrap().crop_draft, draft);
        // The next Esc (key first again) cancels the draft, and only that.
        key(&mut app, Named::Escape);
        canvas(&mut app, CanvasMessage::Cancelled);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None, "the next Esc cancels the draft");
        assert_eq!(ed.model.selected_annotation(), Some(0), "and only that");
    }

    #[test]
    fn esc_during_any_canvas_drag_ends_only_the_drag_in_either_order() {
        use iced::keyboard::key::Named;
        use readshot_ui::CanvasMessage;
        let rect0 = Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 20.0, 20.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        };
        for key_first in [false, true] {
            let mut app = crop_app_with_rect_selected();
            let id = iced::window::Id::unique();
            app.editor.as_mut().unwrap().window_id = Some(id);
            let esc = |app: &mut App, canvas: CanvasMessage| {
                let key = Message::EditorKeyPressed {
                    window: id,
                    key: iced::keyboard::Key::Named(Named::Escape),
                    modifiers: iced::keyboard::Modifiers::empty(),
                    status_ignored: false,
                };
                if key_first {
                    let _ = update(app, key);
                    let _ = update(app, Message::EditorCanvas(canvas));
                } else {
                    let _ = update(app, Message::EditorCanvas(canvas));
                    let _ = update(app, key);
                }
            };
            let draft = app.editor.as_ref().unwrap().crop_draft;

            // A shape draw, over a selection, a crop draft and a text
            // draft: the Esc ends the draw and keeps all three.
            let ed = app.editor.as_mut().unwrap();
            ed.model.set_tool(readshot_ui::editor::ToolState::Rectangle);
            ed.pending_text = Some(crate::editor::PendingText {
                origin: PointLike::new(50.0, 50.0),
                content: "typing".into(),
                edit_index: None,
            });
            let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
            esc(&mut app, CanvasMessage::DragEscaped);
            let ed = app.editor.as_ref().unwrap();
            assert!(!ed.canvas_drag_in_progress(), "key first: {key_first}");
            assert_eq!(ed.model.selected_annotation(), Some(0), "{key_first}");
            assert!(ed.pending_text.is_some(), "{key_first}");
            assert_eq!(ed.crop_draft, draft, "{key_first}");
            assert_eq!(ed.escape_ended_drag, None, "{key_first}");
            app.editor.as_mut().unwrap().pending_text = None;

            // A Select move: the Esc puts the annotation back and keeps
            // it selected (and the crop draft open).
            app.editor
                .as_mut()
                .unwrap()
                .model
                .set_tool(readshot_ui::editor::ToolState::Select);
            let _ = update(
                &mut app,
                Message::EditorCanvas(CanvasMessage::SelectPressed(PointLike::new(5.0, 5.0))),
            );
            let _ = update(
                &mut app,
                Message::EditorCanvas(CanvasMessage::SelectDragged(PointLike::new(15.0, 15.0))),
            );
            assert_ne!(app.editor.as_ref().unwrap().model.annotations()[0], rect0);
            esc(&mut app, CanvasMessage::DragEscaped);
            let ed = app.editor.as_ref().unwrap();
            assert!(!ed.canvas_drag_in_progress(), "{key_first}");
            assert_eq!(ed.model.annotations()[0], rect0, "moved back: {key_first}");
            assert_eq!(ed.model.selected_annotation(), Some(0), "{key_first}");
            assert_eq!(ed.crop_draft, draft, "{key_first}");

            // The next Esc does what it always did: the crop draft, then
            // the selection.
            esc(&mut app, CanvasMessage::Cancelled);
            let ed = app.editor.as_ref().unwrap();
            assert_eq!(ed.crop_draft, None, "{key_first}");
            assert_eq!(ed.model.selected_annotation(), Some(0), "{key_first}");
            esc(&mut app, CanvasMessage::Cancelled);
            let ed = app.editor.as_ref().unwrap();
            assert_eq!(ed.model.selected_annotation(), None, "{key_first}");
        }
    }

    #[test]
    fn esc_exits_the_eyedropper_before_the_crop_draft() {
        let mut app = crop_app_with_rect_selected();
        app.editor.as_mut().unwrap().eyedropper = Some(crate::editor::EyedropperState {
            loupe: None,
            target: Default::default(),
        });
        let _ = update(&mut app, Message::EditorTextCancel);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.eyedropper.is_none());
        assert!(ed.crop_draft.is_some());
    }

    #[test]
    fn undo_and_redo_only_cancel_an_open_crop_draft() {
        for redo in [false, true] {
            let mut app = crop_app_with_rect_selected();
            let msg = if redo {
                readshot_ui::ToolbarMessage::Redo
            } else {
                readshot_ui::ToolbarMessage::Undo
            };
            let _ = update(&mut app, Message::EditorToolbar(msg));
            let ed = app.editor.as_ref().unwrap();
            assert_eq!(ed.crop_draft, None);
            assert_eq!(ed.model.undo_depth(), 1, "history must not move");
        }
    }

    #[test]
    fn enter_does_not_apply_a_crop_while_a_text_draft_is_open() {
        let mut app = crop_app_with_rect_selected();
        app.editor.as_mut().unwrap().pending_text = Some(crate::editor::PendingText {
            origin: PointLike::new(1.0, 1.0),
            content: "typing".into(),
            edit_index: None,
        });
        let _ = update(&mut app, Message::EditorEditSelectedText);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.crop_draft.is_some());
        assert_eq!(ed.effective_image_size(), (100, 80));
    }

    #[test]
    fn applying_a_crop_is_one_undo_step() {
        let mut app = crop_app_with_rect_selected();
        let _ = update(&mut app, Message::EditorCropApply);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), 2);
        assert_eq!(ed.effective_image_size(), (40, 30));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.effective_image_size(), (100, 80));
        assert_eq!(ed.model.undo_depth(), 1);
    }

    #[test]
    fn a_crop_released_after_switching_tools_leaves_no_draft() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(100, 80)));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::SelectTool(
                readshot_ui::editor::ToolState::Rectangle,
            )),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                Annotation::Crop {
                    rect: RectLike::new(0.0, 0.0, 50.0, 40.0),
                },
            )),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, None);
        assert_eq!(ed.model.undo_depth(), 0);
    }

    #[test]
    fn copy_and_save_apply_an_open_crop_draft_first() {
        for save in [false, true] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            let mut ed = crate::editor::EditorSession::new(solid(100, 80));
            ed.crop_draft = Some(RectLike::new(0.0, 0.0, 50.0, 40.0));
            app.editor = Some(ed);
            let _ = update(
                &mut app,
                if save {
                    Message::EditorSaveRequested
                } else {
                    Message::EditorCopyImageRequested
                },
            );
            let ed = app.editor.as_ref().unwrap();
            assert_eq!(ed.crop_draft, None);
            assert_eq!(ed.effective_image_size(), (50, 40), "save={save}");
            assert_eq!(ed.model.undo_depth(), 1, "save={save}");
        }
    }

    #[test]
    fn an_image_menu_action_drops_the_crop_draft() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(100, 80));
        ed.crop_draft = Some(RectLike::new(0.0, 0.0, 50.0, 40.0));
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
    }

    #[test]
    fn an_aspect_change_with_a_draft_leaves_a_selected_committed_crop_alone() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(100, 80));
        let committed = RectLike::new(0.0, 0.0, 80.0, 40.0);
        ed.model
            .commit_annotation(Annotation::Crop { rect: committed });
        assert!(ed.model.selected_annotation().is_some());
        ed.crop_draft = Some(RectLike::new(0.0, 0.0, 60.0, 30.0));
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorStyle(readshot_ui::editor::style::StyleMessage::CropAspect(
                readshot_ui::editor::style::CropAspect::Square,
            )),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, Some(RectLike::new(15.0, 0.0, 30.0, 30.0)));
        assert_eq!(ed.model.undo_depth(), 1);
        assert_eq!(
            ed.model.annotations().first(),
            Some(&Annotation::Crop { rect: committed })
        );
    }

    #[test]
    fn enter_does_not_apply_a_crop_while_a_field_popover_is_open() {
        for popover in [
            crate::editor::EditorPopover::Color,
            crate::editor::EditorPopover::Resize,
        ] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            let mut ed = crate::editor::EditorSession::new(solid(100, 80));
            ed.crop_draft = Some(RectLike::new(0.0, 0.0, 50.0, 40.0));
            ed.popover = Some(popover);
            app.editor = Some(ed);
            let _ = update(&mut app, Message::EditorEditSelectedText);
            let ed = app.editor.as_ref().unwrap();
            assert!(ed.crop_draft.is_some(), "{popover:?}");
            assert_eq!(ed.effective_image_size(), (100, 80));
        }
    }

    // ---- Task 9: combine ----

    #[test]
    fn add_capture_below_hides_the_editor_and_appends_the_capture() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        assert!(
            matches!(app.combine_capture, Some(crate::app::CombineCapture { session: s, join: Join::Below, selected: false }) if s == session)
        );
        assert_eq!(
            app.pending_intent,
            Some(crate::app::CaptureIntent::CombineIntoEditor)
        );
        assert_eq!(
            overlay_auto_confirm_intent(&app),
            Some(crate::app::CaptureIntent::CombineIntoEditor)
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(32, 16))));
        let ed = app.editor.as_ref().expect("same editor");
        assert_eq!(ed.session_id, session, "no new editor opened");
        assert_eq!(ed.model.part_ids().len(), 2);
        assert_eq!(ed.image_size(), (64, 48));
        assert_eq!(
            ed.status.as_deref(),
            Some("Added capture below. ⌘Z to undo.")
        );
        assert!(app.combine_capture.is_none());
        assert!(app.pending_intent.is_none());
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 1);
    }

    #[test]
    fn a_failed_or_cancelled_combine_capture_restores_the_editor_unchanged() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Beside),
        );
        let _ = update(
            &mut app,
            Message::RegionCaptureCompleted(Err("boom".into())),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 1);
        assert!(ed
            .status
            .as_deref()
            .unwrap()
            .starts_with("Couldn't add the capture"));
        assert!(app.combine_capture.is_none());
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Beside),
        );
        let _ = update(&mut app, Message::OverlayCancelled);
        assert!(app.combine_capture.is_none());
        assert!(app.pending_intent.is_none());
    }

    #[test]
    fn a_file_image_is_added_below_and_a_stale_session_is_ignored() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(10, 10)));
        let session = app.editor.as_ref().unwrap().session_id;
        let img = Arc::new(solid(10, 5));
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session: session + 1000,
                result: Ok(Some(Arc::clone(&img))),
            },
        );
        assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 1);
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session,
                result: Ok(Some(img)),
            },
        );
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (10, 15));
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session,
                result: Ok(None),
            },
        );
        assert_eq!(
            app.editor.as_ref().unwrap().model.part_ids().len(),
            2,
            "a cancelled pick changes nothing"
        );
    }

    #[test]
    fn gap_slider_is_one_undo_step_and_alignment_moves_parts() {
        use readshot_core::document::{Align, Join};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(40, 10));
        ed.model.add_part(solid(20, 10), Join::Below).unwrap();
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        for g in [4, 8, 12] {
            let _ = update(&mut app, Message::EditorLayoutGapPreview(g));
        }
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 32));
        assert_eq!(ed.model.undo_depth(), depth + 1);
        let _ = update(&mut app, Message::EditorLayoutAlign(Align::End));
        let g = app.editor.as_ref().unwrap().model.geometry().clone();
        assert_eq!(g.layout().placements[1].x, 20);
    }

    #[test]
    fn an_oversized_addition_is_refused() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(image::RgbaImage::new(
            10, 30_000,
        )));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(
            &mut app,
            Message::RegionCaptureCompleted(Ok(image::RgbaImage::new(10, 5_000))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 1);
        assert!(ed.status.as_deref().unwrap().contains("32767"));
    }

    fn two_part_editor() -> crate::editor::EditorSession {
        let mut ed = crate::editor::EditorSession::new(solid(40, 10));
        ed.model
            .add_part(solid(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        ed.refresh_image();
        ed
    }

    /// An editor whose every resize / alignment / gap preview composes
    /// off the UI thread.
    fn async_compose(mut ed: crate::editor::EditorSession) -> crate::editor::EditorSession {
        ed.async_compose_min_pixels = 0;
        ed
    }

    /// The running compose's request, run here as its worker would.
    fn finish_compose(app: &App) -> Message {
        let ed = app.editor.as_ref().unwrap();
        let job = ed.compose_job.clone().expect("a compose is running");
        compose_result(ed.session_id, &job)
    }

    fn compose_result(session: u64, job: &crate::editor::ComposeRequest) -> Message {
        Message::EditorComposed {
            session,
            seq: job.seq,
            output: crate::editor::ComposedOutput::new(Some(super::compose_jobs::compose_with(
                job.next.clone(),
                job.base.clone(),
            ))),
        }
    }

    /// Run every compose the editor is waiting on, as their workers
    /// would, until none is left.
    fn finish_all(app: &mut App) {
        for _ in 0..16 {
            if app
                .editor
                .as_ref()
                .is_none_or(|ed| ed.compose_job.is_none())
            {
                return;
            }
            let landed = finish_compose(app);
            let _ = update(app, landed);
        }
        panic!("composes never settle");
    }

    fn handle_id(ed: &crate::editor::EditorSession) -> iced::widget::image::Handle {
        ed.image_handle.clone()
    }

    #[test]
    fn a_large_resize_composes_off_thread_and_lands_as_one_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        ed.model.commit_annotations(rect_at(5.0));
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let shown = handle_id(app.editor.as_ref().unwrap());
        let _ = update(&mut app, Message::EditorResizeOpen);
        let _ = update(&mut app, Message::EditorResizeWidthChanged("80".into()));
        let _ = update(&mut app, Message::EditorResizeApply);
        let ed = app.editor.as_ref().unwrap();
        // Nothing changes until the result lands: the previous image stays.
        assert!(ed.compose_job.is_some());
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth);
        assert_eq!(handle_id(ed), shown);
        assert_eq!(ed.popover, None);
        let landed = finish_compose(&app);
        let _ = update(&mut app, landed);
        let ed = app.editor.as_mut().unwrap();
        assert!(ed.compose_job.is_none());
        assert_eq!(ed.image_size(), (80, 40));
        assert_eq!(ed.model.undo_depth(), depth + 1, "one undo step");
        assert_ne!(handle_id(ed), shown);
        assert!(ed
            .status
            .as_deref()
            .unwrap()
            .starts_with("Resized to 80 × 40"));
        // The same document and pixels as the synchronous path.
        let mut sync = crate::editor::EditorSession::new(solid(40, 20));
        sync.model.commit_annotations(rect_at(5.0));
        assert!(sync.model.scale_by(2.0, 2.0).unwrap());
        assert_eq!(ed.model.annotations(), sync.model.annotations());
        assert_eq!(
            ed.model.geometry().transform,
            sync.model.geometry().transform
        );
        assert!(ed.model.flatten() == sync.model.flatten(), "flattened");
        assert!(*ed.model.base() == *sync.model.base(), "base");
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        // A large undo lands composed too.
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth);
    }

    fn rect_at(x: f32) -> Vec<readshot_core::Annotation> {
        vec![readshot_core::Annotation::Rectangle {
            rect: RectLike::new(x, 2.0, 6.0, 4.0),
            color: readshot_core::Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        }]
    }

    fn apply_resize_width(app: &mut App, width: &str) {
        let _ = update(app, Message::EditorResizeOpen);
        let _ = update(app, Message::EditorResizeWidthChanged(width.into()));
        let _ = update(app, Message::EditorResizeApply);
    }

    fn undo_msg() -> Message {
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo)
    }

    #[test]
    fn a_pending_resize_is_applied_before_the_next_edit_never_dropped() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        app.editor
            .as_mut()
            .unwrap()
            .model
            .commit_annotations(rect_at(5.0));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        // Another edit before the result lands: the resize is applied first.
        let ed = app.editor.as_mut().unwrap();
        ed.model.select_at(PointLike::new(6.0, 3.0));
        assert!(ed.model.selected_annotation().is_some());
        let _ = update(&mut app, Message::EditorDeleteSelected);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40), "resized");
        assert!(ed.model.annotations().is_empty(), "then deleted");
        assert_eq!(ed.model.undo_depth(), depth + 2);
        assert!(
            ed.compose_job.as_ref().is_none_or(|j| !j.kind.is_step()),
            "only the delete's re-render may run"
        );
        // Its late result changes nothing.
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert_eq!(ed.model.undo_depth(), depth + 2);
    }

    #[test]
    fn a_rotate_after_a_pending_resize_queues_behind_it() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        // ⌘] before the result lands: queued behind the resize, nothing
        // composed on the UI thread.
        let before = super::compose_jobs::composes_on_this_thread();
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        assert_eq!(super::compose_jobs::composes_on_this_thread(), before);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20), "nothing applied yet");
        assert_eq!(ed.model.undo_depth(), depth);
        assert_eq!(ed.compose_job.as_ref().map(|j| j.steps.len()), Some(2));
        // The resize lands; the rotate composes next.
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40), "resized");
        assert_eq!(ed.model.undo_depth(), depth + 1);
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 80), "resized, then rotated");
        assert_eq!(ed.model.undo_depth(), depth + 2);
    }

    #[test]
    fn a_canvas_press_that_lands_a_pending_resize_is_mapped_into_the_new_frame() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        // A redaction at x 5–11; after a 2× resize it sits at x 10–22.
        ed.model.commit_annotation(Annotation::Pixelate {
            rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        });
        ed.model.clear_selection();
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        // A Select press on the redaction in the image shown (x 8): it
        // lands the resize and selects what is under it in the new frame
        // (x 16). Before, the press was consumed and selected nothing.
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                8.0, 4.0,
            ))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40), "the resize landed");
        assert_eq!(ed.model.undo_depth(), depth + 1, "resize only");
        assert_eq!(ed.model.selected_annotation(), Some(0), "selected");
        assert!(ed.move_drag.is_some(), "a move begins");
        // The drag the canvas sent before the next view is in the old
        // frame too: +2 there is +4 here, the redaction moves in one frame.
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
                10.0, 4.0,
            ))),
        );
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectReleased),
        );
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 2, "resize, then move");
        assert_eq!(
            ed.model.annotations(),
            [Annotation::Pixelate {
                rect: RectLike::new(14.0, 4.0, 12.0, 8.0),
                block_size: 8.0,
            }]
        );
    }

    #[test]
    fn a_text_click_that_lands_a_pending_resize_opens_where_it_was_aimed() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        apply_resize_width(&mut app, "80");
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::RequestText(PointLike::new(
                8.0, 4.0,
            ))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert_eq!(
            ed.pending_text.as_ref().map(|t| t.origin),
            Some(PointLike::new(16.0, 8.0))
        );
    }

    #[test]
    fn a_press_that_lands_a_pending_combine_is_still_dropped() {
        // An added part is not an affine of the frame shown: the press
        // can't be mapped and is dropped, as before.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        ed.model.commit_annotation(Annotation::Pixelate {
            rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        });
        ed.model.clear_selection();
        app.editor = Some(ed);
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session,
                result: Ok(Some(Arc::new(ramp(20, 10)))),
            },
        );
        assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 1);
        for msg in [
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(8.0, 4.0)),
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(20.0, 4.0)),
            readshot_ui::CanvasMessage::SelectReleased,
        ] {
            let _ = update(&mut app, Message::EditorCanvas(msg));
        }
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 2, "the combine landed");
        assert_eq!(ed.model.selected_annotation(), None, "the press dropped");
        assert!(ed.move_drag.is_none());
        assert!(ed.press_remap.is_none());
        assert!(matches!(
            ed.model.annotations(),
            [Annotation::Pixelate { rect, .. }] if *rect == RectLike::new(5.0, 2.0, 6.0, 4.0)
        ));
    }

    #[test]
    fn a_drawing_press_that_lands_a_pending_resize_starts_no_drag() {
        use readshot_ui::CanvasMessage;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let seq = app.editor.as_ref().unwrap().drag_cancel_seq;
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert!(!ed.canvas_drag_active, "no drag started");
        assert_ne!(ed.drag_cancel_seq, seq, "the canvas drops its drag");
        // …which it reports on its next event.
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::Cancelled));
        // A click tool's commit (a pin) carries its point: it is mapped
        // into the new frame (the old image's (10, 10) is the new (5, 5)).
        apply_resize_width(&mut app, "40");
        let pin = |x, y| Annotation::NumberedPin {
            origin: PointLike::new(x, y),
            number: 1,
            color: Rgba::OPAQUE_BLACK,
        };
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::CommitAnnotation(pin(10.0, 10.0))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.annotations(), [pin(5.0, 5.0)]);
        assert_eq!(ed.model.undo_depth(), depth + 3, "two resizes, the pin");
        // One off the shown image can't be mapped: dropped.
        apply_resize_width(&mut app, "80");
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::CommitAnnotation(pin(60.0, 10.0))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert_eq!(ed.model.annotations().len(), 1, "dropped");
        // Pressed again with nothing pending: drawn as it is.
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::CommitAnnotation(pin(60.0, 10.0))),
        );
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 2);
    }

    #[test]
    fn a_redaction_drag_whose_press_landed_a_pending_resize_commits_nothing() {
        use readshot_ui::CanvasMessage;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        ed.model.set_tool(readshot_ui::editor::ToolState::Pixelate);
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        // A pixelate drag over x 5–11 of the old image: its press lands
        // the resize; its commit still arrives (old-frame points).
        let old_frame = Annotation::Pixelate {
            rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        };
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::CommitAnnotation(old_frame.clone())),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert!(ed.model.annotations().is_empty(), "no old-frame redaction");
        assert_eq!(ed.model.undo_depth(), depth + 1, "the resize only");
        assert!(!ed.canvas_drag_active);
        // The next drag, begun on the new frame, commits.
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::CommitAnnotation(old_frame.clone())),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().model.annotations(),
            [old_frame]
        );
    }

    #[test]
    fn a_dropped_drag_stays_dropped_when_its_window_loses_focus() {
        use readshot_ui::CanvasMessage;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        ed.model.set_tool(readshot_ui::editor::ToolState::Pixelate);
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let a = iced::window::Id::unique();
        let redaction = Annotation::Pixelate {
            rect: RectLike::new(5.0, 2.0, 6.0, 4.0),
            block_size: 4.0,
        };
        // The press lands the pending resize; its window then loses focus
        // before the release, and the old-frame commit arrives late.
        let _ = update(&mut app, Message::EditorPointerButton(true, Some(a)));
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        let _ = update(&mut app, Message::EditorPointerWindowUnfocused(a));
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::CommitAnnotation(redaction.clone())),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert!(
            ed.model.annotations().is_empty(),
            "the late commit is refused"
        );
        assert_eq!(ed.model.undo_depth(), depth + 1, "the resize only");
        // The next real drag commits.
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        let _ = update(
            &mut app,
            Message::EditorCanvas(CanvasMessage::CommitAnnotation(redaction.clone())),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().model.annotations(),
            [redaction]
        );
    }

    #[test]
    fn a_drag_whose_release_never_came_refuses_no_later_commit() {
        use readshot_ui::CanvasMessage;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
        let a = iced::window::Id::unique();
        let pin = Annotation::NumberedPin {
            origin: PointLike::new(5.0, 5.0),
            number: 1,
            color: Rgba::OPAQUE_BLACK,
        };
        let commit = |app: &mut App, a: Annotation| {
            let _ = update(
                app,
                Message::EditorCanvas(CanvasMessage::CommitAnnotation(a)),
            );
        };
        // A drag begins; its window closes before the release reaches us.
        let _ = update(&mut app, Message::EditorPointerButton(true, Some(a)));
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        let _ = update(&mut app, Message::WindowClosed(a));
        let ed = app.editor.as_mut().unwrap();
        assert!(ed.drag_started_in.is_none(), "its frame is forgotten");
        // The frame changes later (a resize).
        ed.canvas_drag_active = false;
        assert!(ed.model.scale_by(2.0, 2.0).unwrap());
        ed.refresh_image();
        commit(&mut app, pin.clone());
        assert_eq!(app.editor.as_ref().unwrap().model.annotations().len(), 1);
        // A pin never answers for a drag's frame, even one left behind.
        let ed = app.editor.as_mut().unwrap();
        ed.drag_started_in = Some(crate::editor::CanvasDragFrame::Live(Arc::new(
            readshot_core::document::DocGeometry::original(Arc::new(solid(1, 1))),
        )));
        ed.canvas_drag_active = true;
        commit(&mut app, pin);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.annotations().len(), 2);
        assert!(ed.drag_started_in.is_none());
    }

    #[test]
    fn each_resize_applies_in_order_and_foreign_results_are_ignored() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let first = finish_compose(&app);
        // A second Apply settles the first: both are kept, in order.
        apply_resize_width(&mut app, "60");
        let second = finish_compose(&app);
        let _ = update(&mut app, first);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40), "the first, settled");
        let foreign = compose_result(ed.session_id + 1, ed.compose_job.as_ref().unwrap());
        let _ = update(&mut app, foreign);
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
        let _ = update(&mut app, second);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (60, 30));
        assert_eq!(ed.model.undo_depth(), depth + 2);
    }

    #[test]
    fn cmd_z_right_after_apply_undoes_the_resize_itself() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        ed.model.commit_annotations(rect_at(5.0));
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        let _ = update(&mut app, undo_msg());
        let _ = update(&mut app, late);
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth, "the earlier step is kept");
        assert_eq!(ed.model.annotations().len(), 1);
        assert!(ed.model.can_redo());
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
        );
        finish_all(&mut app);
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
    }

    #[test]
    fn a_busy_action_after_apply_keeps_the_resize() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(two_part_editor()));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        // Add Image from File sets `busy` while its picker is open.
        let _ = update(&mut app, Message::EditorAddImageFromFile);
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert_eq!(ed.model.undo_depth(), depth + 1);
        // Copy Text too.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        let _ = update(&mut app, Message::EditorCopyTextRequested);
        let _ = update(&mut app, late);
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
    }

    #[test]
    fn a_text_draft_after_apply_is_in_the_new_frame_and_save_keeps_both() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        let request = Message::EditorCanvas(readshot_ui::CanvasMessage::RequestText(
            PointLike::new(50.0, 30.0),
        ));
        // The click that lands the resize was aimed at the old image: it
        // opens no draft.
        let _ = update(&mut app, request.clone());
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
        assert!(app.editor.as_ref().unwrap().pending_text.is_none());
        // Clicked again: the draft opens on the resized document.
        let _ = update(&mut app, request);
        assert!(app.editor.as_ref().unwrap().pending_text.is_some());
        let _ = update(&mut app, Message::EditorTextChanged("Hi".into()));
        let _ = update(&mut app, Message::EditorSaveRequested);
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40), "saved with the resize");
        assert_eq!(ed.model.undo_depth(), depth + 2);
        assert!(matches!(
            ed.model.annotations().last(),
            Some(Annotation::Text { origin, .. }) if *origin == PointLike::new(50.0, 30.0)
        ));
    }

    #[test]
    fn a_crop_draft_after_apply_is_in_the_new_frame() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        ed.model.set_tool(readshot_ui::editor::ToolState::Crop);
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        let rect = RectLike::new(10.0, 10.0, 50.0, 20.0);
        let drag = |app: &mut App| {
            for m in [
                readshot_ui::CanvasMessage::DragStarted,
                readshot_ui::CanvasMessage::CommitAnnotation(Annotation::Crop { rect }),
            ] {
                let _ = update(app, Message::EditorCanvas(m));
            }
        };
        // The press lands the resize: that drag was aimed at the old
        // image, so its commit is refused.
        drag(&mut app);
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (80, 40));
        assert_eq!(app.editor.as_ref().unwrap().crop_draft, None);
        // Dragged again, on the resized document.
        drag(&mut app);
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.crop_draft, Some(rect));
        assert_eq!(ed.image_size(), (80, 40));
        assert_eq!(ed.model.undo_depth(), depth + 1);
        let _ = update(&mut app, Message::EditorCropApply);
        assert_eq!(
            app.editor.as_ref().unwrap().effective_image_size(),
            (50, 20)
        );
    }

    /// History-backed 40×20 editor with a Blur redaction at (4, 4, 10, 10).
    fn redacted_history_editor() -> (tempfile::TempDir, Arc<readshot_core::FsHistoryStore>, App) {
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(40, 20);
        let record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 40, 20, "d");
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        let mut ed = async_compose(crate::editor::EditorSession::from_history(img, record));
        ed.model.commit_annotation(Annotation::Blur {
            rect: RectLike::new(4.0, 4.0, 10.0, 10.0),
            radius: 4.0,
        });
        ed.model.clear_selection();
        app.editor = Some(ed);
        (dir, store, app)
    }

    fn compose_stored_record(
        store: &readshot_core::FsHistoryStore,
        app: &App,
    ) -> readshot_core::CaptureRecord {
        app.coordinator.flush_history_updates();
        let id = app
            .editor
            .as_ref()
            .and_then(|ed| ed.source_record.as_ref())
            .map(|r| r.id);
        store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| Some(r.id) == id || id.is_none())
            .unwrap()
    }

    #[test]
    fn a_select_drag_around_a_landing_resize_keeps_redactions_in_the_new_frame() {
        let (_dir, store, mut app) = redacted_history_editor();
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let landed = finish_compose(&app);
        // Press on the blur in the frame shown (10, 10): it lands the
        // resize and is mapped into the new frame (20, 20), still on the
        // blur. The late result changes nothing; a view is built, and the
        // drag (new-frame points) moves the blur within the new frame.
        let canvas = |m| Message::EditorCanvas(m);
        let _ = update(
            &mut app,
            canvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                10.0, 10.0,
            ))),
        );
        assert_eq!(
            app.editor.as_ref().unwrap().model.selected_annotation(),
            Some(0)
        );
        let _ = update(&mut app, landed);
        built_a_view(&app);
        let _ = update(
            &mut app,
            canvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
                24.0, 20.0,
            ))),
        );
        let _ = update(&mut app, canvas(readshot_ui::CanvasMessage::SelectReleased));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert_eq!(ed.model.undo_depth(), depth + 2, "resize, then move");
        // The blur (4, 4, 10, 10) is (8, 8, 20, 20) resized, then moved.
        let new_frame = RectLike::new(12.0, 8.0, 20.0, 20.0);
        assert!(matches!(
            ed.model.annotations(),
            [Annotation::Blur { rect, .. }] if *rect == new_frame
        ));
        // History holds the resized image with the new-frame redaction, as
        // drawn: widened over the resize's fringe.
        let drawn = ed.model.geometry().redaction_cover(new_frame);
        assert!(drawn.x < new_frame.x && drawn.width > new_frame.width);
        let stored = compose_stored_record(&store, &app);
        assert_eq!((stored.width_px, stored.height_px), (80, 40));
        assert!(stored.redacted);
        assert!(matches!(
            stored.redactions.as_slice(),
            [Annotation::Blur { rect, .. }] if *rect == drawn
        ));
    }

    #[test]
    fn a_released_gap_drag_is_synced_to_history_when_the_window_closes() {
        let (_dir, store, mut app) = redacted_history_editor();
        let coord = app.coordinator.clone();
        let id = iced::window::Id::unique();
        {
            let ed = app.editor.as_mut().unwrap();
            ed.window_id = Some(id);
            ed.model
                .add_part(solid(20, 10), readshot_core::document::Join::Below)
                .unwrap();
            ed.refresh_image();
            write_editor_history(ed, &coord);
        }
        let before = compose_stored_record(&store, &app).geometry_id;
        let _ = update(&mut app, Message::EditorLayoutGapPreview(10));
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        assert!(app.editor.as_ref().unwrap().gap_commit_on_land);
        let _ = update(&mut app, Message::WindowClosed(id));
        assert!(app.editor.is_none());
        app.coordinator.flush_history_updates();
        let stored = store.list().unwrap().into_iter().next().unwrap();
        assert_ne!(stored.geometry_id, before, "the gap step reached History");
        assert_eq!((stored.width_px, stored.height_px), (40, 40));
    }

    #[test]
    fn a_released_gap_drag_is_synced_to_history_before_a_pin() {
        let (_dir, store, mut app) = redacted_history_editor();
        let coord = app.coordinator.clone();
        {
            let ed = app.editor.as_mut().unwrap();
            ed.model
                .add_part(solid(20, 10), readshot_core::document::Join::Below)
                .unwrap();
            ed.refresh_image();
            write_editor_history(ed, &coord);
        }
        let _ = update(&mut app, Message::EditorLayoutGapPreview(10));
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let _ = update(&mut app, Message::EditorPinRequested);
        assert!(app.editor.is_none());
        app.coordinator.flush_history_updates();
        let stored = store.list().unwrap().into_iter().next().unwrap();
        assert_eq!((stored.width_px, stored.height_px), (40, 40));
    }

    #[test]
    fn a_select_drag_after_a_gap_release_waits_for_a_press_on_the_settled_frame() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(two_part_editor());
        // In the lower part: it moves down with the gap.
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(2.0, 12.0, 6.0, 6.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        ed.model.clear_selection();
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(&mut app, Message::EditorLayoutGapPreview(10));
        let late = finish_compose(&app);
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        // The first press lands the gap. It was aimed at the frame shown
        // before (the rectangle at y 14), so it and the drag the canvas
        // sent with it (no view built since) are mapped with the lower
        // part, 10 px down: the rectangle moves 2 px right, in the settled
        // frame.
        select_drag(&mut app, (4.0, 14.0), (6.0, 14.0));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.geometry().gap, 10);
        assert_eq!(ed.model.undo_depth(), depth + 2, "gap, then move");
        assert!(matches!(
            ed.model.annotations(),
            [Annotation::Rectangle { rect, .. }] if *rect == RectLike::new(4.0, 22.0, 6.0, 6.0)
        ));
        // Once a view is built, the canvas's points are in the settled
        // frame and pass as they are.
        built_a_view(&app);
        // (Unselected, so the press moves it rather than grab a handle.)
        app.editor.as_mut().unwrap().model.clear_selection();
        select_drag(&mut app, (4.0, 24.0), (6.0, 24.0));
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 3, "and moved again");
        assert!(matches!(
            ed.model.annotations(),
            [Annotation::Rectangle { rect, .. }] if *rect == RectLike::new(6.0, 22.0, 6.0, 6.0)
        ));
    }

    #[test]
    fn a_same_batch_drag_over_another_part_moves_with_the_press_map() {
        // A gap step pending on two parts; the press (lower part) lands it
        // and is mapped 10 px down. The drag point the canvas sent with it
        // is over the upper part, which did not move: it must still go
        // through the press's map, so the move is the 10 px up the user
        // dragged, not 20.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(two_part_editor());
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(2.0, 12.0, 6.0, 6.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        ed.model.clear_selection();
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorLayoutGapPreview(10));
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        select_drag(&mut app, (4.0, 14.0), (4.0, 4.0));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.geometry().gap, 10);
        assert!(
            matches!(
                ed.model.annotations(),
                [Annotation::Rectangle { rect, .. }] if *rect == RectLike::new(2.0, 12.0, 6.0, 6.0)
            ),
            "{:?}",
            ed.model.annotations()
        );
    }

    #[test]
    fn a_press_after_a_same_batch_frame_change_is_mapped_or_dropped() {
        use readshot_ui::CanvasMessage;
        let outline = Annotation::Rectangle {
            rect: RectLike::new(2.0, 2.0, 10.0, 6.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        };
        let fresh = || {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            let mut ed = crate::editor::EditorSession::new(solid(40, 20));
            ed.model.commit_annotation(outline.clone());
            ed.model.clear_selection();
            ed.model.set_tool(readshot_ui::editor::ToolState::Select);
            app.editor = Some(ed);
            built_a_view(&app);
            app
        };
        let press = |app: &mut App| {
            // On the rectangle's left edge in the frame the view showed.
            let _ = update(
                app,
                Message::EditorCanvas(CanvasMessage::SelectPressed(PointLike::new(2.0, 5.0))),
            );
        };
        // A rotate handled earlier in the same batch (small document: no
        // pending step) turned the frame: the press is mapped with it and
        // selects the rectangle. Before, it passed unmapped and missed.
        let mut app = fresh();
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        press(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (20, 40));
        assert_eq!(ed.model.selected_annotation(), Some(0));
        // An image added in the same batch is not an affine of the frame
        // shown: the press is dropped (before, it passed unmapped).
        let mut app = fresh();
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session,
                result: Ok(Some(Arc::new(solid(20, 10)))),
            },
        );
        assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 2);
        press(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.selected_annotation(), None);
        assert!(ed.move_drag.is_none());
        // A drawing drag begun there is refused too, and its commit with it.
        let _ = update(&mut app, Message::EditorCanvas(CanvasMessage::DragStarted));
        assert!(matches!(
            app.editor.as_ref().unwrap().drag_started_in,
            Some(crate::editor::CanvasDragFrame::Dropped)
        ));
        // Once a view shows the new frame, a press works as it is.
        built_a_view(&app);
        press(&mut app);
        assert_eq!(
            app.editor.as_ref().unwrap().model.selected_annotation(),
            Some(0)
        );
    }

    #[test]
    fn a_remapped_press_hit_tests_handles_at_the_scale_it_was_made_at() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = async_compose(crate::editor::EditorSession::new(solid(40, 20)));
        ed.model.commit_annotation(Annotation::Rectangle {
            rect: RectLike::new(2.0, 2.0, 10.0, 6.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: Default::default(),
        });
        assert_eq!(ed.model.select_at(PointLike::new(2.0, 5.0)), Some(0));
        ed.model.set_tool(readshot_ui::editor::ToolState::Select);
        // Shown at 1 screen px per document px.
        ed.render_scale.set(1.0);
        app.editor = Some(ed);
        apply_resize_width(&mut app, "80");
        // 9 screen px below the bottom-right handle (12, 8): inside the
        // 14 px grab radius on the image shown. In the resized frame that
        // is 18 document px, which is still 9 screen px as pressed.
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectPressed(PointLike::new(
                12.0, 17.0,
            ))),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40));
        assert!(
            matches!(
                ed.move_drag.as_ref().map(|d| d.kind),
                Some(crate::editor::MoveDragKind::Resize(
                    readshot_ui::editor::ResizeHandle::SouthEast
                ))
            ),
            "the handle is grabbed (before: missed at the old scale)"
        );
    }

    /// What `view()` does to the canvas frame: points the canvas sends
    /// from now on are in the present frame.
    fn built_a_view(app: &App) {
        app.editor.as_ref().unwrap().note_canvas_built();
    }

    #[test]
    fn a_gap_tick_after_apply_settles_the_resize_first() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(two_part_editor()));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        apply_resize_width(&mut app, "80");
        let late = finish_compose(&app);
        let _ = update(&mut app, Message::EditorLayoutGapPreview(10));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (80, 40), "resized before the drag");
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(matches!(
            ed.compose_job.as_ref().map(|j| j.kind),
            Some(crate::editor::ComposeKind::GapPreview(10))
        ));
        let _ = update(&mut app, late);
        let gap = finish_compose(&app);
        let _ = update(&mut app, gap);
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 2);
        assert_eq!(ed.model.geometry().gap, 10);
    }

    #[test]
    fn a_large_alignment_lands_as_one_undo_step() {
        use readshot_core::document::Align;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(two_part_editor()));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(&mut app, Message::EditorLayoutAlign(Align::End));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.geometry().align, Align::Start);
        assert_eq!(ed.model.undo_depth(), depth);
        let landed = finish_compose(&app);
        let _ = update(&mut app, landed);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.geometry().layout().placements[1].x, 20);
        assert_eq!(ed.model.undo_depth(), depth + 1);
    }

    /// Two-part editor with a gap drag opened at gap 10 (composing).
    fn gap_drag_composing(app: &mut App) -> (usize, u64) {
        app.editor = Some(async_compose(two_part_editor()));
        let ed = app.editor.as_ref().unwrap();
        let (depth, session) = (ed.model.undo_depth(), ed.session_id);
        let _ = update(app, Message::EditorLayoutGapPreview(10));
        assert!(matches!(
            app.editor
                .as_ref()
                .unwrap()
                .compose_job
                .as_ref()
                .map(|j| j.kind),
            Some(crate::editor::ComposeKind::GapPreview(10))
        ));
        (depth, session)
    }

    #[test]
    fn large_gap_previews_coalesce_to_the_newest_value_and_commit_once() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, session) = gap_drag_composing(&mut app);
        let first = finish_compose(&app);
        // Ticks while it composes: throttled, then queued (one compose at
        // a time); the newest wins.
        for g in [14, 18, 22] {
            let _ = update(&mut app, Message::EditorLayoutGapPreview(g));
            let _ = update(&mut app, Message::EditorLayoutGapFlush { session });
        }
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.gap_queued, Some(22));
        assert_eq!(ed.image_size(), (40, 20), "still the previous image");
        let _ = update(&mut app, first);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 30), "gap 10 shown");
        assert_eq!(ed.model.undo_depth(), depth, "a preview, no step");
        assert!(matches!(
            ed.compose_job.as_ref().map(|j| j.kind),
            Some(crate::editor::ComposeKind::GapPreview(22))
        ));
        assert_eq!(ed.gap_queued, None);
        // Released while 22 composes: the commit waits for it.
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.gap_commit_on_land);
        assert_eq!(ed.model.undo_depth(), depth);
        let last = finish_compose(&app);
        let _ = update(&mut app, last.clone());
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 42));
        assert_eq!(ed.model.undo_depth(), depth + 1, "one step for the drag");
        assert!(ed.snapshot_drag_baseline.is_none());
        // A duplicate delivery changes nothing.
        let _ = update(&mut app, last);
        assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        finish_all(&mut app);
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 20));
    }

    #[test]
    fn a_cancelled_gap_drag_drops_its_running_compose() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, _) = gap_drag_composing(&mut app);
        let landed = finish_compose(&app);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        let _ = update(&mut app, landed);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth);
        assert!(ed.compose_job.is_none());
    }

    #[test]
    fn a_released_gap_drag_is_kept_when_another_edit_comes_first() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, _) = gap_drag_composing(&mut app);
        let late = finish_compose(&app);
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        assert!(app.editor.as_ref().unwrap().gap_commit_on_land);
        // Rotate before the compose lands: the released gap is kept as its
        // own step first (queued, its running compose reused), never
        // cancelled; the rotate queues behind it.
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let _ = update(&mut app, late);
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 2);
        assert_eq!(ed.model.geometry().gap, 10);
        assert_eq!(ed.image_size(), (30, 40));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        finish_all(&mut app);
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 30));
        // Undo right after a release (before landing) undoes the gap step.
        let (depth, _) = gap_drag_composing(&mut app);
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth);
        assert!(ed.model.can_redo(), "the gap step is on the redo stack");
        assert_eq!(ed.image_size(), (40, 20));
    }

    #[test]
    fn an_output_mid_resize_applies_the_resize_first() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(async_compose(crate::editor::EditorSession::new(solid(
            40, 20,
        ))));
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(&mut app, Message::EditorResizeOpen);
        let _ = update(&mut app, Message::EditorResizeWidthChanged("80".into()));
        let _ = update(&mut app, Message::EditorResizeApply);
        let late = finish_compose(&app);
        let coord = app.coordinator.clone();
        let ed = app.editor.as_mut().unwrap();
        settle_editor_for_output(ed, &coord);
        assert_eq!(ed.image_size(), (80, 40), "the output includes the resize");
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(ed.compose_job.is_none());
        // Its late result is ignored: still one step.
        let _ = update(&mut app, late);
        assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 1);
    }

    #[test]
    fn an_output_mid_compose_keeps_the_newest_gap_now() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, session) = gap_drag_composing(&mut app);
        let late = finish_compose(&app);
        let _ = update(&mut app, Message::EditorLayoutGapPreview(16));
        let _ = update(&mut app, Message::EditorLayoutGapFlush { session });
        let coord = app.coordinator.clone();
        let ed = app.editor.as_mut().unwrap();
        settle_editor_for_output(ed, &coord);
        assert_eq!(ed.image_size(), (40, 36), "the newest value, composed now");
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(ed.compose_job.is_none());
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 36));
        assert_eq!(ed.model.undo_depth(), depth + 1);
    }

    #[test]
    fn gap_ticks_inside_the_throttle_are_coalesced_not_recomposed() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(two_part_editor());
        let _ = update(&mut app, Message::EditorLayoutGapPreview(4));
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 24));
        // Within the throttle interval: kept, not composed.
        let _ = update(&mut app, Message::EditorLayoutGapPreview(8));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 24));
        assert_eq!(ed.layout_gap_pending, Some(8));
        // The trailing flush shows it.
        let session = ed.session_id;
        let _ = update(&mut app, Message::EditorLayoutGapFlush { session });
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 28));
        assert_eq!(ed.layout_gap_pending, None);
    }

    #[test]
    fn undo_mid_gap_drag_cancels_only_the_drag_and_held_ticks_are_ignored() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(two_part_editor());
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(&mut app, Message::EditorLayoutGapPreview(4));
        // The mouse is down: this is a held slider drag.
        let _ = update(&mut app, Message::EditorPointerButton(true, None));
        let _ = update(&mut app, Message::EditorLayoutGapPreview(9));
        let _ = update(
            &mut app,
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20), "back to the pre-drag gap");
        assert_eq!(ed.model.part_ids().len(), 2, "the part stays");
        assert_eq!(ed.model.undo_depth(), depth);
        assert_eq!(ed.layout_gap_pending, None);
        // The slider is still held: its ticks and the trailing flush do
        // nothing until release.
        let session = ed.session_id;
        let _ = update(&mut app, Message::EditorLayoutGapFlush { session });
        let _ = update(&mut app, Message::EditorLayoutGapPreview(12));
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 20));
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth);
        assert!(!ed.snapshot_drag_cancelled);
    }

    #[test]
    fn copy_keeps_a_coalesced_gap_value_as_one_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(two_part_editor());
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let _ = update(&mut app, Message::EditorLayoutGapPreview(4));
        let _ = update(&mut app, Message::EditorLayoutGapPreview(12));
        let _ = update(&mut app, Message::EditorCopyImageRequested);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 32), "the slider's value is output");
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(ed.snapshot_drag_baseline.is_none());
    }

    #[test]
    fn combining_a_rotated_document_maps_its_text_through_the_bake() {
        use readshot_core::document::{Join, ORIGINAL_PART};
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(30, 20));
        ed.text_panel.part_docs.insert(
            ORIGINAL_PART,
            Arc::new(positioned_doc("first", RectLike::new(2.0, 4.0, 10.0, 3.0))),
        );
        app.editor = Some(ed);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(20, 10))));
        let ed = app.editor.as_ref().unwrap();
        let ids = ed.model.part_ids();
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ORIGINAL_PART, "baked into a fresh part");
        let baked = ed.text_panel.part_docs.get(&ids[0]).expect("carried over");
        // The original's box, turned with the pixels (30×20 → 20×30).
        assert_eq!(
            baked.lines[0].rect,
            Some(RectLike::new(13.0, 2.0, 3.0, 10.0))
        );
        assert_eq!(
            ed.text_panel.recognizing_part,
            Some(ids[1]),
            "only the new part is recognized; never the turned pixels"
        );
    }

    #[test]
    fn a_bake_before_recognition_recognizes_the_source_part_upright() {
        use readshot_core::document::{Join, ORIGINAL_PART};
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = true;
        app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(20, 10))));
        let ed = app.editor.as_ref().unwrap();
        let ids = ed.model.part_ids();
        assert_eq!(
            ed.text_panel.recognizing_part,
            Some(ORIGINAL_PART),
            "the baked part's source, in its own upright pixels"
        );
        let _ = update(
            &mut app,
            Message::EditorTextRecognized {
                session,
                part: ORIGINAL_PART,
                result: Ok(positioned_doc("first", RectLike::new(2.0, 4.0, 10.0, 3.0))),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.text_panel.part_docs.contains_key(&ids[0]));
        assert_eq!(ed.text_panel.recognizing_part, Some(ids[1]));
        assert!(
            ed.text_panel.document().is_none(),
            "the new part is pending"
        );
    }

    #[tokio::test]
    async fn a_combined_record_stores_no_text_until_every_part_is_recognized() {
        use readshot_core::document::Join;
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(8, 4);
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        record.ocr_text = Some("original".into());
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        let ed = crate::editor::EditorSession::from_history(img, record.clone());
        let session = ed.session_id;
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(8, 4))));
        app.coordinator.flush_history_updates();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        assert_eq!((stored.width_px, stored.height_px), (8, 8));
        assert_eq!(stored.ocr_text, None, "no text while a part has none");
        let ids = app.editor.as_ref().unwrap().model.part_ids();
        for (part, text) in [(ids[0], "first"), (ids[1], "second")] {
            let _ = update(
                &mut app,
                Message::EditorTextRecognized {
                    session,
                    part,
                    result: Ok(readshot_core::text_lines::TextDocument::from_plain(text)),
                },
            );
        }
        app.coordinator.flush_history_updates();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        let text = stored.ocr_text.expect("every part recognized");
        assert!(text.contains("first") && text.contains("second"), "{text}");
    }

    #[tokio::test]
    async fn a_combined_scrubbed_record_keeps_no_partial_text() {
        use readshot_core::document::Join;
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(8, 4);
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        record.ocr_text = Some("██████".into());
        record.redacted = true;
        record.redactions = vec![Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 4.0, 4.0),
            radius: 4.0,
        }];
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(8, 4))));
        app.coordinator.flush_history_updates();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        assert_eq!((stored.width_px, stored.height_px), (8, 8));
        assert_eq!(
            stored.ocr_text, None,
            "the opened image's text alone isn't the combined document's"
        );
    }

    #[test]
    fn a_combine_capture_for_a_replaced_editor_is_dropped() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        // Another editor took over while the overlay was up.
        app.editor = Some(crate::editor::EditorSession::new(solid(16, 16)));
        let other = app.editor.as_ref().unwrap().session_id;
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(32, 16))));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.session_id, other);
        assert_eq!(ed.model.part_ids().len(), 1, "nothing leaks into it");
        assert_eq!(ed.image_size(), (16, 16));
        assert!(app.combine_capture.is_none());
    }

    #[test]
    fn an_overwritten_intent_still_returns_the_capture_to_the_editor() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        // A tray click meanwhile sets another intent.
        app.pending_intent = Some(crate::app::CaptureIntent::Editor);
        assert_eq!(
            overlay_auto_confirm_intent(&app),
            Some(crate::app::CaptureIntent::CombineIntoEditor)
        );
        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: Default::default(),
                rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
                intent: crate::app::CaptureIntent::Editor,
            },
        );
        assert_eq!(
            app.pending_intent,
            Some(crate::app::CaptureIntent::CombineIntoEditor)
        );
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 16))));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.session_id, session);
        assert_eq!(ed.model.part_ids().len(), 2);
    }

    #[test]
    fn no_display_or_a_retake_never_strands_a_combine_capture() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        app.last_region_display_id = Some("primary".to_string());
        app.last_regions.insert(
            "primary".to_string(),
            crate::app::LastRegion {
                rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
                display_scale: 1.0,
                display_bounds: None,
            },
        );
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RetakeLastRegionRequested);
        assert!(!app.capture_in_flight, "no retake while choosing a region");
        let _ = update(&mut app, Message::OpenOverlayRequested);
        let _ = update(&mut app, Message::OverlayDisplaysListed(Ok(Vec::new())));
        assert!(app.combine_capture.is_none());
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.status.as_deref(),
            Some("Couldn't add the capture — no display was detected.")
        );
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::OpenOverlayRequested);
        let _ = update(
            &mut app,
            Message::OverlayDisplaysListed(Err("backend unavailable".into())),
        );
        assert!(app.combine_capture.is_none());
        assert!(app
            .editor
            .as_ref()
            .unwrap()
            .status
            .as_deref()
            .unwrap()
            .starts_with("Couldn't add the capture"));
    }

    #[test]
    fn an_overlay_that_cannot_open_gives_the_editor_back() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        // Another capture started before the chained overlay request ran.
        app.capture_in_flight = true;
        let _ = update(&mut app, Message::OpenOverlayRequested);
        assert!(app.combine_capture.is_none());
        assert!(
            app.pending_intent.is_none(),
            "a stale combine intent would swallow the next capture"
        );
        assert!(app
            .editor
            .as_ref()
            .unwrap()
            .status
            .as_deref()
            .unwrap()
            .starts_with("Couldn't add the capture"));
    }

    const NO_DOC: PartBudget = PartBudget {
        doc_width: 0,
        doc_height: 0,
        gap: 0,
    };

    #[test]
    fn an_overlay_refused_for_the_welcome_gives_the_editor_back() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        app.welcome = crate::welcome::WelcomeState::Pending;
        let _ = update(&mut app, Message::OpenOverlayRequested);
        assert!(app.combine_capture.is_none());
        assert!(app.pending_intent.is_none());
    }

    #[test]
    fn an_overlay_closed_by_the_os_gives_the_editor_back() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        app.overlay_opening = false;
        let (a, b) = (window::Id::unique(), window::Id::unique());
        app.windows.register(a, WindowKind::Overlay);
        app.windows.register(b, WindowKind::Overlay);
        let _ = update(&mut app, Message::WindowClosed(a));
        assert!(app.combine_capture.is_some(), "another overlay is still up");
        let _ = update(&mut app, Message::WindowClosed(b));
        assert!(app.combine_capture.is_none());
        assert!(app.pending_intent.is_none());
    }

    #[test]
    fn closing_the_overlays_after_a_selection_keeps_the_combine_capture() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let id = window::Id::unique();
        app.windows.register(id, WindowKind::Overlay);
        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: Default::default(),
                rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
                intent: crate::app::CaptureIntent::CombineIntoEditor,
            },
        );
        // The close event lands before the capture request runs.
        let _ = update(&mut app, Message::WindowClosed(id));
        assert!(app.combine_capture.is_some());
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 16))));
        assert_eq!(app.editor.as_ref().unwrap().model.part_ids().len(), 2);
    }

    #[test]
    fn a_file_that_would_overflow_the_combined_document_is_refused_before_decoding() {
        // 20000 × 10000 is fine alone; below a 20000 × 10000 document it
        // is 400 MP.
        let budget = PartBudget {
            doc_width: 20_000,
            doc_height: 10_000,
            gap: 0,
        };
        let err = decode_part_image(&png_header_only(20_000, 10_000), budget).unwrap_err();
        assert!(err.contains("combined"), "{err}");
        let tall = PartBudget {
            doc_width: 10,
            doc_height: 30_000,
            gap: 40,
        };
        let err = decode_part_image(&png_header_only(10, 3_000), tall).unwrap_err();
        assert!(err.contains("32767"), "{err}");
        // The budget follows the document: a rotated one counts as baked.
        let mut ed = crate::editor::EditorSession::new(solid(30, 20));
        ed.model.rotate(true).unwrap();
        assert_eq!(
            PartBudget::below(ed.model.geometry()),
            PartBudget {
                doc_width: 20,
                doc_height: 30,
                gap: 0
            }
        );
    }

    #[test]
    fn only_regular_files_are_read() {
        let dir = tempfile::TempDir::new().unwrap();
        let fake = dir.path().join("x.png");
        std::fs::create_dir(&fake).unwrap();
        let err = load_part_file(&fake, NO_DOC).unwrap_err();
        assert_eq!(err, "that isn't an image file");
        let real = dir.path().join("y.png");
        std::fs::write(&real, readshot_core::encode_png(&solid(3, 2)).unwrap()).unwrap();
        assert_eq!(load_part_file(&real, NO_DOC).unwrap().dimensions(), (3, 2));
    }

    #[test]
    fn a_bake_source_no_state_can_reach_is_released() {
        use readshot_core::document::Join;
        let undo = || Message::EditorToolbar(readshot_ui::ToolbarMessage::Undo);
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(20, 10))));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 2, "combined");
        assert_eq!(ed.text_panel.bake_sources.len(), 1, "baked");
        let source = Arc::downgrade(ed.text_panel.bake_sources.values().next().unwrap());
        // Undone, the bake is still on the redo stack: kept.
        let _ = update(&mut app, undo());
        let _ = update(&mut app, undo());
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 1);
        assert_eq!(
            ed.text_panel.bake_sources.len(),
            1,
            "redo can still reach it"
        );
        // A new edit truncates the redo stack: nothing reaches the baked
        // part, so its source (a whole earlier document) is released.
        let _ = update(&mut app, Message::EditorRotate { clockwise: false });
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.model.can_redo());
        assert!(ed.text_panel.bake_sources.is_empty(), "source kept");
        assert!(source.upgrade().is_none(), "source geometry still alive");
    }

    #[test]
    fn a_bake_of_a_bake_keeps_the_inner_source() {
        use readshot_core::document::Join;
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
        for _ in 0..2 {
            let _ = update(&mut app, Message::EditorRotate { clockwise: true });
            let _ = update(
                &mut app,
                Message::EditorCombineCaptureRequested(Join::Below),
            );
            let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(20, 10))));
        }
        let ed = app.editor.as_ref().unwrap();
        // The second bake's source holds the first baked part, whose own
        // source must stay to place the original's text.
        assert_eq!(ed.text_panel.bake_sources.len(), 2);
        assert!(
            super::text_panel::original_part_affine(&ed.text_panel, ed.model.geometry()).is_some()
        );
    }

    #[test]
    fn a_baked_part_is_never_recognized_in_its_own_pixels() {
        use readshot_core::document::Join;
        let mut app = build_app_with_positions(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = true;
        app.editor = Some(crate::editor::EditorSession::new(solid(30, 20)));
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(20, 10))));
        let ed = app.editor.as_mut().unwrap();
        let ids = ed.model.part_ids();
        // The source can't be placed (say, its geometry lost the original):
        // the baked part then has no text rather than OCR of turned pixels.
        let src = ed.text_panel.bake_sources.get(&ids[0]).cloned().unwrap();
        let mut broken = (*src).clone();
        broken.parts.clear();
        ed.text_panel.bake_sources.insert(ids[0], Arc::new(broken));
        ed.text_panel.recognition = crate::editor::TextRecognition::Idle;
        ed.text_panel.recognizing_part = None;
        ed.text_panel.part_docs.insert(
            ids[1],
            Arc::new(positioned_doc("x", RectLike::new(0.0, 0.0, 2.0, 2.0))),
        );
        let _ = request_text_recognition(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.text_panel.recognizing_part, None);
        assert!(!ed.text_panel.part_docs.contains_key(&ids[0]));
    }

    #[tokio::test]
    async fn a_scrubbed_record_keeps_its_redactions_through_a_bake() {
        use readshot_core::document::Join;
        let dir = tempfile::TempDir::new().unwrap();
        let store = Arc::new(readshot_core::FsHistoryStore::new(dir.path().join("h")));
        let mut app = build_app_with_history(Arc::new(FakePermissions::granted()), store.clone());
        let img = solid(8, 4);
        let mut record = readshot_core::CaptureRecord::new(chrono::Utc::now(), 8, 4, "d");
        record.ocr_text = Some("██████".into());
        record.redacted = true;
        record.redactions = vec![Annotation::Blur {
            rect: RectLike::new(0.0, 0.0, 4.0, 2.0),
            radius: 4.0,
        }];
        record.redactions_complete = true;
        store
            .save(&record, &readshot_core::encode_png(&img).unwrap())
            .unwrap();
        app.editor = Some(crate::editor::EditorSession::from_history(
            img,
            record.clone(),
        ));
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(4, 4))));
        assert_ne!(
            app.editor.as_ref().unwrap().model.part_ids()[0],
            readshot_core::document::ORIGINAL_PART,
            "baked"
        );
        app.coordinator.flush_history_updates();
        let stored = store
            .list()
            .unwrap()
            .into_iter()
            .find(|r| r.id == record.id)
            .unwrap();
        assert!(stored.redacted);
        // The blur over the original's top-left 4 × 2, turned clockwise
        // with the 8 × 4 image: the right edge's top 2 × 4.
        assert_eq!(
            stored.redactions,
            vec![Annotation::Blur {
                rect: RectLike::new(2.0, 0.0, 2.0, 4.0),
                radius: 4.0,
            }]
        );
    }

    #[test]
    fn an_overlay_request_during_the_combine_capture_never_replaces_the_editor() {
        use readshot_core::document::Join;
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorCombineCaptureRequested(Join::Below),
        );
        app.overlay_opening = false;
        let id = window::Id::unique();
        app.windows.register(id, WindowKind::Overlay);
        let display_id: readshot_capture::DisplayId = Default::default();
        let rect = readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap();
        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: display_id.clone(),
                rect,
                intent: crate::app::CaptureIntent::CombineIntoEditor,
            },
        );
        let _ = update(
            &mut app,
            Message::CaptureRegionRequested { display_id, rect },
        );
        assert!(app.capture_in_flight);
        // A readshot:// NewCapture or History's Capture button meanwhile.
        let _ = update(&mut app, Message::OpenOverlayRequested);
        assert!(app.combine_capture.is_some(), "the combine is in flight");
        assert_eq!(
            app.pending_intent,
            Some(crate::app::CaptureIntent::CombineIntoEditor)
        );
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 16))));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.session_id, session, "never a new editor");
        assert_eq!(ed.model.part_ids().len(), 2);
        assert!(ed.source_record.is_none(), "no History record made");
        assert!(app.combine_capture.is_none());
    }

    // ---- Task 9b: the combine's own selection flag ----

    /// Run a scroll capture through its real start (an overlay selection
    /// with the scroll intent), then end its session as a stitch does.
    fn app_after_a_scroll_capture() -> App {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let overlay = window::Id::unique();
        app.windows.register(overlay, WindowKind::Overlay);
        app.overlay_displays.insert(
            overlay,
            crate::app::OverlayDisplay {
                display_id: "display-a".into(),
                scale: 2.0,
                origin_x: 0.0,
                origin_y: 0.0,
                width: 800.0,
                height: 600.0,
            },
        );
        app.pending_intent = Some(crate::app::CaptureIntent::ScrollCapture);
        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: "display-a".into(),
                rect: readshot_core::geom::Rect::from_xywh(10.0, 10.0, 200.0, 100.0).unwrap(),
                intent: crate::app::CaptureIntent::ScrollCapture,
            },
        );
        let s = app.scroll_session.take().expect("scroll session started");
        for id in [s.hud_window_id, s.region_window_id].into_iter().flatten() {
            app.windows.forget(id);
        }
        assert!(!app.windows.kinds().any(|k| k == WindowKind::Overlay));
        app
    }

    fn start_add_capture(app: &mut App) {
        app.editor = Some(crate::editor::EditorSession::new(solid(64, 32)));
        let _ = update(
            app,
            Message::EditorCombineCaptureRequested(readshot_core::document::Join::Below),
        );
        assert!(app.combine_capture.is_some(), "Add Capture started");
    }

    #[test]
    fn a_finished_scroll_capture_leaves_no_pending_display() {
        let app = app_after_a_scroll_capture();
        assert_eq!(app.pending_display_id, None);
        assert_eq!(app.pending_display_scale, None);
        assert_eq!(app.pending_intent, None);
    }

    #[test]
    fn add_capture_after_a_scroll_capture_is_never_stranded() {
        // The overlay is refused (welcome showing): the editor comes back.
        let mut app = app_after_a_scroll_capture();
        start_add_capture(&mut app);
        app.welcome = crate::welcome::WelcomeState::Pending;
        let _ = update(&mut app, Message::OpenOverlayRequested);
        assert!(app.combine_capture.is_none(), "welcome refusal restores");
        assert!(app.pending_intent.is_none());

        // The overlay is closed by the OS before any selection.
        let mut app = app_after_a_scroll_capture();
        start_add_capture(&mut app);
        app.overlay_opening = false;
        let id = window::Id::unique();
        app.windows.register(id, WindowKind::Overlay);
        let _ = update(&mut app, Message::WindowClosed(id));
        assert!(app.combine_capture.is_none(), "OS close restores");
        assert!(app.pending_intent.is_none());

        // The user cancels the overlay.
        let mut app = app_after_a_scroll_capture();
        start_add_capture(&mut app);
        let _ = update(&mut app, Message::OverlayCancelled);
        assert!(app.combine_capture.is_none(), "cancel restores");
    }

    #[test]
    fn a_combine_with_no_selection_restores_despite_a_stale_pending_display() {
        // Another flow left the shared pending display set; the combine
        // has made no selection, so its safety nets still fire.
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        start_add_capture(&mut app);
        app.pending_display_id = Some("display-a".into());
        app.welcome = crate::welcome::WelcomeState::Pending;
        let _ = update(&mut app, Message::OpenOverlayRequested);
        assert!(app.combine_capture.is_none(), "welcome refusal restores");

        let mut app = build_app(Arc::new(FakePermissions::granted()));
        start_add_capture(&mut app);
        app.pending_display_id = Some("display-a".into());
        app.overlay_opening = false;
        let id = window::Id::unique();
        app.windows.register(id, WindowKind::Overlay);
        let _ = update(&mut app, Message::WindowClosed(id));
        assert!(app.combine_capture.is_none(), "OS close restores");

        let mut app = build_app(Arc::new(FakePermissions::granted()));
        start_add_capture(&mut app);
        app.pending_display_id = Some("display-a".into());
        let _ = abort_combine_capture(&mut app, None);
        assert!(app.combine_capture.is_none(), "abort restores");
    }

    /// An Add Capture whose overlay selection is made (its capture in
    /// flight), with the overlay window still registered. Returns the
    /// editor session and the overlay id.
    fn app_with_a_selected_combine() -> (App, u64, window::Id) {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        start_add_capture(&mut app);
        let session = app.editor.as_ref().unwrap().session_id;
        app.overlay_opening = false;
        let id = window::Id::unique();
        app.windows.register(id, WindowKind::Overlay);
        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: Default::default(),
                rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
                intent: crate::app::CaptureIntent::CombineIntoEditor,
            },
        );
        assert!(app.combine_capture.is_some_and(|cc| cc.selected));
        (app, session, id)
    }

    /// The in-flight capture still lands in the same editor.
    fn assert_the_combine_capture_lands(mut app: App, session: u64) {
        let _ = update(&mut app, Message::RegionCaptureCompleted(Ok(solid(64, 16))));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.session_id, session);
        assert_eq!(ed.model.part_ids().len(), 2);
        assert!(app.combine_capture.is_none());
    }

    #[test]
    fn a_stray_cancel_after_a_combine_selection_keeps_the_capture() {
        let (mut app, session, _) = app_with_a_selected_combine();
        app.pending_display_scale = Some(2.0);
        app.pending_hide_cursor = false;
        // A stray cancel leaves the in-flight capture's pending state be.
        let _ = update(&mut app, Message::OverlayCancelled);
        assert!(app.combine_capture.is_some(), "cancel after selection");
        assert_eq!(app.pending_display_scale, Some(2.0));
        assert!(!app.pending_hide_cursor, "the show-cursor choice survives");
        assert_eq!(
            app.pending_intent,
            Some(crate::app::CaptureIntent::CombineIntoEditor)
        );
        assert_the_combine_capture_lands(app, session);
    }

    #[test]
    fn an_os_close_after_a_combine_selection_keeps_the_capture() {
        let (mut app, session, id) = app_with_a_selected_combine();
        assert!(!app.capture_in_flight, "only the selection guards it");
        let _ = update(&mut app, Message::WindowClosed(id));
        assert!(app.combine_capture.is_some(), "OS close after selection");
        assert_the_combine_capture_lands(app, session);
    }

    #[test]
    fn the_welcome_after_a_combine_selection_keeps_the_capture() {
        let (mut app, session, id) = app_with_a_selected_combine();
        // The overlay is gone and the capture not started yet: only the
        // selection keeps the welcome refusal from restoring the editor.
        app.windows.forget(id);
        assert!(!app.capture_in_flight);
        app.welcome = crate::welcome::WelcomeState::Pending;
        let _ = update(&mut app, Message::OpenOverlayRequested);
        assert!(app.combine_capture.is_some(), "welcome after selection");
        assert_the_combine_capture_lands(app, session);
    }

    #[test]
    fn an_abort_after_a_combine_selection_keeps_the_capture() {
        let (mut app, session, _) = app_with_a_selected_combine();
        let _ = abort_combine_capture(&mut app, None);
        assert!(app.combine_capture.is_some(), "abort after selection");
        assert_the_combine_capture_lands(app, session);
    }

    #[test]
    fn a_selected_combine_capture_that_fails_gives_the_editor_back() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        start_add_capture(&mut app);
        let _ = update(
            &mut app,
            Message::OverlaySelected {
                display_id: Default::default(),
                rect: readshot_core::geom::Rect::from_xywh(0.0, 0.0, 10.0, 10.0).unwrap(),
                intent: crate::app::CaptureIntent::CombineIntoEditor,
            },
        );
        let _ = update(
            &mut app,
            Message::RegionCaptureCompleted(Err("boom".into())),
        );
        assert!(app.combine_capture.is_none());
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 1);
        assert!(ed
            .status
            .as_deref()
            .unwrap()
            .starts_with("Couldn't add the capture"));
    }

    fn png_header_only(w: u32, h: u32) -> Vec<u8> {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in bytes {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xEDB8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
            }
            !c
        }
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut chunk = b"IHDR".to_vec();
        chunk.extend_from_slice(&w.to_be_bytes());
        chunk.extend_from_slice(&h.to_be_bytes());
        chunk.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut push = |kind: &[u8], data: &[u8]| {
            let mut chunk = kind.to_vec();
            chunk.extend_from_slice(data);
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&chunk);
            out.extend_from_slice(&crc32(&chunk).to_be_bytes());
        };
        push(b"IHDR", &chunk[4..]);
        // A few bytes of (bogus) data: the header is all that's read.
        push(b"IDAT", &[0x78, 0x9C, 0x03, 0x00]);
        push(b"IEND", &[]);
        out
    }

    #[test]
    fn a_picked_file_is_decoded_as_untrusted_input() {
        let ok = readshot_core::encode_png(&solid(3, 2)).unwrap();
        assert_eq!(decode_part_image(&ok, NO_DOC).unwrap().dimensions(), (3, 2));
        let err = decode_part_image(b"not an image at all", NO_DOC).unwrap_err();
        assert!(err.contains("isn't an image"), "{err}");
        let mut truncated = ok.clone();
        truncated.truncate(ok.len() / 2);
        assert!(decode_part_image(&truncated, NO_DOC).is_err());
        // Refused from the header alone, before any pixel is allocated.
        let err = decode_part_image(&png_header_only(40_000, 1), NO_DOC).unwrap_err();
        assert!(err.contains("32767"), "{err}");
        let err = decode_part_image(&png_header_only(20_000, 20_000), NO_DOC).unwrap_err();
        assert!(err.contains("256000000"), "{err}");
        let err = decode_part_image(&png_header_only(0, 5), NO_DOC).unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn a_file_that_fails_to_load_leaves_the_editor_unchanged() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let mut ed = crate::editor::EditorSession::new(solid(10, 10));
        ed.busy = true;
        let session = ed.session_id;
        app.editor = Some(ed);
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session,
                result: Err("that file isn't an image Readshot can read".into()),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.busy);
        assert_eq!(ed.model.part_ids().len(), 1);
        assert_eq!(
            ed.status.as_deref(),
            Some("Couldn't add the image — that file isn't an image Readshot can read.")
        );
    }

    // ===== D1: no UI-thread compose or render at large sizes =====

    /// A non-uniform image, so a turned or flipped document differs.
    fn ramp(w: u32, h: u32) -> image::RgbaImage {
        image::RgbaImage::from_fn(w, h, |x, y| {
            image::Rgba([(x * 6) as u8, (y * 11) as u8, (x ^ y) as u8, 255])
        })
    }

    #[test]
    fn a_dragged_redaction_shows_its_real_effect_when_the_document_renders_here() {
        use readshot_ui::CanvasMessage;
        let pixelate = Annotation::Pixelate {
            rect: RectLike::new(2.0, 2.0, 10.0, 8.0),
            block_size: 4.0,
        };
        for large in [false, true] {
            let mut app = build_app(Arc::new(FakePermissions::granted()));
            let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
            ed.model.commit_annotation(pixelate.clone());
            ed.model.set_tool(readshot_ui::editor::ToolState::Select);
            ed.refresh_image();
            app.editor = Some(if large { async_compose(ed) } else { ed });
            let _ = update(
                &mut app,
                Message::EditorCanvas(CanvasMessage::SelectPressed(PointLike::new(5.0, 5.0))),
            );
            let _ = update(
                &mut app,
                Message::EditorCanvas(CanvasMessage::SelectDragged(PointLike::new(20.0, 9.0))),
            );
            let ed = app.editor.as_ref().unwrap();
            assert_ne!(ed.model.annotations()[0], pixelate, "it moved");
            if large {
                // Rendered off the UI thread: the canvas's opaque cover
                // stands in until then.
                assert!(!ed.shows_present_render());
                continue;
            }
            // Before, the image was the document without the redaction
            // and the canvas covered it with an opaque grey box.
            assert!(ed.shows_present_render());
            let expected = readshot_core::render(
                &ed.model.shared_base(),
                &ed.model.geometry().cover_redactions(ed.model.annotations()),
            );
            match &ed.image_handle {
                iced::widget::image::Handle::Rgba { pixels, .. } => {
                    assert_eq!(&pixels[..], expected.as_raw().as_slice())
                }
                other => panic!("expected pixels, got {other:?}"),
            }
            // Released, it commits; Esc on another drag puts it back.
            let _ = update(
                &mut app,
                Message::EditorCanvas(CanvasMessage::SelectReleased),
            );
            assert!(app.editor.as_ref().unwrap().shows_present_render());
        }
    }

    /// An async editor over `ramp(40, 20)` with one selected rectangle.
    fn large_editor(app: &mut App) -> usize {
        let mut ed = sync_editor();
        ed.refresh_image();
        app.editor = Some(async_compose(ed));
        app.editor.as_ref().unwrap().model.undo_depth()
    }

    /// The same document built synchronously, for comparing outputs.
    fn sync_editor() -> crate::editor::EditorSession {
        let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
        ed.model.commit_annotation(rect_at(5.0).remove(0));
        ed
    }

    fn redo_msg() -> Message {
        Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo)
    }

    fn job_kind(app: &App) -> Option<crate::editor::ComposeKind> {
        app.editor
            .as_ref()
            .unwrap()
            .compose_job
            .as_ref()
            .map(|j| j.kind)
    }

    /// Nothing composed or rendered on the UI thread since `before`.
    fn composes(app: &App) -> (u64, u64) {
        (
            super::compose_jobs::composes_on_this_thread(),
            app.editor.as_ref().unwrap().model.base_composes(),
        )
    }

    #[test]
    fn a_large_rotate_composes_off_thread_and_lands_as_one_undo_step() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let shown = handle_id(app.editor.as_ref().unwrap());
        let before = composes(&app);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        // Queued: nothing composed here, nothing changes until it lands.
        assert_eq!(composes(&app), before);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            job_kind(&app),
            Some(crate::editor::ComposeKind::Rotate { clockwise: true })
        );
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth);
        assert_eq!(handle_id(ed), shown);
        let landed = finish_compose(&app);
        let _ = update(&mut app, landed);
        let ed = app.editor.as_mut().unwrap();
        assert!(ed.compose_job.is_none());
        assert_eq!(ed.image_size(), (20, 40));
        assert_eq!(ed.model.undo_depth(), depth + 1, "one undo step");
        assert_ne!(handle_id(ed), shown);
        assert_eq!(ed.status.as_deref(), Some("Rotated right. ⌘Z to undo."));
        let mut sync = sync_editor();
        assert!(sync.model.rotate(true).unwrap());
        assert_eq!(ed.model.annotations(), sync.model.annotations());
        assert!(*ed.model.base() == *sync.model.base(), "base");
        assert!(
            editor_output_image(ed) == editor_output_image(&mut sync),
            "the output is the rotated document"
        );
    }

    #[test]
    fn an_edit_before_a_large_rotate_lands_applies_it_first_and_drops_its_late_result() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let late = finish_compose(&app);
        // Delete is not chained: the rotate is applied first, then the
        // selected rectangle (now in the turned frame) is deleted.
        let _ = update(&mut app, Message::EditorDeleteSelected);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (20, 40), "rotated");
        assert!(ed.model.annotations().is_empty(), "then deleted");
        assert_eq!(ed.model.undo_depth(), depth + 2);
        assert!(!super::compose_jobs::steps_pending(ed));
        // The late result is stale: dropped.
        let _ = update(&mut app, late);
        finish_all(&mut app);
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(ed.image_size(), (20, 40));
        assert_eq!(ed.model.undo_depth(), depth + 2);
        let mut sync = sync_editor();
        sync.model.rotate(true).unwrap();
        assert!(sync.model.delete_selected_annotation());
        assert!(editor_output_image(ed) == editor_output_image(&mut sync));
    }

    #[test]
    fn rotate_flip_undo_and_redo_on_a_large_document_queue_and_land_in_order() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let before = composes(&app);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        // ⌘[ ⌘] ⌘Z ⇧⌘Z arrive as keys while the rotate composes: each
        // queues behind it, none settles it.
        let window = iced::window::Id::unique();
        app.editor.as_mut().unwrap().window_id = Some(window);
        let key = |c: &str, shift: bool| {
            let mut modifiers = command_modifiers();
            if shift {
                modifiers.insert(iced::keyboard::Modifiers::SHIFT);
            }
            Message::EditorKeyPressed {
                window,
                key: iced::keyboard::Key::Character(c.into()),
                modifiers,
                status_ignored: true,
            }
        };
        let _ = update(
            &mut app,
            Message::EditorFlip(readshot_core::document::FlipAxis::Horizontal),
        );
        let _ = update(&mut app, key("z", false));
        let _ = update(&mut app, key("z", true));
        let _ = update(&mut app, key("[", false));
        assert_eq!(composes(&app), before, "nothing composed on the UI thread");
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth, "nothing applied yet");
        assert_eq!(ed.compose_job.as_ref().unwrap().steps.len(), 5);
        assert_eq!(ed.compose_job.as_ref().unwrap().composed, 1);
        finish_all(&mut app);
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 3, "rotate, flip, rotate");
        assert!(!ed.model.can_redo());
        let mut sync = sync_editor();
        sync.model.rotate(true).unwrap();
        sync.model
            .flip(readshot_core::document::FlipAxis::Horizontal)
            .unwrap();
        assert!(sync.model.undo());
        assert!(sync.model.redo());
        sync.model.rotate(false).unwrap();
        assert_eq!(
            ed.model.geometry().transform,
            sync.model.geometry().transform
        );
        assert_eq!(ed.model.annotations(), sync.model.annotations());
        assert!(editor_output_image(ed) == editor_output_image(&mut sync));
    }

    #[test]
    fn a_large_undo_and_redo_across_a_rotate_swap_the_cached_bases_in() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let original = app.editor.as_ref().unwrap().model.shared_base();
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        finish_all(&mut app);
        let rotated = app.editor.as_ref().unwrap().model.shared_base();
        let before = composes(&app);
        let _ = update(&mut app, undo_msg());
        let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        assert_eq!(job.kind, crate::editor::ComposeKind::Undo);
        assert!(
            job.base.as_ref().is_some_and(|b| Arc::ptr_eq(b, &original)),
            "the worker gets the cached base"
        );
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (20, 40));
        finish_all(&mut app);
        assert_eq!(composes(&app), before, "no recompose for the undo");
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth);
        assert!(Arc::ptr_eq(&ed.model.shared_base(), &original));
        assert_eq!(ed.status.as_deref(), Some("Undid edit. ⇧⌘Z to redo."));
        let mut sync = sync_editor();
        assert!(editor_output_image(ed) == editor_output_image(&mut sync));
        let _ = update(&mut app, redo_msg());
        let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        assert!(job.base.as_ref().is_some_and(|b| Arc::ptr_eq(b, &rotated)));
        finish_all(&mut app);
        assert_eq!(composes(&app), before, "no recompose for the redo");
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (20, 40));
        assert_eq!(ed.model.undo_depth(), depth + 1);
    }

    #[test]
    fn rapid_large_undos_land_in_order_one_compose_at_a_time() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        for msg in [
            Message::EditorRotate { clockwise: true },
            Message::EditorFlip(readshot_core::document::FlipAxis::Vertical),
            Message::EditorRotate { clockwise: true },
        ] {
            let _ = update(&mut app, msg);
            finish_all(&mut app);
        }
        assert_eq!(app.editor.as_ref().unwrap().model.undo_depth(), depth + 3);
        for _ in 0..3 {
            let _ = update(&mut app, undo_msg());
        }
        let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        assert_eq!((job.steps.len(), job.composed), (3, 1));
        let first = finish_compose(&app);
        let _ = update(&mut app, first);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 2, "the first undo landed");
        let job = ed.compose_job.clone().unwrap();
        assert_eq!(
            (job.steps.len(), job.composed),
            (2, 2),
            "the rest compose next"
        );
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth);
        assert_eq!(ed.model.redo_depth(), 3);
        assert!(ed.model.geometry().is_original());
        assert_eq!(ed.image_size(), (40, 20));
    }

    #[test]
    fn an_edit_before_a_large_undo_lands_applies_it_first_and_drops_its_late_result() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        finish_all(&mut app);
        let _ = update(&mut app, undo_msg());
        let late = finish_compose(&app);
        let _ = update(&mut app, Message::EditorDeleteSelected);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20), "undone");
        assert!(ed.model.annotations().is_empty(), "then deleted");
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert!(!ed.model.can_redo(), "the delete dropped the redo");
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.image_size(), (40, 20));
        assert!(ed.model.annotations().is_empty());
        assert_eq!(ed.model.undo_depth(), depth + 1);
    }

    #[test]
    fn a_large_added_part_lands_as_one_undo_step_with_its_bake() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.preferences.editor_panel_open = true;
        let mut ed = sync_editor();
        // A turned document is baked into one part first (D4).
        ed.model.rotate(true).unwrap();
        let mut ed = async_compose(ed);
        ed.refresh_image();
        app.editor = Some(ed);
        let depth = app.editor.as_ref().unwrap().model.undo_depth();
        let session = app.editor.as_ref().unwrap().session_id;
        let before = composes(&app);
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session,
                result: Ok(Some(Arc::new(ramp(20, 10)))),
            },
        );
        assert_eq!(composes(&app), before, "the bake reused the cached base");
        assert!(matches!(
            job_kind(&app),
            Some(crate::editor::ComposeKind::AddPart { .. })
        ));
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 1, "not applied yet");
        assert_eq!(ed.model.undo_depth(), depth);
        let landed = finish_compose(&app);
        let _ = update(&mut app, landed);
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(ed.model.part_ids().len(), 2);
        assert_eq!(ed.model.undo_depth(), depth + 1, "one undo step");
        assert_eq!(ed.status.as_deref(), Some("Added image below. ⌘Z to undo."));
        let baked = ed.model.part_ids()[0];
        assert!(ed.text_panel.bake_sources.contains_key(&baked));
        assert!(
            matches!(
                ed.text_panel.recognition,
                crate::editor::TextRecognition::Recognizing { .. }
            ),
            "the new part is recognized"
        );
        let mut sync = sync_editor();
        sync.model.rotate(true).unwrap();
        sync.model
            .add_part(ramp(20, 10), readshot_core::document::Join::Below)
            .unwrap();
        assert_eq!(ed.model.part_ids(), sync.model.part_ids());
        assert_eq!(ed.model.annotations(), sync.model.annotations());
        assert!(editor_output_image(ed) == editor_output_image(&mut sync));
        let _ = update(&mut app, undo_msg());
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 1);
        assert_eq!(ed.model.undo_depth(), depth);
    }

    #[test]
    fn an_edit_before_a_large_added_part_lands_applies_it_first() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let session = app.editor.as_ref().unwrap().session_id;
        let _ = update(
            &mut app,
            Message::EditorPartLoaded {
                session,
                result: Ok(Some(Arc::new(ramp(20, 10)))),
            },
        );
        let late = finish_compose(&app);
        let _ = update(&mut app, Message::EditorDeleteSelected);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 2, "added");
        assert!(ed.model.annotations().is_empty(), "then deleted");
        assert_eq!(ed.model.undo_depth(), depth + 2);
        let _ = update(&mut app, late);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.part_ids().len(), 2);
        assert_eq!(ed.model.undo_depth(), depth + 2);
    }

    #[test]
    fn cancelling_a_large_gap_preview_puts_the_baseline_back_without_a_render() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, _) = gap_drag_composing(&mut app);
        let baseline = handle_id(app.editor.as_ref().unwrap());
        let landed = finish_compose(&app);
        let _ = update(&mut app, landed);
        assert_eq!(app.editor.as_ref().unwrap().image_size(), (40, 30));
        let before = composes(&app);
        // ⌘Z while the slider is held cancels the drag.
        let _ = update(&mut app, undo_msg());
        assert_eq!(composes(&app), before, "the baseline's base was kept");
        let ed = app.editor.as_mut().unwrap();
        assert_eq!(ed.status.as_deref(), Some("Cancelled the slider change."));
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(handle_id(ed), baseline, "the baseline's display is back");
        assert!(ed.model.has_flattened(), "and its flattened image");
        assert!(ed.compose_job.is_none(), "nothing to render");
        assert_eq!(ed.model.undo_depth(), depth);
        let mut sync = two_part_editor();
        assert!(editor_output_image(ed) == editor_output_image(&mut sync));
    }

    #[test]
    fn an_annotation_commit_on_a_large_document_renders_off_the_ui_thread() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let shown = handle_id(app.editor.as_ref().unwrap());
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                rect_at(20.0).remove(0),
            )),
        );
        let ed = app.editor.as_mut().unwrap();
        // The edit is in the model at once (one step); the previous render
        // stays on screen while the new one renders off the UI thread.
        assert_eq!(ed.model.annotations().len(), 2);
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert_eq!(handle_id(ed), shown);
        assert!(ed.display_dirty);
        assert!(!ed.model.has_flattened());
        assert_eq!(job_kind(&app), Some(crate::editor::ComposeKind::Flatten));
        // An output meanwhile is the exact present document.
        let ed = app.editor.as_mut().unwrap();
        let expected = readshot_core::render(ed.model.base(), ed.model.annotations());
        assert!(editor_output_image(ed) == expected);
        let landed = finish_compose(&app);
        let _ = update(&mut app, landed);
        let ed = app.editor.as_ref().unwrap();
        assert_ne!(handle_id(ed), shown);
        assert!(!ed.display_dirty);
        assert!(ed.compose_job.is_none());
        assert!(ed
            .shown
            .shows(&ed.model.shared_base(), ed.model.annotations()));
    }

    #[test]
    fn an_output_takes_the_render_of_the_present_already_made() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                rect_at(20.0).remove(0),
            )),
        );
        let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        assert_eq!(job.kind, crate::editor::ComposeKind::Flatten);
        // Its worker finished, as on its thread; then ⌘C.
        job.output
            .run(|| super::compose_jobs::compose_with(job.next.clone(), job.base.clone()));
        let coord = app.coordinator.clone();
        let ed = app.editor.as_mut().unwrap();
        settle_editor_for_output(ed, &coord);
        assert!(ed.model.has_flattened(), "no second render for the output");
        assert!(!ed.display_dirty);
        assert!(ed.compose_job.is_none());
        let expected = readshot_core::render(ed.model.base(), ed.model.annotations());
        assert!(editor_output_image(ed) == expected);
    }

    #[test]
    fn a_stale_flatten_is_shown_while_the_newest_edit_renders() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        let commit = |x: f32| {
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                rect_at(x).remove(0),
            ))
        };
        let _ = update(&mut app, commit(20.0));
        let first = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        let _ = update(&mut app, commit(30.0));
        // One render in flight: the second edit waits for it.
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.compose_job.as_ref().unwrap().seq, first.seq);
        let shown = handle_id(ed);
        let landed = finish_compose(&app);
        let _ = update(&mut app, landed);
        let ed = app.editor.as_ref().unwrap();
        // Newer than what was on screen (continuous input must not freeze
        // it), but not the present: shown, still dirty, never cached.
        assert_ne!(handle_id(ed), shown, "the older render is shown");
        assert_eq!(ed.shown.annotations.as_ref().unwrap().len(), 2);
        assert!(ed.display_dirty);
        assert!(!ed.model.has_flattened(), "a display is never output");
        let second = ed.compose_job.clone().expect("the newest is rendered next");
        assert_ne!(second.seq, first.seq);
        assert_eq!(second.next.annotations, ed.model.annotations());
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.display_dirty);
        assert!(ed
            .shown
            .shows(&ed.model.shared_base(), ed.model.annotations()));
    }

    #[test]
    fn a_crop_on_a_large_document_shows_the_previous_render_cut_to_its_frame() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        app.editor
            .as_mut()
            .unwrap()
            .model
            .set_tool(readshot_ui::editor::ToolState::Crop);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                Annotation::Crop {
                    rect: RectLike::new(10.0, 5.0, 20.0, 10.0),
                },
            )),
        );
        let _ = update(&mut app, Message::EditorCropApply);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.effective_image_size(), (20, 10));
        // Until the cropped render lands, the uncropped one is shown cut
        // to the crop: the same pixels in the same place.
        let part = super::view::shown_part_of_frame(ed, ed.crop_offset(), (20, 10));
        assert_eq!(
            part,
            Some(Some(super::view::ShownPart {
                region: iced::Rectangle {
                    x: 10,
                    y: 5,
                    width: 20,
                    height: 10
                },
                at: (0.0, 0.0),
                size: (20.0, 10.0),
            }))
        );
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            super::view::shown_part_of_frame(ed, ed.crop_offset(), (20, 10)),
            None,
            "the cropped render is the frame"
        );
    }

    #[test]
    fn a_large_select_drag_renders_the_background_once_and_a_cancel_restores_it() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let depth = large_editor(&mut app);
        let shown = handle_id(app.editor.as_ref().unwrap());
        app.editor.as_mut().unwrap().model.clear_selection();
        for msg in [
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(5.0, 3.0)),
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(9.0, 4.0)),
        ] {
            let _ = update(&mut app, Message::EditorCanvas(msg));
        }
        // The document without the dragged rectangle renders off the UI
        // thread; then every tick reuses it.
        assert_eq!(job_kind(&app), Some(crate::editor::ComposeKind::Flatten));
        assert!(app
            .editor
            .as_ref()
            .unwrap()
            .compose_job
            .as_ref()
            .unwrap()
            .next
            .annotations
            .is_empty());
        finish_all(&mut app);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectDragged(PointLike::new(
                12.0, 4.0,
            ))),
        );
        assert!(app.editor.as_ref().unwrap().compose_job.is_none());
        // Esc cancels the drag: the display before it is back, no render.
        let ed = app.editor.as_mut().unwrap();
        cancel_editor_previews(ed);
        assert_eq!(handle_id(ed), shown);
        assert!(!ed.display_dirty);
        assert_eq!(ed.model.undo_depth(), depth);
    }

    #[test]
    fn a_released_gap_drag_settled_by_another_action_reuses_its_running_compose() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, session) = gap_drag_composing(&mut app);
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        assert!(app.editor.as_ref().unwrap().gap_commit_on_land);
        // Its worker finishes, as on its thread, before the next action.
        let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        job.output
            .run(|| super::compose_jobs::compose_with(job.next.clone(), job.base.clone()));
        let before = composes(&app);
        // Delete is not chained: the released drag is settled first.
        let _ = update(&mut app, Message::EditorDeleteSelected);
        assert_eq!(composes(&app), before, "nothing composed on the UI thread");
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 1, "one step for the drag");
        assert_eq!(ed.model.geometry().gap, 10);
        assert_eq!(ed.image_size(), (40, 30));
        assert!(ed.snapshot_drag_baseline.is_none());
        assert!(!ed.gap_commit_on_land);
        // Its message, arriving late, changes nothing.
        let _ = update(
            &mut app,
            Message::EditorComposed {
                session,
                seq: job.seq,
                output: job.output.clone(),
            },
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.model.undo_depth(), depth + 1);
        assert_eq!(ed.image_size(), (40, 30));
    }

    #[test]
    fn undo_during_a_held_large_gap_drag_cancels_it_without_composing_its_value() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, session) = gap_drag_composing(&mut app);
        let first = finish_compose(&app);
        let _ = update(&mut app, first);
        // A newer value waits on the throttle; the slider is still held.
        let _ = update(&mut app, Message::EditorLayoutGapPreview(30));
        let _ = update(&mut app, Message::EditorLayoutGapFlush { session });
        let before = composes(&app);
        let _ = update(&mut app, undo_msg());
        assert_eq!(
            composes(&app),
            before,
            "the dropped value is never composed"
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.status.as_deref(), Some("Cancelled the slider change."));
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth);
        assert!(ed.compose_job.is_none());
    }

    #[test]
    fn a_new_gap_drag_after_a_release_keeps_the_released_value_with_its_compose() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, _) = gap_drag_composing(&mut app);
        let _ = update(&mut app, Message::EditorSnapshotSliderCommit);
        let job = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        job.output
            .run(|| super::compose_jobs::compose_with(job.next.clone(), job.base.clone()));
        let before = composes(&app);
        // The next drag starts before the released one landed.
        let _ = update(&mut app, Message::EditorLayoutGapPreview(4));
        assert_eq!(composes(&app), before, "the running compose was reused");
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(
            ed.model.undo_depth(),
            depth + 1,
            "the released drag is a step"
        );
        assert_eq!(ed.model.geometry().gap, 10);
        assert!(matches!(
            ed.compose_job.as_ref().map(|j| j.kind),
            Some(crate::editor::ComposeKind::GapPreview(4))
        ));
    }

    // ===== D1 fix round 1 =====

    fn key_press(window: iced::window::Id, key: iced::keyboard::Key, cmd: bool) -> Message {
        Message::EditorKeyPressed {
            window,
            key,
            modifiers: if cmd {
                command_modifiers()
            } else {
                iced::keyboard::Modifiers::default()
            },
            status_ignored: true,
        }
    }

    #[test]
    fn bare_modifiers_and_zoom_keys_never_settle_pending_steps() {
        use iced::keyboard::{key::Named, Key};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        let window = iced::window::Id::unique();
        app.editor.as_mut().unwrap().window_id = Some(window);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let before = composes(&app);
        // ⌘ and ⇧ alone (as winit sends them before ⌘Z / ⇧⌘Z), then ⌘=.
        for key in [
            Key::Named(Named::Super),
            Key::Named(Named::Shift),
            Key::Named(Named::Alt),
            Key::Named(Named::Control),
        ] {
            let _ = update(&mut app, key_press(window, key, false));
        }
        let _ = update(
            &mut app,
            key_press(window, Key::Character("=".into()), true),
        );
        // Then ⌘Z: chained, never settled.
        let _ = update(
            &mut app,
            key_press(window, Key::Character("z".into()), true),
        );
        assert_eq!(composes(&app), before, "nothing composed on the UI thread");
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.compose_job.as_ref().unwrap().steps.len(), 2);
        assert_eq!(ed.image_size(), (40, 20), "nothing settled");
        // A key that edits still settles first.
        let _ = update(
            &mut app,
            key_press(window, Key::Named(Named::Delete), false),
        );
        assert!(!super::compose_jobs::steps_pending(
            app.editor.as_ref().unwrap()
        ));
    }

    #[test]
    fn continuous_edits_keep_the_large_display_moving() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        let mut shown = handle_id(app.editor.as_ref().unwrap());
        // Each landing is already stale (the next edit came first), yet it
        // replaces the older image on screen.
        for x in [10.0, 14.0, 18.0, 22.0] {
            let _ = update(
                &mut app,
                Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                    rect_at(x).remove(0),
                )),
            );
            if app.editor.as_ref().unwrap().compose_job.is_some() {
                let landed = finish_compose(&app);
                let _ = update(
                    &mut app,
                    Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                        rect_at(x + 1.0).remove(0),
                    )),
                );
                let _ = update(&mut app, landed);
                let ed = app.editor.as_ref().unwrap();
                assert_ne!(handle_id(ed), shown, "the screen moved on");
                shown = handle_id(ed);
            }
        }
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.display_dirty);
        assert!(ed
            .shown
            .shows(&ed.model.shared_base(), ed.model.annotations()));
    }

    #[test]
    fn edits_not_yet_rendered_are_drawn_over_the_large_display() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        assert!(app.editor.as_ref().unwrap().pending_overlay().is_empty());
        let redaction = Annotation::Pixelate {
            rect: RectLike::new(20.0, 4.0, 8.0, 8.0),
            block_size: 4.0,
        };
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                redaction.clone(),
            )),
        );
        let ed = app.editor.as_ref().unwrap();
        assert_eq!(ed.pending_overlay(), vec![redaction], "drawn at once");
        finish_all(&mut app);
        assert!(app.editor.as_ref().unwrap().pending_overlay().is_empty());
    }

    #[test]
    fn a_large_select_release_draws_the_moved_annotation_until_rendered() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        app.editor.as_mut().unwrap().model.clear_selection();
        for msg in [
            readshot_ui::CanvasMessage::SelectPressed(PointLike::new(5.0, 3.0)),
            readshot_ui::CanvasMessage::SelectDragged(PointLike::new(9.0, 4.0)),
        ] {
            let _ = update(&mut app, Message::EditorCanvas(msg));
        }
        finish_all(&mut app);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::SelectReleased),
        );
        let ed = app.editor.as_ref().unwrap();
        // The background was rendered without it: the canvas draws it in
        // its new place until the render with it lands.
        assert_eq!(ed.pending_overlay(), ed.model.annotations().to_vec());
        assert_eq!(ed.pending_overlay().len(), 1);
        finish_all(&mut app);
        assert!(app.editor.as_ref().unwrap().pending_overlay().is_empty());
    }

    #[test]
    fn esc_during_a_held_large_gap_drag_never_composes_its_value() {
        use iced::keyboard::{key::Named, Key};
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let (depth, session) = gap_drag_composing(&mut app);
        let window = iced::window::Id::unique();
        app.editor.as_mut().unwrap().window_id = Some(window);
        let first = finish_compose(&app);
        let _ = update(&mut app, first);
        let _ = update(&mut app, Message::EditorLayoutGapPreview(30));
        let _ = update(&mut app, Message::EditorLayoutGapFlush { session });
        let before = composes(&app);
        let mut esc = key_press(window, Key::Named(Named::Escape), false);
        if let Message::EditorKeyPressed { status_ignored, .. } = &mut esc {
            *status_ignored = false; // the canvas captured it
        }
        let _ = update(&mut app, esc);
        assert_eq!(
            composes(&app),
            before,
            "the dropped value is never composed"
        );
        let ed = app.editor.as_ref().unwrap();
        assert!(ed.snapshot_drag_baseline.is_none());
        assert_eq!(ed.image_size(), (40, 20));
        assert_eq!(ed.model.undo_depth(), depth);
    }

    #[test]
    fn small_edits_send_nothing_to_the_dropper_thread() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        app.editor = Some(crate::editor::EditorSession::new(solid(40, 20)));
        let before = super::compose_jobs::sent_to_dropper();
        for x in [2.0, 6.0, 10.0] {
            let _ = update(
                &mut app,
                Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                    rect_at(x).remove(0),
                )),
            );
        }
        let _ = update(&mut app, undo_msg());
        assert_eq!(super::compose_jobs::sent_to_dropper(), before);
        // A big image let go of is freed on the dropper thread.
        app.editor = Some(crate::editor::EditorSession::new(solid(1_100, 1_000)));
        let before = super::compose_jobs::sent_to_dropper();
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                rect_at(2.0).remove(0),
            )),
        );
        assert_eq!(super::compose_jobs::sent_to_dropper(), before + 1);
    }

    #[test]
    fn the_display_shares_the_flattened_pixels_with_the_model() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        let flat = ed.model.flattened_shared().expect("cached");
        assert_eq!(ed.image_handle.id(), flat.handle().id(), "one copy");
        // A small edit shares too.
        let mut ed = crate::editor::EditorSession::new(solid(40, 20));
        ed.model.commit_annotations(rect_at(5.0));
        ed.refresh_image();
        assert_eq!(
            ed.image_handle.id(),
            ed.model.flattened_shared().unwrap().handle().id()
        );
    }

    #[test]
    fn a_replaced_flatten_worker_is_abandoned() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        let _ = update(
            &mut app,
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                rect_at(20.0).remove(0),
            )),
        );
        let flatten = app.editor.as_ref().unwrap().compose_job.clone().unwrap();
        assert_eq!(flatten.kind, crate::editor::ComposeKind::Flatten);
        // A rotate replaces it before its worker starts: it never runs.
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let ran = std::cell::Cell::new(false);
        flatten.output.run(|| {
            ran.set(true);
            super::compose_jobs::compose_with(flatten.next.clone(), flatten.base.clone())
        });
        assert!(!ran.get());
        assert!(flatten.output.take().is_none());
    }

    #[test]
    fn the_toolbar_shows_undo_and_redo_as_they_will_be_after_queued_steps() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        large_editor(&mut app);
        let _ = update(&mut app, Message::EditorRotate { clockwise: true });
        let _ = update(&mut app, undo_msg());
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.model.can_redo(), "nothing applied yet");
        assert_eq!(
            super::compose_jobs::can_undo_redo(ed),
            (true, true),
            "the rotate will be redoable"
        );
        // ⇧⌘Z (the enabled Redo) queues behind.
        let _ = update(&mut app, redo_msg());
        assert_eq!(
            super::compose_jobs::can_undo_redo(app.editor.as_ref().unwrap()),
            (true, false)
        );
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert_eq!((ed.model.can_undo(), ed.model.can_redo()), (true, false));
    }

    #[test]
    fn a_large_editor_opens_on_a_placeholder_and_renders_off_the_ui_thread() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        // 16 MP: at the threshold.
        let mut ed = crate::editor::EditorSession::new(ramp(4_000, 4_000));
        assert!(ed.display_dirty);
        assert!(!ed.model.has_flattened(), "nothing rendered");
        assert!(ed.shown.scale < 1.0);
        match &ed.image_handle {
            iced::widget::image::Handle::Rgba { width, height, .. } => {
                assert!(*width <= crate::editor::PLACEHOLDER_MAX_SIDE);
                assert!(*height <= crate::editor::PLACEHOLDER_MAX_SIDE);
            }
            other => panic!("unexpected handle {other:?}"),
        }
        // An output before the render is the exact document, never the
        // placeholder.
        ed.model.commit_annotations(rect_at(5.0));
        let expected = readshot_core::render(ed.model.base(), ed.model.annotations());
        assert!(editor_output_image(&mut ed) == expected);
        app.editor = Some(crate::editor::EditorSession::new(ramp(4_000, 4_000)));
        let _ = update(&mut app, Message::EditorStatusTick);
        assert_eq!(job_kind(&app), Some(crate::editor::ComposeKind::Flatten));
        finish_all(&mut app);
        let ed = app.editor.as_ref().unwrap();
        assert!(!ed.display_dirty);
        assert_eq!(ed.shown.scale, 1.0);
        assert!(ed
            .shown
            .shows(&ed.model.shared_base(), ed.model.annotations()));
    }

    #[test]
    fn a_placeholder_is_cut_to_a_crop_frame_at_its_own_scale() {
        let base = Arc::new(ramp(3_200, 1_600));
        let record_crop = Annotation::Crop {
            rect: RectLike::new(800.0, 400.0, 1_600.0, 800.0),
        };
        let mut ed = crate::editor::EditorSession::new(ramp(40, 20));
        ed.model = readshot_ui::editor::EditorState::with_annotations(
            Arc::unwrap_or_clone(base),
            vec![record_crop],
        );
        ed.shown = crate::editor::ShownImage {
            base: ed.model.shared_base(),
            annotations: Some(Vec::new()),
            offset: (0.0, 0.0),
            size: (3_200, 1_600),
            scale: 0.5,
        };
        let part = super::view::shown_part_of_frame(&ed, ed.crop_offset(), (1_600, 800));
        assert_eq!(
            part,
            Some(Some(super::view::ShownPart {
                region: iced::Rectangle {
                    x: 400,
                    y: 200,
                    width: 800,
                    height: 400,
                },
                at: (0.0, 0.0),
                size: (1_600.0, 800.0),
            }))
        );
    }

    /// UI-thread time of rotate, undo, redo and an annotation commit at
    /// ~96 MP: the message itself, then the landing of its off-thread
    /// result (the worker's compose is run untimed, as on its thread).
    /// Release build: `cargo test --release -p readshot-app --lib
    /// ui_thread_time -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement; run in release"]
    fn ui_thread_time_of_rotate_and_undo_at_96_mp() {
        let mut app = build_app(Arc::new(FakePermissions::granted()));
        let image = solid(12_000, 8_000);
        let t = std::time::Instant::now();
        let mut ed = crate::editor::EditorSession::new(image);
        println!("96 MP UI thread: open editor: {:?}", t.elapsed());
        ed.model.commit_annotations(rect_at(50.0));
        app.editor = Some(ed);
        let step = |app: &mut App, what: &str, msg: Message| {
            let t = std::time::Instant::now();
            let _ = update(app, msg);
            let handled = t.elapsed();
            let mut landed = std::time::Duration::ZERO;
            while app.editor.as_ref().unwrap().compose_job.is_some() {
                let done = finish_compose(app);
                let t = std::time::Instant::now();
                let _ = update(app, done);
                landed += t.elapsed();
            }
            println!("96 MP UI thread: {what}: message {handled:?}, landing {landed:?}");
        };
        step(
            &mut app,
            "rotate",
            Message::EditorRotate { clockwise: true },
        );
        step(&mut app, "undo", undo_msg());
        step(
            &mut app,
            "redo",
            Message::EditorToolbar(readshot_ui::ToolbarMessage::Redo),
        );
        step(&mut app, "undo again", undo_msg());
        step(
            &mut app,
            "commit annotation",
            Message::EditorCanvas(readshot_ui::CanvasMessage::CommitAnnotation(
                rect_at(90.0).remove(0),
            )),
        );
        let session = app.editor.as_ref().unwrap().session_id;
        step(
            &mut app,
            "add part (12000 x 1000)",
            Message::EditorPartLoaded {
                session,
                result: Ok(Some(Arc::new(solid(12_000, 1_000)))),
            },
        );
        step(&mut app, "undo the part", undo_msg());
    }
}
