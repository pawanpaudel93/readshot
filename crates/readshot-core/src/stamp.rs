//! Stamp annotations (spec §3.4).
//!
//! Symbol stamps are vector marks on a disc in the stamp color, so they
//! are deterministic and pixel-snapshot tested. Emoji stamps prefer a
//! rasterizer the app registers ([`set_emoji_rasterizer`]; CoreText on
//! macOS, so core never reads the large Apple Color Emoji file). Without
//! one, core reads a color-emoji font file (Noto Color Emoji CBDT on
//! Linux; Segoe UI Emoji on Windows gives a tinted silhouette). The five
//! emoji are rasterised once per process and cached. With nothing
//! available the stamp still draws (a disc with `?`), so it stays
//! visible and selectable.

use std::sync::OnceLock;

use ab_glyph::{Font as _, FontRef, FontVec, GlyphImageFormat, PxScale};
use image::RgbaImage;
use tiny_skia::{
    FillRule, LineCap, LineJoin, PathBuilder, Pixmap, Rect as SkRect, Stroke, Transform,
};

use crate::annotation::{Rgba, StampKind};
use crate::geom::PointLike;
use crate::render::{composite_premultiplied, draw_centered_label, solid_paint};

/// Stamps are sized like numbered pins.
pub const STAMP_RADIUS: f32 = 14.0;

/// Rasterizes one emoji into a roughly `px`-square straight-alpha RGBA
/// bitmap. The app registers one at startup (macOS: CoreText).
pub type EmojiRasterizer = fn(&str, u32) -> Option<RgbaImage>;

static RASTERIZER: OnceLock<EmojiRasterizer> = OnceLock::new();

/// Register the platform emoji rasterizer. Call once, before the first
/// stamp renders (the cache is built on first use); later calls are
/// ignored.
pub fn set_emoji_rasterizer(f: EmojiRasterizer) {
    let _ = RASTERIZER.set(f);
}

/// Build the emoji cache now. Call from a background thread so the
/// first emoji stamp doesn't rasterize on the UI thread.
pub fn prewarm_emoji() {
    let _ = emoji_cache();
}

/// Font files tried when no rasterizer is registered (never Apple Color
/// Emoji: macOS uses the CoreText hook). Segoe UI Emoji is COLR (vector
/// layers): it yields no bitmap, so its outline is tinted with the
/// stamp color.
const EMOJI_FONT_CANDIDATES: &[&str] = &[
    "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto/NotoColorEmoji.ttf",
    "/usr/share/fonts/google-noto-emoji/NotoColorEmoji.ttf",
    "/usr/share/fonts/noto-emoji/NotoColorEmoji.ttf",
    "C:\\Windows\\Fonts\\seguiemj.ttf",
];

/// Strike requested from bitmap emoji fonts (scaled down when drawn).
const EMOJI_STRIKE_PX: u16 = 160;

/// `centre` is already in output-pixmap coordinates.
pub(crate) fn draw_stamp(
    pixmap: &mut Pixmap,
    centre: PointLike,
    kind: StampKind,
    color: Rgba,
    font: &FontRef<'static>,
) {
    if kind.emoji().is_some() {
        draw_emoji(pixmap, centre, kind, color, font);
    } else {
        draw_symbol(pixmap, centre, kind, color);
    }
}

