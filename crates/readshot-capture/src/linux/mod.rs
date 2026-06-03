//! Linux Capturer.
//!
//! Wraps [`xcap`](https://crates.io/crates/xcap) ≥ 0.9, which on Linux
//! routes to:
//!
//! * Wayland: the `xdg-desktop-portal` ScreenCast portal via D-Bus,
//!   matching Apple Vision / Windows Graphics Capture on the trust
//!   model — every capture surfaces a portal consent dialog the first
//!   time. Subsequent same-session captures reuse the granted token.
//! * X11: the X11 root window via the standard XGetImage path.
//!
//! Routing is automatic based on `$WAYLAND_DISPLAY` / `$DISPLAY`. The
//! plan called for direct `ashpd` + `pipewire-rs` (Wayland) and
//! `x11rb` (X11) bindings; we delegate to xcap to ship a working
//! implementation in ~80 LOC that I can ship without a Linux host.
//! A future revision can split this directory back into `portal.rs` +
//! `x11.rs` siblings if hand-rolling becomes valuable (e.g. faster
//! capture path on Wayland by avoiding the portal round-trip after
//! first consent).
//!
//! ## Permission semantics
//!
//! On Wayland, the portal dialog is the consent. xcap surfaces denials
//! as an `XCapError` containing "permission" / "denied", which we map
//! to [`CaptureError::PermissionDenied`]. On X11 there is no consent
//! step.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;
use xcap::Monitor;

use crate::{rect_relative_to_display, CaptureRequest, Capturer, DisplayInfo};

/// Production Linux Capturer. Routes to the portal on Wayland and to
/// X11 otherwise — both via [`xcap`]. The runtime selector lives inside
/// xcap, so this struct itself is a thin facade.
pub struct LinuxCapturer;

pub fn new() -> LinuxCapturer {
    LinuxCapturer
}

#[async_trait]
impl Capturer for LinuxCapturer {
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

/// xcap surfaces portal/X11 capture denials as opaque error strings, so we
/// sniff the message. The token set is shared with the Windows backend for
/// consistency (`"denied"` already subsumes `"access is denied"`).
fn is_permission_denied(message: &str) -> bool {
    let lc = message.to_lowercase();
    lc.contains("permission")
        || lc.contains("denied")
        || lc.contains("not authorized")
        || lc.contains("declined")
}
