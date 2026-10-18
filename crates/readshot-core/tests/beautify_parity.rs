//! Beautify's built-in presets must reproduce the retired Frame styles
//! byte for byte. `legacy` is a frozen copy of
//! `readshot-app/src/runtime/frame_render.rs` at 43a0c63. Never edit it.

use image::{Rgba, RgbaImage};
use readshot_core::beautify::{render, BuiltinPreset};

#[allow(dead_code)]
mod legacy {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Style {
        None,
        Soft,
        Light,
        Dark,
        Minimal,
        Transparent,
    }

    #[derive(Clone, Copy)]
    pub struct FramedImagePreset {
        pub pad: u32,
        pub radius: u32,
        pub background: [u8; 4],
        pub mat: Option<[u8; 4]>,
        pub border: Option<[u8; 4]>,
        pub shadow: Option<FrameShadow>,
    }

    #[derive(Clone, Copy)]
    pub struct FrameShadow {
        color: [u8; 3],
        offset: i64,
        layers: u32,
        alpha: u8,
    }

    pub fn share_framed_image(img: &image::RgbaImage, style: Style) -> image::RgbaImage {
        if style == Style::None {
            return img.clone();
        }

        let preset = framed_image_preset(style);
        let Some((out_w, out_h)) = framed_output_size(img.width(), img.height(), preset.pad) else {
            return img.clone();
        };
        let mut out = image::RgbaImage::from_pixel(out_w, out_h, image::Rgba(preset.background));

        if let Some(shadow) = preset.shadow {
            for layer in (1..=shadow.layers).rev() {
                let spread = layer * 3;
                let alpha = (shadow.alpha / layer as u8).max(3);
                draw_rounded_rect(
                    &mut out,
                    preset.pad as i64 + shadow.offset - spread as i64,
                    preset.pad as i64 + shadow.offset - spread as i64,
                    img.width().saturating_add(spread * 2),
                    img.height().saturating_add(spread * 2),
                    preset.radius.saturating_add(spread),
                    [shadow.color[0], shadow.color[1], shadow.color[2], alpha],
                );
            }
        }

        if let Some(mat) = preset.mat {
            draw_rounded_rect(
                &mut out,
                preset.pad as i64 - 1,
                preset.pad as i64 - 1,
                img.width().saturating_add(2),
                img.height().saturating_add(2),
                preset.radius + 1,
                mat,
            );
        }
        overlay_rounded_image(&mut out, img, preset.pad, preset.pad, preset.radius);
        if let Some(border) = preset.border {
            draw_rounded_stroke(
                &mut out,
                preset.pad as i64 - 1,
                preset.pad as i64 - 1,
                img.width().saturating_add(2),
                img.height().saturating_add(2),
                preset.radius + 1,
                border,
            );
        }
        out
    }

    pub fn framed_output_size(width: u32, height: u32, pad: u32) -> Option<(u32, u32)> {
        let pad_twice = pad.checked_mul(2)?;
        let out_w = width.checked_add(pad_twice)?.max(1);
        let out_h = height.checked_add(pad_twice)?.max(1);
        Some((out_w, out_h))
    }

    pub fn framed_image_preset(style: Style) -> FramedImagePreset {
        match style {
            Style::None => unreachable!("No Frame returns the source image"),
            Style::Soft => FramedImagePreset {
                pad: 72,
                radius: 18,
                background: [229, 234, 240, 255],
                mat: Some([255, 255, 255, 255]),
                border: Some([148, 163, 184, 180]),
                shadow: Some(FrameShadow {
                    color: [15, 23, 42],
                    offset: 18,
                    layers: 8,
                    alpha: 30,
                }),
            },
            Style::Light => FramedImagePreset {
                pad: 56,
                radius: 14,
                background: [248, 250, 252, 255],
                mat: Some([255, 255, 255, 255]),
                border: Some([203, 213, 225, 220]),
                shadow: Some(FrameShadow {
                    color: [71, 85, 105],
                    offset: 12,
                    layers: 5,
                    alpha: 18,
                }),
            },
            Style::Dark => FramedImagePreset {
                pad: 64,
                radius: 18,
                background: [17, 24, 39, 255],
                mat: Some([31, 41, 55, 255]),
                border: Some([94, 234, 212, 180]),
                shadow: Some(FrameShadow {
                    color: [0, 0, 0],
                    offset: 16,
                    layers: 8,
                    alpha: 42,
                }),
            },
            Style::Minimal => FramedImagePreset {
                pad: 24,
                radius: 8,
                background: [255, 255, 255, 255],
                mat: Some([255, 255, 255, 255]),
                border: Some([203, 213, 225, 255]),
                shadow: None,
            },
            Style::Transparent => FramedImagePreset {
                pad: 32,
                radius: 16,
                background: [0, 0, 0, 0],
                mat: None,
                border: Some([255, 255, 255, 180]),
                shadow: None,
            },
        }
    }

    pub fn draw_rounded_rect(
        img: &mut image::RgbaImage,
        x: i64,
        y: i64,
        w: u32,
        h: u32,
        radius: u32,
        color: [u8; 4],
    ) {
        if w == 0 || h == 0 {
            return;
        }

        let x0 = x.max(0) as u32;
        let y0 = y.max(0) as u32;
        let x1 = (x + w as i64).clamp(0, img.width() as i64) as u32;
        let y1 = (y + h as i64).clamp(0, img.height() as i64) as u32;
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        for py in y0..y1 {
            for px in x0..x1 {
                if point_in_rounded_rect(px as i64, py as i64, x, y, w, h, radius) {
                    blend_pixel(img, px, py, color);
                }
            }
        }
    }

