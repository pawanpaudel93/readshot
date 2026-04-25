//! End-to-end smoke test for the macOS Apple Vision OCR engine.
//!
//! Apple Vision runs on-device with no permission prompt, so unlike
//! the capture smoke test we don't gate this behind an env var or the
//! `integration` feature — it runs as part of `cargo nextest run -p
//! readshot-ocr` on any macOS host.
//!
//! The fixture is generated programmatically by Readshot's own
//! annotation renderer (no committed binary blob). The test asserts a
//! substring match rather than exact equality because Vision's output
//! varies slightly with macOS minor versions and font rendering nuances.

#![cfg(target_os = "macos")]

use image::{Rgba, RgbaImage};
use readshot_core::{render, Annotation, PointLike, RectLike, Rgba as CoreRgba};
use readshot_ocr::{macos::AppleVisionEngine, OCREngine, OCRRequest};

fn render_test_image(text: &str) -> RgbaImage {
    // White background gives Vision the best contrast against black
    // text. 512×128 at 48pt fits "Hello, Readshot" comfortably with
    // padding.
    let width = 512u32;
    let height = 128u32;
    let mut base = RgbaImage::new(width, height);
    for px in base.pixels_mut() {
        *px = Rgba([255, 255, 255, 255]);
    }

    let model = vec![
        // Stroked rect just to make sure Vision isn't fooled into
        // recognising decorative shapes as text.
        Annotation::Rectangle {
            rect: RectLike::new(8.0, 8.0, (width - 16) as f32, (height - 16) as f32),
            color: CoreRgba::new(0.85, 0.85, 0.85, 1.0),
            line_width: 1.0,
        },
        Annotation::Text {
            content: text.to_string(),
            origin: PointLike::new(24.0, 80.0),
            color: CoreRgba::OPAQUE_BLACK,
            font_family: "system-ui".to_string(),
            size: 48.0,
        },
    ];
    render(&base, &model)
}

#[tokio::test]
async fn supported_languages_includes_english() {
    let engine = AppleVisionEngine::new();
    let langs = engine.supported_languages();
    assert!(
        !langs.is_empty(),
        "expected Vision to report at least one supported language"
    );
    assert!(
        langs.iter().any(|l| l.starts_with("en")),
        "expected an English variant in supported languages, got: {langs:?}"
    );
}

#[tokio::test]
async fn recognises_rendered_text() {
    let img = render_test_image("Hello, Readshot");
    let engine = AppleVisionEngine::new();
    let result = engine
        .recognise(OCRRequest {
            image: img,
            languages: vec!["en-US".to_string()],
            use_language_correction: true,
        })
        .await
        .expect("Vision recognise should succeed on white-on-black input");

    let recognised = result.text.to_lowercase();
    assert!(
        recognised.contains("hello") || recognised.contains("readshot"),
        "expected recognised text to mention `hello` or `readshot`, got: {:?}",
        result.text,
    );
    assert!(
        result.average_confidence > 0.0,
        "expected positive confidence, got {}",
        result.average_confidence,
    );
}

#[tokio::test]
async fn empty_white_image_returns_empty_text_not_error() {
    // Spec §3.16 requires no-text to be a *successful* result, not an
    // error, so the MCP server can return `{ text: "" }` to agents.
    let mut blank = RgbaImage::new(64, 64);
    for px in blank.pixels_mut() {
        *px = Rgba([255, 255, 255, 255]);
    }
    let engine = AppleVisionEngine::new();
    let result = engine
        .recognise(OCRRequest {
            image: blank,
            languages: vec!["en-US".to_string()],
            use_language_correction: false,
        })
        .await
        .expect("recognise on a blank image should not error");
    assert!(
        result.text.is_empty(),
        "expected empty text on blank input, got: {:?}",
        result.text
    );
}
