//! Windows OCR — `Windows.Media.Ocr` via the `windows` crate.
//!
//! Microsoft's built-in OCR engine ships with every modern Windows
//! install and is free of cost or external model downloads. Compared
//! with Apple Vision it is simpler — no per-word confidence, no
//! language correction toggle — but adequate for the Latin and CJK
//! use cases Readshot's users hit. The user must have at least one
//! language pack installed for the engine to instantiate;
//! `TryCreateFromUserProfileLanguages` returns null otherwise.
//!
//! ## Pipeline
//!
//! 1. Build a [`SoftwareBitmap`] from the request's RGBA8 bytes via
//!    [`DataWriter`] + [`SoftwareBitmap::CreateCopyFromBuffer`].
//! 2. Pick the engine: a specific [`Language`] when the request lists
//!    one, otherwise [`OcrEngine::TryCreateFromUserProfileLanguages`].
//! 3. `RecognizeAsync` returns an `IAsyncOperation<OcrResult>`; we
//!    block on it via `.get()` because the call is short and
//!    [`OCREngine::recognise`] is already async.
//! 4. Join the result's lines with `\n`. Confidence is `0.0` because
//!    Windows.Media.Ocr does not expose per-line or per-word
//!    confidence values; consumers reading MCP responses can ignore
//!    the field on this engine.

use async_trait::async_trait;
use image::RgbaImage;
use readshot_core::error::OCRError;
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

use crate::{OCREngine, OCRRequest, OCRResult};

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
        let langs = match OcrEngine::AvailableRecognizerLanguages() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        for lang in langs {
            if let Ok(tag) = lang.LanguageTag() {
                out.push(tag.to_string());
            }
        }
        out
    }

    async fn recognise(&self, req: OCRRequest) -> Result<OCRResult, OCRError> {
        let bitmap = make_bitmap(&req.image)
            .map_err(|e| OCRError::Backend(format!("SoftwareBitmap: {e}")))?;
        let engine = create_engine(&req.languages)
            .map_err(|e| OCRError::Backend(format!("OcrEngine: {e}")))?;

        let async_op = engine
            .RecognizeAsync(&bitmap)
            .map_err(|e| OCRError::Backend(format!("RecognizeAsync: {e}")))?;
        let result = async_op
            .await
            .map_err(|e| OCRError::Backend(format!("RecognizeAsync await: {e}")))?;

        let lines = result
            .Lines()
            .map_err(|e| OCRError::Backend(format!("OcrResult.Lines: {e}")))?;

        let mut text_lines: Vec<String> = Vec::new();
        for line in lines {
            let s = line
                .Text()
                .map_err(|e| OCRError::Backend(format!("OcrLine.Text: {e}")))?
                .to_string();
            if !s.is_empty() {
                text_lines.push(s);
            }
        }

        Ok(OCRResult {
            text: text_lines.join("\n"),
            // Windows.Media.Ocr does not expose per-line / per-word
            // confidence. Consumers (MCP server, CLI JSON output)
            // treat 0.0 as "engine reports no signal" and ignore it
            // for this backend.
            average_confidence: 0.0,
            lines: Vec::new(),
        })
    }
}

fn make_bitmap(rgba: &RgbaImage) -> Result<SoftwareBitmap, windows::core::Error> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(rgba.as_raw())?;
    let buffer = writer.DetachBuffer()?;
    SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Rgba8,
        rgba.width() as i32,
        rgba.height() as i32,
    )
}

fn create_engine(languages: &[String]) -> Result<OcrEngine, windows::core::Error> {
    if let Some(first) = languages.first() {
        let lang_tag = HSTRING::from(first.as_str());
        let lang = Language::CreateLanguage(&lang_tag)?;
        return OcrEngine::TryCreateFromLanguage(&lang);
    }
    OcrEngine::TryCreateFromUserProfileLanguages()
}
