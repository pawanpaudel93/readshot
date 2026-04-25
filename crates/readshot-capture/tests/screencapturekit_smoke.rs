//! Real-hardware smoke test for the macOS Capturer.
//!
//! Skipped unless the project is built with `--features integration`
//! AND the running environment has the env var
//! `CI_HAS_SCREEN_PERMISSION=1` set. Locally, run as:
//!
//! ```bash
//! CI_HAS_SCREEN_PERMISSION=1 cargo nextest run \
//!     -p readshot-capture --features integration \
//!     --test screencapturekit_smoke
//! ```
//!
//! Why two gates? The `integration` feature keeps the test out of the
//! default test set so `cargo nextest run -p readshot-capture` on a
//! contributor's machine doesn't trigger a Screen Recording prompt.
//! The env var is the second gate so even with the feature on we skip
//! when the runner has no display server (any-OS Linux CI, headless
//! Mac runners without tccutil).

#![cfg(all(target_os = "macos", feature = "integration"))]

use readshot_capture::macos::ScreenCaptureKitCapturer;
use readshot_capture::Capturer;

fn skip_unless_permission_granted() -> bool {
    if std::env::var("CI_HAS_SCREEN_PERMISSION").is_err() {
        eprintln!("skipping: CI_HAS_SCREEN_PERMISSION not set");
        return true;
    }
    false
}

#[tokio::test]
async fn lists_at_least_one_display() {
    if skip_unless_permission_granted() {
        return;
    }
    let cap = ScreenCaptureKitCapturer;
    let displays = cap
        .list_displays()
        .await
        .expect("ScreenCaptureKit::list_displays succeeds with permission");
    assert!(!displays.is_empty(), "expected at least one attached display");
    let primary_count = displays.iter().filter(|d| d.is_primary).count();
    assert!(
        primary_count <= 1,
        "expected zero or one primary display, got {primary_count}"
    );
}
