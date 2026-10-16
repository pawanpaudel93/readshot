//! Eyedropper sampling (spec §3.5). Samples the *source* image (not the
//! annotated render), so picking a color never picks up a stroke.

use image::RgbaImage;

use crate::annotation::Rgba;
use crate::geom::PointLike;

fn pixel(img: &RgbaImage, x: i64, y: i64) -> Option<Rgba> {
    if x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 {
        return None;
    }
    let mut c = Rgba::from_rgba8(img.get_pixel(x as u32, y as u32).0);
    // Annotation colors are opaque.
    c.a = 1.0;
    Some(c)
}

/// Color under `at` (base-image pixels). The far edge (`x == width`,
/// which the canvas's clamped mapping can produce) reads the last pixel.
pub fn sample(img: &RgbaImage, at: PointLike) -> Option<Rgba> {
    if !(at.x.is_finite() && at.y.is_finite()) || at.x < 0.0 || at.y < 0.0 {
        return None;
    }
    if at.x > img.width() as f32 || at.y > img.height() as f32 {
        return None;
    }
    let x = (at.x.floor() as i64).min(img.width() as i64 - 1);
    let y = (at.y.floor() as i64).min(img.height() as i64 - 1);
    pixel(img, x, y)
}

/// `(2r + 1)²` samples around `center`, row-major; `None` off the image.
pub fn sample_grid(img: &RgbaImage, center: PointLike, radius: u32) -> Vec<Option<Rgba>> {
    let r = radius as i64;
    let (cx, cy) = (center.x.floor() as i64, center.y.floor() as i64);
    let mut out = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
    for dy in -r..=r {
        for dx in -r..=r {
            out.push(pixel(img, cx + dx, cy + dy));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img() -> RgbaImage {
        let mut i = RgbaImage::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        i.put_pixel(1, 2, image::Rgba([255, 0, 0, 128]));
        i.put_pixel(3, 3, image::Rgba([0, 255, 0, 255]));
        i
    }

    #[test]
    fn sample_reads_the_pixel_and_drops_alpha() {
        assert_eq!(
            sample(&img(), PointLike::new(1.7, 2.2)),
            Some(Rgba::new(1.0, 0.0, 0.0, 1.0))
        );
    }

    #[test]
    fn sample_clamps_the_far_edge_and_rejects_outside() {
        assert_eq!(
            sample(&img(), PointLike::new(4.0, 4.0)),
            Some(Rgba::new(0.0, 1.0, 0.0, 1.0))
        );
        assert_eq!(sample(&img(), PointLike::new(-0.5, 1.0)), None);
        assert_eq!(sample(&img(), PointLike::new(4.5, 1.0)), None);
        assert_eq!(sample(&img(), PointLike::new(f32::NAN, 1.0)), None);
    }

    #[test]
    fn grid_is_row_major_with_none_off_image() {
        let g = sample_grid(&img(), PointLike::new(0.0, 0.0), 1);
        assert_eq!(g.len(), 9);
        assert_eq!(g[0], None);
        assert_eq!(g[4], Some(Rgba::new(0.0, 0.0, 0.0, 1.0)));
        assert!(g[8].is_some());
    }
}
