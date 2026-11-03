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

use readshot_capture::CaptureRequest;
use readshot_core::Preferences;
use readshot_ui::SettingsMessage;

use crate::editor::{EditorSession, Status, StatusTone};
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
mod history_select;
pub(crate) use history_select::*;
mod key_maps;
pub(crate) use key_maps::*;
mod toast;
pub(crate) use toast::*;
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
mod history_images;
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
mod keys;
pub(crate) use keys::*;
mod app_ops;
mod capture_ops;
mod tick_ops;
mod welcome_ops;
mod window_ops;

/// Initial URL action set from `main.rs` before `iced::daemon` starts.
/// `start()` consumes this and feeds an extra `TickMsg::UrlAction`
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

/// Whether the app reads the OS appearance itself (`os_theme_mode`, the
/// 1 Hz poll). Only macOS can. Elsewhere iced is the source:
/// `SystemThemeChanged` carries the desktop portal's colour scheme on
/// Linux and the theme each new window reads from the system on Windows,
/// so nothing may overwrite it with `os_theme_mode`'s fixed Dark.
pub(crate) const READS_OS_THEME: bool = cfg!(target_os = "macos");

/// Whether `subscription` runs the 1 Hz OS appearance poll: only where
/// the app reads the OS itself, and only while a window follows it.
pub(crate) fn theme_poll_active(state: &App) -> bool {
    READS_OS_THEME && theme_poll_needed(state)
}

