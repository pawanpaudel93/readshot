//! iced application skeleton.
//!
//! This module wires the typed message enums from `readshot-ui` and
//! the service `CaptureCoordinator` into a single state machine. The
//! actual `iced::daemon` invocation lives in `main.rs`; this module
//! exposes the pieces it needs (`App`, `Message`, `update`, `view`).
//!
//! The full iced wiring — multi-window orchestration, marching-ants
//! animation timer, `tray-icon` and `global-hotkey` integrations —
//! is structurally laid out below but commented as TODOs the runtime
//! implementer fills in once the GUI is exercised on a real display.
//! Everything that is *testable* without iced (state transitions,
//! permission gating, message routing) is covered by unit tests in
//! sibling modules.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use iced::window;
use readshot_core::Preferences;
use readshot_ui::{
    ActionMessage, CanvasMessage, HotkeyMessage, SelectionMessage, SettingsMessage, ToolbarMessage,
    TrayMessage,
};

use crate::coordinator::CaptureCoordinator;
use crate::permissions::PermissionsProvider;
use crate::welcome::WelcomeState;

/// Distinct purposes a top-level iced window can serve. The runtime
/// uses this to dispatch `view` and `title` per `window::Id`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WindowKind {
    Welcome,
    /// Transparent fullscreen region-selection overlay. Phase B
    /// supports a single overlay (primary display only); the value
    /// is reserved for future per-display indexing.
    Overlay,
    /// Annotation / actions editor for a freshly-captured image.
    /// Phase C ships the action row only (Save / Copy / Copy Text /
    /// Discard); annotation tools land in a follow-up.
    Editor,
    /// Floating "pin" window — a borderless, always-on-top thumbnail
    /// of a flattened capture the user can drag around so it stays
    /// visible while they work in another app. Multiple pins can be
    /// alive at once.
    Pin,
}

/// Mapping from live `window::Id`s to their kind, so the daemon's
/// per-window callbacks know what tree to render. Phase A only ever
/// has a single Welcome window; the structure is built to grow.
#[derive(Default)]
pub struct Windows {
    by_id: HashMap<window::Id, WindowKind>,
}

impl Windows {
    pub fn register(&mut self, id: window::Id, kind: WindowKind) {
        self.by_id.insert(id, kind);
    }

    pub fn forget(&mut self, id: window::Id) {
        self.by_id.remove(&id);
    }

    pub fn kind(&self, id: window::Id) -> Option<WindowKind> {
        self.by_id.get(&id).copied()
    }

    /// Iterate every (`Id`, `WindowKind`) pair. Used by the runtime
    /// to find live overlay/editor windows when the App needs to
    /// dispatch a close.
    pub fn iter(&self) -> impl Iterator<Item = (&window::Id, &WindowKind)> {
        self.by_id.iter()
    }

    /// Iterate every kind currently mapped. Convenient for "is there
    /// already an overlay open?" checks.
    pub fn kinds(&self) -> impl Iterator<Item = WindowKind> + '_ {
        self.by_id.values().copied()
    }
}

