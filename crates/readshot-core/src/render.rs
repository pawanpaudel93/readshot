//! Annotation renderer — implementation lands in Task 4.
//!
//! Given a base [`image::RgbaImage`] and a slice of [`Annotation`]s, this
//! module produces a flattened RGBA image. Drawing happens on a
//! [`tiny_skia::Pixmap`] for vector primitives plus per-pixel fall-back
//! filters (blur and pixelate). Text is rasterised via [`ab_glyph`] over a
//! bundled [JetBrains Mono](https://github.com/JetBrains/JetBrainsMono)
//! font (SIL OFL 1.1 — see `assets/fonts/OFL.txt`).
//!
//! Output is bit-reproducible across architectures, which is what makes
//! the snapshot tests in `tests/render_snapshot.rs` reliable on every CI
//! runner.

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont as _};
use image::{Rgba as ImgRgba, RgbaImage};
use tiny_skia::{
    FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Rect as SkRect, Shader, Stroke, Transform,
};

use crate::annotation::{Annotation, Rgba};
use crate::filters::{blur_rect, pixelate_rect};
use crate::geom::PointLike;
use crate::{arrowhead, RectLike};

/// Bundled JetBrains Mono Regular font bytes (OFL 1.1).
///
/// Exposed publicly so the iced runtime can register the same font
/// via `iced::daemon::font(...)` for visually consistent text.
pub const FONT_DATA: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// Render `model` over `base` and return the flattened result.
///
/// If the model contains an [`Annotation::Crop`], the output dimensions
/// equal the crop rect (clamped to the base image's bounds) and every
/// other annotation is translated so its coordinates are relative to the
/// crop's top-left. Multiple `Crop` entries: the **last** one wins.
pub fn render(base: &RgbaImage, model: &[Annotation]) -> RgbaImage {
    let (output_w, output_h, offset) = compute_output_geometry(base.width(), base.height(), model);

    // Build the canvas at the output size and copy the (possibly cropped)
    // base into it.
    let mut pixmap = Pixmap::new(output_w, output_h).expect("non-zero canvas");
    blit_base(&mut pixmap, base, offset);

    // Lazy font load — kept inside `render` so callers don't need an init
    // step. `unwrap` is safe: the bundled font is parsed at compile time.
    let font = FontRef::try_from_slice(FONT_DATA).expect("bundled font is valid");

    for ann in model {
        match ann {
            Annotation::Crop { .. } => {
                // Already applied — see compute_output_geometry above.
            }
            Annotation::Rectangle {
                rect,
                color,
                line_width,
            } => draw_rectangle(&mut pixmap, *rect, *color, *line_width, offset),
            Annotation::Ellipse {
                rect,
                color,
                line_width,
            } => draw_ellipse(&mut pixmap, *rect, *color, *line_width, offset),
            Annotation::Line {
                a,
                b,
                color,
                line_width,
            } => draw_polyline(
                &mut pixmap,
                &[*a, *b],
                *color,
                *line_width,
                offset,
                /* round_caps = */ true,
            ),
            Annotation::Arrow {
                a,
                b,
                color,
                line_width,
            } => draw_arrow(&mut pixmap, *a, *b, *color, *line_width, offset),
            Annotation::Pen {
                points,
                color,
                line_width,
            } => draw_polyline(&mut pixmap, points, *color, *line_width, offset, true),
            Annotation::Highlighter {
                points,
                color,
                line_width,
            } => draw_polyline(
                &mut pixmap,
                points,
                *color,
                *line_width,
                offset,
                /* round_caps = */ false,
            ),
            Annotation::Text {
                content,
                origin,
                color,
                size,
                ..
            } => draw_text(&mut pixmap, content, *origin, *color, *size, offset, &font),
            Annotation::Blur { rect, radius } => {
                let (rx, ry, rw, rh) = rect_to_pixels(*rect, offset);
                blur_rect(&mut pixmap, rx, ry, rw, rh, *radius);
            }
            Annotation::Pixelate { rect, block_size } => {
                let (rx, ry, rw, rh) = rect_to_pixels(*rect, offset);
                pixelate_rect(&mut pixmap, rx, ry, rw, rh, *block_size);
            }
            Annotation::NumberedPin {
                origin,
                number,
                color,
            } => draw_numbered_pin(&mut pixmap, *origin, *number, *color, offset, &font),
        }
    }

    pixmap_to_image(&pixmap)
}

// ---------------------------------------------------------------------------
//  Output geometry
// ---------------------------------------------------------------------------

