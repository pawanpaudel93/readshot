//! The application's state and message types.
//!
//! [`App`] holds everything the running app knows: preferences, the
//! capture coordinator, which window is which ([`Windows`]), the welcome
//! flow, the History browser, the region overlay, scroll and combine
//! captures, pins, and the open editor session. [`Message`] is every
//! event the app handles, from capture triggers (tray, hotkeys, URL
//! scheme, CLI) to editor, History and Settings interactions.
//!
//! The iced daemon itself is started in `main.rs` with
//! `crate::runtime::{start, update, view}`; `runtime` turns messages
//! into state changes and `iced::Task`s. [`App::update_sync`] is the
//! synchronous part of that state machine (permissions, settings,
//! History paging) and is unit-tested here without a display.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use iced::window;
use readshot_core::Preferences;
use readshot_ui::{CanvasMessage, SettingsMessage, ToolbarMessage};

use crate::coordinator::CaptureCoordinator;
use crate::permissions::PermissionsProvider;
use crate::welcome::WelcomeState;

/// Number of matching history rows the browser renders per page. The
/// list widget tree is built eagerly, so this bounds open time and
/// memory for large (Unlimited-retention) histories. "Show More"
/// grows the visible window in these increments.
pub const HISTORY_PAGE_SIZE: usize = 50;

/// Distinct purposes a top-level iced window can serve. The runtime
/// uses this to dispatch `view` and `title` per `window::Id`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum WindowKind {
    Welcome,
    /// Transparent region-selection overlay (one per display).
    Overlay,
    /// The editor for a capture or a History record: annotation tools,
    /// the Image menu (resize, rotate, flip, crop presets, combine), the
    /// Text and Beautify side panel, and the Save / Copy / Copy Text /
    /// Pin / Discard actions.
    Editor,
    /// Floating "pin" window — a borderless, always-on-top thumbnail
    /// of a flattened capture the user can drag around so it stays
    /// visible while they work in another app. Multiple pins can be
    /// alive at once.
    Pin,
    /// Persistent capture browser. Reads the on-disk history archive
    /// and renders a scrollable list. Single instance — re-opening
    /// while it's already up just refocuses the existing window.
    History,
    /// User-preferences window (retention policy, capture hotkey, …).
    /// Single instance — re-opening focuses the existing window
    /// rather than spawning a duplicate.
    Settings,
    /// Command-line setup instructions window.
    CliTools,
    /// Heads-up display shown while a scrolling-capture session is
    /// active. Floats above the user's content with a frame counter
    /// and a Stop button so the user can end the session manually.
    ScrollHud,
    /// Transparent, click-through, always-on-top overlay covering the
    /// active display while a scrolling-capture session is running.
    /// Renders only the captured-rect border so the user can see
    /// exactly which area is being captured as they scroll the page
    /// underneath. iced 0.14's `enable_mouse_passthrough` keeps it
    /// from blocking scroll wheel events.
    ScrollRegion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryKeyboardAction {
    Previous,
    Next,
    Open,
    Delete,
    CopyText,
    CopyImage,
    /// ⌘F: focus the search field (shown in the History key legend).
    FocusSearch,
    /// ⌥⌘R (Ctrl+Shift+R off macOS): reveal the selected capture in
    /// Finder / its folder.
    Reveal,
    /// ⌘P: pin the selected capture.
    Pin,
    /// ⌥⇧⌘C (Ctrl+Shift+A off macOS): copy the text of every visible
    /// capture.
    CopyVisibleText,
    /// ⇧⌘⌫: arm Clear All; pressed again while armed, confirm it.
    ClearAll,
    /// esc: cancel an armed Clear All or delete, else clear the search.
    Escape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalHotkeyAction {
    Capture,
    History,
    Settings,
    /// esc while a scrolling capture runs: Stop & Stitch, from any app.
    ScrollStop,
    /// ⇧esc while a scrolling capture runs: discard (arms, then confirms).
    ScrollDiscard,
}

/// Mapping from live `window::Id`s to their kind, so the daemon's
/// per-window callbacks know what tree to render (several kinds are
/// open at once: overlays, the editor, pins, History, Settings).
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

/// A Settings control the keyboard focus ring is on (WCAG 2.1.1: iced
/// 0.14 buttons, togglers, checkboxes and pick-lists can't take focus).
/// Tab / ⇧Tab walk the current pane's controls, Space / ↵ press, ← / →
/// change a picker; see `settings_focus_items`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsFocus {
    PermissionRecheck,
    PermissionReset,
    PermissionOpen,
    HotkeyRecord,
    HotkeyReset,
    Retention,
    FolderChoose,
    FolderOpen,
    FolderReset,
    FilenameField,
    OcrEngine,
    OcrLanguage(usize),
    Format,
    DebugLogging,
    LaunchAtLogin,
    ResetAll,
    ResetConfirm,
    ResetCancel,
    UninstallPurge,
    Uninstall,
    UninstallConfirm,
    UninstallCancel,
}

/// One page of the Settings window's sidebar. ⌘1–⌘7 select them in
/// this order. `pub` (not `pub(crate)`) because the public [`Message`]
/// carries it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SettingsPane {
    #[default]
    Permissions,
    Capture,
    Files,
    TextRecognition,
    Export,
    Diagnostics,
    App,
}

impl SettingsPane {
    pub(crate) const ALL: [Self; 7] = [
        Self::Permissions,
        Self::Capture,
        Self::Files,
        Self::TextRecognition,
        Self::Export,
        Self::Diagnostics,
        Self::App,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Permissions => "Permissions",
            Self::Capture => "Capture",
            Self::Files => "Files",
            Self::TextRecognition => "Text Recognition",
            Self::Export => "Export",
            Self::Diagnostics => "Diagnostics",
            Self::App => "App",
        }
    }

    pub(crate) fn icon(self) -> crate::icons::AppIcon {
        use crate::icons::AppIcon as I;
        match self {
            Self::Permissions => crate::icons::action::SCREEN_RECORDING,
            Self::Capture => I::Crosshair,
            Self::Files => I::Folder,
            Self::TextRecognition => I::ScanText,
            Self::Export => I::Image,
            Self::Diagnostics => I::Terminal,
            Self::App => I::Power,
        }
    }

    /// `1` → the first pane … `7` → the last; anything else → `None`.
    pub(crate) fn from_digit(d: u8) -> Option<Self> {
        Self::ALL.get(d.checked_sub(1)? as usize).copied()
    }
}

/// Top-level application message — every event from every UI surface
/// or background task funnels through this enum. Each variant wraps the
/// message enum of one handler module, and that module's handler matches
/// its own enum exhaustively: `runtime::update` only unwraps, so a new
/// message can only be added where it is handled.
#[derive(Clone, Debug)]
pub enum Message {
    /// Editor and pin window messages (`runtime::editor_ops`, through
    /// `window_ops::route_editor_message`).
    Editor(EditorMsg),
    /// History browser and History persistence messages (`runtime::history_ops`).
    History(HistoryMsg),
    /// Settings and command-line setup window messages (`runtime::settings_ops`).
    Settings(SettingsMsg),
    /// Scrolling-capture messages (`runtime::scroll`).
    Scroll(ScrollMsg),
    /// Window lifecycle, and the editor window's pointer, modifier, hover,
    /// size and scroll events (`runtime::window_ops`).
    Window(WindowMsg),
    /// Welcome window messages (`runtime::welcome_ops`).
    Welcome(WelcomeMsg),
    /// Background drains (hotkeys, tray, URL scheme, reopen, theme) and the
    /// actions they deliver (`runtime::tick_ops`).
    Tick(TickMsg),
    /// Capture messages: the overlay, region and full-display captures, and
    /// what is done with a result outside the editor (`runtime::capture_ops`).
    Capture(CaptureMsg),
    /// App-level messages: the screen-recording permission, quit, restart
    /// and reopen (`runtime::app_ops`).
    App(AppMsg),
    /// No-op message used purely to consume mouse presses on the
    /// pin window's controls row so they don't fall through to the
    /// underlying drag layer and turn an opacity-slider drag into a
    /// window move.
    NoOp,
}

