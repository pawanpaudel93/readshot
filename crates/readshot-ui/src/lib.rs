//! Iced-based UI for Readshot.
//!
//! The annotation editor, settings window, and global hotkey
//! widgets. The production region-selection overlay lives in the
//! `readshot-app` composition root (`crate::overlay::OverlayProgram`),
//! which owns the iced runtime — the earlier scaffolding overlay that
//! used to live here was superseded and has been removed. The tray
//! menu likewise lives in `readshot-app` (`crate::tray`); this crate's
//! old `tray` module was a superseded duplicate and has been deleted.

pub mod editor;
pub mod hotkey;
pub mod settings;

pub use editor::{
    ActionMessage, CanvasMessage, EditorCanvas, EditorOutcome, EditorState, History, ToolState,
    ToolbarMessage,
};
pub use hotkey::{HotkeyModifiers, HotkeyParseError, HotkeySpec};
pub use settings::{SettingsMessage, SettingsTab};