/// Top-level application message — every event from every UI surface
/// or background task funnels through this enum.
#[derive(Clone, Debug)]
pub enum Message {
    Tray(TrayMessage),
    Hotkey(HotkeyMessage),
    Overlay(SelectionMessage),
    EditorCanvas(CanvasMessage),
    EditorToolbar(ToolbarMessage),
    EditorAction(ActionMessage),
    Settings(SettingsMessage),
    /// Permission-status poll fired on a timer.
    PermissionPoll(crate::permissions::PermissionStatus),
    /// Subscription tick — `runtime::update` reads the current status
    /// and feeds it as `PermissionPoll`. Separate from the latter so
    /// tests can drive `PermissionPoll` directly without faking the
    /// timer.
    PermissionTick,
    /// User clicked "Grant" in the welcome window.
    GrantPermissionRequested,
    /// User clicked "Open Settings" in the denied state.
    OpenPermissionSettingsRequested,
    /// User clicked "Quit" — graceful exit so the next launch picks
    /// up newly-granted TCC permissions.
    QuitRequested,
    /// User clicked "Restart" — relaunch the bundle so TCC's cached
    /// Screen Recording grant becomes visible to the new process.
    RestartRequested,
    /// User clicked "Capture primary display" in the welcome window.
    CaptureFullPrimaryRequested,
    /// 50 ms drain tick — runtime polls the global-hotkey receiver and
    /// emits a `CaptureFullPrimaryRequested` if any events arrived.
    HotkeyTick,
    /// 80 ms tick driving the region-overlay's marching-ants
    /// animation. Only fires while at least one overlay window is
    /// open.
    OverlayTick,
    /// 100 ms drain tick — runtime polls the tray + menu event channels.
    TrayTick,
    /// User selected an item in the tray menu (or left-clicked the icon).
    TrayActionPerformed(crate::tray::TrayAction),
    /// User asked for a region capture (welcome button / tray menu).
    OpenOverlayRequested,
    /// First view of the overlay window — used by the runtime to
    /// remember its `window::Id` mapping.
    OverlayWindowReady(iced::window::Id),
    /// Drag completed inside the overlay. `display_id` identifies
    /// the monitor the user dragged on; `rect` is in display-local
    /// logical pixels.
    OverlaySelected {
        display_id: readshot_capture::DisplayId,
        rect: readshot_core::geom::Rect,
    },
    /// User cancelled the overlay (ESC, sub-pixel click, etc.).
    OverlayCancelled,
    /// Async list_displays result that bootstraps the overlay flow:
    /// the runtime opens one transparent overlay window per display.
    OverlayDisplaysListed(Result<Vec<readshot_capture::DisplayInfo>, String>),
    /// Internal: capture a region of the named display, save the
    /// PNG, and toast the result. Emitted by the overlay flow.
    CaptureRegionRequested {
        display_id: readshot_capture::DisplayId,
        rect: readshot_core::geom::Rect,
    },
    /// Async region-capture finished — `Ok(image)` opens an editor
    /// window with the captured pixels; `Err` toasts the failure on
    /// the welcome window.
    RegionCaptureCompleted(Result<image::RgbaImage, String>),
    /// First view callback for the editor window — used to record
    /// the `window::Id` so Discard can close the right one.
    EditorWindowReady(iced::window::Id),
    /// User clicked Save in the editor.
    EditorSaveRequested,
    /// Save task completed.
    EditorSaved(Result<std::path::PathBuf, String>),
    /// User clicked Copy (image to clipboard).
    EditorCopyImageRequested,
    /// Image-copy task completed.
    EditorCopyImageDone(Result<(), String>),
    /// User clicked Copy Text (run OCR + clipboard).
    EditorCopyTextRequested,
    /// OCR + clipboard write completed.
    EditorCopyTextDone(Result<String, String>),
    /// User clicked Discard. Closes the editor window.
    EditorDiscardRequested,
    /// User clicked Pin in the editor — flatten the current state,
    /// open a borderless always-on-top "pin" window with that
    /// image, and close the editor.
    EditorPinRequested,
    /// First view of a freshly-opened pin window — used to record
    /// its `window::Id` against its image handle in `App::pins`.
    PinWindowReady(iced::window::Id, iced::widget::image::Handle),
    /// User pressed the close `×` on a pin window. Closes the
    /// window and removes its entry from `App::pins`.
    PinClosePressed(iced::window::Id),
    /// User mouse-pressed inside the body of a pin window — kicks
    /// off a native window-drag so the pin can be repositioned.
    PinDragRequested(iced::window::Id),
    /// Text-tool inline input — content typed by the user. Empty
    /// means the input is cleared.
    EditorTextChanged(String),
    /// User pressed Enter / clicked Commit on the text input. Builds
    /// an `Annotation::Text` from the pending state and commits it.
    EditorTextCommit,
    /// User pressed Escape / clicked Cancel on the text input. Drops
    /// the pending state without committing.
    EditorTextCancel,
    /// Background capture-and-save task finished. Carries the final
    /// PNG path or a stringified error.
    CaptureSaved(Result<PathBuf, String>),
    /// First `view` call after the welcome window is opened. Used by
    /// the runtime to detect the window-ready transition.
    WelcomeWindowReady,
    /// Capture coordinator finished a `capture_region` call.
    CaptureCompleted(Result<image::RgbaImage, String>),
    /// OCR engine finished a `recognise` call.
    OcrCompleted(Result<String, String>),
    /// User invoked the URL scheme.
    UrlActionReceived(crate::url_scheme::UrlAction),
}

/// Top-level application state.
pub struct App {
    pub coordinator: CaptureCoordinator,
    pub permissions: Arc<dyn PermissionsProvider>,
    pub preferences: Preferences,
    pub welcome: WelcomeState,
    pub windows: Windows,
    /// `true` while a capture-and-save task is in flight; the welcome
    /// window's button is disabled in that state.
    pub capture_in_flight: bool,
    /// Last toast text shown under the capture button. Cleared when
    /// the user kicks off a new capture.
    pub last_capture_status: Option<String>,
    /// Live `GlobalHotKeyManager` for the registered capture shortcut.
    /// Held here so it isn't dropped (which unregisters the hotkey).
    /// Tests and CLI invocations leave this `None`.
    pub hotkey_manager: Option<global_hotkey::GlobalHotKeyManager>,
    /// Live tray-icon controller. Held to keep the icon visible.
    /// Tests and CLI invocations leave this `None`.
    pub tray: Option<crate::tray::TrayController>,
    /// Active editor session, if any. Phase C only allows one
    /// editor at a time; opening a new one replaces the old.
    pub editor: Option<crate::editor::EditorSession>,
    /// `window::Id` → display info for every live overlay window.
    /// One entry per monitor when the overlay flow is active; empty
    /// otherwise. The runtime uses this to look up the display id for
    /// a `view()` callback (so the canvas knows which display id to
    /// stamp into `Message::OverlaySelected`) and to convert the
    /// returned rect back into a `CaptureRequest` with the right
    /// scale.
    pub overlay_displays: HashMap<iced::window::Id, OverlayDisplay>,
    /// Live pin windows mapped to their pre-rendered image handle.
    /// Each pin window's `view` reads its handle from this map. The
    /// map shrinks as pins close.
    pub pins: HashMap<iced::window::Id, iced::widget::image::Handle>,
    /// Monotonically-increasing counter the region-overlay reads to
    /// animate its marching-ants stroke. Bumped by `OverlayTick`.
    pub overlay_tick: u32,
}

