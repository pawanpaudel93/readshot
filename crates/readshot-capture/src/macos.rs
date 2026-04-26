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
use screencapturekit::cg::CGRect as SkCgRect;
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

        let physical_w = ((req.rect.width() * req.scale).round() as u32).max(1);
        let physical_h = ((req.rect.height() * req.scale).round() as u32).max(1);

        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&[])
            .build();

        let source_rect = SkCgRect::new(
            req.rect.x() as f64,
            req.rect.y() as f64,
            req.rect.width() as f64,
            req.rect.height() as f64,
        );

        let config = SCStreamConfiguration::new()
            .with_width(physical_w)
            .with_height(physical_h)
            .with_source_rect(source_rect)
            .with_shows_cursor(!req.hide_cursor);

        let cg_image = SCScreenshotManager::capture_image(&filter, &config).map_err(map_err)?;

        let width = cg_image.width() as u32;
        let height = cg_image.height() as u32;
        let rgba = cg_image.rgba_data().map_err(map_err)?;
        RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            CaptureError::Backend("rgba_data length does not match width × height × 4".to_string())
        })
    }
}

fn display_info_from_sc(display: SCDisplay, primary_id: u32) -> DisplayInfo {
    let id = display.display_id();
    let w = display.width() as f32;
    let h = display.height() as f32;
    let bounds = Rect::from_xywh(0.0, 0.0, w.max(1.0), h.max(1.0))
        .unwrap_or_else(|| Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap());
    DisplayInfo {
        id: id.to_string(),
        bounds,
        scale: 1.0,
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
