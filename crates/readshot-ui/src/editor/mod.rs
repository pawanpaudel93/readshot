//! Annotation editor — the second-stage UI after a capture confirms.
//!
//! Public surface (used by the composition root (`readshot-app`)):
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
//! What this module does **not** own: the iced window
//! itself, the secondary-toolbar window, the keyboard-shortcut event
//! filter that maps ⌘/Ctrl+S to `ActionMessage::Save`, and any
//! permission / IO side-effects.

use image::RgbaImage;
use std::path::PathBuf;

use readshot_core::ExportFormat;

pub mod action_bar;
pub mod canvas;
pub mod state;
pub mod style;
pub mod tool_state;
pub mod toolbar;
pub mod undo;

pub use action_bar::ActionMessage;
pub use canvas::{CanvasMessage, EditorCanvas, Loupe, LOUPE_RADIUS};
pub use state::{DocOp, EditorState, Flattened, ResizeHandle, TextEdit};
pub use style::{
    apply_style_to_annotation, options_for, tool_for_annotation, LinePathKind, OptionControl,
    StyleMessage, TextSize, ToolStyle,
};
pub use tool_state::ToolState;
pub use toolbar::ToolbarMessage;
pub use undo::History;
pub use undo::{DocState, Snapshot};

/// What the editor returns to the capture coordinator when the user
/// commits an action.
#[derive(Debug)]
pub enum EditorOutcome {
    CopyImage(RgbaImage),
    CopyText(String),
    Save { path: PathBuf, format: ExportFormat },
    Pin(RgbaImage),
    Discard,
}
