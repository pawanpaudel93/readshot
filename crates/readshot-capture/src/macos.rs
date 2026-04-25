//! macOS Capturer skeleton.
//!
//! The real implementation arrives in Task 7; until then `list_displays`
//! and `capture_region` panic with `unimplemented!()` so callers surface
//! the missing piece loudly during integration rather than silently
//! returning empty results.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;

use crate::{CaptureRequest, Capturer, DisplayInfo};

/// Production macOS Capturer — wraps the `screencapturekit` crate's
/// `SCScreenshotManager` API. Body lands in Task 7.
pub struct ScreenCaptureKitCapturer;

pub fn new() -> ScreenCaptureKitCapturer {
    ScreenCaptureKitCapturer
}

#[async_trait]
impl Capturer for ScreenCaptureKitCapturer {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        unimplemented!("ScreenCaptureKitCapturer::list_displays — Task 7")
    }

    async fn capture_region(&self, _req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
        unimplemented!("ScreenCaptureKitCapturer::capture_region — Task 7")
    }
}
