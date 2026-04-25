//! Shared test fixtures for the Readshot workspace.
//!
//! The single canonical fixture is [`base_256`], a 256×256 RGBA canvas used
//! as the base for the annotation-renderer snapshot tests in
//! `readshot-core/tests/render_snapshot.rs`. It is **generated
//! programmatically** rather than checked in as a `.png` so that
//!
//! 1. The repo stays free of opaque binary blobs.
//! 2. The fixture is bit-identical across machines and architectures —
//!    important because the snapshot tests rely on the renderer's output
//!    being byte-reproducible.
//! 3. Adjusting the fixture (or adding new ones) is a code change, not a
//!    binary-asset commit, so review-by-eye is possible.

use image::{Rgba, RgbaImage};

/// 256×256 RGBA canvas: a 32-pixel two-tone-grey checkerboard.
///
/// The two tones are deliberately close (light vs lighter grey) so any
/// annotation drawn on top stands out clearly, while the tile boundaries
/// help the eye gauge the position and scale of an annotation in the
/// snapshot diff.
pub fn base_256() -> RgbaImage {
    const SIZE: u32 = 256;
    const TILE: u32 = 32;
    const LIGHT: Rgba<u8> = Rgba([240, 240, 240, 255]);
    const LIGHTER: Rgba<u8> = Rgba([224, 224, 224, 255]);

    let mut img = RgbaImage::new(SIZE, SIZE);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let on_dark_tile = ((x / TILE) + (y / TILE)).is_multiple_of(2);
            img.put_pixel(x, y, if on_dark_tile { LIGHTER } else { LIGHT });
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_correct_size() {
        let img = base_256();
        assert_eq!(img.width(), 256);
        assert_eq!(img.height(), 256);
    }

    #[test]
    fn fixture_is_deterministic() {
        // Two independently-built fixtures must have identical bytes.
        // The renderer's snapshot tests assume this invariant — without
        // it, any insta diff could be fixture drift rather than renderer
        // change.
        let a = base_256();
        let b = base_256();
        assert_eq!(a.as_raw(), b.as_raw());
    }

    #[test]
    fn corner_pixels_match_expected_tile_colours() {
        let img = base_256();
        // (0,0) is in tile (0,0) → on a dark tile → LIGHTER.
        assert_eq!(img.get_pixel(0, 0), &Rgba([224, 224, 224, 255]));
        // (32,0) is in tile (1,0) → on a light tile → LIGHT.
        assert_eq!(img.get_pixel(32, 0), &Rgba([240, 240, 240, 255]));
    }
}
