//! Windows OCR skeleton — Task 12 fills in `Windows.Media.Ocr`.

use async_trait::async_trait;
use readshot_core::error::OCRError;

use crate::{OCREngine, OCRRequest, OCRResult};

/// Production Windows OCR engine — wraps `Windows::Media::Ocr` via the
/// `windows` crate. Body lands in Task 12.
pub struct WindowsMediaOcrEngine;

impl WindowsMediaOcrEngine {
    pub fn new() -> Self {
        WindowsMediaOcrEngine
    }
}

impl Default for WindowsMediaOcrEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OCREngine for WindowsMediaOcrEngine {
    fn supported_languages(&self) -> Vec<String> {
        unimplemented!("WindowsMediaOcrEngine::supported_languages — Task 12")
    }

    async fn recognise(&self, _req: OCRRequest) -> Result<OCRResult, OCRError> {
        unimplemented!("WindowsMediaOcrEngine::recognise — Task 12")
    }
}
