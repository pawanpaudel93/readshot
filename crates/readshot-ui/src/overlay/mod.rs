//! Capture overlay — transparent, always-on-top window per display
//! that lets the user drag a selection rectangle.
//!
//! ## What this module owns in Task 13
//!
//! 1. The public types ([`OverlayResult`], [`SelectionMessage`]) that
//!    Task 16's composition root sends and receives.
//! 2. [`SelectionCanvas`] — an `iced::widget::canvas::Program`
//!    implementation that tracks drag state and renders the
//!    marching-ants selection rectangle. This compiles and unit-tests
//!    cleanly today.
//! 3. [`per_os`] — pure helpers that produce the window-level and
//!    transparency attributes each operating system needs for an
//!    overlay window.
//!
//! ## What this module does **not** own yet
//!
//! The runtime that opens one iced window per [`DisplayInfo`] and
//! wires the canvas into the iced `daemon` event loop. That lives
//! inside Task 16's `readshot-app` composition root because it is the
//! application binary that owns the iced runtime — having the library
//! crate spawn an iced app from inside `async fn show_overlay` would
//! conflict with the runtime model. Task 16 calls into the surfaces
//! defined here.

pub mod per_os;
pub mod selection_canvas;

use readshot_core::geom::Rect;

pub use selection_canvas::{SelectionCanvas, SelectionMessage};

/// Outcome the overlay returns to the capture coordinator: which
/// display the selection landed on, the rectangle in logical pixels,
/// and the display's HiDPI scale factor.
///
/// Logical (point) coordinates rather than physical pixels because
/// `Capturer::capture_region` (spec §3.6) takes points and applies the
/// scale internally. Carrying the scale through the result lets the
/// editor (Task 14) display dimensions in user-friendly numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlayResult {
    pub display_id: String,
    pub rect_logical: Rect,
    pub scale_factor: f32,
}