/// Per-overlay-window record. Tracks which display the window covers
/// so capture can run against the right monitor.
#[derive(Clone, Debug)]
pub struct OverlayDisplay {
    pub display_id: readshot_capture::DisplayId,
    /// HiDPI scale factor of the display (physical / logical).
    pub scale: f32,
}

impl App {
    pub fn new(
        coordinator: CaptureCoordinator,
        permissions: Arc<dyn PermissionsProvider>,
        preferences: Preferences,
    ) -> Self {
        let welcome = WelcomeState::from_status(coordinator.pre_capture_gate());
        Self {
            coordinator,
            permissions,
            preferences,
            welcome,
            windows: Windows::default(),
            capture_in_flight: false,
            last_capture_status: None,
            hotkey_manager: None,
            tray: None,
            editor: None,
            overlay_displays: HashMap::new(),
            pins: HashMap::new(),
            overlay_tick: 0,
        }
    }

    /// Handle a top-level message and produce side-effects.
    ///
    /// Returns `true` if the change is user-visible (caller may
    /// re-render). Most messages eventually need to dispatch async
    /// work via `iced::Task` — that wiring lives in `main.rs` once
    /// the daemon is up; this method handles only the synchronous
    /// state-machine portion that's testable today.
    pub fn update_sync(&mut self, message: Message) -> bool {
        match message {
            Message::PermissionPoll(status) => self.welcome.observe(status),
            Message::GrantPermissionRequested => {
                self.permissions.request();
                self.welcome = WelcomeState::AwaitingGrant;
                true
            }
            Message::OpenPermissionSettingsRequested => {
                self.permissions.open_settings();
                false
            }
            Message::Settings(msg) => readshot_ui::settings::apply(&mut self.preferences, msg),
            // The other message variants drive UI flows that need
            // the iced Task machinery (capture, OCR, save, history).
            // They're handled in main.rs once the daemon is running.
            // For now the synchronous handler is a no-op for them.
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::fake::FakePermissions;
    use crate::permissions::PermissionStatus;
    use readshot_capture::fake::FakeCapturer;
    use readshot_ocr::fake::FakeOcrEngine;
    use std::sync::atomic::Ordering;

    fn build_app(perms: Arc<FakePermissions>) -> (App, Arc<FakePermissions>) {
        let coordinator = CaptureCoordinator::new(
            Arc::new(FakeCapturer::new()),
            Arc::new(FakeOcrEngine::with_text("hello")),
            perms.clone(),
            None,
        );
        let app = App::new(coordinator, perms.clone(), Preferences::default());
        (app, perms)
    }

    #[test]
    fn welcome_starts_dismissed_when_already_granted() {
        let (app, _) = build_app(Arc::new(FakePermissions::granted()));
        assert!(!app.welcome.should_show());
    }

    #[test]
    fn welcome_starts_visible_when_denied() {
        let (app, _) = build_app(Arc::new(FakePermissions::denied()));
        assert!(app.welcome.should_show());
    }

    #[test]
    fn grant_request_invokes_permissions_and_advances_state() {
        let (mut app, perms) = build_app(Arc::new(FakePermissions::denied()));
        let changed = app.update_sync(Message::GrantPermissionRequested);
        assert!(changed);
        assert_eq!(perms.request_calls.load(Ordering::SeqCst), 1);
        assert_eq!(app.welcome, WelcomeState::AwaitingGrant);
    }

    #[test]
    fn permission_poll_dismisses_welcome_on_grant() {
        let perms = Arc::new(FakePermissions::denied());
        let (mut app, _) = build_app(perms.clone());
        // User clicks "Grant", OS prompt fires, then poll sees the new state.
        app.update_sync(Message::GrantPermissionRequested);
        perms.flip_to_granted();
        let changed = app.update_sync(Message::PermissionPoll(PermissionStatus::Granted));
        assert!(changed);
        assert!(!app.welcome.should_show());
    }

    #[test]
    fn settings_message_applies_to_preferences() {
        let (mut app, _) = build_app(Arc::new(FakePermissions::granted()));
        assert!(!app.preferences.debug_logging);
        let changed = app.update_sync(Message::Settings(SettingsMessage::SetDebugLogging(true)));
        assert!(changed);
        assert!(app.preferences.debug_logging);
    }

    #[test]
    fn open_settings_request_invokes_permissions_no_state_change() {
        let (mut app, perms) = build_app(Arc::new(FakePermissions::denied()));
        let changed = app.update_sync(Message::OpenPermissionSettingsRequested);
        assert!(!changed);
        assert_eq!(perms.open_settings_calls.load(Ordering::SeqCst), 1);
    }
}