/// Register a newly opened window. On macOS a system-themed window
/// re-reads the OS appearance here (in `update`, on the main thread) so
/// it opens in the right mode even though the 1 Hz poll was idle until
/// now.
pub(crate) fn register_window(state: &mut App, id: window::Id, kind: WindowKind) {
    state.windows.register(id, kind);
    if READS_OS_THEME && follows_system_theme(kind) {
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

/// What the `__interactive-capture` child exits with when it wrote no
/// image: a cancel unless a failure was recorded.
static CLI_INTERACTIVE_EXIT: std::sync::atomic::AtomicI32 =
    std::sync::atomic::AtomicI32::new(crate::cli::INTERACTIVE_CHILD_CANCELLED);

/// The exit code for an interactive child that produced no image
/// (`main.rs`): 64 for a cancel, 77 for missing permission, 71 else.
pub fn cli_interactive_exit_code() -> i32 {
    CLI_INTERACTIVE_EXIT.load(std::sync::atomic::Ordering::Acquire)
}

fn record_cli_interactive_failure(err: &str) {
    CLI_INTERACTIVE_EXIT.store(
        crate::cli::interactive_child_failure_code(err),
        std::sync::atomic::Ordering::Release,
    );
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
    editor.set_status(StatusTone::Success, "Editor QA fixture loaded.");
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

use crate::app::{
    App, AppMsg, BeautifyMsg, CaptureMsg, CombineMsg, EditorMsg, FocusMsg, GlobalHotkeyAction,
    HistoryKeyboardAction, HistoryMsg, ImageMsg, Message, PinMsg, ScrollMsg, SettingsMsg,
    TextPanelMsg, TickMsg, WelcomeMsg, WindowKind, WindowMsg,
};
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
    app.instance_mailbox = crate::single_instance::registered_mailbox();
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
        target: readshot_core::log::cat::PERMS,
        "initial status: {:?}",
        initial_permission,
    );
    arm_relaunch_on_system_quit(matches!(
        initial_permission,
        crate::permissions::PermissionStatus::Denied
    ));

    if cli_interactive_request.is_some() {
        let task = if app.welcome.should_show() {
            Task::done(Message::Capture(CaptureMsg::CliInteractiveWritten(Err(
                "Screen Recording permission is required for interactive capture".into(),
            ))))
        } else {
            Task::done(Message::Capture(CaptureMsg::OpenOverlayRequested))
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
        app.window_hotkey_registered = registration.window_registered;
    }
    if app.preferences.launch_at_login {
        if let Err(e) = crate::startup::set_launch_at_login(true) {
            tracing::warn!(target: readshot_core::log::cat::APP, "launch-at-login setup failed: {e}");
        }
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = crate::url_events::install_platform_handler() {
        tracing::warn!(target: readshot_core::log::cat::URL, "running-app URL handler unavailable: {e}");
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = crate::app_events::install_platform_handler() {
        tracing::warn!(target: readshot_core::log::cat::APP, "app reopen handler unavailable: {e}");
    }
    #[cfg(target_os = "macos")]
    if let Err(e) = crate::updater::install() {
        tracing::warn!(target: readshot_core::log::cat::UPDATER, "Sparkle updater unavailable: {e}");
    }
    // Same fail-soft contract for the tray. Linux without an
    // appindicator daemon, or a Windows session without a Shell_Notify
    // surface, will simply not see the tray entry.
    let hotkey_label = pretty_hotkey(&app.preferences.capture_hotkey);
    app.tray = crate::tray::install(Some(&app.preferences.capture_hotkey), Some(&hotkey_label));
    if let Some(tray) = app.tray.as_ref() {
        let window = app.preferences.window_capture_hotkey.trim();
        tray.set_window_capture_hotkey(app.window_hotkey_registered.then_some(window));
    }

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
        vec![iced::system::theme().map(|a| Message::Tick(TickMsg::SystemThemeChanged(a)))];
    let needs_welcome_window = needs_welcome_window_on_boot(
        app.welcome,
        initial_url_action.is_some(),
        launched_at_login(),
    );
    if needs_welcome_window {
        let (id, open_task) = window::open(welcome_window_settings());
        app.windows.register(id, WindowKind::Welcome);
        tasks.push(open_task.map(|a| Message::Welcome(WelcomeMsg::WindowReady(a))));
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
        tasks.push(Task::done(Message::Tick(TickMsg::UrlAction(action))));
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
    background_subscriptions(state, &mut subs);
    capture_subscriptions(state, &mut subs);
    editor_subscriptions(state, &mut subs);
    window_key_subscriptions(state, &mut subs);
    // Watch for the OS X-button closing any tracked window. Without
    // this, `state.windows` accumulates stale ids, and helpers like
    // `show_or_focus_welcome` end up calling `gain_focus` on dead
    // windows (silent no-op) instead of opening a fresh one.
    subs.push(window::close_events().map(|a| Message::Window(WindowMsg::Closed(a))));
    // The editor window opts out of iced's auto-close
    // (`exit_on_close_request: false`) so an OS close (red X / ⌘W) can
    // be routed through the unsaved-work confirmation. Every other
    // window auto-closes and only needs `close_events`; this handler
    // ignores them.
    subs.push(window::close_requests().map(|a| Message::Window(WindowMsg::CloseRequested(a))));
    Subscription::batch(subs)
}

/// The appearance poll, the permission poll and the OS-event drains
/// (hotkeys, tray, `readshot://` URLs, reopen).
fn background_subscriptions(state: &App, subs: &mut Vec<Subscription<Message>>) {
    subs.push(iced::system::theme_changes().map(|a| Message::Tick(TickMsg::SystemThemeChanged(a))));
    // iced pins each open window's appearance to its theme, so winit
    // suppresses theme-change events for those windows; poll the OS too.
    // The handler is idempotent, so identical repeats are free.
    // The map closure runs on the tokio executor, where `os_theme_mode`
    // (main-thread only) would always read Dark, so emit a bare tick and
    // read the OS appearance in `update()`.
    // Only while a system-themed window is open (see `theme_poll_needed`),
    // and only where the app can read the OS itself (`READS_OS_THEME`).
    if theme_poll_active(state) {
        subs.push(
            iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick(TickMsg::ThemePoll)),
        );
    }
    if state.welcome.should_show() {
        // Polling at 500 ms keeps the UI responsive without burning
        // CPU. The TCC db is already cached in-process so each poll
        // is essentially a no-op syscall.
        subs.push(
            iced::time::every(Duration::from_millis(500))
                .map(|_| Message::App(AppMsg::PermissionPollTick)),
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
        subs.push(
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick(TickMsg::Hotkey)),
        );
    }
    if state.tray.is_some() {
        // 250 ms is fine for tray clicks — humans can't tell the
        // difference between a 100 ms and 250 ms tray menu response.
        subs.push(
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick(TickMsg::Tray)),
        );
    }
    {
        // macOS delivers URL + reopen AppleEvents to AppKit callbacks
        // while the app is already running; on Windows and Linux a
        // second launch leaves a request file (`single_instance`), read
        // on the reopen tick. Either way they are queued, and these
        // ticks drain them back into iced. Both are rare, external,
        // user-initiated events, so a slower poll costs nothing.
        subs.push(
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick(TickMsg::Url)),
        );
        subs.push(
            iced::time::every(Duration::from_millis(500))
                .map(|_| Message::Tick(TickMsg::AppReopen)),
        );
    }
}

/// The scrolling capture's frame loop and Esc keys, and the Shift
/// watcher the overlay and the scrolling capture use.
fn capture_subscriptions(state: &App, subs: &mut Vec<Subscription<Message>>) {
    if state.scroll_session.is_some() {
        // Drive the scrolling-capture frame loop. The capture
        // coordinator's async future is debounced inside the session
        // (the tick handler bails out if a capture is still in
        // flight) so a slow backend can't pile up requests.
        subs.push(
            iced::time::every(Duration::from_millis(SCROLL_FRAME_INTERVAL_MS))
                .map(|_| Message::Scroll(ScrollMsg::Tick)),
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
            let (key, modifiers) = key_press(event)?;
            scroll_key_message_beside_globals(&key, modifiers, PLAIN, SHIFT)
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
                | iced::Event::Keyboard(KbEvent::ModifiersChanged(modifiers)) => Some(
                    Message::Capture(CaptureMsg::OverlayShiftChanged(modifiers.shift())),
                ),
                _ => None,
            }
        }));
    }
}

