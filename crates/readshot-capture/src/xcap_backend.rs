//! Shared xcap-based Capturer for Windows and Linux.
//!
//! Both platforms wrap the [`xcap`](https://crates.io/crates/xcap)
//! crate (≥ 0.9): on Windows it sits on `Windows.Graphics.Capture`
//! with a DXGI Desktop Duplication fallback; on Linux it routes to the
//! `xdg-desktop-portal` ScreenCast portal on Wayland and XGetImage on
//! X11 (selected automatically from `$WAYLAND_DISPLAY` / `$DISPLAY`).
//! Versus hand-rolling the per-OS FFI this eliminates hundreds of
//! lines of unsafe code we cannot validate without those hosts, and
//! inherits xcap's permission handling.
//!
//! The two backends used to be near-verbatim copies in `windows.rs`
//! and `linux/mod.rs`; the only real platform difference is how
//! monitor bounds are normalised to logical pixels (Windows reports
//! physical pixels that must be divided by the scale factor, Linux
//! reports logical values already) — captured in [`logical_bounds`].
//!
//! ## Permission semantics
//!
//! * Windows: no explicit grant for non-elevated apps capturing
//!   non-elevated content. Capturing a UAC-elevated window from a
//!   non-elevated process returns an empty / protected frame; the fix
//!   is to elevate Readshot itself (spec §3.12).
//! * Linux/Wayland: the portal consent dialog is the grant; the token
//!   is reused for the rest of the session. X11 has no consent step.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;
use xcap::Monitor;

use crate::{rect_relative_to_display, CaptureRequest, Capturer, DisplayInfo};

/// Production Capturer for Windows and Linux, backed by xcap.
pub struct XcapCapturer;

pub fn new() -> XcapCapturer {
    XcapCapturer
}

#[async_trait]
impl Capturer for XcapCapturer {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        crate::run_capture_blocking("list_displays", || {
            let monitors = Monitor::all().map_err(map_err)?;
            let mut out = Vec::with_capacity(monitors.len());
            for m in monitors {
                out.push(monitor_to_display_info(&m)?);
            }
            Ok(out)
        })
        .await
    }

    async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
        crate::run_capture_blocking("capture_region", move || {
            let id_num: u32 = req
                .display_id
                .parse()
                .map_err(|_| CaptureError::DisplayNotFound(req.display_id.clone()))?;

            let monitors = Monitor::all().map_err(map_err)?;
            let monitor = monitors
                .into_iter()
                .find(|m| m.id().ok() == Some(id_num))
                .ok_or_else(|| CaptureError::DisplayNotFound(req.display_id.clone()))?;

            // xcap returns the entire monitor's image; crop in software to
            // the requested logical rect (scaled to physical pixels).
            let full = monitor.capture_image().map_err(map_err)?;
            let display = monitor_to_display_info(&monitor)?;
            let rect = rect_relative_to_display(req.rect, display.bounds);
            crop_rgba(full, rect, req.scale)
        })
        .await
    }
}

fn monitor_to_display_info(m: &Monitor) -> Result<DisplayInfo, CaptureError> {
    let id = m.id().map_err(map_err)?.to_string();
    let x = m.x().map_err(map_err)? as f32;
    let y = m.y().map_err(map_err)? as f32;
    let w = m.width().map_err(map_err)? as f32;
    let h = m.height().map_err(map_err)? as f32;
    let scale = m.scale_factor().map_err(map_err)?;
    let name = m.friendly_name().or_else(|_| m.name()).map_err(map_err)?;
    let is_primary = m.is_primary().map_err(map_err)?;
    let (x, y, w, h) = logical_bounds(x, y, w, h, scale);
    let bounds = Rect::from_xywh(x, y, w.max(1.0), h.max(1.0))
        .unwrap_or_else(|| Rect::from_xywh(0.0, 0.0, 1.0, 1.0).unwrap());
    Ok(DisplayInfo {
        id,
        bounds,
        scale,
        name,
        is_primary,
    })
}

/// Windows reports monitor geometry in physical pixels; divide by the
/// scale factor to get the logical bounds [`DisplayInfo`] promises.
#[cfg(target_os = "windows")]
fn logical_bounds(x: f32, y: f32, w: f32, h: f32, scale: f32) -> (f32, f32, f32, f32) {
    let safe_scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    (
        x / safe_scale,
        y / safe_scale,
        w / safe_scale,
        h / safe_scale,
    )
}

/// Linux already reports logical values.
#[cfg(target_os = "linux")]
fn logical_bounds(x: f32, y: f32, w: f32, h: f32, _scale: f32) -> (f32, f32, f32, f32) {
    (x, y, w, h)
}

/// Crop a full-monitor `RgbaImage` to the requested logical rect, scaled
/// to physical pixels by `scale`. The result is what
/// `Capturer::capture_region` is expected to return.
fn crop_rgba(full: RgbaImage, rect_logical: Rect, scale: f32) -> Result<RgbaImage, CaptureError> {
    let x0 = (rect_logical.x() * scale).round().max(0.0) as u32;
    let y0 = (rect_logical.y() * scale).round().max(0.0) as u32;
    let w_target = (rect_logical.width() * scale).round().max(1.0) as u32;
    let h_target = (rect_logical.height() * scale).round().max(1.0) as u32;
    let w = w_target.min(full.width().saturating_sub(x0));
    let h = h_target.min(full.height().saturating_sub(y0));
    if w == 0 || h == 0 {
        return Err(CaptureError::InvalidRegion(
            "region is outside the captured display bounds".to_string(),
        ));
    }
    Ok(image::imageops::crop_imm(&full, x0, y0, w, h).to_image())
}

fn map_err(e: xcap::XCapError) -> CaptureError {
    let msg = format!("{e}");
    if is_permission_denied(&msg) {
        CaptureError::PermissionDenied
    } else {
        CaptureError::Backend(msg)
    }
}

/// xcap surfaces capture denials (Windows access errors, Wayland portal
/// declines) as opaque error strings, so we sniff the message
/// (`"denied"` already subsumes `"access is denied"`).
fn is_permission_denied(message: &str) -> bool {
    let lc = message.to_lowercase();
    lc.contains("permission")
        || lc.contains("denied")
        || lc.contains("not authorized")
        || lc.contains("declined")
}