fn compute_output_geometry(
    base_w: u32,
    base_h: u32,
    model: &[Annotation],
) -> (u32, u32, (f32, f32)) {
    // The last Crop wins — same as Photoshop's interpretation of a stack of
    // crop instructions.
    let crop = model
        .iter()
        .rev()
        .find_map(|a| match a {
            Annotation::Crop { rect } => Some(*rect),
            _ => None,
        });

    if let Some(rect) = crop {
        // Clamp to base bounds so a crop that falls partially outside the
        // image still produces a valid pixmap.
        let x0 = rect.x.max(0.0);
        let y0 = rect.y.max(0.0);
        let x1 = (rect.x + rect.width).min(base_w as f32);
        let y1 = (rect.y + rect.height).min(base_h as f32);
        let w = (x1 - x0).max(1.0).round() as u32;
        let h = (y1 - y0).max(1.0).round() as u32;
        (w, h, (-x0, -y0))
    } else {
        (base_w, base_h, (0.0, 0.0))
    }
}

fn blit_base(pixmap: &mut Pixmap, base: &RgbaImage, offset: (f32, f32)) {
    let (ox, oy) = offset;
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let bw = base.width() as i32;
    let bh = base.height() as i32;
    let data = pixmap.data_mut();
    for py in 0..ph {
        for px in 0..pw {
            let bx = px - ox as i32;
            let by = py - oy as i32;
            if bx < 0 || by < 0 || bx >= bw || by >= bh {
                continue;
            }
            let pixel = base.get_pixel(bx as u32, by as u32);
            let [r, g, b, a] = pixel.0;
            // image::RgbaImage is straight (non-premultiplied) RGBA; the
            // pixmap is premultiplied. For typical screen captures alpha is
            // 255 so this is identity, but the math handles transparent
            // bases correctly.
            let pr = ((r as u16 * a as u16 + 127) / 255) as u8;
            let pg = ((g as u16 * a as u16 + 127) / 255) as u8;
            let pb = ((b as u16 * a as u16 + 127) / 255) as u8;
            let i = (py * pw + px) as usize * 4;
            data[i] = pr;
            data[i + 1] = pg;
            data[i + 2] = pb;
            data[i + 3] = a;
        }
    }
}

fn pixmap_to_image(pixmap: &Pixmap) -> RgbaImage {
    let w = pixmap.width();
    let h = pixmap.height();
    let mut img = RgbaImage::new(w, h);
    let data = pixmap.data();
    for (i, p) in img.pixels_mut().enumerate() {
        let pr = data[i * 4];
        let pg = data[i * 4 + 1];
        let pb = data[i * 4 + 2];
        let pa = data[i * 4 + 3];
        let (r, g, b) = if pa == 0 {
            (0, 0, 0)
        } else {
            let a = pa as u16;
            let half = a / 2;
            (
                ((pr as u16 * 255 + half) / a).min(255) as u8,
                ((pg as u16 * 255 + half) / a).min(255) as u8,
                ((pb as u16 * 255 + half) / a).min(255) as u8,
            )
        };
        *p = ImgRgba([r, g, b, pa]);
    }
    img
}

// ---------------------------------------------------------------------------
//  Coordinate helpers
// ---------------------------------------------------------------------------

fn shift_point(p: PointLike, offset: (f32, f32)) -> PointLike {
    PointLike::new(p.x + offset.0, p.y + offset.1)
}

fn shift_rect(r: RectLike, offset: (f32, f32)) -> RectLike {
    RectLike::new(r.x + offset.0, r.y + offset.1, r.width, r.height)
}

fn rect_to_pixels(rect: RectLike, offset: (f32, f32)) -> (i32, i32, i32, i32) {
    let r = shift_rect(rect, offset);
    (
        r.x.round() as i32,
        r.y.round() as i32,
        r.width.round() as i32,
        r.height.round() as i32,
    )
}

// ---------------------------------------------------------------------------
//  Paint helpers
// ---------------------------------------------------------------------------

fn rgba_to_color(c: Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba(
        c.r.clamp(0.0, 1.0),
        c.g.clamp(0.0, 1.0),
        c.b.clamp(0.0, 1.0),
        c.a.clamp(0.0, 1.0),
    )
    .unwrap_or(tiny_skia::Color::BLACK)
}

fn solid_paint(color: Rgba) -> Paint<'static> {
    Paint {
        shader: Shader::SolidColor(rgba_to_color(color)),
        anti_alias: true,
        ..Paint::default()
    }
}

fn stroke_paint(color: Rgba) -> Paint<'static> {
    solid_paint(color)
}

fn make_stroke(line_width: f32, round_caps: bool) -> Stroke {
    Stroke {
        width: line_width.max(0.5),
        line_cap: if round_caps { LineCap::Round } else { LineCap::Butt },
        line_join: LineJoin::Round,
        ..Stroke::default()
    }
}

