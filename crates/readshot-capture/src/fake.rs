//! In-memory [`Capturer`] used by every higher crate's tests.
//!
//! [`FakeCapturer::new`] returns a single fake display (`fake-0`,
//! 256×256, primary, scale 1.0) and yields the canonical
//! [`readshot_test_fixtures::base_256`] fixture for every capture
//! request. The capture rect and `hide_cursor` flag are ignored — the
//! fake is for exercising trait wiring, not for realism.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::CaptureError;
use readshot_core::geom::Rect;
use readshot_test_fixtures::base_256;

use crate::{CaptureRequest, Capturer, DisplayInfo, WindowCaptureRequest, WindowId, WindowInfo};

pub struct FakeCapturer {
    image: RgbaImage,
    displays: Vec<DisplayInfo>,
    windows: Vec<WindowInfo>,
}

impl FakeCapturer {
    pub fn new() -> Self {
        let bounds =
            Rect::from_xywh(0.0, 0.0, 256.0, 256.0).expect("256x256 bounds is a valid Rect");
        let display_id = "fake-0".to_string();
        Self {
            image: base_256(),
            displays: vec![DisplayInfo {
                id: display_id.clone(),
                bounds,
                scale: 1.0,
                name: "Fake Display".to_string(),
                is_primary: true,
            }],
            windows: vec![WindowInfo {
                id: WindowId("fake-window-0".to_string()),
                title: "Fake Window".to_string(),
                app_name: "Readshot Test".to_string(),
                display_id,
                bounds: Rect::from_xywh(10.0, 20.0, 120.0, 80.0)
                    .expect("fake window bounds are valid"),
            }],
        }
    }
}

impl Default for FakeCapturer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Capturer for FakeCapturer {
    async fn list_displays(&self) -> Result<Vec<DisplayInfo>, CaptureError> {
        Ok(self.displays.clone())
    }

    async fn capture_region(&self, _req: CaptureRequest) -> Result<RgbaImage, CaptureError> {
        Ok(self.image.clone())
    }

    async fn list_windows(&self) -> Result<Vec<WindowInfo>, CaptureError> {
        Ok(self.windows.clone())
    }

    async fn capture_window(&self, req: WindowCaptureRequest) -> Result<RgbaImage, CaptureError> {
        if self.windows.iter().any(|window| window.id == req.window_id) {
            Ok(self.image.clone())
        } else {
            Err(CaptureError::WindowNotFound(req.window_id.0))
        }
    }
}