/// The open editor's ticks (status auto-dismiss, recognizing skeleton),
/// keys, modifiers, pointer button and window size.
fn editor_subscriptions(state: &App, subs: &mut Vec<Subscription<Message>>) {
    if state
        .editor
        .as_ref()
        .is_some_and(crate::editor::status_tick_needed)
    {
        // Slow tick drives the auto-dismiss of stale status pills.
        // Only runs while there's actually something to dismiss so
        // the editor doesn't burn CPU on idle windows.
        subs.push(
            iced::time::every(Duration::from_millis(1000))
                .map(|_| Message::Editor(EditorMsg::StatusTick)),
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
        subs.push(
            iced::time::every(Duration::from_millis(80))
                .map(|_| Message::Window(WindowMsg::EditorPanelTick)),
        );
    }
    if state.editor.is_some() {
        // Keyboard sub: ⌘Z / Ctrl+Z = Undo, ⇧⌘Z / Ctrl+Shift+Z = Redo,
        // ⌘S / Ctrl+S = Save. Iced 0.14's `event::listen_with` is the
        // window-agnostic event tap; we filter to KeyPressed events
        // and only react when the editor window is the focused one
        // (the canvas captures Escape; Esc is always a cancel, never
        // Discard, which is ⌘W). `editor_key_press` drops auto-repeats
        // of all but navigation, step and undo keys, so a held ⌘W never
        // discards.
        subs.push(iced::event::listen_with(editor_event_message));
        // Global Shift watcher for the editor — iced 0.14 doesn't reliably route
        // modifier changes into canvas widgets, so the editor canvas's
        // "hold Shift = constrain" relies on this forwarding modifier
        // state into App.editor_shift_held (mirrors the overlay watcher).
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            match event {
                iced::Event::Keyboard(KbEvent::KeyPressed { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::KeyReleased { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::ModifiersChanged(modifiers)) => Some(
                    Message::Window(WindowMsg::EditorShiftChanged(modifiers.shift())),
                ),
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
                iced::Event::Mouse(MouseEvent::ButtonPressed(Button::Left)) => Some(
                    Message::Window(WindowMsg::EditorPointerButton(true, Some(window))),
                ),
                iced::Event::Mouse(MouseEvent::ButtonReleased(Button::Left)) => Some(
                    Message::Window(WindowMsg::EditorPointerButton(false, Some(window))),
                ),
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Window(
                    WindowMsg::EditorPointerWindowUnfocused(window),
                )),
                _ => None,
            }
        }));
        // Command state for ⌘-click multi-select in the Text panel.
        subs.push(iced::event::listen_with(|event, _status, _window| {
            use iced::keyboard::Event as KbEvent;
            match event {
                iced::Event::Keyboard(KbEvent::KeyPressed { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::KeyReleased { modifiers, .. })
                | iced::Event::Keyboard(KbEvent::ModifiersChanged(modifiers)) => Some(
                    Message::Window(WindowMsg::EditorCommandChanged(modifiers.command())),
                ),
                _ => None,
            }
        }));
    }
}

