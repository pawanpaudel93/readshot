//! Iced-based UI for Readshot.
//!
//! The annotation editor, settings window, and tray + global hotkey
//! widgets. The production region-selection overlay lives in the
//! `readshot-app` composition root (`crate::overlay::OverlayProgram`),
//! which owns the iced runtime — the earlier scaffolding overlay that
//! used to live here was superseded and has been removed.

pub mod editor;
pub mod hotkey;
pub mod settings;
pub mod tray;

pub use editor::{
    ActionMessage, CanvasMessage, EditorCanvas, EditorOutcome, EditorState, History, ToolState,
    ToolbarMessage,
};
pub use hotkey::{HotkeyMessage, HotkeyModifiers, HotkeyParseError, HotkeySpec};
pub use settings::{SettingsMessage, SettingsTab};
pub use tray::{TrayMenuRow, TrayMessage};
