//! Composition root for Readshot.
//!
//! Layer responsibilities:
//!
//! * [`coordinator::CaptureCoordinator`] — service bag the iced App
//!   calls into. Pure async methods, fully tested via fakes.
//! * [`permissions`] — typed permission status + per-OS providers.
//! * [`url_scheme`] — `readshot://...` URL parser.
//! * [`welcome`] — first-run welcome-window state machine.
//!
//! The iced application skeleton lives in `app.rs` and binds these
//! pieces to the GUI runtime. `main.rs` is the binary entry that
//! constructs the container and starts iced.

pub mod app;
pub mod cli;
pub mod cli_tools;
pub mod coordinator;
pub mod editor;
mod editor_icons;
pub mod overlay;
pub mod permissions;
pub mod runtime;
pub mod startup;
mod system_cursor;
pub mod tray;
pub mod updater;
pub mod url_events;
pub mod url_scheme;
pub mod welcome;