/// Editor and pin window messages. Each one settles pending history steps
/// as it needs, clears the canvas geometry cache and syncs History
/// (`runtime::window_ops::route_editor_message`), then is handled by
/// `runtime::editor_ops::handle_editor_message`.
#[derive(Clone, Debug)]
pub enum EditorMsg {
    /// Pin window messages (`runtime::editor_ops::pins`).
    Pin(PinMsg),
    /// Keyboard focus-zone chords (`runtime::key_focus`).
    Focus(FocusMsg),
    /// Add Capture / Add Image and layout messages (`runtime::combine`).
    Combine(CombineMsg),
    /// Image menu messages: rotate, flip and Resize (`runtime::image_menu`).
    Image(ImageMsg),
    /// Beautify tab messages (`runtime::beautify_panel`).
    Beautify(BeautifyMsg),
    /// Side panel and Text tab messages (`runtime::text_panel`).
    TextPanel(TextPanelMsg),
    Canvas(CanvasMessage),
    Toolbar(ToolbarMessage),
    /// Slow tick (~1 s) that fires while the editor is open AND the
    /// status pill is showing a dismissable message. Purely drives a
    /// re-render so the view fn can re-check the status's age
    /// (`Status::set_at`) and hide the toast once it ages out.
    StatusTick,
    /// First view callback for the editor window — used to record
    /// the `window::Id` so Discard can close the right one.
    WindowReady(iced::window::Id),
    /// User clicked Save in the editor.
    SaveRequested,
    /// Save task completed for the editor `session` it started in.
    /// `Ok(Some(path))` = written to disk; `Ok(None)` = the user
    /// cancelled the file picker.
    Saved {
        session: u64,
        result: Result<Option<std::path::PathBuf>, String>,
    },
    /// User clicked Copy (image to clipboard).
    CopyImageRequested,
    /// Image-copy task completed for the editor `session`.
    CopyImageDone {
        session: u64,
        result: Result<(), String>,
    },
    /// User selected a presentation frame for image outputs.
    /// A built-in Beautify preset was chosen (Frame popover; Beautify tab).
    BeautifyPreset(readshot_core::beautify::BuiltinPreset),
    /// A snapshot slider (Beautify, combine gap) was released: commit
    /// the preview as one undo step.
    SnapshotSliderCommit,
    /// User clicked Copy Text (run OCR + clipboard).
    CopyTextRequested,
    /// OCR + clipboard write completed for the editor `session`.
    CopyTextDone {
        session: u64,
        result: Result<String, String>,
    },
    /// User clicked Discard. Closes the editor window.
    DiscardRequested,
    /// "Keep Editing" on the armed-discard toast: back out of Discard.
    DiscardDisarmed,
    /// Dismiss the editor status toast (the × on error toasts).
    StatusDismissed,
    /// Open the given editor popover, or close it when it is already open.
    PopoverToggled(crate::editor::EditorPopover),
    /// Close whichever editor popover is open (click outside / Esc).
    PopoverClosed,
    /// User clicked Pin in the editor — flatten the current state,
    /// open a borderless always-on-top "pin" window with that
    /// image, and close the editor.
    PinRequested,
    /// Bump the editor's line width by `delta` (positive grows,
    /// negative shrinks). Triggered by `[` / `]` keyboard
    /// shortcuts; the runtime resolves the new width against the
    /// current model state and clamps to the published range.
    WidthBump(f32),
    /// An off-thread document compose (large resize, alignment or gap
    /// preview) finished; applied only when `seq` is still the newest
    /// request of `session` and nothing moved meanwhile.
    Composed {
        session: u64,
        seq: u64,
        output: crate::editor::ComposedOutput,
    },
    /// Cycle the editor's active palette colour. `+1` moves to the
    /// next swatch in `PALETTE`, `-1` to the previous; the runtime
    /// wraps via `rem_euclid` so the cycle is endless. Triggered by
    /// `,` / `.` keyboard shortcuts.
    ColorCycle(i32),
    /// Zoom editor preview in one step.
    ZoomIn,
    /// Zoom editor preview out one step.
    ZoomOut,
    /// Reset editor preview to exact image pixels.
    ZoomActual,
    /// Reset editor preview to fit-down mode.
    ZoomFit,
    /// Text-tool inline input — content typed by the user. Empty
    /// means the input is cleared.
    TextChanged(String),
    /// An edit action from the inline multi-line text editor widget.
    /// The handler applies it to the backing `text_editor::Content` and
    /// mirrors the result into `PendingText::content`.
    TextAction(iced::widget::text_editor::Action),
    /// User pressed Enter / clicked Commit on the text input. Builds
    /// an `Annotation::Text` from the pending state and commits it.
    TextCommit,
    /// User pressed Escape / clicked Cancel on the text input. Drops
    /// the pending state without committing.
    TextCancel,
    /// Commit the pending crop (Apply ↵).
    CropApply,
    /// Drop the pending crop (Cancel / esc).
    CropCancel,
    /// Delete the currently selected editor annotation, if any.
    DeleteSelected,
    /// Nudge the selected annotation by `(dx, dy)` base-image pixels via
    /// the arrow keys (Shift = larger step). Each press is one undo step.
    NudgeSelected(f32, f32),
    /// Edit the selected text annotation, if one is selected.
    EditSelectedText,
    /// Tab / ⇧Tab on the canvas: select the next (`1`) or previous (`-1`)
    /// annotation, so arrows, ⌫ and ↵ reach it without a click.
    CycleSelection(i32),
    /// The answer to a canvas Tab's check of the Beautify hex field: with
    /// the field focused, Tab only leaves it; otherwise the selection
    /// moves by `step`.
    TabResolved {
        step: i32,
        field_focused: bool,
    },
    /// The answer to an Undo, Redo or Up/Down nudge key's check of the
    /// Beautify hex field: with the field focused the key is the field's
    /// (iced's text_input has no undo and ignores Up/Down, so they reach
    /// the editor) and does nothing; otherwise `message` runs.
    FieldKeyResolved {
        /// The editor that asked; an answer for another is dropped.
        session: u64,
        message: Box<Message>,
        field_focused: bool,
    },
    /// ⌥ + arrow: grow or shrink the crop draft or the selection from
    /// its bottom-right edge; a selected magnifier moves only its bubble.
    ResizeByKeys(f32, f32),
    /// Keyboard event observed while an editor exists. Runtime filters
    /// it to the live editor window before acting.
    KeyPressed {
        window: iced::window::Id,
        key: iced::keyboard::Key,
        modifiers: iced::keyboard::Modifiers,
        status_ignored: bool,
    },
    /// Preview selected-annotation width while the toolbar slider is dragged.
    LineWidthPreview(f32),
    /// Commit the toolbar slider's selected-annotation width preview.
    LineWidthCommit,
    /// An options-bar change (fill, dashes, curve, heads, text size…).
    /// Applies to the next annotation and, when it fits, to the selected
    /// one as one undo step.
    Style(readshot_ui::editor::StyleMessage),
    /// Slider preview of an options-bar change on the selection (no
    /// history until `EditorStyleCommit`).
    StylePreview(readshot_ui::editor::StyleMessage),
    /// Commit the options-bar slider preview as one undo step.
    StyleCommit,
    /// The color popover's hex field changed.
    HexChanged(String),
    /// Enter in the hex field: apply the color if it parses.
    HexSubmitted,
    /// Turn the eyedropper on or off (I, or the button in the color
    /// popover). While on, a click on the image picks its color.
    EyedropperToggled,
}

/// Pin window messages: the pin rendered after the editor closed, and
/// each pin window's controls and keys. Handled by
/// `runtime::editor_ops::pins::handle_pin_message`.
#[derive(Clone, Debug)]
pub enum PinMsg {
    /// A pin's beautified image, rendered off the UI thread after Pin
    /// closed the editor; opens the pin window (`display_bounds`: the
    /// editor's source display, for placing it).
    Rendered {
        result: Result<image::RgbaImage, String>,
        display_bounds: Option<(f32, f32, f32, f32)>,
    },
    /// First view of a freshly-opened pin window — used to record
    /// its `window::Id` against its image state in `App::pins`.
    WindowReady(iced::window::Id, iced::widget::image::Handle),
    /// User pressed the close `×` on a pin window. Closes the
    /// window and removes its entry from `App::pins`.
    ClosePressed(iced::window::Id),
    /// User mouse-pressed inside the body of a pin window — kicks
    /// off a native window-drag so the pin can be repositioned.
    DragRequested(iced::window::Id),
    /// User adjusted a pin's image opacity.
    OpacityChanged(iced::window::Id, f32),
    /// User toggled whether a pin should stay put when clicked.
    LockToggled(iced::window::Id),
    /// Pointer entered (`true`) or left (`false`) a pin window.
    Hovered(iced::window::Id, bool),
    /// Keyboard event observed while at least one pin window is open.
    /// iced 0.14 buttons can't take focus, so the runtime taps keys
    /// globally and validates `window` against `App::pins` before
    /// acting (close / lock / opacity / nudge). WCAG 2.1.1.
    KeyPressed {
        window: iced::window::Id,
        key: iced::keyboard::Key,
        modifiers: iced::keyboard::Modifiers,
    },
}

/// Keyboard chords that move the editor's focus zone: into the Image
/// menu, onto a side-panel tab, or to the next zone. Handled by
/// `runtime::key_focus::handle_focus_message`.
#[derive(Clone, Debug)]
pub enum FocusMsg {
    /// ⇧⌘I: open (or close) the Image menu with its first row focused.
    ImageMenuKey,
    /// ⌥⌘T / ⌥⌘B (Ctrl+Shift+T / B off macOS): show the side panel on
    /// that tab, its tabs focused.
    PanelTabKey(crate::editor::PanelTab),
    /// ⌃Tab / F6 (`true`) or ⌃⇧Tab / ⇧F6: move the keyboard focus to the
    /// next (previous) zone: canvas → options bar → side panel.
    ZoneCycle(bool),
}

/// Image › Add Capture / Add Image from File, and the combined image's
/// gap and alignment. Handled by `runtime::combine::handle_combine_message`.
#[derive(Clone, Debug)]
pub enum CombineMsg {
    /// Image › Add Capture Below… / Beside…
    CaptureRequested(readshot_core::document::Join),
    /// Image › Add Image from File…
    AddImageFromFile,
    /// A picked file decoded (`Ok(None)` = the picker was cancelled).
    PartLoaded {
        session: u64,
        result: Result<Option<std::sync::Arc<image::RgbaImage>>, String>,
    },
    /// Image › Layout gap slider (preview; release commits).
    GapPreview(u32),
    /// A coalesced gap slider value is due (trailing edge of the
    /// preview throttle).
    GapFlush {
        session: u64,
    },
    Align(readshot_core::document::Align),
}

/// Image menu messages: rotate, flip and the Resize dialog. Handled by
/// `runtime::image_menu::handle_image_message`.
#[derive(Clone, Debug)]
pub enum ImageMsg {
    /// Rotate the document a quarter turn (⌘] right, ⌘[ left).
    Rotate {
        clockwise: bool,
    },
    Flip(readshot_core::document::FlipAxis),
    /// Open the Resize dialog (⌥⌘I; Ctrl+Shift+R off macOS; or
    /// Image › Resize…).
    ResizeOpen,
    ResizeWidthChanged(String),
    ResizeHeightChanged(String),
    ResizeUnitChanged(crate::editor::ResizeUnit),
    ResizeLockToggled(bool),
    ResizeApply,
}

/// Beautify tab messages: saved presets, controls, sliders and the
/// background color. Handled by
/// `runtime::beautify_panel::handle_beautify_message`.
#[derive(Clone, Debug)]
pub enum BeautifyMsg {
    /// A saved Beautify preset (by list index) was chosen.
    SavedPresetApplied(usize),
    /// A discrete Beautify control changed (swatch, canvas size, toggle).
    Edit(readshot_core::beautify::BeautifyEdit),
    /// A Beautify slider moved: live preview, no undo step yet.
    SliderPreview(readshot_core::beautify::BeautifyEdit),
    /// Save the current Beautify settings as a named preset.
    SavePreset,
    /// Remove a saved Beautify preset (by list index).
    DeletePreset(usize),
    /// Undo in the "Removed …" toast: put the removed preset back.
    RestorePreset,
    /// The Beautify background hex field was edited.
    HexChanged(String),
    /// The Beautify background hex field was submitted.
    HexSubmitted,
    /// Start the eyedropper to pick Beautify's background color.
    PickBackground,
}

