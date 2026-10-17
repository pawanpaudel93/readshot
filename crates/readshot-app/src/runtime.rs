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
mod key_focus;
pub(crate) use key_focus::*;

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
        // Esc chords registered globally (`scroll_escape_hotkeys`) are
        // left to the hotkey, so one press is never seen twice (a doubled
        // ⇧esc would confirm a discard). Subscriptions take plain fns, so
        // there is one per combination.
        fn listen<const PLAIN: bool, const SHIFT: bool>(
            event: iced::Event,
            _status: iced::event::Status,
            _window: iced::window::Id,
        ) -> Option<Message> {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                return scroll_key_message_beside_globals(&key, modifiers, PLAIN, SHIFT);
            }
            None
        }
        subs.push(iced::event::listen_with(
            match scroll_escape_globals(state) {
                (false, false) => listen::<false, false>,
                (true, false) => listen::<true, false>,
                (false, true) => listen::<false, true>,
                (true, true) => listen::<true, true>,
            },
        ));
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
        .is_some_and(crate::editor::status_tick_needed)
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
        subs.push(iced::event::listen_with(|event, status, window| {
            use iced::keyboard::Event as KbEvent;
            if let iced::Event::Keyboard(KbEvent::KeyPressed { key, modifiers, .. }) = event {
                return Some(Message::SettingsKeyPressed {
                    window,
                    key,
                    modifiers,
                    captured: status == iced::event::Status::Captured,
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
    // ⌥⌘R reveals, ⌘P pins, ⌥⇧⌘C copies every visible capture's text,
    // ⇧⌘⌫ arms (then confirms) Clear All. Pointer-only before (WCAG
    // 2.1.1). A focused search field keeps none of these but ⇧⌘⌫.
    if modifiers.command() && !extra_command_modifier(modifiers) {
        match key {
            // ⌥⌘R on macOS, Ctrl+Shift+R elsewhere (Ctrl+Alt is AltGr).
            _ if editor_alt_chord(key, modifiers, "r") => {
                return action(HistoryKeyboardAction::Reveal);
            }
            Key::Character(c)
                if !modifiers.alt() && !modifiers.shift() && c.eq_ignore_ascii_case("p") =>
            {
                return action(HistoryKeyboardAction::Pin);
            }
            // ⌥⇧⌘C on macOS, Ctrl+Shift+A elsewhere (Ctrl+Shift+C is Copy
            // Text there too, and Ctrl+Alt is AltGr).
            _ if history_copy_visible_chord(key, modifiers) => {
                // A focused search field may take ⌘C chords for itself.
                return chord(HistoryKeyboardAction::CopyVisibleText);
            }
            Key::Named(Named::Backspace | Named::Delete)
                if modifiers.shift() && !modifiers.alt() =>
            {
                return chord(HistoryKeyboardAction::ClearAll);
            }
            _ => {}
        }
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
        Key::Named(Named::Escape) => action(HistoryKeyboardAction::Escape),
        _ => None,
    }
}

/// History's Copy Visible Text chord: ⌥⇧⌘C on macOS, Ctrl+Shift+A
/// elsewhere.
pub(crate) fn history_copy_visible_chord(
    key: &iced::keyboard::Key,
    m: iced::keyboard::Modifiers,
) -> bool {
    let iced::keyboard::Key::Character(c) = key else {
        return false;
    };
    if cfg!(target_os = "macos") {
        m.command() && m.alt() && m.shift() && c.eq_ignore_ascii_case("c")
    } else {
        m.command() && m.shift() && !m.alt() && !m.logo() && c.eq_ignore_ascii_case("a")
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
    sync_scroll_escape_hotkeys(state);
    let announce = announce_capture_status(state);
    Task::batch([task, announce, compose_jobs::after_update(state)])
}

/// Whether a capture status reports a problem worth a notification:
/// an error, or a refusal like "No previous region…". Successes and a
/// cancel the user chose are not.
pub(crate) fn capture_status_is_problem(status: &str) -> bool {
    match status_toast_kind(status) {
        ToastKind::Error => true,
        ToastKind::Info => !status.to_lowercase().contains("cancelled"),
        _ => false,
    }
}

/// How a capture problem no open window shows reaches the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureProblemRoute {
    /// A system notification (macOS, through osascript).
    Notification,
    /// The Welcome window, which shows the status, brought forward
    /// (Windows and Linux, which have no notification path yet).
    WelcomeWindow,
}

pub(crate) const CAPTURE_PROBLEM_ROUTE: CaptureProblemRoute = if cfg!(target_os = "macos") {
    CaptureProblemRoute::Notification
} else {
    CaptureProblemRoute::WelcomeWindow
};

/// `last_capture_status` is drawn only in the Welcome window. A capture
/// started by the hotkey or the tray that fails with no Welcome window
/// open is raised another way ([`CAPTURE_PROBLEM_ROUTE`]), once.
fn announce_capture_status(state: &mut App) -> Task<Message> {
    let Some(status) = state.last_capture_status.clone() else {
        state.announced_capture_status = None;
        return Task::none();
    };
    if state.announced_capture_status.as_ref() == Some(&status) {
        return Task::none();
    }
    state.announced_capture_status = Some(status.clone());
    let welcome_open = state.windows.kinds().any(|k| k == WindowKind::Welcome);
    if welcome_open || !capture_status_is_problem(&status) {
        return Task::none();
    }
    surface_capture_problem(state, &status, CAPTURE_PROBLEM_ROUTE)
}

pub(crate) fn surface_capture_problem(
    state: &mut App,
    status: &str,
    route: CaptureProblemRoute,
) -> Task<Message> {
    match route {
        CaptureProblemRoute::Notification => {
            notify_capture_problem(status);
            Task::none()
        }
        CaptureProblemRoute::WelcomeWindow => show_or_focus_welcome(state),
    }
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
        | Message::EditorBeautifyRestorePreset
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
        | Message::EditorImageMenuKey
        | Message::EditorPanelTabKey(..)
        | Message::EditorFocusZoneCycle(..)
        | Message::EditorCycleSelection(..)
        | Message::EditorTabResolved { .. }
        | Message::EditorResizeByKeys(..)
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
mod tests;
