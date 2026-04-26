//! Windows Capturer.
//!
//! Wraps the [`xcap`](https://crates.io/crates/xcap) crate (≥ 0.9), which
//! itself sits on top of modern `Windows.Graphics.Capture` with a DXGI
//! Desktop Duplication fallback. Versus hand-rolling the
//! `windows`-crate D3D11 staging-texture dance directly, this:
//!
//! 1. Eliminates ~300 LOC of unsafe FFI we cannot validate without a
//!    Windows host.
//! 2. Inherits xcap's permission-checking and error mapping, which has
//!    been hardened across many downstream consumers.
//! 3. Leaves the trait surface unchanged — a future revision can swap
//!    to `windows::Graphics::Capture` directly with no public API
//!    impact.
//!
//! ## Permission semantics
//!
//! Windows does not require an explicit Screen Recording grant for
//! non-elevated apps capturing non-elevated content. Capturing a
//! UAC-elevated window from a non-elevated process returns an empty /
//! protected frame; the user-facing fix is to elevate Readshot itself.
//! That asymmetry is documented in spec §3.12.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;
use xcap::Monitor;

use crate::{CaptureRequest, Capturer, DisplayInfo};

/// Production Windows Capturer.
pub struct WindowsGraphicsCapturer;

pub fn new() -> WindowsGraphicsCapturer {
    WindowsGraphicsCapturer
}

#[async_trait]
impl Capturer for WindowsGraphicsCapturer {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        let monitors = Monitor::all().map_err(map_err)?;
        let mut out = Vec::with_capacity(monitors.len());
        for m in monitors {
            out.push(monitor_to_display_info(&m)?);
        }
        Ok(out)
    }

    async fn capture_region(&self, req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
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
        Ok(crop_rgba(full, req.rect, req.scale))
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

/// Crop a full-monitor `RgbaImage` to the requested logical rect, scaled
/// to physical pixels by `scale`. The result is what
/// `Capturer::capture_region` is expected to return.
fn crop_rgba(full: RgbaImage, rect_logical: Rect, scale: f32) -> RgbaImage {
    let x0 = (rect_logical.x() * scale).round().max(0.0) as u32;
    let y0 = (rect_logical.y() * scale).round().max(0.0) as u32;
    let w_target = (rect_logical.width() * scale).round().max(1.0) as u32;
    let h_target = (rect_logical.height() * scale).round().max(1.0) as u32;
    let w = w_target.min(full.width().saturating_sub(x0));
    let h = h_target.min(full.height().saturating_sub(y0));
    image::imageops::crop_imm(&full, x0, y0, w, h).to_image()
}

fn map_err(e: xcap::XCapError) -> CaptureError {
    let msg = format!("{e}");
    let lc = msg.to_lowercase();
    if lc.contains("permission") || lc.contains("denied") || lc.contains("access is denied") {
        CaptureError::PermissionDenied
    } else {
        CaptureError::Backend(msg)
    }
}