// ---------------------------------------------------------------------------
//  Per-variant draw fns
// ---------------------------------------------------------------------------

fn draw_rectangle(
    pixmap: &mut Pixmap,
    rect: RectLike,
    color: Rgba,
    line_width: f32,
    offset: (f32, f32),
) {
    let r = shift_rect(rect, offset);
    let Some(sk_rect) = SkRect::from_xywh(r.x, r.y, r.width, r.height) else {
        return;
    };
    let path = PathBuilder::from_rect(sk_rect);
    let paint = stroke_paint(color);
    let stroke = make_stroke(line_width, false);
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

fn draw_ellipse(
    pixmap: &mut Pixmap,
    rect: RectLike,
    color: Rgba,
    line_width: f32,
    offset: (f32, f32),
) {
    let r = shift_rect(rect, offset);
    let Some(sk_rect) = SkRect::from_xywh(r.x, r.y, r.width, r.height) else {
        return;
    };
    let Some(path) = PathBuilder::from_oval(sk_rect) else {
        return;
    };
    let paint = stroke_paint(color);
    let stroke = make_stroke(line_width, false);
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

fn draw_polyline(
    pixmap: &mut Pixmap,
    points: &[PointLike],
    color: Rgba,
    line_width: f32,
    offset: (f32, f32),
    round_caps: bool,
) {
    if points.len() < 2 {
        return;
    }
    let mut pb = PathBuilder::new();
    let first = shift_point(points[0], offset);
    pb.move_to(first.x, first.y);
    for p in &points[1..] {
        let q = shift_point(*p, offset);
        pb.line_to(q.x, q.y);
    }
    let Some(path) = pb.finish() else {
        return;
    };
    let paint = stroke_paint(color);
    let stroke = make_stroke(line_width, round_caps);
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

fn draw_arrow(
    pixmap: &mut Pixmap,
    a: PointLike,
    b: PointLike,
    color: Rgba,
    line_width: f32,
    offset: (f32, f32),
) {
    let head = arrowhead::compute(a, b, line_width);
    if head.is_degenerate() {
        return;
    }
    // Stroke the shaft up to the base of the head (so the head fully
    // encloses the line tip without a visible gap).
    let base_x = (head.left.x + head.right.x) * 0.5;
    let base_y = (head.left.y + head.right.y) * 0.5;
    let shaft_b = PointLike::new(base_x, base_y);
    draw_polyline(pixmap, &[a, shaft_b], color, line_width, offset, true);

    // Fill the head triangle.
    let mut pb = PathBuilder::new();
    let tip = shift_point(head.tip, offset);
    let left = shift_point(head.left, offset);
    let right = shift_point(head.right, offset);
    pb.move_to(tip.x, tip.y);
    pb.line_to(left.x, left.y);
    pb.line_to(right.x, right.y);
    pb.close();
    let Some(path) = pb.finish() else {
        return;
    };
    let paint = solid_paint(color);
    pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
}

fn draw_text(
    pixmap: &mut Pixmap,
    content: &str,
    origin: PointLike,
    color: Rgba,
    size: f32,
    offset: (f32, f32),
    font: &FontRef<'static>,
) {
    let scaled = font.as_scaled(PxScale::from(size));
    let baseline = shift_point(origin, offset);
    let mut x = baseline.x;

    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let data = pixmap.data_mut();
    let color_rgba_u8 = [
        (color.r.clamp(0.0, 1.0) * 255.0) as u8,
        (color.g.clamp(0.0, 1.0) * 255.0) as u8,
        (color.b.clamp(0.0, 1.0) * 255.0) as u8,
        (color.a.clamp(0.0, 1.0) * 255.0) as u8,
    ];

    let mut last_glyph: Option<ab_glyph::GlyphId> = None;
    for c in content.chars() {
        let glyph_id = scaled.glyph_id(c);
        if let Some(prev) = last_glyph {
            x += scaled.kern(prev, glyph_id);
        }
        let glyph = glyph_id.with_scale_and_position(
            PxScale::from(size),
            ab_glyph::point(x, baseline.y),
        );
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, alpha| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= pw || py >= ph {
                    return;
                }
                composite_premultiplied(data, (py * pw + px) as usize * 4, color_rgba_u8, alpha);
            });
        }
        x += scaled.h_advance(glyph_id);
        last_glyph = Some(glyph_id);
    }
}

