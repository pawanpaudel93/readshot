//! Linux Capturer skeleton.
//!
//! Task 9 fills in the real implementation as two siblings (`portal.rs`
//! for Wayland via `ashpd` + `pipewire-rs`, `x11.rs` for X11 via
//! `x11rb`) plus a runtime selector. The submodule directory layout
//! exists from Task 6 onwards so Task 9's diff is purely additive.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;

use crate::{CaptureRequest, Capturer, DisplayInfo};

/// Production Linux Capturer — picks `PortalCapturer` (Wayland) or
/// `X11Capturer` based on `WAYLAND_DISPLAY` / `DISPLAY` at runtime.
/// Body lands in Task 9.
pub struct LinuxCapturer;

pub fn new() -> LinuxCapturer {
    LinuxCapturer
}

#[async_trait]
impl Capturer for LinuxCapturer {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        unimplemented!("LinuxCapturer::list_displays — Task 9")
    }

    async fn capture_region(&self, _req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
        unimplemented!("LinuxCapturer::capture_region — Task 9")
    }
}
