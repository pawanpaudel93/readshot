//! PNG encoding helpers shared by GUI, CLI, MCP, OCR, and history.
//!
//! Screenshots are usually fully opaque. Encoding those bytes as RGB
//! instead of RGBA keeps output lossless while avoiding a redundant
//! alpha channel, and `CompressionType::Best` trades a little CPU for
//! distribution-quality file size.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ColorType, ImageEncoder, ImageResult, RgbaImage};

pub fn encode(img: &RgbaImage) -> ImageResult<Vec<u8>> {
    encode_with(img, CompressionType::Best)
}

/// Fast-compression encode for in-memory handoffs (e.g. feeding the
/// bytes straight into Apple Vision) where the PNG is decoded
/// immediately and never hits disk: compression ratio is irrelevant
/// there, while the encode CPU sits directly on hotkey latency.
pub fn encode_fast(img: &RgbaImage) -> ImageResult<Vec<u8>> {
    encode_with(img, CompressionType::Fast)
}

fn encode_with(img: &RgbaImage, compression: CompressionType) -> ImageResult<Vec<u8>> {
    let opaque = is_opaque(img);
    let bytes_per_pixel = if opaque { 3 } else { 4 };
    let mut out =
        Vec::with_capacity(img.width() as usize * img.height() as usize * bytes_per_pixel);
    write_with(img, &mut out, compression, opaque)?;
    Ok(out)
}

pub fn write<W: Write>(img: &RgbaImage, writer: W) -> ImageResult<()> {
    write_with(img, writer, CompressionType::Best, is_opaque(img))
}

fn write_with<W: Write>(
    img: &RgbaImage,
    writer: W,
    compression: CompressionType,
    opaque: bool,
) -> ImageResult<()> {
    let encoder = PngEncoder::new_with_quality(writer, compression, FilterType::Adaptive);
    if opaque {
        let mut rgb = Vec::with_capacity(img.width() as usize * img.height() as usize * 3);
        for pixel in img.pixels() {
            rgb.extend_from_slice(&pixel.0[..3]);
        }
        encoder.write_image(&rgb, img.width(), img.height(), ColorType::Rgb8.into())
    } else {
        encoder.write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ColorType::Rgba8.into(),
        )
    }
}

pub fn save(img: &RgbaImage, path: &Path) -> ImageResult<()> {
    let file = File::create(path)?;
    write(img, BufWriter::new(file))
}

fn is_opaque(img: &RgbaImage) -> bool {
    img.pixels().all(|pixel| pixel.0[3] == u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_color_type(bytes: &[u8]) -> u8 {
        bytes[25]
    }

    #[test]
    fn opaque_screenshots_encode_as_rgb_png() {
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([20, 30, 40, 255]));

        let png = encode(&img).unwrap();

        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(png_color_type(&png), 2);
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8(), img);
    }

    #[test]
    fn transparent_images_keep_rgba_png() {
        let img = RgbaImage::from_pixel(4, 4, image::Rgba([20, 30, 40, 128]));

        let png = encode(&img).unwrap();

        assert_eq!(png_color_type(&png), 6);
        assert_eq!(image::load_from_memory(&png).unwrap().to_rgba8(), img);
    }

    #[test]
    fn opaque_png_is_smaller_than_default_rgba_encoder_for_ui_content() {
        let img = RgbaImage::from_fn(256, 128, |x, y| {
            let stripe = if (x / 16 + y / 16) % 2 == 0 { 220 } else { 245 };
            image::Rgba([stripe, stripe, 255, 255])
        });
        let mut default = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut default),
            image::ImageFormat::Png,
        )
        .unwrap();

        let optimised = encode(&img).unwrap();

        assert!(optimised.len() < default.len());
    }
}
