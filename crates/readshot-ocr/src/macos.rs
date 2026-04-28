//! macOS OCR — Apple Vision via `objc2-vision`.
//!
//! `VNRecognizeTextRequest` is Apple's primary text-recognition API on
//! macOS 14+. We feed it the captured image as PNG-encoded bytes
//! through `VNImageRequestHandler::initWithData:options:`, which lets
//! Vision pick whichever pixel format it prefers internally and avoids
//! us hand-rolling a `CGImageRef` from raw RGBA pointers.
//!
//! ## Recognition tuning
//!
//! * `recognitionLevel = .accurate` — the user explicitly invoked OCR;
//!   speed-over-accuracy isn't the right trade.
//! * `usesLanguageCorrection` — driven by [`OCRRequest::use_language_correction`].
//! * `recognitionLanguages` — set from [`OCRRequest::languages`] when
//!   non-empty; otherwise Vision's default ordering applies.
//!
//! ## Confidence
//!
//! Each `VNRecognizedTextObservation` exposes `topCandidates(n)`. We
//! ask for the single top candidate per observation, take its
//! `confidence` (0.0…1.0), and average across the lines that contained
//! recognised text. Empty observations don't contribute to the average.

use std::io::Cursor;

use async_trait::async_trait;
use image::{ImageFormat, RgbaImage};
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRecognizedTextObservation,
    VNRequest, VNRequestTextRecognitionLevel,
};
use readshot_core::error::OCRError;

use crate::{OCREngine, OCRRequest, OCRResult};

/// We only ever need the highest-confidence candidate per observation.
/// Vision allows up to 10; asking for 1 keeps the call cheap.
const TOP_CANDIDATES_PER_LINE: usize = 1;

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
        // The Vision Framework surfaces supported languages as a static
        // method on the request, parameterised by recognition level
        // and revision. We use the level we actually run with
        // (`.accurate`) so the list reflects what `recognise` will use.
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        unsafe {
            request
                .supportedRecognitionLanguagesAndReturnError()
                .map(|arr| arr.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        }
    }

    async fn recognise(&self, req: OCRRequest) -> Result<OCRResult, OCRError> {
        let png_bytes = encode_png(&req.image)?;
        run_request(png_bytes, &req.languages, req.use_language_correction)
    }
}

fn run_request(
    png_bytes: Vec<u8>,
    languages: &[String],
    use_language_correction: bool,
) -> Result<OCRResult, OCRError> {
    // SAFETY: we live inside one synchronous function, hold no shared
    // state across `await` points, and only call APIs documented as
    // thread-safe by Apple (Vision's request + handler can be created
    // and used off the main thread).
    unsafe {
        let ns_data = NSData::dataWithBytes_length(
            png_bytes.as_ptr() as *mut std::ffi::c_void,
            png_bytes.len(),
        );

        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(use_language_correction);

        if !languages.is_empty() {
            let lang_strings: Vec<Retained<NSString>> =
                languages.iter().map(|s| NSString::from_str(s)).collect();
            let lang_refs: Vec<&NSString> = lang_strings.iter().map(|s| s.as_ref()).collect();
            let lang_array: Retained<NSArray<NSString>> = NSArray::from_slice(&lang_refs);
            request.setRecognitionLanguages(&lang_array);
        }

        // Vision's handler initWithData:options: requires an options
        // dictionary; an empty one is fine for our use case.
        let empty_options: Retained<NSDictionary<VNImageOption, objc2::runtime::AnyObject>> =
            NSDictionary::new();
        let handler = VNImageRequestHandler::initWithData_options(
            VNImageRequestHandler::alloc(),
            &ns_data,
            &empty_options,
        );

        // The performRequests:error: API takes NSArray<VNRequest>. Our
        // request is a subclass; cast through the supertype.
        let request_super: Retained<VNRequest> = Retained::cast_unchecked(request.clone());
        let request_refs: Vec<&VNRequest> = vec![&*request_super];
        let request_array: Retained<NSArray<VNRequest>> = NSArray::from_slice(&request_refs);

        handler
            .performRequests_error(&request_array)
            .map_err(|e| OCRError::Backend(format!("Vision performRequests: {e:?}")))?;

        let observations = match request.results() {
            Some(r) => r,
            None => return Ok(OCRResult::empty()),
        };

        Ok(extract_text(&observations))
    }
}

unsafe fn extract_text(observations: &NSArray<VNRecognizedTextObservation>) -> OCRResult {
    use readshot_core::ocr_layout::RecognizedLine;

    let mut text_lines: Vec<String> = Vec::new();
    let mut positioned: Vec<RecognizedLine> = Vec::new();
    let mut total_confidence: f64 = 0.0;
    let mut counted: u32 = 0;

    for observation in observations.iter() {
        let candidates = observation.topCandidates(TOP_CANDIDATES_PER_LINE as _);
        if let Some(top) = candidates.iter().next() {
            let line = top.string().to_string();
            if !line.is_empty() {
                total_confidence += top.confidence() as f64;
                counted += 1;
                text_lines.push(line.clone());

                // Vision's bounding box is normalised [0..1] with the
                // origin at the *bottom-left* of the image. Flip Y so
                // every backend feeds top-left-origin boxes into
                // `RecognizedLine`, which is what `ocr_layout` expects.
                let bbox = observation.boundingBox();
                let x = bbox.origin.x as f32;
                let bottom_y = bbox.origin.y as f32;
                let w = bbox.size.width as f32;
                let h = bbox.size.height as f32;
                let top_y = (1.0 - bottom_y - h).clamp(0.0, 1.0);
                positioned.push(RecognizedLine {
                    text: line,
                    x,
                    y: top_y,
                    w,
                    h,
                });
            }
        }
    }

    let text = text_lines.join("\n");
    let average_confidence = if counted > 0 {
        (total_confidence / counted as f64) as f32
    } else {
        0.0
    };
    OCRResult {
        text,
        average_confidence,
        lines: positioned,
    }
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, OCRError> {
    let mut buf = Vec::with_capacity((img.width() * img.height() * 4) as usize);
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
        .map_err(|e| OCRError::Backend(format!("PNG encode for Vision: {e}")))?;
    Ok(buf)
}

impl OCRResult {
    fn empty() -> Self {
        Self {
            text: String::new(),
            average_confidence: 0.0,
            lines: Vec::new(),
        }
    }
}