/// The shortcut recorder, the app-wide shortcuts, and the History,
/// Settings, pin and Welcome windows' key maps.
fn window_key_subscriptions(state: &App, subs: &mut Vec<Subscription<Message>>) {
    if state.settings_recording_hotkey {
        subs.push(iced::event::listen_with(|event, _status, _window| {
            let (key, modifiers) = key_press(event)?;
            if shortcut_cancelled_by_keypress(&key, modifiers) {
                return Some(Message::Settings(SettingsMsg::HotkeyRecordingCancelled));
            }
            // Ignore bare modifier presses: the user is mid-chord,
            // holding Cmd/Ctrl/Shift before the real key. Reporting
            // those as invalid is what made recording feel stuck.
            if is_modifier_key(&key) {
                return None;
            }
            Some(
                shortcut_string_from_keypress(&key, modifiers)
                    .map(|a| Message::Settings(SettingsMsg::HotkeyRecorded(a)))
                    .unwrap_or(Message::Settings(SettingsMsg::HotkeyRecordingInvalid)),
            )
        }));
    } else {
        subs.push(iced::event::listen_with(|event, _status, _window| {
            let (key, modifiers) = key_press(event)?;
            app_window_shortcut_message(key, modifiers)
        }));
    }
    if state.history_window_id.is_some() {
        subs.push(iced::event::listen_with(history_event_message));
    }
    if state.settings_window_id.is_some() && !state.settings_recording_hotkey {
        // Settings is pointer-only (togglers, pick-lists, buttons), none
        // of which iced 0.14 can focus. Tap keys and route them to the
        // settings handler, which validates the window id. The filename
        // text_input keeps plain typing because every shortcut is
        // ⌘-prefixed (Esc aside). See `settings_key_action`.
        subs.push(iced::event::listen_with(|event, status, window| {
            let (key, modifiers) = key_press(event)?;
            Some(Message::Settings(SettingsMsg::KeyPressed {
                window,
                key,
                modifiers,
                captured: status == iced::event::Status::Captured,
            }))
        }));
    }
    if !state.pins.is_empty() {
        // Pins are borderless, always-on-top thumbnails whose only
        // chrome is pointer-driven; give the focused pin a keyboard map
        // (close / lock / opacity / move). See `pin_key_action`.
        subs.push(iced::event::listen_with(|event, _status, window| {
            let (key, modifiers) = key_press(event)?;
            Some(Message::Editor(EditorMsg::Pin(PinMsg::KeyPressed {
                window,
                key,
                modifiers,
            })))
        }));
    }
    if state.windows.kinds().any(|k| k == WindowKind::Welcome) {
        // Bind Enter to the welcome card's primary action so the
        // permission / capture flow is operable without the mouse.
        // See `welcome_key_message`.
        subs.push(iced::event::listen_with(|event, _status, window| {
            let (key, modifiers) = key_press(event)?;
            Some(Message::Welcome(WelcomeMsg::KeyPressed {
                window,
                key,
                modifiers,
            }))
        }));
    }
}

/// The editor window's listener: scale and size changes, and key presses
/// as `editor_key_press` passes them (a held ⌘W never discards; a held ⌘Z
/// or `]` still repeats).
pub(crate) fn editor_event_message(
    event: iced::Event,
    status: iced::event::Status,
    window: iced::window::Id,
) -> Option<Message> {
    if let iced::Event::Window(iced::window::Event::Rescaled(scale)) = event {
        return Some(Message::Window(WindowMsg::Rescaled(window, scale)));
    }
    if let iced::Event::Window(iced::window::Event::Resized(size)) = event {
        return Some(Message::Window(WindowMsg::EditorWindowResized(
            window, size.width,
        )));
    }
    let (key, modifiers) = editor_key_press(event)?;
    Some(Message::Editor(EditorMsg::KeyPressed {
        window,
        key,
        modifiers,
        status_ignored: status == iced::event::Status::Ignored,
    }))
}

