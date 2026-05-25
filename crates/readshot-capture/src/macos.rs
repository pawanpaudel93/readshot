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
use screencapturekit::shareable_content::{SCDisplay, SCShareableContent, SCWindow};
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;

use crate::{CaptureRequest, Capturer, DisplayInfo, WindowCaptureRequest, WindowId, WindowInfo};

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

        // Exclude every window owned by our own process so the
        // scroll-capture HUD (and any pinned readshot windows that
        // happen to be visible) never bleed into the captured image
        // and break the stitch.
        let own_pid = std::process::id() as i32;
        let all_windows = content.windows();
        let own_windows: Vec<_> = all_windows
            .iter()
            .filter(|w| {
                w.owning_application()
                    .map(|app| app.process_id() == own_pid)
                    .unwrap_or(false)
            })
            .collect();
        let filter = SCContentFilter::create()
            .with_display(display)
            .with_excluding_windows(&own_windows)
            .build();

        let (capture_w, capture_h) =
            native_capture_size(display.display_id(), display.width(), display.height());
        let config = SCStreamConfiguration::new()
            .with_width(capture_w)
            .with_height(capture_h)
            .with_shows_cursor(!req.hide_cursor);

        let cg_image = SCScreenshotManager::capture_image(&filter, &config).map_err(map_err)?;

        let width = cg_image.width() as u32;
        let height = cg_image.height() as u32;
        let rgba = cg_image.rgba_data().map_err(map_err)?;
        let full = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            CaptureError::Backend("rgba_data length does not match width × height × 4".to_string())
        })?;
        let (scale_x, scale_y) =
            capture_scales_for_image(display.display_id(), width, height, req.scale);
        Ok(crop_rgba(full, req.rect, scale_x, scale_y))
    }

    async fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        let content = SCShareableContent::get().map_err(map_err)?;
        let primary = primary_display_id();
        let displays = content
            .displays()
            .into_iter()
            .map(|d| display_info_from_sc(d, primary))
            .collect::<Vec<_>>();
        Ok(content
            .windows()
            .into_iter()
            .filter(|window| window.is_on_screen() && window.window_layer() == 0)
            .filter_map(|window| window_info_from_sc(&window, &displays))
            .collect())
    }

    async fn capture_window(&self, req: WindowCaptureRequest) -> Result<RgbaImage, CaptureError> {
        let content = SCShareableContent::get().map_err(map_err)?;
        let windows = content.windows();
        let window = find_window(&windows, &req.window_id)
            .ok_or_else(|| CaptureError::WindowNotFound(req.window_id.0.clone()))?;

        let primary = primary_display_id();
        let displays = content
            .displays()
            .into_iter()
            .map(|d| display_info_from_sc(d, primary))
            .collect::<Vec<_>>();
        let display = window_info_from_sc(window, &displays)
            .and_then(|info| {
                displays
                    .iter()
                    .find(|display| display.id == info.display_id)
                    .cloned()
            })
            .or_else(|| displays.iter().find(|display| display.is_primary).cloned())
            .or_else(|| displays.first().cloned());
        let scale = display.map(|display| display.scale).unwrap_or(1.0);
        let frame = window.frame();
        let size = frame.size();
        let capture_w = (size.width as f32 * scale).round().max(1.0) as u32;
        let capture_h = (size.height as f32 * scale).round().max(1.0) as u32;

        let filter = SCContentFilter::create().with_window(window).build();
        let config = SCStreamConfiguration::new()
            .with_width(capture_w)
            .with_height(capture_h)
            .with_shows_cursor(false)
            .with_ignores_shadows_single_window(req.ignore_shadows)
            .with_ignore_global_clip_single_window(true);

        let cg_image = SCScreenshotManager::capture_image(&filter, &config).map_err(map_err)?;

        let width = cg_image.width() as u32;
        let height = cg_image.height() as u32;
        let rgba = cg_image.rgba_data().map_err(map_err)?;
        RgbaImage::from_raw(width, height, rgba).ok_or_else(|| {
            CaptureError::Backend("rgba_data length does not match width × height × 4".to_string())
        })
    }
}

fn crop_rgba(full: RgbaImage, rect_logical: Rect, scale_x: f32, scale_y: f32) -> RgbaImage {
    let x0 = ((rect_logical.x() * scale_x).round().max(0.0) as u32).min(full.width());
    let y0 = ((rect_logical.y() * scale_y).round().max(0.0) as u32).min(full.height());
    let w_target = (rect_logical.width() * scale_x).round().max(1.0) as u32;
    let h_target = (rect_logical.height() * scale_y).round().max(1.0) as u32;
    let w = w_target.min(full.width().saturating_sub(x0));
    let h = h_target.min(full.height().saturating_sub(y0));
    image::imageops::crop_imm(&full, x0, y0, w, h).to_image()
}

fn capture_scales_for_image(
    display_id: u32,
    image_width: u32,
    image_height: u32,
    fallback: f32,
) -> (f32, f32) {
    use core_graphics::display::CGDisplay;
    let bounds = CGDisplay::new(display_id).bounds();
    capture_scales_from_values(
        bounds.size.width as f32,
        bounds.size.height as f32,
        image_width,
        image_height,
        fallback,
    )
}

fn capture_scales_from_values(
    logical_w: f32,
    logical_h: f32,
    image_width: u32,
    image_height: u32,
    fallback: f32,
) -> (f32, f32) {
    (
        capture_axis_scale_from_values(logical_w, image_width, fallback),
        capture_axis_scale_from_values(logical_h, image_height, fallback),
    )
}

