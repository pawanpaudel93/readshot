//! Iced-based UI for Readshot.
//!
//! v0.1 ships the capture-overlay scaffolding (Task 13). The annotation
//! editor (Task 14), settings window (Task 15), and tray + global
//! hotkey wiring (Task 15) follow.
//!
//! The `overlay` module owns the public surface that Task 16's
//! composition root binds to the iced daemon at runtime.

pub mod editor;
pub mod hotkey;
pub mod overlay;
pub mod settings;
pub mod tray;

pub use editor::{
    ActionMessage, CanvasMessage, EditorCanvas, EditorOutcome, EditorState, History, ToolState,
    ToolbarMessage,
};
pub use hotkey::{HotkeyMessage, HotkeyModifiers, HotkeyParseError, HotkeySpec};
pub use overlay::{OverlayResult, SelectionCanvas, SelectionMessage};
pub use settings::{SettingsMessage, SettingsTab};
pub use tray::{TrayMenuRow, TrayMessage};