/// The History window's key listener: `history_key_message` for a key
/// press `key_press` passes (an auto-repeat of ⇧⌘⌫ or ⌦ never confirms).
pub(crate) fn history_event_message(
    event: iced::Event,
    status: iced::event::Status,
    window: iced::window::Id,
) -> Option<Message> {
    let (key, modifiers) = key_press(event)?;
    history_key_message(
        window,
        &key,
        modifiers,
        status == iced::event::Status::Ignored,
    )
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
    let action = |a| Some(Message::History(HistoryMsg::KeyboardShortcut(window, a)));
    // A focused search field captures ⌘C and ⌫/⌦ even when it has nothing
    // to copy or delete. Subscriptions cannot see app state, so captured
    // chords go out as `HistoryCapturedShortcut` and the handler applies
    // them only while the search query is empty.
    let chord = |a| {
        Some(if status_ignored {
            Message::History(HistoryMsg::KeyboardShortcut(window, a))
        } else {
            Message::History(HistoryMsg::CapturedShortcut(window, a))
        })
    };
    // ⌘F focuses the search field, wherever focus is. (Off macOS
    // `command()` is Ctrl itself, so the extra modifier to refuse is
    // the logo key, not Ctrl: `extra_command_modifier`.)
    if modifiers.command()
        && !modifiers.shift()
        && !modifiers.alt()
        && !extra_command_modifier(modifiers)
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
    if modifiers.command() && !modifiers.alt() && !extra_command_modifier(modifiers) {
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
    // Delete: ⌦ or ⌘⌫ (Ctrl+Backspace elsewhere); deletion is permanent.
    // Plain ⌫ is never bound.
    if !modifiers.alt() && !extra_command_modifier(modifiers) && !modifiers.shift() {
        match key {
            Key::Named(Named::Delete) if !modifiers.command() => {
                return chord(HistoryKeyboardAction::Delete)
            }
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
        Key::Character(c) if c.eq_ignore_ascii_case("y") => {
            Some(Message::History(HistoryMsg::OpenRequested))
        }
        Key::Character(c) if c == "," => Some(Message::Settings(SettingsMsg::OpenRequested)),
        _ => None,
    }
}

/// Top-level update fn. Delegates testable transitions to
/// [`App::update_sync`] and adds the iced-only async branches. After
/// every message the editor's off-thread document work is started
/// (`compose_jobs::after_update`).
pub fn update(state: &mut App, message: Message) -> Task<Message> {
    if state.uninstalled && !uninstalled_allows(&message) {
        return Task::none();
    }
    let task = update_message(state, message);
    sync_scroll_escape_hotkeys(state);
    let announce = announce_capture_status(state);
    Task::batch([task, announce, compose_jobs::after_update(state)])
}

/// What an uninstalled Readshot (`App::uninstalled`) still does: window
/// events, showing Settings and its panes, the tray drain, theme, and
/// quitting (the tray's Quit, Quit Readshot under the uninstall, the
/// app's quit). Everything else, every capture, History, editor and
/// preferences message, shortcut and URL, is dropped, so nothing the
/// uninstall removed is recreated before the process ends.
pub(crate) fn uninstalled_allows(message: &Message) -> bool {
    match message {
        Message::Window(_) | Message::NoOp => true,
        // KeyPressed takes the inert branch of the Settings key handler
        // (Esc / ⌘W close, ↵ quits, nothing else).
        Message::Settings(m) => matches!(
            m,
            SettingsMsg::OpenRequested
                | SettingsMsg::WindowReady(_)
                | SettingsMsg::PaneSelected(_)
                | SettingsMsg::KeyPressed { .. }
                | SettingsMsg::UninstallFinished { .. }
                | SettingsMsg::QuitAfterUninstall
        ),
        // The tray's Settings… reopens Settings on the uninstall result.
        Message::Tick(t) => matches!(
            t,
            TickMsg::Tray
                | TickMsg::AppReopen
                | TickMsg::ThemePoll
                | TickMsg::SystemThemeChanged(_)
                | TickMsg::TrayAction(
                    crate::tray::TrayAction::Quit | crate::tray::TrayAction::Settings
                )
        ),
        Message::App(m) => matches!(m, AppMsg::QuitRequested),
        Message::Editor(_)
        | Message::History(_)
        | Message::Scroll(_)
        | Message::Welcome(_)
        | Message::Capture(_) => false,
    }
}

/// Whether a capture status reports a problem worth a notification:
/// an error, or a refusal like "No previous region…" (a warning).
/// Successes, notices (a cancel the user chose) and work in flight are
/// not.
pub(crate) fn capture_status_is_problem(status: &Status) -> bool {
    matches!(status.tone, StatusTone::Error | StatusTone::Warning)
}

/// How a capture problem no open window shows reaches the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureProblemRoute {
    /// A system notification (macOS, posted as Readshot through
    /// UNUserNotificationCenter).
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
    if state.announced_capture_status.as_ref() == Some(&status.text) {
        return Task::none();
    }
    state.announced_capture_status = Some(status.text.clone());
    let welcome_open = state.windows.kinds().any(|k| k == WindowKind::Welcome);
    if welcome_open || !capture_status_is_problem(&status) {
        return Task::none();
    }
    surface_capture_problem(state, &status, capture_problem_route())
}

/// [`CAPTURE_PROBLEM_ROUTE`], except that once a notification has been
/// refused because Readshot's notifications are off, capture problems go
/// to the Welcome window, which shows them.
pub(crate) fn capture_problem_route() -> CaptureProblemRoute {
    if CAPTURE_PROBLEM_ROUTE == CaptureProblemRoute::Notification && notifications_off() {
        CaptureProblemRoute::WelcomeWindow
    } else {
        CAPTURE_PROBLEM_ROUTE
    }
}

/// The notification path's news, on a tick: notices it could not show go
/// into the app ([`surface_undelivered_notices`]), and once notifications
/// work again the tray's "Notifications Are Off" item goes, so the next
/// time they are off it is shown again.
pub(crate) fn process_notices(state: &mut App) -> Task<Message> {
    let task = surface_undelivered_notices(state, take_undelivered_notices());
    if state.notifications_off_told && !notifications_off() {
        state.notifications_off_told = false;
        if let Some(tray) = &state.tray {
            tray.hide_notifications_off();
        }
    }
    task
}

/// Show in the app what a notification could not: in the editor's toast
/// when an editor is open, or in the Welcome window when it is the capture
/// problem it shows. When notifications turn out to be off (the first
/// time, or again after they worked), the tray menu also gains an item
/// saying so that opens the setting.
pub(crate) fn surface_undelivered_notices(
    state: &mut App,
    notices: Vec<UndeliveredNotice>,
) -> Task<Message> {
    let mut tasks = Vec::new();
    for notice in notices {
        let off = notice.reason == NotShownReason::NotificationsOff;
        if let Some(editor) = state.editor.as_mut() {
            // The tone its writer chose when it was posted.
            let tone = notice.tone;
            if off {
                editor.set_status(tone, format!("{} {NOTIFICATIONS_OFF_HINT}", notice.text));
            } else {
                editor.set_status(tone, notice.text.clone());
            }
        } else if state.last_capture_status_text() == Some(notice.text.as_str()) {
            tasks.push(show_or_focus_welcome(state));
        }
        if off && !state.notifications_off_told {
            state.notifications_off_told = true;
            if let Some(tray) = &state.tray {
                tray.show_notifications_off();
            }
        }
    }
    Task::batch(tasks)
}

pub(crate) fn surface_capture_problem(
    state: &mut App,
    status: &Status,
    route: CaptureProblemRoute,
) -> Task<Message> {
    match route {
        CaptureProblemRoute::Notification => {
            notify_capture_problem(&status.text, status.tone);
            Task::none()
        }
        CaptureProblemRoute::WelcomeWindow => show_or_focus_welcome(state),
    }
}

fn update_message(state: &mut App, message: Message) -> Task<Message> {
    match message {
        Message::App(msg) => app_ops::handle_app_message(state, msg),
        Message::Welcome(msg) => welcome_ops::handle_welcome_message(state, msg),
        Message::Capture(msg) => capture_ops::handle_capture_message(state, msg),
        Message::Tick(msg) => tick_ops::handle_tick_message(state, msg),

        Message::History(msg) => {
            let task = history_ops::handle_history_message(state, msg);
            Task::batch([task, editor_ops::history_write_followups(state)])
        }
        Message::Window(msg) => window_ops::handle_window_message(state, msg),
        Message::Editor(msg) => window_ops::route_editor_message(state, msg),

        Message::NoOp => Task::none(),

        Message::Scroll(msg) => scroll::handle_scroll_message(state, msg),

        Message::Settings(msg) => settings_ops::handle_settings_message(state, msg),
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
    open_task.map(|a| Message::Welcome(WelcomeMsg::WindowReady(a)))
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
    // Whatever closed them, a window pick ends with its overlays.
    state.window_pick = None;
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
/// defaults when the file is missing.
///
/// A file that exists but can't be read or parsed is renamed aside to
/// `preferences.toml.corrupt-<unix-ts>` (so no save can overwrite it)
/// and the app starts with the first-launch set except
/// `history_retention = Unlimited`: the user's real retention is
/// unknown, and `Unlimited` never prunes yet keeps saving to History.
/// That set is saved in the file's place (only after a successful
/// rename), so the next launch isn't a first launch on Last 50.
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
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let mut quarantine = path.as_os_str().to_owned();
            quarantine.push(format!(".corrupt-{ts}"));
            let moved = std::fs::rename(path, &quarantine).is_ok();
            let prefs = Preferences {
                history_retention: readshot_core::HistoryRetention::Unlimited,
                ..first_launch_preferences()
            };
            // Saved in its place so the next launch isn't a first launch
            // (Last 50). Only once the original is safely aside.
            let saved = moved && prefs.save(path).is_ok();
            // Redaction policy (see `log.rs`): no $HOME-rooted path.
            tracing::warn!(
                target: readshot_core::log::cat::PREFERENCES,
                "preferences.toml unreadable ({e}); moved aside: {moved}; \
                 defaults saved: {saved}; History retention unlimited",
            );
            prefs
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
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetCaptureHotkey(defaults.capture_hotkey),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetWindowCaptureHotkey(defaults.window_capture_hotkey),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetSaveFolder(defaults.save_folder),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetFilenameTemplate(defaults.filename_template),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetDefaultFormat(defaults.default_format),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetHistoryRetention(defaults.history_retention),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetOcrLanguages(defaults.ocr_languages),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetLaunchAtLogin(defaults.launch_at_login),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetUpdateChannel(defaults.update_channel),
    )));
    state.update_sync(Message::Settings(SettingsMsg::Edit(
        SettingsMessage::SetDebugLogging(defaults.debug_logging),
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
/// * [`TextPanelMsg::Recognized`] when `editor_session` is set, the
///   moment OCR resolves (not after the slower PNG encode / save): the
///   same OCR feeds that editor's Text panel (one OCR per capture),
///   errors included so the editor never stays Recognizing. Engines map
///   a panicked OCR worker to an error, so this is always sent;
/// * [`HistoryMsg::RecordSaved`] the moment the record hits disk —
///   the new row shows up without waiting on OCR;
/// * [`HistoryMsg::RecordPersisted`] once both resolve — `Ok(true)`
///   when text landed, `Ok(false)` when OCR failed or the update did.
///   Not sent when the PNG encode or the save failed (no record was
///   saved);
/// * [`HistoryMsg::RecordSaveFailed`] instead of the two above when
///   the store refused the record.
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
                    .send(Message::Editor(EditorMsg::TextPanel(
                        TextPanelMsg::Recognized {
                            session,
                            part: readshot_core::document::ORIGINAL_PART,
                            result,
                        },
                    )))
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
                    tracing::warn!(target: readshot_core::log::cat::HISTORY, "png encode: {e}");
                    return false;
                }
                Err(e) => {
                    tracing::warn!(target: readshot_core::log::cat::HISTORY, "png encode worker: {e}");
                    return false;
                }
            };
            let record_id = record.id;
            if let Err(e) = coord.record_history(record, buf, policy, now).await {
                // Not saved: no success signal, no text step.
                let _ = output
                    .send(Message::History(HistoryMsg::RecordSaveFailed {
                        session: editor_session,
                        record_id,
                        error: e.to_string(),
                    }))
                    .await;
                return false;
            }
            let _ = output.send(Message::History(HistoryMsg::RecordSaved)).await;
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
                    tracing::warn!(target: readshot_core::log::cat::HISTORY, "ocr update failed: {e}");
                    false
                }
                Err(e) => {
                    tracing::warn!(target: readshot_core::log::cat::HISTORY, "ocr update worker: {e}");
                    false
                }
            }
        }
        Err(e) => {
            tracing::warn!(target: readshot_core::log::cat::HISTORY, "ocr failed: {e}");
            false
        }
    };
    let _ = output
        .send(Message::History(HistoryMsg::RecordPersisted(Ok(persisted))))
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

