//! macOS OCR — Apple Vision via `objc2-vision`.
//!
//! `VNRecognizeTextRequest` is Apple's primary text-recognition API on
//! macOS 14+. Rust still owns the public backend contract, but the
//! request execution goes through a small Swift shim so Vision receives
//! the same native overlay call shape as Apple's working examples.
//!
//! ## Recognition tuning
//!
//! * `recognitionLevel = .accurate` — the user explicitly invoked OCR;
//!   speed-over-accuracy isn't the right trade.
//! * `usesLanguageCorrection` — driven by [`OCRRequest::use_language_correction`].
//! * `automaticallyDetectsLanguage` — enabled when no preferred languages
//!   are supplied, so multilingual screenshots use Vision's language model
//!   selection instead of the default Latin-biased behavior.
//! * `recognitionLanguages` — set from [`OCRRequest::languages`] when
//!   non-empty; explicit languages are treated as a user override.
//!
//! ## Confidence
//!
//! Each `VNRecognizedTextObservation` exposes `topCandidates(n)`. We
//! ask for the single top candidate per observation, take its
//! `confidence` (0.0…1.0), and average across the lines that contained
//! recognised text. Empty observations don't contribute to the average.

use async_trait::async_trait;
use image::RgbaImage;
use objc2_vision::{VNRecognizeTextRequest, VNRequestTextRecognitionLevel};
use readshot_core::error::OCRError;
use readshot_core::ocr_layout::RecognizedLine;
use serde::Deserialize;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::ptr;

use crate::{OCREngine, OCRRequest, OCRResult};

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
        // The Vision call (and the PNG encode feeding it) is fully
        // synchronous and can take seconds on large captures. Running it
        // inline would park a tokio worker — the same failure mode
        // readshot-capture isolates with `run_capture_blocking` — so move
        // the work to a blocking thread and bound it with a timeout.
        let OCRRequest {
            image,
            languages,
            use_language_correction,
        } = req;
        let work = tokio::task::spawn_blocking(move || {
            let png_bytes = encode_png(&image)?;
            run_request(png_bytes, &languages, use_language_correction)
        });
        match tokio::time::timeout(VISION_TIMEOUT, work).await {
            Ok(Ok(result)) => result,
            Ok(Err(join_err)) => Err(OCRError::Backend(format!(
                "Vision OCR worker thread failed: {join_err}"
            ))),
            Err(_elapsed) => Err(OCRError::Backend(format!(
                "Vision OCR timed out after {VISION_TIMEOUT:?}; the OS text-recognition call did not return"
            ))),
        }
    }
}

/// Upper bound on one Vision request. Accurate-mode OCR on a Retina
/// region is normally well under a second; this is a backstop against
/// a wedged OS call, not a tuning knob.
const VISION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn run_request(
    png_bytes: Vec<u8>,
    languages: &[String],
    use_language_correction: bool,
) -> Result<OCRResult, OCRError> {
    recognise_with_native_vision(&png_bytes, languages, use_language_correction)
}

#[derive(Deserialize)]
struct VisionPayload {
    text: String,
    average_confidence: f32,
    lines: Vec<VisionLine>,
}

#[derive(Deserialize)]
struct VisionLine {
    text: String,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl From<VisionPayload> for OCRResult {
    fn from(payload: VisionPayload) -> Self {
        OCRResult {
            text: payload.text,
            average_confidence: payload.average_confidence,
            lines: payload
                .lines
                .into_iter()
                .map(|line| RecognizedLine {
                    text: line.text,
                    x: line.x,
                    y: line.y,
                    w: line.w,
                    h: line.h,
                })
                .collect(),
        }
    }
}

unsafe extern "C" {
    fn readshot_vision_recognize_png(
        png_bytes: *const u8,
        png_len: usize,
        languages: *const *const c_char,
        language_count: usize,
        use_language_correction: bool,
        out_json: *mut *mut c_char,
        out_error: *mut *mut c_char,
    ) -> c_int;

    fn readshot_vision_free_string(string: *mut c_char);
}

fn recognise_with_native_vision(
    png_bytes: &[u8],
    languages: &[String],
    use_language_correction: bool,
) -> Result<OCRResult, OCRError> {
    let language_strings: Vec<CString> = languages
        .iter()
        .map(|language| {
            CString::new(language.as_str()).map_err(|_| {
                OCRError::Backend(format!("invalid OCR language contains NUL: {language:?}"))
            })
        })
        .collect::<Result<_, _>>()?;
    let language_ptrs: Vec<*const c_char> = language_strings
        .iter()
        .map(|language| language.as_ptr())
        .collect();

    let mut out_json: *mut c_char = ptr::null_mut();
    let mut out_error: *mut c_char = ptr::null_mut();
    let language_ptr = if language_ptrs.is_empty() {
        ptr::null()
    } else {
        language_ptrs.as_ptr()
    };

    let status = unsafe {
        readshot_vision_recognize_png(
            png_bytes.as_ptr(),
            png_bytes.len(),
            language_ptr,
            language_ptrs.len(),
            use_language_correction,
            &mut out_json,
            &mut out_error,
        )
    };

    if status != 0 {
        let message = unsafe { take_vision_string(out_error) }
            .unwrap_or_else(|| "Vision OCR failed without an error message".to_string());
        return Err(OCRError::Backend(message));
    }

    let json = unsafe { take_vision_string(out_json) }
        .ok_or_else(|| OCRError::Backend("Vision OCR returned no result payload".to_string()))?;
    let payload: VisionPayload = serde_json::from_str(&json)
        .map_err(|e| OCRError::Backend(format!("Vision OCR result parse: {e}")))?;
    Ok(payload.into())
}

unsafe fn take_vision_string(ptr: *mut c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let value = CStr::from_ptr(ptr).to_string_lossy().into_owned();
    readshot_vision_free_string(ptr);
    Some(value)
}

fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, OCRError> {
    // Fast compression: the bytes only cross the FFI boundary and are
    // decoded by Vision immediately — Best-compression zlib here would
    // burn seconds of CPU on large captures for nothing.
    readshot_core::encode_png_fast(img)
        .map_err(|e| OCRError::Backend(format!("PNG encode for Vision: {e}")))
}