fn draw_disc(pixmap: &mut Pixmap, c: PointLike, color: Rgba) {
    let Some(r) = SkRect::from_xywh(
        c.x - STAMP_RADIUS,
        c.y - STAMP_RADIUS,
        STAMP_RADIUS * 2.0,
        STAMP_RADIUS * 2.0,
    ) else {
        return;
    };
    let Some(path) = PathBuilder::from_oval(r) else {
        return;
    };
    pixmap.fill_path(
        &path,
        &solid_paint(color),
        FillRule::Winding,
        Transform::identity(),
        None,
    );
    let outline = Stroke {
        width: 2.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(
        &path,
        &solid_paint(Rgba::OPAQUE_WHITE),
        &outline,
        Transform::identity(),
        None,
    );
}

fn draw_symbol(pixmap: &mut Pixmap, c: PointLike, kind: StampKind, color: Rgba) {
    draw_disc(pixmap, c, color);
    let white = solid_paint(Rgba::OPAQUE_WHITE);
    let mark = Stroke {
        width: 3.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let stroke_poly = |pixmap: &mut Pixmap, pts: &[(f32, f32)]| {
        let mut pb = PathBuilder::new();
        pb.move_to(c.x + pts[0].0, c.y + pts[0].1);
        for &(x, y) in &pts[1..] {
            pb.line_to(c.x + x, c.y + y);
        }
        if let Some(path) = pb.finish() {
            pixmap.stroke_path(&path, &white, &mark, Transform::identity(), None);
        }
    };
    let fill_poly = |pixmap: &mut Pixmap, pts: &[(f32, f32)]| {
        let mut pb = PathBuilder::new();
        pb.move_to(c.x + pts[0].0, c.y + pts[0].1);
        for &(x, y) in &pts[1..] {
            pb.line_to(c.x + x, c.y + y);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            pixmap.fill_path(
                &path,
                &white,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    };
    let fill_dot = |pixmap: &mut Pixmap, (x, y): (f32, f32)| {
        if let Some(path) = PathBuilder::from_circle(c.x + x, c.y + y, 1.9) {
            pixmap.fill_path(
                &path,
                &white,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    };
    match kind {
        StampKind::Check => stroke_poly(pixmap, &[(-5.5, 0.5), (-1.5, 4.5), (6.0, -4.5)]),
        StampKind::Cross => {
            stroke_poly(pixmap, &[(-5.0, -5.0), (5.0, 5.0)]);
            stroke_poly(pixmap, &[(5.0, -5.0), (-5.0, 5.0)]);
        }
        // `!` and `?` are vector marks at the ✓/✕ stroke weight, so they
        // read as boldly as the other symbols (a font glyph came out thin).
        StampKind::Exclaim => {
            stroke_poly(pixmap, &[(0.0, -6.5), (0.0, 1.0)]);
            fill_dot(pixmap, (0.0, 5.5));
        }
        StampKind::Question => {
            let mut pb = PathBuilder::new();
            pb.move_to(c.x - 3.8, c.y - 3.2);
            pb.cubic_to(
                c.x - 3.8,
                c.y - 8.0,
                c.x + 3.8,
                c.y - 8.0,
                c.x + 3.8,
                c.y - 3.6,
            );
            pb.cubic_to(c.x + 3.8, c.y - 1.0, c.x, c.y - 1.0, c.x, c.y + 1.5);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &white, &mark, Transform::identity(), None);
            }
            fill_dot(pixmap, (0.0, 5.5));
        }
        StampKind::Star => fill_poly(pixmap, &STAR_POINTS),
        StampKind::Heart => {
            let mut pb = PathBuilder::new();
            pb.move_to(c.x, c.y + 6.0);
            pb.cubic_to(c.x - 9.0, c.y, c.x - 6.0, c.y - 8.0, c.x, c.y - 3.0);
            pb.cubic_to(c.x + 6.0, c.y - 8.0, c.x + 9.0, c.y, c.x, c.y + 6.0);
            pb.close();
            if let Some(path) = pb.finish() {
                pixmap.fill_path(
                    &path,
                    &white,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
        StampKind::ArrowRight => {
            stroke_poly(pixmap, &[(-6.0, 0.0), (2.0, 0.0)]);
            fill_poly(pixmap, &[(1.0, -5.0), (7.0, 0.0), (1.0, 5.0)]);
        }
        // Emoji kinds never reach here (see draw_stamp).
        StampKind::ThumbsUp
        | StampKind::ThumbsDown
        | StampKind::Fire
        | StampKind::Party
        | StampKind::Eyes => {}
    }
}

/// Five-point star, tip up: vertex `i` sits at angle `-90° + 36°·i` on
/// radius 8.0 (even `i`) or 3.4 (odd `i`). Written as literals rather than
/// computed with `cos`/`sin`, whose last bits vary across platform libms,
/// so the rendered pixels are identical everywhere.
const STAR_POINTS: [(f32, f32); 10] = [
    (0.0, -8.0),
    (1.998_47, -2.750_658),
    (7.608_452, -2.472_136),
    (3.233_592, 1.050_658),
    (4.702_282, 6.472_136),
    (0.0, 3.4),
    (-4.702_282, 6.472_136),
    (-3.233_592, 1.050_658),
    (-7.608_452, -2.472_136),
    (-1.998_47, -2.750_658),
];

struct EmojiBitmap {
    image: RgbaImage,
    /// White coverage to be tinted with the stamp color (outline fallback).
    tint: bool,
}

fn emoji_cache() -> &'static [(StampKind, Option<EmojiBitmap>)] {
    static CACHE: OnceLock<Vec<(StampKind, Option<EmojiBitmap>)>> = OnceLock::new();
    CACHE.get_or_init(|| load_emoji_bitmaps(RASTERIZER.get().copied(), EMOJI_FONT_CANDIDATES))
}

/// One bitmap per emoji kind at a fixed strike (cached per process,
/// so per (emoji, size)). A registered rasterizer wins; otherwise the
/// first readable font file; otherwise `None` (placeholder).
fn load_emoji_bitmaps(
    rasterizer: Option<EmojiRasterizer>,
    font_candidates: &[&str],
) -> Vec<(StampKind, Option<EmojiBitmap>)> {
    let kinds: Vec<StampKind> = StampKind::ALL
        .into_iter()
        .filter(|k| k.emoji().is_some())
        .collect();
    if let Some(raster) = rasterizer {
        return kinds
            .into_iter()
            .map(|k| {
                let bitmap = k
                    .emoji()
                    .and_then(|e| raster(e, EMOJI_STRIKE_PX as u32))
                    .map(|img| EmojiBitmap {
                        image: trim_transparent(img),
                        tint: false,
                    });
                (k, bitmap)
            })
            .collect();
    }
    let font = font_candidates
        .iter()
        .find_map(|path| std::fs::read(path).ok())
        .and_then(|bytes| FontVec::try_from_vec_and_index(bytes, 0).ok());
    let Some(font) = font else {
        tracing::debug!(target: "readshot::render", "no emoji rasterizer or font; emoji stamps draw a placeholder");
        return kinds.into_iter().map(|k| (k, None)).collect();
    };
    // `font` (the whole file) drops when this returns; only the small
    // bitmaps stay resident.
    kinds
        .into_iter()
        .map(|k| {
            let bitmap = rasterise_emoji(&font, k).map(|b| EmojiBitmap {
                image: trim_transparent(b.image),
                tint: b.tint,
            });
            (k, bitmap)
        })
        .collect()
}

/// Crop to the non-transparent pixels so every emoji fills the stamp
/// the same way, whatever padding its source bitmap has.
fn trim_transparent(img: RgbaImage) -> RgbaImage {
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
    }
    if x0 > x1 || y0 > y1 {
        return img;
    }
    image::imageops::crop_imm(&img, x0, y0, x1 - x0 + 1, y1 - y0 + 1).to_image()
}

fn rasterise_emoji(font: &FontVec, kind: StampKind) -> Option<EmojiBitmap> {
    let ch = kind.emoji()?.chars().next()?;
    let id = font.glyph_id(ch);
    if id.0 == 0 {
        return None;
    }
    if let Some(img) = font.glyph_raster_image2(id, EMOJI_STRIKE_PX) {
        let decoded = match img.format {
            GlyphImageFormat::Png => {
                image::load_from_memory_with_format(img.data, image::ImageFormat::Png)
                    .ok()
                    .map(|d| d.to_rgba8())
            }
            GlyphImageFormat::BitmapPremulBgra32 => {
                bgra_premul_to_rgba(img.data, img.width as u32, img.height as u32)
            }
            _ => None,
        };
        if let Some(image) = decoded {
            return Some(EmojiBitmap { image, tint: false });
        }
    }
    let size = STAMP_RADIUS * 2.0;
    let glyph = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(0.0, size));
    let outlined = font.outline_glyph(glyph)?;
    let b = outlined.px_bounds();
    let (w, h) = (
        b.width().ceil().max(1.0) as u32,
        b.height().ceil().max(1.0) as u32,
    );
    let mut image = RgbaImage::new(w, h);
    outlined.draw(|x, y, cov| {
        if x < w && y < h {
            image.put_pixel(
                x,
                y,
                image::Rgba([255, 255, 255, (cov.clamp(0.0, 1.0) * 255.0).round() as u8]),
            );
        }
    });
    Some(EmojiBitmap { image, tint: true })
}

fn bgra_premul_to_rgba(data: &[u8], w: u32, h: u32) -> Option<RgbaImage> {
    if data.len() < (w as usize) * (h as usize) * 4 {
        return None;
    }
    let mut out = RgbaImage::new(w, h);
    for (i, px) in out.pixels_mut().enumerate() {
        let (b, g, r, a) = (
            data[i * 4],
            data[i * 4 + 1],
            data[i * 4 + 2],
            data[i * 4 + 3],
        );
        let un = |c: u8| {
            if a == 0 {
                0
            } else {
                ((c as u16 * 255 + a as u16 / 2) / a as u16).min(255) as u8
            }
        };
        *px = image::Rgba([un(r), un(g), un(b), a]);
    }
    Some(out)
}

fn draw_emoji(
    pixmap: &mut Pixmap,
    c: PointLike,
    kind: StampKind,
    color: Rgba,
    font: &FontRef<'static>,
) {
    let bitmap = emoji_cache()
        .iter()
        .find(|(k, _)| *k == kind)
        .and_then(|(_, b)| b.as_ref());
    let Some(bitmap) = bitmap else {
        draw_disc(pixmap, c, color);
        draw_centered_label(pixmap, c, "?", STAMP_RADIUS * 1.2, [255; 4], font);
        return;
    };
    let side = STAMP_RADIUS * 2.0;
    let (iw, ih) = (bitmap.image.width() as f32, bitmap.image.height() as f32);
    let k = side / iw.max(ih).max(1.0);
    let (tw, th) = (
        (iw * k).round().max(1.0) as u32,
        (ih * k).round().max(1.0) as u32,
    );
    let scaled =
        image::imageops::resize(&bitmap.image, tw, th, image::imageops::FilterType::Triangle);
    let x0 = (c.x - tw as f32 * 0.5).round() as i32;
    let y0 = (c.y - th as f32 * 0.5).round() as i32;
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let tint = [
        (color.r.clamp(0.0, 1.0) * 255.0) as u8,
        (color.g.clamp(0.0, 1.0) * 255.0) as u8,
        (color.b.clamp(0.0, 1.0) * 255.0) as u8,
    ];
    let data = pixmap.data_mut();
    for (x, y, px) in scaled.enumerate_pixels() {
        let (dx, dy) = (x0 + x as i32, y0 + y as i32);
        if dx < 0 || dy < 0 || dx >= pw || dy >= ph {
            continue;
        }
        let src = if bitmap.tint {
            [tint[0], tint[1], tint[2], px[3]]
        } else {
            px.0
        };
        composite_premultiplied(data, (dy * pw + dx) as usize * 4, src, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_FONT: &[&str] = &["/definitely/not/a/font.ttf"];

    /// 160 px canvas with a 20 × 10 red block, padded with transparency.
    fn fake_raster(_emoji: &str, px: u32) -> Option<RgbaImage> {
        let mut img = RgbaImage::new(px, px);
        for y in 10..20 {
            for x in 10..30 {
                img.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        Some(img)
    }

    #[test]
    fn registered_rasterizer_is_preferred_and_trimmed() {
        let cache = load_emoji_bitmaps(Some(fake_raster), NO_FONT);
        assert_eq!(cache.len(), 5);
        for (kind, bitmap) in &cache {
            let b = bitmap
                .as_ref()
                .unwrap_or_else(|| panic!("{kind:?} has no bitmap"));
            assert!(!b.tint);
            assert_eq!(b.image.dimensions(), (20, 10));
            assert_eq!(b.image.get_pixel(0, 0).0, [255, 0, 0, 255]);
        }
    }

    #[test]
    fn no_rasterizer_and_no_font_gives_placeholders() {
        let cache = load_emoji_bitmaps(None, NO_FONT);
        assert_eq!(cache.len(), 5);
        assert!(cache.iter().all(|(_, b)| b.is_none()));
    }

    #[test]
    fn star_points_match_the_star_geometry() {
        for (i, &(x, y)) in STAR_POINTS.iter().enumerate() {
            let angle = (-90.0_f64 + i as f64 * 36.0).to_radians();
            let r = if i % 2 == 0 { 8.0 } else { 3.4 };
            assert!((x as f64 - r * angle.cos()).abs() < 1e-4, "x{i}");
            assert!((y as f64 - r * angle.sin()).abs() < 1e-4, "y{i}");
        }
    }

    #[test]
    fn trimming_a_fully_transparent_image_keeps_it() {
        let img = RgbaImage::new(4, 4);
        assert_eq!(trim_transparent(img).dimensions(), (4, 4));
    }
}