/// Editor side panel and Text tab messages: recognition, line selection
/// and copy, smart redaction, and the History rewrite a redaction queues.
/// Handled by `runtime::text_panel::handle_text_panel_message`.
#[derive(Clone, Debug)]
pub enum TextPanelMsg {
    /// Start (or retry) recognizing the editor's base image for the Text
    /// panel. No-op while recognizing or once ready.
    Recognize,
    /// Text-panel recognition of part `part` (in that part's pixels)
    /// finished for editor session `session`.
    Recognized {
        session: u64,
        part: readshot_core::document::PartId,
        result: Result<readshot_core::text_lines::TextDocument, String>,
    },
    /// The newest History rewrite (scrub after a redaction, or restore)
    /// queued by editor session `session` finished. `None` when a newer
    /// rewrite of the same record superseded it before it ran.
    HistoryWritten {
        session: u64,
        result: Option<crate::coordinator::HistoryRewriteResult>,
    },
    /// Show / hide the editor side panel (⌥⌘P; Ctrl+Shift+P off macOS; or
    /// the panel buttons). In
    /// narrow windows this opens / closes the temporary overlay instead.
    PanelToggled,
    /// A side-panel tab was chosen.
    PanelTabSelected(crate::editor::PanelTab),
    /// Click outside the narrow-window overlay panel.
    PanelOverlayDismissed,
    /// "Open Settings" on the Text tab's no-text state.
    OpenTextSettings,
    /// Click on Text-panel line `index` (⇧ range, ⌘ toggle).
    LineClicked(usize),
    /// Copy the selected Text-panel lines.
    CopyLines,
    /// Copy N Lines finished: `Ok(n)` lines copied.
    CopyLinesDone(Result<usize, String>),
    /// "Remove line breaks when copying" toggled.
    RemoveLineBreaksToggled(bool),
    /// Smart-redact style (Solid block / Pixelate / Blur).
    RedactStyleChanged(readshot_core::redact::RedactStyle),
    /// Redact every pending sensitive item as one undo step.
    RedactAll,
    /// Redact the pending sensitive items in Text-panel line `index`.
    RedactLine(usize),
}

/// History browser messages, and the completions of History writes made
/// for a capture. Handled by `runtime::history_ops::handle_history_message`.
// `ReopenedAfterSync` carries the reopened image by value, as it did in
// the flat `Message` (which was as large): a message is moved, not
// copied, through one update, so boxing it would buy nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum HistoryMsg {
    /// Fire-and-forget history persistence completion. The boolean
    /// payload is `true` when the chain completed all the way through
    /// the OCR-and-update step (so the browser should reload to see
    /// the new text); `false` when history was disabled or OCR
    /// failed. Errors are logged but never block the user flow.
    RecordPersisted(Result<bool, String>),
    /// User asked for the history browser (tray menu / future
    /// hotkey). Either focuses the existing window or opens a new
    /// one and triggers a fresh `HistoryListLoaded`.
    OpenRequested,
    /// First view of the freshly-opened history window — runtime
    /// records the `window::Id` so close + focus paths work.
    WindowReady(iced::window::Id),
    /// Async list of persisted captures finished — populates the
    /// history browser. `Err` toasts an error and leaves the list
    /// empty.
    ListLoaded(Result<Vec<readshot_core::CaptureRecord>, String>),
    /// User closed the history browser window.
    Closed,
    /// User typed in the history browser's search box. Empty string
    /// resets to "show every record".
    SearchChanged(String),
    /// Dismiss button on the History status toast (errors stay until
    /// dismissed or replaced by the next status).
    StatusDismissed,
    /// User selected a capture in the history browser.
    Select(readshot_core::Uuid),
    /// Move selection to the previous visible history capture.
    SelectPrevious,
    /// Move selection to the next visible history capture.
    SelectNext,
    /// Open the selected history capture in the editor.
    OpenSelected,
    /// Delete the selected history capture.
    DeleteSelected,
    /// Keyboard shortcut from a history window. The runtime validates
    /// the window id before applying the action, because iced event
    /// subscriptions are global to the daemon.
    KeyboardShortcut(iced::window::Id, HistoryKeyboardAction),
    /// A copy / delete chord a widget (the search field) already
    /// captured. Applied only while the search query is empty, since an
    /// empty field has nothing to copy or delete.
    CapturedShortcut(iced::window::Id, HistoryKeyboardAction),
    /// Browser-level "Clear All" — first click arms the confirm
    /// prompt. The button label flips and a Cancel option appears;
    /// a second click sends `HistoryClearAllConfirmed`.
    ClearAllRequested,
    /// Confirm the armed "Clear All" — actually wipes the history.
    ClearAllConfirmed,
    /// Cancel the armed "Clear All" — returns the button to its
    /// idle state without touching any records.
    ClearAllCancelled,
    /// Open a History record in the editor (the row's Open, a
    /// double-click, Enter): `history_ops::open_history_record`, the one
    /// path that does. Annotation edits flow back into the JSON sidecar.
    OpenInEditor(readshot_core::Uuid),
    /// A History record to open, read fresh from disk once every History
    /// write of it was done or cancelled (`history_ops::load_after_writes`).
    /// `unsaved` names writes of earlier edits that failed.
    ReopenedAfterSync {
        record_id: readshot_core::Uuid,
        result: Result<(image::RgbaImage, readshot_core::CaptureRecord), String>,
        unsaved: Option<String>,
    },
    /// Per-row "Copy Image" — load PNG, decode, write to clipboard.
    CopyImage(readshot_core::Uuid),
    /// Per-row image clipboard write completed.
    CopyImageDone(Result<(), String>),
    /// Per-row "Reveal" — ask the OS file manager to select the PNG
    /// or open its containing folder.
    Reveal(readshot_core::Uuid),
    /// Per-row "Copy Text" — read the cached `ocr_text` and write it
    /// to the clipboard. Synchronous (no OCR re-run).
    CopyText(readshot_core::Uuid),
    /// Per-row OCR-text clipboard write completed.
    CopyTextDone(Result<String, String>),
    /// Copy OCR text from every currently visible history row.
    CopyVisibleTextRequested,
    /// Bulk OCR-text clipboard write completed.
    CopyVisibleTextDone(Result<usize, String>),
    /// Per-row "Pin" — load PNG, decode, open as a borderless
    /// always-on-top pin window.
    Pin(readshot_core::Uuid),
    /// Async PNG-decode finished for the row that asked to pin.
    PinReady(Result<image::RgbaImage, String>),
    /// Per-row "Delete" — first press arms the destructive prompt for
    /// that capture (the row's button flips to "Delete Capture"). A
    /// second press for the same id confirms; selecting another row,
    /// changing the search, or pressing Escape cancels.
    Delete(readshot_core::Uuid),
    /// Confirm the armed single-capture delete — actually removes the
    /// PNG + sidecar + index entry from disk and refreshes the list.
    DeleteConfirmed(readshot_core::Uuid),
    /// Cancel the armed single-capture delete without touching disk.
    DeleteCancelled,
    /// A freshly-captured record was written to disk (before OCR
    /// runs). Refreshes the open browser so the new row appears right
    /// away instead of only after OCR completes.
    RecordSaved,
    /// A freshly-captured record could not be written to History (a full
    /// disk, a read-only or unreadable History folder). `session` is the
    /// editor opened for that capture, if any. The capture is not in
    /// History; the user is told so and what to do.
    RecordSaveFailed {
        session: Option<u64>,
        /// The record that isn't in History: editors drop their link to
        /// it and the coordinator its parked rewrites.
        record_id: readshot_core::Uuid,
        error: String,
    },
    /// User clicked "Show More" in the history browser — reveal the
    /// next page of matching rows.
    ShowMore,
}

/// Settings and command-line setup window messages. Handled by
/// `runtime::settings_ops::handle_settings_message`.
#[derive(Clone, Debug)]
pub enum SettingsMsg {
    Edit(SettingsMessage),
    /// User asked for the settings window — tray "Settings…" entry or
    /// (future) a "Settings" command from elsewhere. Single-instance:
    /// focuses the existing window if one is already open.
    OpenRequested,
    /// User clicked the native folder picker button in Settings.
    ChooseSaveFolderRequested,
    /// Native folder picker completed. `Ok(None)` means cancelled.
    SaveFolderPicked(Result<Option<std::path::PathBuf>, String>),
    /// User clicked "Open" for the configured save folder in Settings.
    OpenSaveFolderRequested,
    /// Opening the configured save folder completed.
    OpenSaveFolderDone(Result<(), String>),
    /// User clicked "Record shortcut" in Settings.
    StartHotkeyRecording,
    /// Settings shortcut recorder captured a valid key combination.
    HotkeyRecorded(String),
    /// Settings shortcut recorder captured a key that is not valid as
    /// a global shortcut.
    HotkeyRecordingInvalid,
    /// Settings shortcut recorder was cancelled or captured an invalid key.
    HotkeyRecordingCancelled,
    /// User clicked the reset-all-settings button.
    ResetAllRequested,
    /// User confirmed reset-all-settings.
    ResetAllConfirmed,
    /// User cancelled reset-all-settings.
    ResetAllCancelled,
    /// User clicked "Uninstall Readshot…" — arms the confirm prompt.
    UninstallRequested,
    /// User cancelled the armed uninstall prompt.
    UninstallCancelled,
    /// User toggled the "Also delete history and preferences" checkbox
    /// in the armed uninstall prompt.
    UninstallPurgeToggled(bool),
    /// User confirmed the uninstall — runs the executor off the UI
    /// thread, then quits.
    UninstallConfirmed,
    /// The uninstall executor finished; drives the app quit.
    UninstallFinished,
    /// First `view` call after the settings window is opened. Records
    /// the `window::Id` so close + focus paths work.
    WindowReady(iced::window::Id),
    /// User picked a page in the Settings sidebar (click or ⌘1–⌘7).
    PaneSelected(SettingsPane),
    /// Keyboard event observed while the settings window is open. iced
    /// 0.14 buttons / togglers / pick-lists can't take focus, so the
    /// runtime taps keys globally and validates `window` against
    /// `App::settings_window_id` before acting. WCAG 2.1.1.
    KeyPressed {
        window: iced::window::Id,
        key: iced::keyboard::Key,
        modifiers: iced::keyboard::Modifiers,
        /// A text field (the filename template) took the key; its Esc
        /// only leaves the field.
        captured: bool,
    },
    /// User asked for the command-line setup instructions window.
    OpenCliToolsRequested,
    /// First `view` call after the command-line setup window opens.
    CliToolsWindowReady(iced::window::Id),
    /// User clicked Copy Commands for one shell in the command-line setup window.
    CliToolsCopyRequested(crate::cli_tools::Shell),
    /// Command-line setup instructions were copied, or failed to copy.
    CliToolsCopyDone(crate::cli_tools::Shell, Result<(), String>),
}

