//! macOS OCR skeleton — Task 11 fills in `VNRecognizeTextRequest`.

use async_trait::async_trait;
use readshot_core::error::OCRError;

use crate::{OCREngine, OCRRequest, OCRResult};

/// Production macOS OCR engine — wraps Apple's Vision framework via
/// `objc2-vision`. Body lands in Task 11.
pub struct AppleVisionEngine;

impl AppleVisionEngine {
    pub fn new() -> Self {
        AppleVisionEngine
    }
}

impl Default for AppleVisionEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OCREngine for AppleVisionEngine {
    fn supported_languages(&self) -> Vec<String> {
        unimplemented!("AppleVisionEngine::supported_languages — Task 11")
    }

    async fn recognise(&self, _req: OCRRequest) -> Result<OCRResult, OCRError> {
        unimplemented!("AppleVisionEngine::recognise — Task 11")
    }
}
