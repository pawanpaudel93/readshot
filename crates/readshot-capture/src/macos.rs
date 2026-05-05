//! macOS Capturer using Apple's ScreenCaptureKit framework.
//!
//! Wraps [`screencapturekit`](https://crates.io/crates/screencapturekit)
//! ≥ 1.5 and exposes display enumeration + region capture through the
//! crate-shared [`Capturer`] trait. Targets macOS 14 (Sonoma) and newer:
//! the `SCScreenshotManager` API used here landed in macOS 14.
//!
//! ## Permissions
//!
//! `SCShareableContent::get()` triggers the system Screen Recording
//! prompt the first time it is called from an app. If the user denies
//! consent the call returns an error which we map to
//! [`CaptureError::PermissionDenied`]; the higher layers (CLI,
//! welcome window, MCP) handle the user-facing recovery.
//!
//! ## Pixel format
//!
//! `SCScreenshotManager::capture_image` returns the crate's `CGImage`
//! wrapper, which exposes `rgba_data()` as a ready-to-use straight
//! RGBA byte buffer. We construct an `image::RgbaImage` directly from
//! it — no manual BGRA→RGBA conversion needed.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;
use screencapturekit::error::SCError;
use screencapturekit::screenshot_manager::SCScreenshotManager;
use screencapturekit::shareable_content::{SCDisplay, SCShareableContent};
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;

use crate::{CaptureRequest, Capturer, DisplayInfo};

/// Production macOS Capturer.
pub struct ScreenCaptureKitCapturer;

pub fn new() -> ScreenCaptureKitCapturer {
    ScreenCaptureKitCapturer
}

#[async_trait]
impl Capturer for ScreenCaptureKitCapturer {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        let content = SCShareableContent::get().map_err(map_err)?;
        let displays = content.displays();
        let primary = primary_display_id();
        Ok(displays
            .into_iter()
            .map(|d| display_info_from_sc(d, primary))
            .collect())
    }

    async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
        let content = SCShareableContent::get().map_err(map_err)?;
        let displays = content.displays();
        let display = find_display(&displays, &req.display_id)
            .ok_or_else(|| CaptureError::DisplayNotFound(req.display_id.clone()))?;

        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .build();

        let config = SCStreamConfiguration::new()
            .with_width(display.width())
            .with_height(display.height())
            .with_shows_cursor(!req.hide_cursor);

        let cg_image = SCScreenshotManager::capture_image(&filter, &config).map_err(map_err)?;

        let width = cg_image.width() as u32;
        let height = cg_image.height() as u32;
        let rgba = cg_image.rgba_data().map_err(map_err)?;
        let full = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            CaptureError::Backend("rgba_data length does not match width × height × 4".to_string())
        })?;
        Ok(crop_rgba(full, req.rect, req.scale))
    }
}

fn crop_rgba(full: RgbaImage, rect_logical: Rect, scale: f32) -> RgbaImage {
    let x0 = ((rect_logical.x() * scale).round().max(0.0) as u32).min(full.width());
    let y0 = ((rect_logical.y() * scale).round().max(0.0) as u32).min(full.height());
    let w_target = (rect_logical.width() * scale).round().max(1.0) as u32;
    let h_target = (rect_logical.height() * scale).round().max(1.0) as u32;
    let w = w_target.min(full.width().saturating_sub(x0));
    let h = h_target.min(full.height().saturating_sub(y0));
    image::imageops::crop_imm(&full, x0, y0, w, h).to_image()
}

fn display_info_from_sc(display: SCDisplay, primary_id: u32) -> DisplayInfo {
    let id = display.display_id();
    // Use CoreGraphics for global bounds + scale because
    // ScreenCaptureKit's `SCDisplay` only exposes physical width/height
    // and no origin. The overlay window needs the *global* logical
    // origin so it can position itself across a multi-monitor setup,
    // and the capture needs a real scale so HiDPI displays render at
    // sharp native resolution.
    use core_graphics::display::CGDisplay;
    let cg = CGDisplay::new(id);
    let cg_bounds = cg.bounds();
    let logical_w = (cg_bounds.size.width as f32).max(1.0);
    let logical_h = (cg_bounds.size.height as f32).max(1.0);
    let origin_x = cg_bounds.origin.x as f32;
    let origin_y = cg_bounds.origin.y as f32;
    let pixels_w = cg.pixels_wide() as f32;
    let scale = if logical_w > 0.5 {
        (pixels_w / logical_w).max(1.0)
    } else {
        1.0
    };
    let bounds = Rect::from_xywh(origin_x, origin_y, logical_w, logical_h)
        .unwrap_or_else(|| Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap());
    DisplayInfo {
        id: id.to_string(),
        bounds,
        scale,
        name: format!("Display {id}"),
        is_primary: id == primary_id,
    }
}

fn find_display<'a>(displays: &'a [SCDisplay], requested_id: &str) -> Option<&'a SCDisplay> {
    let id_num: u32 = requested_id.parse().ok()?;
    displays.iter().find(|d| d.display_id() == id_num)
}

/// `CGMainDisplayID` returns the system's primary display id. If the
/// FFI call fails (e.g. running headless), 0 is returned — every real
/// display id is non-zero, so `is_primary` becomes false everywhere
/// rather than wrong.
fn primary_display_id() -> u32 {
    use core_graphics::display::CGMainDisplayID;
    unsafe { CGMainDisplayID() }
}

fn map_err(e: SCError) -> CaptureError {
    let msg = format!("{e:?}");
    let lc = msg.to_lowercase();
    if lc.contains("permission") || lc.contains("denied") || lc.contains("not authorized") {
        CaptureError::PermissionDenied
    } else {
        CaptureError::Backend(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 0, 255]))
    }

    #[test]
    fn crop_rgba_scales_logical_rect_to_physical_pixels() {
        let cropped = crop_rgba(
            solid(400, 300),
            Rect::from_xywh(10.0, 20.0, 30.0, 40.0).unwrap(),
            2.0,
        );

        assert_eq!(cropped.width(), 60);
        assert_eq!(cropped.height(), 80);
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([20, 40, 0, 255]));
    }

    #[test]
    fn crop_rgba_clamps_to_full_image_bounds() {
        let cropped = crop_rgba(
            solid(100, 100),
            Rect::from_xywh(40.0, 40.0, 20.0, 20.0).unwrap(),
            2.0,
        );

        assert_eq!(cropped.width(), 20);
        assert_eq!(cropped.height(), 20);
    }

    #[test]
    fn crop_rgba_handles_edge_rounding_outside_bounds() {
        let cropped = crop_rgba(
            solid(100, 100),
            Rect::from_xywh(51.0, 51.0, 10.0, 10.0).unwrap(),
            2.0,
        );

        assert_eq!(cropped.width(), 0);
        assert_eq!(cropped.height(), 0);
    }
}