    pub fn draw_rounded_stroke(
        img: &mut image::RgbaImage,
        x: i64,
        y: i64,
        w: u32,
        h: u32,
        radius: u32,
        color: [u8; 4],
    ) {
        if w <= 2 || h <= 2 {
            draw_rounded_rect(img, x, y, w, h, radius, color);
            return;
        }

        for py in y..y + h as i64 {
            for px in x..x + w as i64 {
                if px < 0 || py < 0 || px >= img.width() as i64 || py >= img.height() as i64 {
                    continue;
                }
                let outer = point_in_rounded_rect(px, py, x, y, w, h, radius);
                let inner = point_in_rounded_rect(
                    px,
                    py,
                    x + 1,
                    y + 1,
                    w.saturating_sub(2),
                    h.saturating_sub(2),
                    radius.saturating_sub(1),
                );
                if outer && !inner {
                    blend_pixel(img, px as u32, py as u32, color);
                }
            }
        }
    }

    pub fn overlay_rounded_image(
        out: &mut image::RgbaImage,
        src: &image::RgbaImage,
        x: u32,
        y: u32,
        radius: u32,
    ) {
        for sy in 0..src.height() {
            for sx in 0..src.width() {
                let dx = x.saturating_add(sx);
                let dy = y.saturating_add(sy);
                if dx >= out.width() || dy >= out.height() {
                    continue;
                }
                if point_in_rounded_rect(
                    dx as i64,
                    dy as i64,
                    x as i64,
                    y as i64,
                    src.width(),
                    src.height(),
                    radius,
                ) {
                    out.put_pixel(dx, dy, *src.get_pixel(sx, sy));
                }
            }
        }
    }

    pub fn point_in_rounded_rect(
        px: i64,
        py: i64,
        x: i64,
        y: i64,
        w: u32,
        h: u32,
        radius: u32,
    ) -> bool {
        if w == 0 || h == 0 {
            return false;
        }

        let right = x + w as i64 - 1;
        let bottom = y + h as i64 - 1;
        if px < x || py < y || px > right || py > bottom {
            return false;
        }

        let r = radius.min(w / 2).min(h / 2) as i64;
        if r <= 0 {
            return true;
        }

        let cx = if px < x + r {
            x + r
        } else if px > right - r {
            right - r
        } else {
            px
        };
        let cy = if py < y + r {
            y + r
        } else if py > bottom - r {
            bottom - r
        } else {
            py
        };

        let dx = px - cx;
        let dy = py - cy;
        dx * dx + dy * dy <= r * r
    }

    pub fn blend_pixel(img: &mut image::RgbaImage, x: u32, y: u32, src: [u8; 4]) {
        let dst = img.get_pixel_mut(x, y);
        let src_alpha = src[3] as f32 / 255.0;
        let dst_alpha = dst[3] as f32 / 255.0;
        let out_alpha = src_alpha + dst_alpha * (1.0 - src_alpha);
        if out_alpha <= f32::EPSILON {
            dst.0 = [0, 0, 0, 0];
            return;
        }
        let dst_weight = dst_alpha * (1.0 - src_alpha);
        dst.0 = [
            ((src[0] as f32 * src_alpha + dst[0] as f32 * dst_weight) / out_alpha).round() as u8,
            ((src[1] as f32 * src_alpha + dst[1] as f32 * dst_weight) / out_alpha).round() as u8,
            ((src[2] as f32 * src_alpha + dst[2] as f32 * dst_weight) / out_alpha).round() as u8,
            (out_alpha * 255.0).round() as u8,
        ];
    }
}

fn fixtures() -> Vec<RgbaImage> {
    // Opaque, odd-sized with partial alpha, a 1×1 and a 0×0.
    let opaque = RgbaImage::from_pixel(64, 64, Rgba([200, 10, 20, 255]));
    let mut ramp = RgbaImage::new(37, 23);
    for (x, y, px) in ramp.enumerate_pixels_mut() {
        *px = Rgba([
            (x * 7) as u8,
            (y * 11) as u8,
            ((x + y) * 3) as u8,
            (40 + x * 5) as u8,
        ]);
    }
    let dot = RgbaImage::from_pixel(1, 1, Rgba([1, 2, 3, 255]));
    vec![opaque, ramp, dot, RgbaImage::new(0, 0)]
}

fn pairs() -> [(BuiltinPreset, legacy::Style); 6] {
    [
        (BuiltinPreset::None, legacy::Style::None),
        (BuiltinPreset::Soft, legacy::Style::Soft),
        (BuiltinPreset::Light, legacy::Style::Light),
        (BuiltinPreset::Dark, legacy::Style::Dark),
        (BuiltinPreset::Minimal, legacy::Style::Minimal),
        (BuiltinPreset::Transparent, legacy::Style::Transparent),
    ]
}

#[test]
fn builtin_presets_match_the_retired_frame_styles_byte_for_byte() {
    for img in fixtures() {
        for (preset, style) in pairs() {
            let new = render(&img, &preset.settings()).unwrap();
            let old = legacy::share_framed_image(&img, style);
            assert_eq!(
                (new.width(), new.height()),
                (old.width(), old.height()),
                "{preset:?} size on {}×{}",
                img.width(),
                img.height()
            );
            assert!(
                new.as_raw() == old.as_raw(),
                "{preset:?} pixels differ on {}×{}",
                img.width(),
                img.height()
            );
        }
    }
}

#[test]
fn no_frame_is_the_source_image() {
    for img in fixtures() {
        let out = render(&img, &BuiltinPreset::None.settings()).unwrap();
        assert_eq!(out.as_raw(), img.as_raw());
        assert_eq!(out.dimensions(), img.dimensions());
    }
}