/// Open a native file-save dialog seeded at `seed_dir` (or
/// `~/Desktop` if `None` / not a directory) with a timestamped
/// default filename in Settings › Default format, then write the
/// image to whatever the user picks. Returns `Ok(Some(path))` on
/// success, `Ok(None)` when the user cancels.
async fn save_image_via_picker(
    img: image::RgbaImage,
    seed_dir: Option<PathBuf>,
    filename_template: String,
    format: readshot_core::ExportFormat,
) -> Result<Option<PathBuf>, CaptureRunError> {
    let default_name = default_save_filename(&filename_template, chrono::Local::now(), format);
    let initial_dir = seed_dir
        .filter(|p| p.is_dir())
        .or_else(|| directories::UserDirs::new().and_then(|d| d.desktop_dir().map(PathBuf::from)))
        .unwrap_or_else(std::env::temp_dir);
    // The default format's filter first, so the dialog starts on it;
    // the others stay choosable.
    let mut dialog = rfd::AsyncFileDialog::new();
    for f in std::iter::once(format).chain(
        readshot_core::ExportFormat::ALL
            .into_iter()
            .filter(|f| *f != format),
    ) {
        dialog = dialog.add_filter(format!("{f} image"), f.extensions());
    }
    let handle = dialog
        .set_directory(&initial_dir)
        .set_file_name(&default_name)
        .save_file()
        .await;
    let Some(handle) = handle else {
        return Ok(None);
    };
    write_picked_save(&img, handle.path().to_path_buf(), format).map(Some)
}