/// Scrolling-capture messages: the HUD and region windows, the frame
/// loop and stitching. Handled by `runtime::scroll::handle_scroll_message`.
#[derive(Clone, Debug)]
pub enum ScrollMsg {
    /// Periodic tick driving the scrolling-capture frame loop. Fires
    /// from a 250 ms timer subscription while a session is active.
    Tick,
    /// Async per-frame capture for the scrolling-capture session
    /// finished. `Ok` appends the frame; `Err` logs and continues
    /// (a one-off backend error shouldn't kill the whole session).
    Frame {
        seq: u64,
        result: Result<image::RgbaImage, String>,
    },
    /// Background motion-judge verdict for the frame at `seq`. `image`
    /// is the candidate frame handed back from the worker (`None` only
    /// if the worker died); `moved` says whether it scrolled vs the
    /// previous accepted frame and should join the session. `offset`
    /// is the measured vertical motion (`None` if unmeasurable, e.g. a
    /// size change); `rows` is the downscaled preview slice for the
    /// newly revealed rows, computed on the worker (`Some` only when
    /// `moved`).
    FrameJudged {
        seq: u64,
        image: Option<image::RgbaImage>,
        moved: bool,
        offset: Option<u32>,
        rows: Option<image::RgbaImage>,
    },
    /// User clicked Stop in the HUD (or pressed Esc / hit an auto-
    /// stop limit). Closes the HUD and kicks off stitching of the
    /// captured frames.
    StopRequested,
    /// User clicked Cancel in the HUD. Throws the session away
    /// without stitching — captured frames are dropped on the floor.
    CancelRequested,
    /// User clicked Keep Going on an armed cancel. Disarms the cancel
    /// so the session carries on recording.
    KeepGoing,
    /// Stitching task finished. `Ok` opens the stitched image in the
    /// editor; `Err` toasts a status and clears the session.
    Stitched(Result<image::RgbaImage, String>),
    /// First view of the scrolling-capture HUD window — records the
    /// `window::Id` so the runtime can close it when the session ends.
    HudWindowReady(iced::window::Id),
    /// User pressed the HUD's drag handle. Hands off the OS window
    /// drag so they can reposition the HUD without it stealing focus
    /// from the page they're scrolling.
    HudDragRequested,
    /// First view of the transparent click-through region-indicator
    /// window — runtime records the id and enables mouse passthrough
    /// so scroll events fall through to the page underneath.
    RegionWindowReady(iced::window::Id),
}

/// Window lifecycle (close, rescale, drag) and the editor window's
/// high-frequency pointer, modifier, hover, size and scroll events. These
/// never reach the editor handler, so they never clear the canvas geometry
/// cache or settle pending history steps. Handled by
/// `runtime::window_ops::handle_window_message`.
#[derive(Clone, Debug)]
pub enum WindowMsg {
    /// OS reports a window was closed (X button, `window::close`, …).
    /// The handler forgets the id from `Windows` and runs any
    /// kind-specific cleanup (e.g. clearing history-browser state).
    Closed(iced::window::Id),
    /// OS requested a window close (X button / ⌘W) but the window has
    /// `exit_on_close_request: false`, so iced has NOT closed it yet.
    /// The handler decides whether to close: the editor routes through
    /// the same unsaved-work confirmation as the in-app Discard button;
    /// every other window closes immediately.
    CloseRequested(iced::window::Id),
    /// A window was resized: `(window, logical width)`. Only the editor
    /// window's width is kept. Handled at top level (no cache clear).
    EditorWindowResized(iced::window::Id, f32),
    /// Redraw tick for the recognizing skeleton (not under Reduce Motion).
    /// Handled at top level (no cache clear).
    EditorPanelTick,
    /// Pointer entered Text-panel line `index`. Handled at top level
    /// (no cache clear): it fires per pointer move.
    EditorTextLineHovered(usize),
    /// Pointer left Text-panel line `index`. Handled at top level.
    EditorTextLineUnhovered(usize),
    /// Command (⌘ / Ctrl) held state changed while an editor is open.
    /// Handled at top level (no cache clear).
    EditorCommandChanged(bool),
    /// Mouse-down on a view's own title row (windows that use the
    /// transparent full-size title bar) — starts a native window drag.
    DragRequested(iced::window::Id),
    /// The editor stage scrolled to this offset (content px).
    EditorStageScrolled(f32, f32),
    /// The OS reported a new scale factor for a tracked window.
    Rescaled(iced::window::Id, f32),
    /// Current Shift state while an editor is open. iced 0.14 doesn't
    /// route keyboard events into canvas widgets, so the runtime watches
    /// modifiers globally and forwards them to the editor canvas, which
    /// uses them to constrain shape drags (square / 45°).
    EditorShiftChanged(bool),
    /// The left mouse button went down (`true`) or up (`false`) in any
    /// window while an editor is open. Slider drags have no press
    /// message of their own; this tells a held drag from a key or
    /// Ctrl+wheel preview. Carries the window the event came from.
    EditorPointerButton(bool, Option<iced::window::Id>),
    /// A window lost focus: if the held press began there, its release
    /// may never reach the app.
    EditorPointerWindowUnfocused(iced::window::Id),
}

/// Welcome window messages. Handled by
/// `runtime::welcome_ops::handle_welcome_message`.
#[derive(Clone, Debug)]
pub enum WelcomeMsg {
    /// First native-window-ready callback after the welcome window is
    /// opened. Used by the runtime to focus the window after Launch
    /// Services reopens an LSUIElement app.
    WindowReady(iced::window::Id),
    /// Dismiss button on the Welcome status toast (errors stay until
    /// dismissed or the next capture clears them).
    StatusDismissed,
    /// Keyboard event observed while a welcome window is open. The
    /// runtime validates `window` is the welcome window and binds
    /// Enter to the primary action of whichever card is showing
    /// (Allow / Open Settings / Capture). WCAG 2.1.1.
    KeyPressed {
        window: iced::window::Id,
        key: iced::keyboard::Key,
        modifiers: iced::keyboard::Modifiers,
    },
}

/// Background drain ticks (global hotkeys, tray, `readshot://` URLs,
/// Launch Services reopen, appearance) and the tray, URL and theme events
/// they deliver. Handled by `runtime::tick_ops::handle_tick_message`.
#[derive(Clone, Debug)]
pub enum TickMsg {
    /// 50 ms drain tick — runtime polls the global-hotkey receiver and
    /// emits a `CaptureFullPrimaryRequested` if any events arrived.
    Hotkey,
    /// 100 ms drain tick — runtime polls the tray + menu event channels.
    Tray,
    /// 100 ms drain tick — runtime polls queued `readshot://` URL events.
    Url,
    /// 100 ms drain tick — runtime polls queued macOS app-reopen events.
    AppReopen,
    /// User selected an item in the tray menu (or left-clicked the icon).
    TrayAction(crate::tray::TrayAction),
    /// User invoked the URL scheme.
    UrlAction(crate::url_scheme::UrlAction),
    /// macOS/Windows appearance changed (or the initial query returned).
    /// `Mode::None` means "unknown" and falls back to dark.
    SystemThemeChanged(iced::theme::Mode),
    /// 1s tick; `update()` re-reads the OS appearance on the app thread.
    ThemePoll,
}

/// Capture messages: the region overlay, region and full-display
/// captures, and the overlay quick actions done without an editor.
/// Handled by `runtime::capture_ops::handle_capture_message`.
#[derive(Clone, Debug)]
pub enum CaptureMsg {
    /// User clicked "Capture primary display" in the welcome window.
    FullPrimaryRequested,
    /// Shift modifier toggled while an overlay window is open. Drives
    /// the "hold Shift = square selection" constraint; iced 0.14 does
    /// not reliably route modifier changes to the canvas, so the runtime
    /// listens globally and forwards the state into the overlay.
    OverlayShiftChanged(bool),
    /// User asked for a region capture (welcome button / tray menu).
    OpenOverlayRequested,
    /// User asked to repeat the most recent overlay selection.
    RetakeLastRegionRequested,
    /// First view of the overlay window — used by the runtime to
    /// remember its `window::Id` mapping.
    OverlayWindowReady(iced::window::Id),
    /// User confirmed an overlay selection. `display_id` identifies
    /// the monitor; `rect` is in display-local logical pixels;
    /// `intent` decides what happens to the captured image
    /// (open editor, copy to clipboard, save to disk, or pin).
    OverlaySelected {
        display_id: readshot_capture::DisplayId,
        rect: readshot_core::geom::Rect,
        intent: CaptureIntent,
    },
    /// Selection rect inside the overlay changed (committed or
    /// during resize/move). Drives the floating action toolbar's
    /// position. `None` means the selection was cleared.
    OverlaySelectionChanged {
        display_id: readshot_capture::DisplayId,
        rect: Option<readshot_core::geom::Rect>,
    },
    /// User cancelled the overlay (ESC, sub-pixel click, etc.).
    OverlayCancelled,
    /// Async list_displays result that bootstraps the overlay flow:
    /// the runtime opens one transparent overlay window per display.
    OverlayDisplaysListed(Result<Vec<readshot_capture::DisplayInfo>, String>),
    /// Internal: capture a region of the named display, save the
    /// PNG, and toast the result. Emitted by the overlay flow.
    RegionRequested {
        display_id: readshot_capture::DisplayId,
        rect: readshot_core::geom::Rect,
    },
    /// Direct (no-editor) clipboard-copy completion for an overlay
    /// quick-action.
    OverlayCopyDone(Result<(), String>),
    /// Direct (no-editor) save-dialog completion for an overlay
    /// quick-action. `Ok(Some(path))` = written; `Ok(None)` =
    /// user cancelled the picker.
    OverlaySaveDone(Result<Option<std::path::PathBuf>, String>),
    /// Direct (no-editor) OCR-text-copy completion. Carries the
    /// recognised text so the toast can report a character count.
    OverlayCopyTextDone(Result<String, String>),
    /// Hidden one-shot CLI interactive capture finished writing its
    /// temp PNG. The runtime exits immediately afterward.
    CliInteractiveWritten(Result<(), String>),
    /// Async region-capture finished — `Ok(image)` opens an editor
    /// window with the captured pixels; `Err` toasts the failure on
    /// the welcome window.
    RegionCaptured(Result<image::RgbaImage, String>),
    /// Background capture-and-save task finished. Carries the final
    /// PNG path or a stringified error.
    Saved(Result<PathBuf, String>),
}

