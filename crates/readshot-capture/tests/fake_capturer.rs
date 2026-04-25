//! Integration tests for `FakeCapturer`.
//!
//! These run on every platform (no OS-level capture grant required) and
//! double as documentation of the trait's expected behaviour.

use readshot_capture::{Capturer, CaptureRequest};
use readshot_capture::fake::FakeCapturer;
use readshot_core::geom::Rect;

fn full_rect_request() -> CaptureRequest {
    CaptureRequest {
        display_id: "fake-0".to_string(),
        rect: Rect::from_xywh(0.0, 0.0, 256.0, 256.0).expect("valid rect"),
        scale: 1.0,
        hide_cursor: true,
    }
}

#[tokio::test]
async fn list_displays_returns_a_single_primary_display() {
    let cap = FakeCapturer::new();
    let displays = cap.list_displays().await.expect("listing succeeds");
    assert_eq!(displays.len(), 1);
    let display = &displays[0];
    assert_eq!(display.id, "fake-0");
    assert_eq!(display.scale, 1.0);
    assert!(display.is_primary);
}

#[tokio::test]
async fn capture_region_returns_the_canonical_fixture() {
    let cap = FakeCapturer::new();
    let img = cap
        .capture_region(full_rect_request())
        .await
        .expect("capture succeeds");
    assert_eq!(img.width(), 256);
    assert_eq!(img.height(), 256);
}

#[tokio::test]
async fn capture_request_arguments_are_ignored_by_the_fake() {
    let cap = FakeCapturer::new();
    let req = CaptureRequest {
        display_id: "anything".to_string(),
        rect: Rect::from_xywh(99.0, 99.0, 50.0, 50.0).unwrap(),
        scale: 2.0,
        hide_cursor: false,
    };
    let img = cap.capture_region(req).await.expect("capture succeeds");
    // The fake returns the full 256×256 fixture regardless of inputs —
    // higher-layer tests rely on this behaviour to feed deterministic
    // pixels into the editor without modelling rect arithmetic.
    assert_eq!(img.width(), 256);
    assert_eq!(img.height(), 256);
}