fn capture_axis_scale_from_values(logical_extent: f32, image_pixels: u32, fallback: f32) -> f32 {
    if logical_extent.is_finite() && logical_extent > 0.0 && image_pixels > 0 {
        image_pixels as f32 / logical_extent
    } else if fallback.is_finite() && fallback > 0.0 {
        fallback
    } else {
        1.0
    }
}

fn native_capture_size(display_id: u32, fallback_w: u32, fallback_h: u32) -> (u32, u32) {
    use core_graphics::display::CGDisplay;
    let cg = CGDisplay::new(display_id);
    if let Some(mode) = cg.display_mode() {
        return native_capture_size_from_values(
            fallback_w,
            fallback_h,
            mode.pixel_width() as u32,
            mode.pixel_height() as u32,
        );
    }
    native_capture_size_from_values(
        fallback_w,
        fallback_h,
        cg.pixels_wide() as u32,
        cg.pixels_high() as u32,
    )
}

fn native_capture_size_from_values(
    fallback_w: u32,
    fallback_h: u32,
    cg_w: u32,
    cg_h: u32,
) -> (u32, u32) {
    if cg_w > 0 && cg_h > 0 {
        (cg_w, cg_h)
    } else {
        (fallback_w.max(1), fallback_h.max(1))
    }
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
    let (native_w, _) = native_capture_size(id, display.width(), display.height());
    let scale = display_scale_from_values(logical_w, native_w);
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

fn display_scale_from_values(logical_w: f32, native_w: u32) -> f32 {
    if logical_w > 0.5 && native_w > 0 {
        (native_w as f32 / logical_w).max(1.0)
    } else {
        1.0
    }
}

fn window_info_from_sc(window: &SCWindow, displays: &[DisplayInfo]) -> Option<WindowInfo> {
    let frame = window.frame();
    let origin = frame.origin();
    let size = frame.size();
    let bounds = Rect::from_xywh(
        origin.x as f32,
        origin.y as f32,
        size.width as f32,
        size.height as f32,
    )?;
    let display_id = best_display_for_rect(bounds, displays)?.id.clone();
    let app_name = window
        .owning_application()
        .map(|app| app.application_name())
        .unwrap_or_default();
    Some(WindowInfo {
        id: WindowId(window.window_id().to_string()),
        title: window.title().unwrap_or_default(),
        app_name,
        display_id,
        bounds,
    })
}

fn best_display_for_rect(rect: Rect, displays: &[DisplayInfo]) -> Option<&DisplayInfo> {
    displays.iter().max_by(|a, b| {
        intersection_area(rect, a.bounds)
            .partial_cmp(&intersection_area(rect, b.bounds))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn intersection_area(a: Rect, b: Rect) -> f32 {
    let x0 = a.x().max(b.x());
    let y0 = a.y().max(b.y());
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    ((x1 - x0).max(0.0)) * ((y1 - y0).max(0.0))
}

fn find_display<'a>(displays: &'a [SCDisplay], requested_id: &str) -> Option<&'a SCDisplay> {
    let id_num: u32 = requested_id.parse().ok()?;
    displays.iter().find(|d| d.display_id() == id_num)
}

fn find_window<'a>(windows: &'a [SCWindow], requested_id: &WindowId) -> Option<&'a SCWindow> {
    let id_num: u32 = requested_id.0.parse().ok()?;
    windows.iter().find(|w| w.window_id() == id_num)
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
            2.0,
        );

        assert_eq!(cropped.width(), 0);
        assert_eq!(cropped.height(), 0);
    }

    #[test]
    fn native_capture_size_prefers_coregraphics_pixels_over_logical_scdisplay_size() {
        assert_eq!(
            native_capture_size_from_values(1512, 982, 3024, 1964),
            (3024, 1964)
        );
    }

    #[test]
    fn native_capture_size_falls_back_when_coregraphics_pixels_are_unavailable() {
        assert_eq!(
            native_capture_size_from_values(1512, 982, 0, 0),
            (1512, 982)
        );
    }

    #[test]
    fn display_scale_uses_native_pixels_over_logical_width() {
        assert_eq!(display_scale_from_values(1512.0, 3024), 2.0);
        assert_eq!(display_scale_from_values(1920.0, 3840), 2.0);
    }

    #[test]
    fn display_scale_never_reports_less_than_one() {
        assert_eq!(display_scale_from_values(1920.0, 1920), 1.0);
        assert_eq!(display_scale_from_values(1920.0, 0), 1.0);
    }

    #[test]
    fn capture_scales_use_actual_image_size_over_expected_display_scale() {
        assert_eq!(
            capture_scales_from_values(1512.0, 982.0, 1512, 982, 2.0),
            (1.0, 1.0)
        );
        assert_eq!(
            capture_scales_from_values(1512.0, 982.0, 3024, 1964, 1.0),
            (2.0, 2.0)
        );
    }

    #[test]
    fn crop_rgba_uses_independent_actual_x_and_y_scales() {
        let cropped = crop_rgba(
            solid(400, 300),
            Rect::from_xywh(10.0, 20.0, 30.0, 40.0).unwrap(),
            1.0,
            1.5,
        );

        assert_eq!(cropped.width(), 30);
        assert_eq!(cropped.height(), 60);
        assert_eq!(cropped.get_pixel(0, 0), &image::Rgba([10, 30, 0, 255]));
    }
}