/// App-level messages: the screen-recording permission, quit, restart
/// and Launch Services reopening the running app. Handled by
/// `runtime::app_ops::handle_app_message`.
#[derive(Clone, Debug)]
pub enum AppMsg {
    /// Permission-status poll fired on a timer.
    PermissionPoll(crate::permissions::PermissionStatus),
    /// Subscription tick — `runtime::update` reads the current status
    /// and feeds it as `PermissionPoll`. Separate from the latter so
    /// tests can drive `PermissionPoll` directly without faking the
    /// timer.
    PermissionTick,
    /// Periodic poll tick. Uses the non-prompting status check so the
    /// system permission alert isn't re-raised every interval.
    PermissionPollTick,
    /// User clicked "Grant" in the welcome window.
    GrantPermissionRequested,
    /// User clicked "Open Settings" in the denied state.
    OpenPermissionSettingsRequested,
    /// User clicked "Reset Permission": clear a stale grant left for an
    /// older code signature, then ask again.
    ResetPermissionRequested,
    /// User clicked "Quit" — graceful exit so the next launch picks
    /// up newly-granted TCC permissions.
    QuitRequested,
    /// User clicked "Restart" — relaunch the bundle so TCC's cached
    /// Screen Recording grant becomes visible to the new process.
    RestartRequested,
    /// Finder / Launch Services reopened the already-running app.
    ReopenRequested,
}

/// Top-level application state.
pub struct App {
    pub coordinator: CaptureCoordinator,
    pub permissions: Arc<dyn PermissionsProvider>,
    pub preferences: Preferences,
    pub welcome: WelcomeState,
    /// Last observed Screen Recording status. Views read this instead of
    /// probing, since a probe from `view()` would run on every redraw.
    pub permission_status: crate::permissions::PermissionStatus,
    pub windows: Windows,
    /// `true` while a capture-and-save task is in flight; the welcome
    /// window's button is disabled in that state.
    pub capture_in_flight: bool,
    /// Last toast text shown under the capture button. Cleared when
    /// the user kicks off a new capture.
    pub last_capture_status: Option<String>,
    /// The last capture status raised as a system notification, so a
    /// status is announced once however many nested updates see it.
    pub announced_capture_status: Option<String>,
    /// The user has been told (once, in the tray menu) that Readshot's
    /// notifications are off and how to turn them on.
    pub notifications_off_told: bool,
    /// Live `GlobalHotKeyManager` for the registered capture shortcut.
    /// Held here so it isn't dropped (which unregisters the hotkey).
    /// Tests and CLI invocations leave this `None`.
    pub hotkey_manager: Option<global_hotkey::GlobalHotKeyManager>,
    /// Maps global-hotkey ids back to app actions. The upstream
    /// event channel is process-wide and only reports ids.
    pub hotkey_actions: HashMap<u32, GlobalHotkeyAction>,
    /// esc / ⇧esc registered as global hotkeys for the running scrolling
    /// capture (`sync_scroll_escape_hotkeys`): the user is scrolling
    /// another app, where the HUD's in-window key would never arrive.
    pub scroll_escape_hotkeys: Vec<global_hotkey::hotkey::HotKey>,
    /// Registering them was tried for the running session (whatever
    /// came of it): a failure is not retried, or logged, on every update.
    pub scroll_escape_tried: bool,
    /// Whether the user-configured capture shortcut is currently
    /// registered. The fixed History / Settings shortcuts can still
    /// work when this is false, so this is tracked separately from
    /// `hotkey_manager`.
    pub capture_hotkey_registered: bool,
    /// Human names of any fixed hotkeys (History, Settings) that failed
    /// to register globally on the last attempt. Surfaced in the settings
    /// window so the failure isn't silent.
    pub fixed_hotkey_failures: Vec<&'static str>,
    /// Live tray-icon controller. Held to keep the icon visible.
    /// Tests and CLI invocations leave this `None`.
    pub tray: Option<crate::tray::TrayController>,
    /// Active editor session, if any. Phase C only allows one
    /// editor at a time; opening a new one replaces the old.
    pub editor: Option<crate::editor::EditorSession>,
    /// Editors closed (or replaced) while a history step still composed
    /// off the UI thread: kept, windowless, until the worker lands, then
    /// finished (History synced) and dropped. Closing never composes on
    /// the UI thread ([`crate::runtime`]'s `retire_editor`).
    pub retiring_editors: Vec<crate::editor::EditorSession>,
    /// `window::Id` → display info for every live overlay window.
    /// One entry per monitor when the overlay flow is active; empty
    /// otherwise. The runtime uses this to look up the display id for
    /// a `view()` callback (so the canvas knows which display id to
    /// stamp into `CaptureMsg::OverlaySelected`) and to convert the
    /// returned rect back into a `CaptureRequest` with the right
    /// scale.
    pub overlay_displays: HashMap<iced::window::Id, OverlayDisplay>,
    /// True while display enumeration for a new overlay session is in
    /// flight. This closes the race where two hotkey/tray clicks can both
    /// enqueue `list_displays()` before any overlay window id exists.
    pub overlay_opening: bool,
    /// Current system appearance (overlay-like windows ignore it).
    pub theme_mode: crate::runtime::ThemeMode,
    /// Live overlay selections per display. Drives the floating
    /// action toolbar's position; populated by
    /// [`CaptureMsg::OverlaySelectionChanged`] from the canvas.
    pub overlay_selections: HashMap<readshot_capture::DisplayId, readshot_core::geom::Rect>,
    /// Most recent committed overlay selection per display. Used by
    /// the "Retake Last Region" flow to avoid re-drawing the same
    /// rectangle during repetitive capture work.
    pub last_regions: HashMap<readshot_capture::DisplayId, LastRegion>,
    /// Display id for the most recent committed overlay selection.
    /// `last_regions` stores one rectangle per display; this pointer
    /// makes "last" deterministic across multi-monitor captures.
    pub last_region_display_id: Option<readshot_capture::DisplayId>,
    /// What to do with the next finished region capture. Set by the
    /// overlay quick-actions; consumed in `RegionCaptureCompleted`.
    /// `None` falls back to the editor.
    pub pending_intent: Option<CaptureIntent>,
    /// The editor session an Add Capture Below/Beside returns to, while
    /// its overlay is up and the editor window hidden.
    pub combine_capture: Option<CombineCapture>,
    /// Display id for the in-flight capture. Carried alongside
    /// `pending_intent` so the resulting history record can be
    /// stamped with the right monitor.
    pub pending_display_id: Option<readshot_capture::DisplayId>,
    /// Screen scale factor for the in-flight capture display. The
    /// editor uses this to make actual-pixels zoom mean physical
    /// pixels instead of logical UI points.
    pub pending_display_scale: Option<f32>,
    /// Logical bounds (origin_x, origin_y, width, height) of the
    /// display the in-flight capture lives on. Captured at
    /// `OverlaySelected` time (before the overlay close-task drains
    /// `overlay_displays`) so downstream flows like scroll-capture
    /// HUD placement and the on-screen region indicator window don't
    /// need to round-trip back through `coordinator.list_displays`.
    pub pending_display_bounds: Option<(f32, f32, f32, f32)>,
    /// Whether the in-flight overlay capture should hide the cursor.
    /// Normal app captures hide it; CLI interactive can opt into
    /// including it with `--show-cursor`.
    pub pending_hide_cursor: bool,
    /// URL action that arrived before Screen Recording permission was
    /// granted. Replayed when the permission gate clears instead of
    /// disappearing into an `OpenOverlayRequested` no-op.
    pub pending_url_after_permission: Option<crate::url_scheme::UrlAction>,
    /// Output path for the hidden one-shot CLI interactive child
    /// process. When set, the overlay writes the selected image here
    /// and exits instead of opening app UI.
    pub cli_interactive_output: Option<PathBuf>,
    /// Live pin windows mapped to their pre-rendered image state.
    /// Each pin window's `view` reads its state from this map. The
    /// map shrinks as pins close.
    pub pins: HashMap<iced::window::Id, PinState>,
    /// The pin window the pointer is currently over; drives the
    /// hover-only control bar.
    pub hovered_pin: Option<iced::window::Id>,
    /// Tracks whether Shift is currently held while the region-overlay
    /// is open. The overlay's canvas Program would read modifiers
    /// itself if iced delivered keyboard events to canvas widgets, but
    /// it doesn't — so the runtime tracks Shift via a global keyboard
    /// subscription and threads it back into each `OverlayProgram` on
    /// view().
    pub overlay_shift_held: bool,
    /// Tracks whether Shift is held while an editor window is open. Same
    /// rationale as `overlay_shift_held`: iced 0.14 doesn't reliably route
    /// modifier changes into canvas widgets, so the runtime tracks Shift globally and
    /// threads it into the editor canvas to constrain shape drags.
    pub editor_shift_held: bool,
    /// Command (⌘ / Ctrl) held while an editor is open, for ⌘-click in
    /// the Text panel (`mouse_area` reports no modifiers).
    pub editor_command_held: bool,
    /// Last directory the user saved into. The editor's save picker
    /// seeds itself there next time so a burst of related captures
    /// lands in the same place. Reset whenever the user picks a new
    /// directory — never written to disk; in-memory only.
    pub last_save_dir: Option<std::path::PathBuf>,
    /// Root of the persistent capture history on disk. `None` when
    /// the system can't supply a project dir (rare; some test
    /// builds). Used by the history browser to resolve relative PNG
    /// paths into iced `image::Handle`s.
    pub history_root: Option<std::path::PathBuf>,
    /// Currently-loaded history records, newest-first. Refreshed
    /// whenever the history window opens.
    pub history_records: Vec<readshot_core::CaptureRecord>,
    /// `window::Id` of the live history browser, if any. The browser
    /// is single-instance — opening it again focuses the existing
    /// window instead of spawning a duplicate.
    pub history_window_id: Option<iced::window::Id>,
    /// Last error from a history list / load operation. Surfaced as
    /// a toast inside the browser when set.
    pub history_status: Option<String>,
    /// A History record being opened whose earlier editor is still
    /// composing: its load starts once that editor has landed and queued
    /// its History sync (`history_ops::load_when_finished`). Edits are
    /// never dropped.
    pub pending_history_open: Option<readshot_core::Uuid>,
    /// The History record whose open is under way (waiting for editors,
    /// writes, or the load; `history_ops::open_history_record`). A second
    /// open of it joins this one. Cleared when History closes or another
    /// editor becomes current: the open is then cancelled, and says so.
    pub awaiting_reopen: Option<readshot_core::Uuid>,
    /// Earlier edits that weren't saved to History, said when the record
    /// `awaiting_reopen` opens.
    pub history_open_note: Option<String>,
    /// The last History open cancelled while it waited, and what was said
    /// then (its failed writes are added when the wait ends).
    pub cancelled_history_open: Option<(readshot_core::Uuid, String)>,
    /// Live search query in the history browser. Empty = show every
    /// record. Substring-matched (case-insensitive) against
    /// `ocr_text` and the formatted timestamp.
    pub history_search: String,
    /// Selected history row. The browser keeps this inside the
    /// currently-visible filtered list, so keyboard navigation and
    /// selected-row actions have a deterministic target.
    pub history_selected_id: Option<readshot_core::Uuid>,
    /// Capture whose single-row delete is armed (awaiting a confirm
    /// press). `Some(id)` flips that row's Delete button to a
    /// two-stage "Delete Capture / Cancel" prompt, mirroring the
    /// "Clear All" guard. Cleared on confirm, cancel, Escape, a search
    /// change, or moving the selection.
    pub history_delete_pending: Option<readshot_core::Uuid>,
    /// How many matching rows the browser renders at once. The list
    /// is built eagerly (iced 0.14 has no virtual list), so an
    /// Unlimited-retention history would otherwise build thousands of
    /// image widgets on open. Capped to a page; "Show More" grows it,
    /// and a search change resets it. The search filter itself still
    /// scans every record.
    pub history_page_limit: usize,
    /// On-disk path the user's preferences are read from at startup
    /// and written back to whenever a `SettingsMsg::Edit` mutation
    /// flips a field. `None` in tests and when the OS can't supply a
    /// config dir; in that case settings changes stay in-memory only.
    pub preferences_path: Option<std::path::PathBuf>,
    /// `window::Id` of the live settings window, if any. Single
    /// instance — re-opening just refocuses the existing window.
    pub settings_window_id: Option<iced::window::Id>,
    /// True while Settings is waiting for the next keyboard chord to
    /// become the global capture hotkey.
    pub settings_recording_hotkey: bool,
    /// Short validation message for the current hotkey recording
    /// attempt, shown inline in Settings.
    pub settings_hotkey_error: Option<String>,
    /// Short success message for the current hotkey registration,
    /// shown inline in Settings.
    pub settings_hotkey_status: Option<String>,
    /// Short general-purpose status text shown at the bottom of
    /// Settings.
    pub settings_status: Option<String>,
    /// True after the user asks to reset settings and before they
    /// confirm or cancel.
    pub settings_reset_all_pending: bool,
    /// True after the user clicks "Uninstall Readshot…" in Settings and
    /// before they confirm or cancel. Drives the two-stage destructive
    /// prompt that guards the built-in uninstall.
    pub settings_uninstall_pending: bool,
    /// Whether the armed uninstall's "Also delete history and
    /// preferences" checkbox is ticked.
    pub settings_uninstall_purge: bool,
    /// Settings sidebar page on screen. Kept across window reopenings
    /// (not persisted across launches).
    pub settings_pane: SettingsPane,
    /// The Settings control the keyboard focus ring is on, if any.
    pub settings_focus: Option<SettingsFocus>,
    /// What the last Settings shortcut changed ("Default format: JPEG"),
    /// shown by the pane title so a ⌘-key change is never silent.
    pub settings_key_feedback: Option<String>,
    /// True after the user clicks "Clear All" in the history browser
    /// and before they confirm or cancel. Drives the two-stage
    /// destructive prompt that guards against accidentally nuking
    /// every saved capture.
    pub history_clear_all_pending: bool,
    /// `window::Id` of the live command-line setup instructions window.
    pub cli_tools_window_id: Option<iced::window::Id>,
    /// Copy-status text shown in the command-line setup window.
    pub cli_tools_status: Option<String>,
    /// Active scrolling-capture session, if any. Holds the per-frame
    /// state (captured frames, no-motion counter, HUD window id) so
    /// the runtime can drive the capture loop, render the HUD, and
    /// stitch the result when the session ends.
    pub scroll_session: Option<ScrollSession>,
    /// Whether the OS "reduce motion" accessibility setting is on.
    /// Read at startup and refreshed when an animated surface (region
    /// overlay / scroll session) opens. When `true` the scroll
    /// window's recording pulse holds still; static cues (outline, counter, the
    /// "● Recording" label) stay. Defaults to `false`; the runtime
    /// fills it from `os_reduce_motion()` so `App::new` stays pure.
    pub reduce_motion: bool,
}

