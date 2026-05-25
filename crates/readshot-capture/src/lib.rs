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
//! The optional `test-fixtures` feature exposes `fake::FakeCapturer`,
//! a deterministic fixture so higher crate tests can substitute
//! capture without an OS-level grant.
//!
//! Errors are typed as [`readshot_core::error::CaptureError`]; the CLI
//! exit-code mapper (spec §3.15) and MCP error mapper (spec §3.16) match
//! on its variants directly.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;

#[cfg(any(test, feature = "test-fixtures"))]
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
    /// Omit native decorative shadows when the backend can control them.
    pub ignore_shadows: bool,
}

/// Crop a captured window image using a rectangle relative to the
/// window's top-left logical coordinate space.
///
/// Window capture backends return physical pixels, while window metadata is
/// reported in logical pixels. This helper derives the x/y scale from the
/// captured image size and the reported window bounds, then crops and clamps
/// the requested rect to the captured image. A rect fully outside the image is
/// rejected as a typed invalid-region error.
pub fn crop_window_relative_rect(
    full: RgbaImage,
    window_bounds: Rect,
    rect_logical: Rect,
) -> Result<RgbaImage, CaptureError> {
    let scale_x = capture_axis_scale(full.width(), window_bounds.width());
    let scale_y = capture_axis_scale(full.height(), window_bounds.height());
    let x0 = ((rect_logical.x() * scale_x).round().max(0.0) as u32).min(full.width());
    let y0 = ((rect_logical.y() * scale_y).round().max(0.0) as u32).min(full.height());
    let w_target = (rect_logical.width() * scale_x).round().max(1.0) as u32;
    let h_target = (rect_logical.height() * scale_y).round().max(1.0) as u32;
    let w = w_target.min(full.width().saturating_sub(x0));
    let h = h_target.min(full.height().saturating_sub(y0));
    if w == 0 || h == 0 {
        return Err(CaptureError::InvalidRegion(
            "window-relative rect is outside the captured window bounds".to_string(),
        ));
    }
    Ok(image::imageops::crop_imm(&full, x0, y0, w, h).to_image())
}

fn capture_axis_scale(image_pixels: u32, logical_extent: f32) -> f32 {
    if logical_extent.is_finite() && logical_extent > 0.0 {
        image_pixels as f32 / logical_extent
    } else {
        1.0
    }
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

    struct RegionOnlyCapturer;

    #[async_trait]
    impl Capturer for RegionOnlyCapturer {
        async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
            Ok(Vec::new())
        }

        async fn capture_region(&self, _req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
            Ok(RgbaImage::new(1, 1))
        }
    }

    #[tokio::test]
    async fn default_window_capture_is_unsupported() {
        let capturer = RegionOnlyCapturer;
        let result = capturer.list_windows().await;

        assert!(matches!(result, Err(CaptureError::Unsupported(_))));
    }

    #[tokio::test]
    async fn default_window_image_capture_is_unsupported() {
        let capturer = RegionOnlyCapturer;
        let result = capturer
            .capture_window(WindowCaptureRequest {
                window_id: WindowId("missing".to_string()),
                ignore_shadows: false,
            })
            .await;

        assert!(matches!(result, Err(CaptureError::Unsupported(_))));
    }

    fn solid(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 0, 255]))
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_xywh(x, y, w, h).expect("test rect must be valid")
    }

    #[test]
    fn crop_window_relative_rect_crops_inside_region() {
        let cropped = crop_window_relative_rect(
            solid(100, 80),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(10.0, 20.0, 30.0, 25.0),
        )
        .unwrap();

        assert_eq!(cropped.width(), 30);
        assert_eq!(cropped.height(), 25);
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([10, 20, 0, 255]));
    }

    #[test]
    fn crop_window_relative_rect_scales_logical_rect_to_physical_pixels() {
        let cropped = crop_window_relative_rect(
            solid(200, 160),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(10.0, 20.0, 30.0, 25.0),
        )
        .unwrap();

        assert_eq!(cropped.width(), 60);
        assert_eq!(cropped.height(), 50);
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([20, 40, 0, 255]));
    }

    #[test]
    fn crop_window_relative_rect_clamps_partial_overlap() {
        let cropped = crop_window_relative_rect(
            solid(100, 80),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(90.0, 70.0, 30.0, 25.0),
        )
        .unwrap();

        assert_eq!(cropped.width(), 10);
        assert_eq!(cropped.height(), 10);
    }

    #[test]
    fn crop_window_relative_rect_rejects_fully_outside_rect() {
        let result = crop_window_relative_rect(
            solid(100, 80),
            rect(0.0, 0.0, 100.0, 80.0),
            rect(120.0, 90.0, 30.0, 25.0),
        );

        assert!(
            matches!(result, Err(CaptureError::InvalidRegion(message)) if message.contains("outside"))
        );
    }
}
