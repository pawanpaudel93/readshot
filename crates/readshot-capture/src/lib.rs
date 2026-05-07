//! Per-OS screen capture for Readshot.
//!
//! The public API is the [`Capturer`] trait plus three platform-specific
//! implementations selected at compile time:
//!
//! * [`macos::ScreenCaptureKitCapturer`] — `screencapturekit` crate
//!   (Task 7 fills in the body).
//! * [`windows::WindowsGraphicsCapturer`] — `windows` crate's
//!   `Graphics::Capture` (Task 8).
//! * [`linux::LinuxCapturer`] — `ashpd` portal on Wayland with `x11rb`
//!   fallback (Task 9).
//!
//! [`fake::FakeCapturer`] returns a deterministic fixture and is
//! re-exported so every higher crate's tests can substitute capture
//! without an OS-level grant.
//!
//! Errors are typed as [`readshot_core::error::CaptureError`]; the CLI
//! exit-code mapper (spec §3.15) and MCP error mapper (spec §3.16) match
//! on its variants directly.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;

pub mod fake;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

/// Opaque OS handle for a display. macOS uses `CGDirectDisplayID` as a
/// decimal string; Windows uses the monitor's interface id; Linux uses
/// the portal's stream node id. Callers treat these as opaque tokens —
/// only the OS Capturer interprets them.
pub type DisplayId = String;

/// One capture request from coordinator to capturer.
///
/// `rect` is in **logical pixels** (a.k.a. points) relative to the target
/// display's bounds. `scale` is the display's HiDPI scale factor; the
/// capturer is responsible for producing an image at `rect.width * scale`
/// by `rect.height * scale` physical pixels so the result is sharp on
/// Retina / 4K displays.
#[derive(Clone, Debug)]
pub struct CaptureRequest {
    pub display_id: DisplayId,
    pub rect: Rect,
    pub scale: f32,
    pub hide_cursor: bool,
}

/// Opaque OS handle for a capturable window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowId(pub String);

/// Information about a capturable window.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub id: WindowId,
    pub title: String,
    pub app_name: String,
    pub display_id: DisplayId,
    /// Window bounds in logical pixels.
    pub bounds: Rect,
}

/// One window capture request from coordinator to capturer.
#[derive(Clone, Debug)]
pub struct WindowCaptureRequest {
    pub window_id: WindowId,
}

/// Information about an attached display, returned by
/// [`Capturer::list_displays`].
#[derive(Clone, Debug)]
pub struct DisplayInfo {
    pub id: DisplayId,
    /// Display bounds in logical pixels.
    pub bounds: Rect,
    pub scale: f32,
    pub name: String,
    pub is_primary: bool,
}

/// The single trait every Readshot capture backend implements.
///
/// `Send + Sync` because the capture coordinator (Task 16) holds a
/// `Box<dyn Capturer>` inside a `tokio::task` that may move between
/// runtime threads.
#[async_trait]
pub trait Capturer: Send + Sync {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError>;
    async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError>;

    async fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        Err(CaptureError::Unsupported(
            "window capture is not supported on this platform".into(),
        ))
    }

    async fn capture_window(
        &self,
        _request: WindowCaptureRequest,
    ) -> Result<RgbaImage, CaptureError> {
        Err(CaptureError::Unsupported(
            "window capture is not supported on this platform".into(),
        ))
    }
}

/// Construct the platform-default Capturer for the current target. The
/// per-OS implementations land in Tasks 7–9; this function is here so
/// the composition root (Task 16) can ask for a Capturer without
/// branching on `cfg(target_os)` at the call site.
///
/// `#[allow(clippy::needless_return)]` is required because the cfg
/// attributes only attach to statement-position expressions, not to
/// tail expressions; an explicit `return` per arm is the canonical
/// way to express this.
#[allow(clippy::needless_return)]
pub fn default_capturer() -> Box<dyn Capturer> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(macos::new());
    }
    #[cfg(target_os = "windows")]
    {
        return Box::new(windows::new());
    }
    #[cfg(target_os = "linux")]
    {
        return Box::new(linux::new());
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        compile_error!(
            "readshot-capture: unsupported target_os; only macOS, Windows, and Linux are supported"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeCapturer;

    #[tokio::test]
    async fn default_window_capture_is_unsupported() {
        let capturer = FakeCapturer::new();
        let result = capturer.list_windows().await;

        assert!(matches!(result, Err(CaptureError::Unsupported(_))));
    }
}