/// State for an active scrolling-capture session.
///
/// One session at a time. Reset when stitching kicks off or the user
/// cancels. The runtime starts captures on timer ticks, buffers
/// completed responses by sequence id, appends only frames with
/// detected vertical scroll, and drains `frames` into the stitcher
/// when the user stops the session or the hard frame limit is hit.
#[derive(Clone, Debug)]
pub struct ScrollSession {
    /// Display the captured region lives on. Carried through every
    /// per-tick `CaptureRequest`.
    pub display_id: readshot_capture::DisplayId,
    /// Capture region in display-local logical pixels.
    pub rect: readshot_core::geom::Rect,
    /// HiDPI scale factor of the display.
    pub scale: f32,
    /// Frames captured so far. Frames come from timer ticks and are
    /// appended in capture sequence order even when backend futures
    /// complete out of order. `Arc` so the background motion-judge
    /// task can borrow the previous frame without a multi-megabyte
    /// clone per comparison.
    pub frames: Vec<std::sync::Arc<image::RgbaImage>>,
    /// Number of consecutive timer ticks that produced a frame with
    /// no detectable vertical scroll vs. the previous accepted frame.
    /// Surfaced for diagnostics; the user explicitly stops the
    /// session when ready.
    pub no_motion_count: u32,
    /// `window::Id` of the HUD floating above the capture region.
    pub hud_window_id: Option<iced::window::Id>,
    /// `window::Id` of the transparent click-through overlay that
    /// draws the captured-rect border on screen, so the user can see
    /// what area is being captured while they scroll the underlying
    /// content.
    pub region_window_id: Option<iced::window::Id>,
    /// Logical bounds (origin_x, origin_y, width, height) of the
    /// display the session is running on. Cached at session-open time
    /// so multi-monitor windows can be sized and positioned without a
    /// coordinator round-trip.
    pub display_size: Option<(f32, f32, f32, f32)>,
    /// Number of `capture_region` futures currently in flight. The
    /// tick handler caps this at `SCROLL_MAX_CONCURRENT_CAPTURES` so
    /// fast scrolls can run multiple captures in parallel while a
    /// slow backend still naturally throttles the loop.
    pub capture_in_flight: u32,
    /// Monotonic id assigned to each per-tick capture request.
    /// Multiple captures may be in flight, so completion order is not
    /// necessarily capture order.
    pub next_capture_seq: u64,
    /// Next capture id that may be appended to `frames`. Responses
    /// are buffered until this sequence arrives so stitching receives
    /// frames in chronological order.
    pub next_frame_seq_to_process: u64,
    /// Completed capture responses waiting for earlier sequence ids.
    pub pending_frames: BTreeMap<u64, Result<image::RgbaImage, String>>,
    /// `Some(seq)` while a frame is being compared against the
    /// previous accepted frame on a blocking worker (the SAD scan is
    /// too expensive for the UI thread). Draining pauses until the
    /// verdict (`ScrollMsg::FrameJudged`) comes back so frames
    /// stay in chronological order.
    pub judge_in_flight: Option<u64>,
    /// True after the user has clicked Stop / pressed Esc / hit a
    /// limit. The next tick observes this and kicks off stitching
    /// instead of capturing another frame.
    pub stopping: bool,
    /// `Some(when)` once the user has clicked Cancel for the first
    /// time. The second click inside a short window confirms the
    /// discard; otherwise the arm expires. Guards a long scroll
    /// session from a single mis-click.
    pub cancel_armed_at: Option<std::time::Instant>,
    /// Wall-clock time the session began — drives the HUD's elapsed
    /// counter so the user can see how long they've been recording.
    pub started_at: std::time::Instant,
    /// Last frame that was accepted (frame count bumped) — used by
    /// the HUD to draw a brief flash when a fresh frame lands.
    pub last_frame_at: Option<std::time::Instant>,
    /// Counter that increments every time a fresh frame is accepted.
    /// Used purely for HUD animation gating (we can't trust the
    /// frames-vec length for animation triggers because some ticks
    /// produce no-motion drops which leave the length unchanged).
    pub frame_tick: u64,
    /// Downscaled (`PREVIEW_STRIP_W` wide) stitched preview of the
    /// session so far: the first frame, then each accepted frame's newly
    /// revealed rows appended below. Capped at `PREVIEW_STRIP_MAX_H`,
    /// keeping the newest rows.
    pub preview_strip: Option<image::RgbaImage>,
    /// Cached `iced::Handle` for `preview_strip`, rebuilt once per
    /// accepted frame so the window doesn't re-clone RGBA per redraw.
    pub preview_handle: Option<iced::widget::image::Handle>,
    /// Estimated stitched height (source px): the first frame's full
    /// height plus each accepted frame's measured scroll offset.
    pub stitched_rows: u32,
    /// Height of the latest accepted frame scaled to the strip width,
    /// so the view can mark where the live frame sits in the strip.
    pub last_frame_rows_scaled: u32,
    /// Frame count handed to the stitcher at stop time (`frames` is
    /// drained into the stitch task, so the window reads this while
    /// "Stitching…").
    pub stitch_frame_count: usize,
    /// Strip-px heights of the most recent appended preview slices
    /// (oldest first, capped at `RECENT_JOINS_MAX`). Each marks a join
    /// between frames; the window draws a tick at each one.
    pub recent_join_heights: std::collections::VecDeque<u32>,
}

