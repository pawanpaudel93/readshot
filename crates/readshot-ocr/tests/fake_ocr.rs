//! Integration tests for `FakeOcrEngine`.
//!
//! These run on every platform (no OS-level OCR or model files needed)
//! and double as documentation of the trait's expected behaviour.

use image::{Rgba, RgbaImage};
use readshot_core::error::OCRError;
use readshot_ocr::{fake::FakeOcrEngine, OCREngine, OCRRequest};

fn dummy_request() -> OCRRequest {
    let mut img = RgbaImage::new(8, 8);
    for px in img.pixels_mut() {
        *px = Rgba([255, 255, 255, 255]);
    }
    OCRRequest {
        image: img,
        languages: vec!["en".to_string()],
        use_language_correction: true,
    }
}

#[tokio::test]
async fn returns_configured_text_with_default_confidence() {
    let engine = FakeOcrEngine::with_text("hello");
    let result = engine.recognise(dummy_request()).await.unwrap();
    assert_eq!(result.text, "hello");
    assert!((result.average_confidence - 0.95).abs() < 1e-3);
}

#[tokio::test]
async fn returns_explicit_confidence_when_set() {
    let engine = FakeOcrEngine::with_text_and_confidence("hello", 0.42);
    let result = engine.recognise(dummy_request()).await.unwrap();
    assert!((result.average_confidence - 0.42).abs() < 1e-3);
}

#[tokio::test]
async fn no_text_variant_returns_empty_string_not_an_error() {
    // Spec §3.16: empty results are NOT errors — the MCP server relies
    // on this so agents see `text: ""` rather than a tool error.
    let engine = FakeOcrEngine::no_text();
    let result = engine.recognise(dummy_request()).await.unwrap();
    assert_eq!(result.text, "");
    assert_eq!(result.average_confidence, 0.0);
}

#[tokio::test]
async fn failing_variant_yields_typed_backend_error() {
    let engine = FakeOcrEngine::failing("disk i/o");
    let err = engine.recognise(dummy_request()).await.unwrap_err();
    match err {
        OCRError::Backend(msg) => assert!(msg.contains("disk i/o")),
        other => panic!("expected Backend error, got {other:?}"),
    }
}

#[tokio::test]
async fn supported_languages_defaults_to_english() {
    let engine = FakeOcrEngine::default();
    assert_eq!(engine.supported_languages(), vec!["en".to_string()]);
}

#[tokio::test]
async fn supported_languages_can_be_customised() {
    let engine =
        FakeOcrEngine::no_text().with_supported_languages(vec!["fr".to_string(), "de".to_string()]);
    assert_eq!(
        engine.supported_languages(),
        vec!["fr".to_string(), "de".to_string()]
    );
}
