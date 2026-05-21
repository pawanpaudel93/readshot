//! Annotation editor — the second-stage UI after a capture confirms.
//!
//! Public surface (used by Task 16's composition root):
//!
//! * [`EditorState`] — the model. Pure logic, fully unit-testable.
//! * [`EditorOutcome`] — what the editor returns to the coordinator.
//! * [`tool_state::ToolState`] / [`undo::History`] — the model's
//!   building blocks; re-exported for callers that drive the model
//!   via direct method calls.
//! * [`canvas::EditorCanvas`] / [`canvas::CanvasMessage`] — the iced
//!   widget that translates mouse events into "commit this annotation"
//!   messages.
//! * [`toolbar::ToolbarMessage`] / [`action_bar::ActionMessage`] —
//!   typed events the secondary toolbar window and the bottom bar
//!   publish, plus their layout constants ([`toolbar::PALETTE`],
//!   [`toolbar::TOOL_ORDER`], [`action_bar::BUTTON_ORDER`]).
//!
//! What this module does **not** own (Task 16): the iced window
//! itself, the secondary-toolbar window, the keyboard-shortcut event
//! filter that maps ⌘/Ctrl+S to `ActionMessage::Save`, and any
//! permission / IO side-effects.

use image::RgbaImage;
use std::path::PathBuf;

use readshot_core::ExportFormat;

pub mod action_bar;
pub mod canvas;
pub mod state;
pub mod tool_state;
pub mod toolbar;
pub mod undo;

pub use action_bar::ActionMessage;
pub use canvas::{CanvasMessage, EditorCanvas};
pub use state::{EditorState, ResizeHandle};
pub use tool_state::ToolState;
pub use toolbar::ToolbarMessage;
pub use undo::History;

/// What the editor returns to the capture coordinator when the user
/// commits an action. Mirrors spec §3.3's `EditorOutcome` enum.
#[derive(Debug)]
pub enum EditorOutcome {
    CopyImage(RgbaImage),
    CopyText(String),
    Save { path: PathBuf, format: ExportFormat },
    Pin(RgbaImage),
    Discard,
}