impl ScrollSession {
    pub fn new(
        display_id: readshot_capture::DisplayId,
        rect: readshot_core::geom::Rect,
        scale: f32,
    ) -> Self {
        Self {
            display_id,
            rect,
            scale,
            frames: Vec::new(),
            no_motion_count: 0,
            hud_window_id: None,
            region_window_id: None,
            display_size: None,
            capture_in_flight: 0,
            next_capture_seq: 0,
            next_frame_seq_to_process: 1,
            pending_frames: BTreeMap::new(),
            judge_in_flight: None,
            stopping: false,
            cancel_armed_at: None,
            started_at: std::time::Instant::now(),
            last_frame_at: None,
            frame_tick: 0,
            preview_strip: None,
            preview_handle: None,
            stitched_rows: 0,
            last_frame_rows_scaled: 0,
            stitch_frame_count: 0,
            recent_join_heights: std::collections::VecDeque::new(),
        }
    }
}

/// Per-overlay-window record. Tracks which display the window covers
/// so capture can run against the right monitor, plus the logical
/// bounds needed for edge-aware toolbar positioning and multi-monitor
/// follow-up windows (scroll-capture region indicator).
#[derive(Clone, Debug)]
pub struct OverlayDisplay {
    pub display_id: readshot_capture::DisplayId,
    /// HiDPI scale factor of the display (physical / logical).
    pub scale: f32,
    /// Logical x origin of the display in global screen coordinates.
    /// macOS: relative to the primary display's top-left.
    pub origin_x: f32,
    /// Logical y origin of the display in global screen coordinates.
    pub origin_y: f32,
    /// Logical width of the overlay window (= display logical width).
    pub width: f32,
    /// Logical height of the overlay window.
    pub height: f32,
}

/// Last committed overlay region for a display.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LastRegion {
    pub rect: readshot_core::geom::Rect,
    pub display_scale: f32,
    pub display_bounds: Option<(f32, f32, f32, f32)>,
}

/// Runtime state for one pinned capture window.
#[derive(Clone, Debug)]
pub struct PinState {
    pub handle: iced::widget::image::Handle,
    pub opacity: f32,
    pub locked: bool,
}

impl PinState {
    pub fn new(handle: iced::widget::image::Handle) -> Self {
        Self {
            handle,
            opacity: 1.0,
            locked: false,
        }
    }
}

/// What happens to the captured image after an overlay confirms.
/// Selected by the floating action toolbar's buttons; `Editor` is the
/// default (Enter / Space, click on the primary "Capture" button).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureIntent {
    /// Open the annotation editor with the captured image. Existing
    /// default flow.
    Editor,
    /// Write the captured PNG to the system clipboard, no editor.
    CopyToClipboard,
    /// Run Apple Vision OCR over the captured image and write the
    /// recognised text to the clipboard, no editor.
    CopyTextDirect,
    /// Open a native save dialog directly, no editor.
    SaveDirect,
    /// Open a borderless always-on-top pin window, no editor.
    Pin,
    /// Start a scrolling-capture session: capture the region repeatedly
    /// while the user scrolls the underlying content, then stitch the
    /// frames into a tall image and open it in the editor.
    ScrollCapture,
    /// Hidden one-shot CLI bridge: write PNG bytes to a temp path
    /// and exit so the parent CLI process can continue.
    CliInteractive,
    /// Append the capture to the open editor's document (Image › Add
    /// Capture…); no History record, no new editor.
    CombineIntoEditor,
}

/// An Add Capture Below/Beside in flight: the editor window
/// is hidden while the overlay is up; the capture lands in this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CombineCapture {
    pub session: u64,
    pub join: readshot_core::document::Join,
    /// The overlay selection for this combine was made: its capture is
    /// in flight and must land in (or fail back to) this editor, so the
    /// "overlay never opened / went away" safety nets leave it alone.
    /// Kept here rather than inferred from `App::pending_display_id`,
    /// which other flows (scroll capture, retake) also set.
    pub selected: bool,
}

impl App {
    /// Forget held ⌘/⇧. Called when an editor opens or closes: the
    /// key-up for a modifier released while no editor window had focus
    /// never arrives, and a stale flag would turn the next click into a
    /// ⌘-click or constrain the next drag.
    pub(crate) fn release_editor_modifiers(&mut self) {
        self.editor_command_held = false;
        self.editor_shift_held = false;
    }

    pub fn new(
        coordinator: CaptureCoordinator,
        permissions: Arc<dyn PermissionsProvider>,
        preferences: Preferences,
    ) -> Self {
        let permission_status = coordinator.pre_capture_gate();
        let welcome = WelcomeState::from_status(permission_status);
        Self {
            coordinator,
            permissions,
            preferences,
            welcome,
            permission_status,
            windows: Windows::default(),
            capture_in_flight: false,
            last_capture_status: None,
            announced_capture_status: None,
            notifications_off_told: false,
            hotkey_manager: None,
            hotkey_actions: HashMap::new(),
            scroll_escape_hotkeys: Vec::new(),
            scroll_escape_tried: false,
            capture_hotkey_registered: false,
            fixed_hotkey_failures: Vec::new(),
            tray: None,
            editor: None,
            retiring_editors: Vec::new(),
            overlay_displays: HashMap::new(),
            overlay_opening: false,
            theme_mode: Default::default(),
            overlay_selections: HashMap::new(),
            last_regions: HashMap::new(),
            last_region_display_id: None,
            pending_intent: None,
            combine_capture: None,
            pending_display_id: None,
            pending_display_scale: None,
            pending_display_bounds: None,
            pending_hide_cursor: true,
            pending_url_after_permission: None,
            cli_interactive_output: None,
            pins: HashMap::new(),
            hovered_pin: None,
            overlay_shift_held: false,
            editor_shift_held: false,
            editor_command_held: false,
            last_save_dir: None,
            history_root: None,
            history_records: Vec::new(),
            history_window_id: None,
            history_status: None,
            pending_history_open: None,
            awaiting_reopen: None,
            history_open_note: None,
            cancelled_history_open: None,
            history_search: String::new(),
            history_selected_id: None,
            history_delete_pending: None,
            history_page_limit: HISTORY_PAGE_SIZE,
            preferences_path: None,
            settings_window_id: None,
            settings_recording_hotkey: false,
            settings_hotkey_error: None,
            settings_hotkey_status: None,
            settings_status: None,
            settings_reset_all_pending: false,
            settings_uninstall_pending: false,
            settings_uninstall_purge: false,
            settings_pane: SettingsPane::default(),
            settings_focus: None,
            settings_key_feedback: None,
            history_clear_all_pending: false,
            cli_tools_window_id: None,
            cli_tools_status: None,
            scroll_session: None,
            reduce_motion: false,
        }
    }

    /// Persist the current `preferences` snapshot to
    /// [`Self::preferences_path`] if one is configured. Called after
    /// every settings mutation so a relaunch sees the user's choices.
    /// IO failures are logged-and-swallowed: a full disk shouldn't
    /// kill the running app, and the in-memory state remains correct.
    fn persist_preferences_if_configured(&self) {
        let Some(path) = self.preferences_path.as_ref() else {
            return;
        };
        if let Err(e) = self.preferences.save(path) {
            tracing::warn!(
                target: readshot_core::log::cat::PREFERENCES,
                "save failed: {e}",
            );
        }
    }

    /// Remember that editor tool `id` was used (clears its "new" dot).
    /// Persists only on the first use.
    pub(crate) fn mark_editor_tool_seen(&mut self, id: &str) -> bool {
        if !self.preferences.mark_tool_seen(id) {
            return false;
        }
        self.persist_preferences_if_configured();
        true
    }

    /// Show or hide the editor side panel and remember it.
    pub(crate) fn set_editor_panel_open(&mut self, open: bool) {
        if self.preferences.editor_panel_open != open {
            self.preferences.editor_panel_open = open;
            self.persist_preferences_if_configured();
        }
    }

    /// Remember "Remove line breaks when copying".
    pub(crate) fn set_copy_remove_line_breaks(&mut self, on: bool) {
        if self.preferences.editor_copy_remove_line_breaks != on {
            self.preferences.editor_copy_remove_line_breaks = on;
            self.persist_preferences_if_configured();
        }
    }

    /// Remember the editor side panel's tab.
    pub(crate) fn set_editor_panel_tab(&mut self, tab: crate::editor::PanelTab) {
        if self.preferences.editor_panel_tab != tab.id() {
            self.preferences.editor_panel_tab = tab.id().to_string();
            self.persist_preferences_if_configured();
        }
    }