/// Write a save-dialog result: in the format its extension names, or
/// with the default format's extension added when it names none (rfd
/// doesn't always append the filter's extension on macOS when the user
/// types a bare name).
fn write_picked_save(
    img: &image::RgbaImage,
    picked: PathBuf,
    default: readshot_core::ExportFormat,
) -> Result<PathBuf, CaptureRunError> {
    let (path, format) = resolve_save_path(picked, default);
    readshot_core::export::save(img, &path, format)?;
    Ok(path)
}

/// The path and format a picked save path is written as. A known image
/// extension (any case) is the user's choice and is kept. Anything else
/// keeps the whole name, dots included ("… at 10.20.30" has none of its
/// own), and gains the default format's extension.
fn resolve_save_path(
    picked: PathBuf,
    default: readshot_core::ExportFormat,
) -> (PathBuf, readshot_core::ExportFormat) {
    if let Some(format) = picked
        .extension()
        .and_then(|e| e.to_str())
        .and_then(readshot_core::ExportFormat::from_extension)
    {
        return (picked, format);
    }
    let mut name = picked.into_os_string();
    name.push(".");
    name.push(default.extension());
    (PathBuf::from(name), default)
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

/// The save dialog's suggested name: the template expanded in `when`'s
/// zone (callers pass local time, so the name shows the wall-clock time
/// of the save) plus the default format's extension.
fn default_save_filename<Tz: chrono::TimeZone>(
    template: &str,
    when: chrono::DateTime<Tz>,
    format: readshot_core::ExportFormat,
) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut filename = readshot_core::expand_filename_template(template, when);
    filename.push('.');
    filename.push_str(format.extension());
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
        |a| Message::History(HistoryMsg::ListLoaded(a)),
    )
}