fn draw_numbered_pin(
    pixmap: &mut Pixmap,
    origin: PointLike,
    number: u32,
    color: Rgba,
    offset: (f32, f32),
    font: &FontRef<'static>,
) {
    const RADIUS: f32 = 14.0;
    let centre = shift_point(origin, offset);

    // Filled circle.
    let Some(circle_rect) = SkRect::from_xywh(
        centre.x - RADIUS,
        centre.y - RADIUS,
        RADIUS * 2.0,
        RADIUS * 2.0,
    ) else {
        return;
    };
    let Some(circle_path) = PathBuilder::from_oval(circle_rect) else {
        return;
    };
    let fill_paint = solid_paint(color);
    pixmap.fill_path(
        &circle_path,
        &fill_paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    // White outline so the pin stays visible on similarly-coloured
    // backgrounds.
    let outline = solid_paint(Rgba::OPAQUE_WHITE);
    let outline_stroke = Stroke {
        width: 2.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(
        &circle_path,
        &outline,
        &outline_stroke,
        Transform::identity(),
        None,
    );

    // Number rendered with the bundled font, centred on the circle.
    let label = number.to_string();
    let size = RADIUS * 1.2; // ≈17 px — fills the circle at default radius.
    let scaled = font.as_scaled(PxScale::from(size));
    let glyph_w: f32 = label
        .chars()
        .map(|c| scaled.h_advance(scaled.glyph_id(c)))
        .sum();
    let baseline_y = centre.y + size * 0.35; // empirical centring offset
    let baseline_x = centre.x - glyph_w * 0.5;
    let mut x = baseline_x;

    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let data = pixmap.data_mut();
    let color_white = [255u8, 255, 255, 255];

    let mut last_glyph: Option<ab_glyph::GlyphId> = None;
    for c in label.chars() {
        let glyph_id = scaled.glyph_id(c);
        if let Some(prev) = last_glyph {
            x += scaled.kern(prev, glyph_id);
        }
        let glyph = glyph_id.with_scale_and_position(PxScale::from(size), ab_glyph::point(x, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, alpha| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= pw || py >= ph {
                    return;
                }
                composite_premultiplied(data, (py * pw + px) as usize * 4, color_white, alpha);
            });
        }
        x += scaled.h_advance(glyph_id);
        last_glyph = Some(glyph_id);
    }
}

// ---------------------------------------------------------------------------
//  Premultiplied blend
// ---------------------------------------------------------------------------

/// Source-over blend `src * src_alpha * coverage` onto `dst[idx..idx + 4]`.
/// Both are premultiplied RGBA; this is the standard porter-duff over.
fn composite_premultiplied(dst: &mut [u8], idx: usize, src: [u8; 4], coverage: f32) {
    let cov = coverage.clamp(0.0, 1.0);
    let src_a = (src[3] as f32 / 255.0) * cov;
    if src_a <= 0.0 {
        return;
    }
    let inv_a = 1.0 - src_a;
    let sr = (src[0] as f32) * cov;
    let sg = (src[1] as f32) * cov;
    let sb = (src[2] as f32) * cov;
    let sa = src_a * 255.0;

    let dr = dst[idx] as f32;
    let dg = dst[idx + 1] as f32;
    let db = dst[idx + 2] as f32;
    let da = dst[idx + 3] as f32;

    dst[idx] = (sr + dr * inv_a).round().clamp(0.0, 255.0) as u8;
    dst[idx + 1] = (sg + dg * inv_a).round().clamp(0.0, 255.0) as u8;
    dst[idx + 2] = (sb + db * inv_a).round().clamp(0.0, 255.0) as u8;
    dst[idx + 3] = (sa + da * inv_a).round().clamp(0.0, 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rgba;

    fn solid_base(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = ImgRgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        img
    }

    #[test]
    fn empty_model_returns_base_unchanged() {
        let base = solid_base(16, 16, [200, 100, 50]);
        let out = render(&base, &[]);
        assert_eq!(base.as_raw(), out.as_raw());
    }

    #[test]
    fn crop_changes_output_dimensions() {
        let base = solid_base(64, 64, [128, 128, 128]);
        let out = render(
            &base,
            &[Annotation::Crop {
                rect: RectLike::new(8.0, 8.0, 16.0, 24.0),
            }],
        );
        assert_eq!(out.width(), 16);
        assert_eq!(out.height(), 24);
    }

    #[test]
    fn rectangle_modifies_image() {
        // A red rectangle stroke must change pixels along its perimeter.
        let base = solid_base(64, 64, [255, 255, 255]);
        let out = render(
            &base,
            &[Annotation::Rectangle {
                rect: RectLike::new(8.0, 8.0, 48.0, 48.0),
                color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                line_width: 2.0,
            }],
        );
        // Sample a pixel on the top edge of the stroke — should have a
        // strong red component now.
        let pixel = out.get_pixel(32, 8);
        assert!(pixel[0] > 200, "expected red dominant, got {pixel:?}");
        assert!(pixel[1] < 100);
        assert!(pixel[2] < 100);
    }
}