    /// Save `settings` as "Preset N" (smallest unused N). `None` when the
    /// list is full.
    pub(crate) fn save_beautify_preset(
        &mut self,
        settings: readshot_core::beautify::BeautifySettings,
    ) -> Option<String> {
        use readshot_core::beautify::{SavedBeautifyPreset, MAX_SAVED_PRESETS};
        let list = &mut self.preferences.editor_beautify_presets;
        if list.len() >= MAX_SAVED_PRESETS {
            return None;
        }
        let name = (1..)
            .map(|n| format!("Preset {n}"))
            .find(|n| !list.iter().any(|p| &p.name == n))
            .expect("unbounded");
        list.push(SavedBeautifyPreset {
            name: name.clone(),
            settings,
        });
        self.persist_preferences_if_configured();
        Some(name)
    }

    /// Remove the saved preset at `index`, returning it so the toast
    /// can offer Undo.
    pub(crate) fn delete_beautify_preset(
        &mut self,
        index: usize,
    ) -> Option<readshot_core::beautify::SavedBeautifyPreset> {
        if index < self.preferences.editor_beautify_presets.len() {
            let removed = self.preferences.editor_beautify_presets.remove(index);
            self.persist_preferences_if_configured();
            Some(removed)
        } else {
            None
        }
    }

    /// Put a removed preset back at `index` (clamped). Refused when the
    /// list is full again.
    pub(crate) fn restore_beautify_preset(
        &mut self,
        index: usize,
        preset: readshot_core::beautify::SavedBeautifyPreset,
    ) -> bool {
        use readshot_core::beautify::MAX_SAVED_PRESETS;
        let list = &mut self.preferences.editor_beautify_presets;
        if list.len() >= MAX_SAVED_PRESETS {
            return false;
        }
        list.insert(index.min(list.len()), preset);
        self.persist_preferences_if_configured();
        true
    }

    /// Add a non-preset color to the editor's Recent row and persist.
    pub(crate) fn remember_custom_color(&mut self, color: readshot_core::Rgba) {
        if readshot_ui::editor::toolbar::is_preset(color) {
            return;
        }
        if self.preferences.push_recent_color(&color.to_hex()) {
            self.persist_preferences_if_configured();
        }
    }

    pub fn mark_onboarding_completed(&mut self) -> bool {
        if self.preferences.onboarding_completed {
            return false;
        }
        self.preferences.onboarding_completed = true;
        self.persist_preferences_if_configured();
        true
    }

    /// Handle the synchronous part of a top-level message.
    ///
    /// Returns `true` if the change is user-visible (caller may
    /// re-render). Messages that need async work (capture, save, OCR,
    /// windows) are handled by `crate::runtime::update`, which calls
    /// this for the shared synchronous transitions.
    pub fn update_sync(&mut self, message: Message) -> bool {
        match message {
            Message::App(AppMsg::PermissionPoll(status)) => {
                self.permission_status = status;
                self.welcome.observe(status)
            }
            Message::App(AppMsg::GrantPermissionRequested) => {
                // Clear any decision macOS still holds for an earlier,
                // differently signed build first. Otherwise its stale row
                // ("Failed to match existing code requirement") means no
                // prompt appears and switching Readshot on never applies to
                // this build. With no existing row this is a no-op.
                self.permissions.reset();
                self.permissions.request();
                self.welcome = WelcomeState::AwaitingGrant;
                true
            }
            Message::App(AppMsg::OpenPermissionSettingsRequested) => {
                self.permissions.open_settings();
                false
            }
            Message::App(AppMsg::ResetPermissionRequested) => {
                self.permissions.reset();
                self.permissions.request();
                self.welcome = WelcomeState::AwaitingGrant;
                true
            }
            Message::Settings(SettingsMsg::Edit(msg)) => {
                let changed = readshot_ui::settings::apply(&mut self.preferences, msg);
                if changed {
                    self.persist_preferences_if_configured();
                }
                changed
            }
            Message::Settings(SettingsMsg::UninstallRequested) => {
                // Only one destructive prompt armed at a time.
                self.settings_reset_all_pending = false;
                self.settings_uninstall_pending = true;
                true
            }
            Message::Settings(SettingsMsg::UninstallCancelled) => {
                let was_armed = self.settings_uninstall_pending;
                self.settings_uninstall_pending = false;
                was_armed
            }
            Message::Settings(SettingsMsg::UninstallPurgeToggled(purge)) => {
                let changed = self.settings_uninstall_purge != purge;
                self.settings_uninstall_purge = purge;
                changed
            }
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
        let changed = app.update_sync(Message::App(AppMsg::GrantPermissionRequested));
        assert!(changed);
        assert_eq!(perms.request_calls.load(Ordering::SeqCst), 1);
        assert_eq!(app.welcome, WelcomeState::AwaitingGrant);
    }

    #[test]
    fn permission_poll_dismisses_welcome_on_grant() {
        let perms = Arc::new(FakePermissions::denied());
        let (mut app, _) = build_app(perms.clone());
        // User clicks "Grant", OS prompt fires, then poll sees the new state.
        app.update_sync(Message::App(AppMsg::GrantPermissionRequested));
        perms.flip_to_granted();
        let changed = app.update_sync(Message::App(AppMsg::PermissionPoll(
            PermissionStatus::Granted,
        )));
        assert!(changed);
        assert!(!app.welcome.should_show());
    }

    #[test]
    fn permission_poll_leaves_onboarding_completion_until_ready_window_opens() {
        let perms = Arc::new(FakePermissions::denied());
        let (mut app, _) = build_app(perms.clone());
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("preferences.toml");
        app.preferences_path = Some(path.clone());

        app.update_sync(Message::App(AppMsg::GrantPermissionRequested));
        perms.flip_to_granted();
        let changed = app.update_sync(Message::App(AppMsg::PermissionPoll(
            PermissionStatus::Granted,
        )));

        assert!(changed);
        assert!(!app.preferences.onboarding_completed);
        assert!(!path.exists());
    }

    #[test]
    fn uninstall_prompt_arms_and_cancels_without_executing() {
        let (mut app, _) = build_app(Arc::new(FakePermissions::granted()));
        assert!(!app.settings_uninstall_pending);

        // Arming an uninstall clears any reset prompt and sets pending.
        app.settings_reset_all_pending = true;
        let changed = app.update_sync(Message::Settings(SettingsMsg::UninstallRequested));
        assert!(changed);
        assert!(app.settings_uninstall_pending);
        assert!(!app.settings_reset_all_pending);

        // The purge checkbox toggles independently of the armed state.
        let changed = app.update_sync(Message::Settings(SettingsMsg::UninstallPurgeToggled(true)));
        assert!(changed);
        assert!(app.settings_uninstall_purge);
        // Toggling to the same value reports no change.
        assert!(!app.update_sync(Message::Settings(SettingsMsg::UninstallPurgeToggled(true))));

        // Cancelling disarms without touching anything else.
        let changed = app.update_sync(Message::Settings(SettingsMsg::UninstallCancelled));
        assert!(changed);
        assert!(!app.settings_uninstall_pending);
        // A second cancel is a no-op.
        assert!(!app.update_sync(Message::Settings(SettingsMsg::UninstallCancelled)));
    }

    #[test]
    fn settings_message_applies_to_preferences() {
        let (mut app, _) = build_app(Arc::new(FakePermissions::granted()));
        assert!(!app.preferences.debug_logging);
        let changed = app.update_sync(Message::Settings(SettingsMsg::Edit(
            SettingsMessage::SetDebugLogging(true),
        )));
        assert!(changed);
        assert!(app.preferences.debug_logging);
    }

    #[test]
    fn settings_mutation_persists_to_disk_when_path_set() {
        let (mut app, _) = build_app(Arc::new(FakePermissions::granted()));
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("preferences.toml");
        app.preferences_path = Some(path.clone());

        // Sanity: nothing on disk yet.
        assert!(!path.exists());

        let changed = app.update_sync(Message::Settings(SettingsMsg::Edit(
            SettingsMessage::SetDebugLogging(true),
        )));
        assert!(changed);

        // The mutation should have been written to disk; reading it
        // back yields the new value.
        let from_disk = Preferences::load(&path).unwrap();
        assert!(from_disk.debug_logging);
    }

    #[test]
    fn settings_mutation_without_path_stays_in_memory() {
        let (mut app, _) = build_app(Arc::new(FakePermissions::granted()));
        // No preferences_path configured (matches CLI / test invocations).
        assert!(app.preferences_path.is_none());
        let changed = app.update_sync(Message::Settings(SettingsMsg::Edit(
            SettingsMessage::SetDebugLogging(true),
        )));
        // In-memory mutation still works; we just don't write to disk.
        assert!(changed);
        assert!(app.preferences.debug_logging);
    }

    #[test]
    fn allow_screen_recording_clears_stale_grant_before_asking() {
        let (mut app, perms) = build_app(Arc::new(FakePermissions::denied()));
        app.update_sync(Message::App(AppMsg::GrantPermissionRequested));
        assert_eq!(perms.reset_calls.load(Ordering::SeqCst), 1);
        assert_eq!(perms.request_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reset_permission_clears_stale_grant_and_asks_again() {
        let (mut app, perms) = build_app(Arc::new(FakePermissions::denied()));
        let changed = app.update_sync(Message::App(AppMsg::ResetPermissionRequested));
        assert!(changed);
        assert_eq!(perms.reset_calls.load(Ordering::SeqCst), 1);
        assert_eq!(perms.request_calls.load(Ordering::SeqCst), 1);
        assert_eq!(app.welcome, WelcomeState::AwaitingGrant);
    }

    #[test]
    fn open_settings_request_invokes_permissions_no_state_change() {
        let (mut app, perms) = build_app(Arc::new(FakePermissions::denied()));
        let changed = app.update_sync(Message::App(AppMsg::OpenPermissionSettingsRequested));
        assert!(!changed);
        assert_eq!(perms.open_settings_calls.load(Ordering::SeqCst), 1);
    }
}
