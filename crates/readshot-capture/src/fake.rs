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

use crate::{CaptureRequest, Capturer, DisplayInfo};

pub struct FakeCapturer {
    image: RgbaImage,
    displays: Vec<DisplayInfo>,
}

impl FakeCapturer {
    pub fn new() -> Self {
        let bounds =
            Rect::from_xywh(0.0, 0.0, 256.0, 256.0).expect("256x256 bounds is a valid Rect");
        Self {
            image: base_256(),
            displays: vec![DisplayInfo {
                id: "fake-0".to_string(),
                bounds,
                scale: 1.0,
                name: "Fake Display".to_string(),
                is_primary: true,
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
}
