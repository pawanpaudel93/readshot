//! Encoding a capture in the user's chosen [`ExportFormat`].
//!
//! Every GUI save path (editor Save, overlay Save, scrolling capture via
//! the editor, the full-display quick save) writes through here, so
//! Settings › Default format decides the encoder and the extension in
//! one place.
//!
//! * PNG: [`crate::png::encode`] (lossless, RGB when opaque).
//! * JPEG: quality 90, the same as the CLI's `--format jpg`. JPEG has
//!   no alpha, so transparent pixels (Beautify's rounded corners, a
//!   transparent background) are composited over white rather than
//!   dropped to whatever colour sits under a zero alpha.
//! * WebP: lossless, alpha kept.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::codecs::webp::WebPEncoder;
use image::{ExtendedColorType, ImageEncoder, ImageResult, RgbaImage};

use crate::preferences::ExportFormat;

/// JPEG quality for saved captures (matches the CLI).
pub const JPEG_QUALITY: u8 = 90;

impl ExportFormat {
    /// The extension a saved file gets, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Png => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Webp => "webp",
        }
    }

    /// Every extension that names this format (for a save dialog's
    /// filter), the preferred one first.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            ExportFormat::Png => &["png"],
            ExportFormat::Jpeg => &["jpg", "jpeg"],
            ExportFormat::Webp => &["webp"],
        }
    }

    /// The format a file extension names, case-insensitively.
    pub fn from_extension(ext: &str) -> Option<ExportFormat> {
        ExportFormat::ALL
            .into_iter()
            .find(|f| f.extensions().iter().any(|e| e.eq_ignore_ascii_case(ext)))
    }

    /// Every format, in the order Settings lists them.
    pub const ALL: [ExportFormat; 3] = [ExportFormat::Png, ExportFormat::Jpeg, ExportFormat::Webp];
}

/// Encode `img` as `format`.
pub fn encode(img: &RgbaImage, format: ExportFormat) -> ImageResult<Vec<u8>> {
    match format {
        ExportFormat::Png => crate::png::encode(img),
        ExportFormat::Jpeg => {
            let rgb = flatten_over_white(img);
            let mut out = Cursor::new(Vec::new());
            JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY).write_image(
                &rgb,
                img.width(),
                img.height(),
                ExtendedColorType::Rgb8,
            )?;
            Ok(out.into_inner())
        }
        ExportFormat::Webp => {
            let mut out = Vec::new();
            WebPEncoder::new_lossless(&mut out).write_image(
                img.as_raw(),
                img.width(),
                img.height(),
                ExtendedColorType::Rgba8,
            )?;
            Ok(out)
        }
    }
}

/// Encode `img` as `format` and write it to `path`.
pub fn save(img: &RgbaImage, path: &Path, format: ExportFormat) -> ImageResult<()> {
    if format == ExportFormat::Png {
        // Streams straight to the file, as before.
        return crate::png::save(img, path);
    }
    let bytes = encode(img, format)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// RGB bytes of `img` composited over opaque white.
fn flatten_over_white(img: &RgbaImage) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(img.width() as usize * img.height() as usize * 3);
    for p in img.pixels() {
        let [r, g, b, a] = p.0;
        if a == u8::MAX {
            rgb.extend_from_slice(&[r, g, b]);
        } else {
            let a = u32::from(a);
            let over = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
            rgb.extend_from_slice(&[over(r), over(g), over(b)]);
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba};

    fn sample() -> RgbaImage {
        let mut img = RgbaImage::from_pixel(8, 6, Rgba([200, 40, 10, 255]));
        img.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        img
    }

    #[test]
    fn each_format_encodes_as_itself() {
        for (format, expected) in [
            (ExportFormat::Png, ImageFormat::Png),
            (ExportFormat::Jpeg, ImageFormat::Jpeg),
            (ExportFormat::Webp, ImageFormat::WebP),
        ] {
            let bytes = encode(&sample(), format).unwrap();
            assert_eq!(image::guess_format(&bytes).unwrap(), expected, "{format}");
            let back = image::load_from_memory(&bytes).unwrap();
            assert_eq!((back.width(), back.height()), (8, 6), "{format}");
        }
    }

    #[test]
    fn webp_is_lossless_and_keeps_alpha() {
        let img = sample();
        let bytes = encode(&img, ExportFormat::Webp).unwrap();
        let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
        assert_eq!(back.get_pixel(0, 0).0[3], 0);
        assert_eq!(back.get_pixel(3, 3).0, img.get_pixel(3, 3).0);
    }

    #[test]
    fn jpeg_puts_transparent_pixels_on_white() {
        let img = RgbaImage::from_pixel(16, 16, Rgba([0, 0, 0, 0]));
        let bytes = encode(&img, ExportFormat::Jpeg).unwrap();
        let back = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert!(back.pixels().all(|p| p.0.iter().all(|c| *c > 245)));
    }

    #[test]
    fn extensions_round_trip() {
        for f in ExportFormat::ALL {
            assert_eq!(ExportFormat::from_extension(f.extension()), Some(f));
        }
        assert_eq!(
            ExportFormat::from_extension("JPEG"),
            Some(ExportFormat::Jpeg)
        );
        assert_eq!(ExportFormat::from_extension("tiff"), None);
    }

    #[test]
    fn save_writes_the_chosen_format() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("a.jpg");
        save(&sample(), &path, ExportFormat::Jpeg).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(image::guess_format(&bytes).unwrap(), ImageFormat::Jpeg);
    }
}