/// The editor's output before Beautify: the model's flattened image of
/// the present, plus the Beautify settings still to apply. Made only
/// once the Beautify output is known to fit the document limits, so a
/// canvas over them is refused before anything is allocated.
pub(crate) struct EditorOutput {
    flat: image::RgbaImage,
    beautify: readshot_core::beautify::BeautifySettings,
}

impl EditorOutput {
    /// No Beautify to apply: the output is the flattened image as is.
    pub(crate) fn is_identity(&self) -> bool {
        self.beautify.clamped().is_identity()
    }

    /// Apply Beautify. Off the UI thread for Save and Copy
    /// (`render_off_thread`); the size was checked when this was made.
    pub(crate) fn render(self) -> Result<image::RgbaImage, readshot_core::document::GeometryError> {
        if self.beautify.clamped().is_identity() {
            return Ok(self.flat);
        }
        readshot_core::beautify::render(&self.flat, &self.beautify)
    }

    /// `render` on a blocking worker: a beautified canvas near the pixel
    /// budget is a gigabyte to fill, which must not stall the UI thread.
    pub(crate) async fn render_off_thread(self) -> Result<image::RgbaImage, String> {
        tokio::task::spawn_blocking(move || self.render())
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| beautify_panel::beautify_refusal("make the image", e))
    }
}

/// The output, or why its Beautify canvas can't be made.
pub(crate) fn editor_output(
    ed: &mut EditorSession,
) -> Result<EditorOutput, readshot_core::document::GeometryError> {
    let beautify = ed.model.beautify();
    // The model's flattened image of the present (rendered here if it
    // has none), never a display.
    let flat = ed.model.flatten();
    readshot_core::beautify::checked_output_geometry(flat.width(), flat.height(), &beautify)?;
    Ok(EditorOutput { flat, beautify })
}

/// The rendered output, in one call (tests).
#[cfg(test)]
fn editor_output_image(
    ed: &mut EditorSession,
) -> Result<image::RgbaImage, readshot_core::document::GeometryError> {
    editor_output(ed)?.render()
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
///
/// With redactions ([`editor_ops::CopyAllMask::Hide`]) every word they hide
/// is masked; text that can't be masked (no word positions) is withheld
/// ([`OcrCopyError::Withheld`]), never copied.
async fn ocr_then_copy(
    coord: CaptureCoordinator,
    img: image::RgbaImage,
    preferences: Preferences,
    remove_breaks: bool,
    mask: editor_ops::CopyAllMask,
) -> Result<String, OcrCopyError> {
    let request = ocr_request_for_image(img, &preferences);
    let result = match mask {
        editor_ops::CopyAllMask::None => coord.recognise(request).await?,
        editor_ops::CopyAllMask::Hide(hide) => {
            let doc = coord.recognise_document(request).await?;
            readshot_core::scrub::masked_text(&doc, &hide).ok_or(OcrCopyError::Withheld)?
        }
        editor_ops::CopyAllMask::Withhold => return Err(OcrCopyError::Withheld),
    };
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
