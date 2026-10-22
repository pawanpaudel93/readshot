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

// Unsafe is only for platform FFI, and each block says why it is
// sound (AGENTS.md, "Things to avoid").
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod app;
pub mod app_events;
pub mod cli;
pub mod coordinator;
pub mod editor;
#[cfg(target_os = "macos")]
pub mod emoji_macos;
mod icons;
pub mod overlay;
pub mod permissions;
pub mod runtime;
pub mod single_instance;
pub mod startup;
#[cfg(test)]
mod test_probe;
pub mod tray;
pub mod uninstall;
pub mod updater;
pub mod url_events;
pub mod url_scheme;
pub mod welcome;
