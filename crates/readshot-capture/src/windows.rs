//! Windows Capturer skeleton.
//!
//! The real implementation arrives in Task 8 using the `windows` crate's
//! `Windows::Graphics::Capture` APIs. Until then both methods panic.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;

use crate::{CaptureRequest, Capturer, DisplayInfo};

/// Production Windows Capturer — wraps `Windows::Graphics::Capture`.
/// Body lands in Task 8.
pub struct WindowsGraphicsCapturer;

pub fn new() -> WindowsGraphicsCapturer {
    WindowsGraphicsCapturer
}

#[async_trait]
impl Capturer for WindowsGraphicsCapturer {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        unimplemented!("WindowsGraphicsCapturer::list_displays — Task 8")
    }

    async fn capture_region(&self, _req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
        unimplemented!("WindowsGraphicsCapturer::capture_region — Task 8")
    }
}
