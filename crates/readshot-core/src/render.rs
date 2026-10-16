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

use ab_glyph::{Font as _, FontRef, PxScale, PxScaleFont, ScaleFont as _};
use image::{Rgba as ImgRgba, RgbaImage};
use tiny_skia::{
    FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, Rect as SkRect, Shader, Stroke,
    StrokeDash, Transform,
};

use crate::annotation::{
    Annotation, ArrowHeads, FillMode, LineStyle, Rgba, ShapeStyle, SpotlightShape, TextStyle,
};
use crate::arrowhead::ArrowheadTriangle;
use crate::filters::{blur_rect, pixelate_rect};
use crate::geom::PointLike;
use crate::{arrowhead, line_geom, RectLike};

/// Bundled JetBrains Mono Regular font bytes (OFL 1.1).
///
/// Exposed publicly so the iced runtime can register the same font
/// via `iced::daemon::font(...)` for visually consistent text.
pub const FONT_DATA: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

/// Family name of the bundled annotation/code font.
pub const FONT_FAMILY: &str = "JetBrains Mono";

/// Fill opacity for `FillMode::Both` (translucent fill + outline). The
/// editor's preview uses the same value.
pub const BOTH_FILL_ALPHA: f32 = 0.2;

/// Pill padding around text, as a fraction of the font size. The
/// editor's text bounds use it so the selection box matches the pill.
pub const TEXT_PILL_PAD: f32 = 0.35;

/// Black or white ink, whichever has the higher WCAG contrast on `fill`.
/// A translucent fill is composited over white first (the usual page
/// colour), then the sRGB channels are linearised.
pub fn contrast_ink(fill: Rgba) -> Rgba {
    let a = fill.a.clamp(0.0, 1.0);
    let lin = |c: f32| {
        let c = c.clamp(0.0, 1.0) * a + (1.0 - a);
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * lin(fill.r) + 0.7152 * lin(fill.g) + 0.0722 * lin(fill.b);
    // Black wins when (L + 0.05) / 0.05 > 1.05 / (L + 0.05), i.e. L > ~0.179.
    if luminance > 0.179 {
        Rgba::OPAQUE_BLACK
    } else {
        Rgba::OPAQUE_WHITE
    }
}

/// Render `model` over `base` and return the flattened result.
///
/// If the model contains an [`Annotation::Crop`], the output dimensions
/// equal the crop rect (clamped to the base image's bounds) and every
/// other annotation is translated so its coordinates are relative to the
/// crop's top-left. Multiple `Crop` entries: the **last** one wins.
pub fn render(base: &RgbaImage, model: &[Annotation]) -> RgbaImage {
    let (output_w, output_h, offset) = compute_output_geometry(base.width(), base.height(), model);

    // Build the canvas at the output size and copy the (possibly cropped)
    // base into it. `Pixmap::new` rejects zero dimensions and very large
    // sizes (~ Skia's 32767 limit); fall back to a 1×1 pixmap so a
    // degenerate input cannot panic the renderer.
    let mut pixmap = Pixmap::new(output_w, output_h)
        .unwrap_or_else(|| Pixmap::new(1, 1).expect("1x1 pixmap is always allocatable"));
    blit_base(&mut pixmap, base, offset);
    apply_spotlights(&mut pixmap, model, offset);

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
                style,
            } => draw_rectangle(&mut pixmap, *rect, *color, *line_width, *style, offset),
            Annotation::Ellipse {
                rect,
                color,
                line_width,
                style,
            } => draw_ellipse(&mut pixmap, *rect, *color, *line_width, *style, offset),
            Annotation::Line {
                a,
                b,
                color,
                line_width,
                style,
            } => draw_line(
                &mut pixmap,
                *a,
                *b,
                *color,
                *line_width,
                *style,
                offset,
                false,
            ),
            Annotation::Arrow {
                a,
                b,
                color,
                line_width,
                style,
            } => draw_line(
                &mut pixmap,
                *a,
                *b,
                *color,
                *line_width,
                *style,
                offset,
                true,
            ),
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
                style,
                ..
            } => draw_text(
                &mut pixmap,
                content,
                *origin,
                *color,
                *size,
                *style,
                offset,
                &font,
            ),
            Annotation::Blur { rect, radius } => {
                let (rx, ry, rw, rh) = rect_to_pixels(*rect, offset);
                blur_rect(&mut pixmap, rx, ry, rw, rh, *radius);
            }
            Annotation::Pixelate { rect, block_size } => {
                let (rx, ry, rw, rh) = rect_to_pixels(*rect, offset);
                pixelate_rect(&mut pixmap, rx, ry, rw, rh, *block_size);
            }
            Annotation::Spotlight { .. } => { /* dimmed before the loop — see apply_spotlights */
            }
            Annotation::NumberedPin {
                origin,
                number,
                color,
            } => draw_numbered_pin(&mut pixmap, *origin, *number, *color, offset, &font),
            Annotation::Stamp {
                origin,
                stamp,
                color,
            } => crate::stamp::draw_stamp(
                &mut pixmap,
                shift_point(*origin, offset),
                *stamp,
                *color,
                &font,
            ),
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
    let crop = model.iter().rev().find_map(|a| match a {
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
    let ox = offset.0 as i32;
    let oy = offset.1 as i32;
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let bw = base.width() as i32;
    let bh = base.height() as i32;
    let data = pixmap.data_mut();

    // Pixmap rows that overlap the (offset) base image.
    let px0 = ox.max(0);
    let px1 = (ox + bw).min(pw);
    let py0 = oy.max(0);
    let py1 = (oy + bh).min(ph);
    if px1 <= px0 || py1 <= py0 {
        return;
    }

    // Screenshots are almost always fully opaque, and at alpha 255 the
    // straight→premultiplied conversion is the identity — each
    // overlapping row is then a plain memcpy instead of per-pixel
    // multiply/divide over the whole frame.
    let opaque = base.as_raw().chunks_exact(4).all(|p| p[3] == u8::MAX);
    if opaque {
        let row_len = ((px1 - px0) as usize) * 4;
        let src_data = base.as_raw();
        for py in py0..py1 {
            let by = py - oy;
            let src_start = ((by * bw) + (px0 - ox)) as usize * 4;
            let dst_start = (py * pw + px0) as usize * 4;
            data[dst_start..dst_start + row_len]
                .copy_from_slice(&src_data[src_start..src_start + row_len]);
        }
        return;
    }

    for py in py0..py1 {
        for px in px0..px1 {
            let bx = px - ox;
            let by = py - oy;
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
    let data = pixmap.data();
    // Fully-opaque output (the common screenshot case): premultiplied
    // and straight RGBA are byte-identical, so the conversion is one
    // buffer copy.
    if data.chunks_exact(4).all(|p| p[3] == u8::MAX) {
        return RgbaImage::from_raw(w, h, data.to_vec())
            .expect("pixmap buffer matches its own dimensions");
    }
    let mut img = RgbaImage::new(w, h);
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

/// Dim everything outside the union of all spotlights, once, before any
/// other annotation draws — so arrows and text on top stay bright. The
/// union uses the largest `dim`.
fn apply_spotlights(pixmap: &mut Pixmap, model: &[Annotation], offset: (f32, f32)) {
    let spots: Vec<(RectLike, SpotlightShape, f32)> = model
        .iter()
        .filter_map(|a| match a {
            Annotation::Spotlight { rect, shape, dim } => Some((*rect, *shape, *dim)),
            _ => None,
        })
        .collect();
    if spots.is_empty() {
        return;
    }
    let dim = spots
        .iter()
        .map(|s| s.2)
        .fold(0.0_f32, f32::max)
        .clamp(0.0, 0.95);
    let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) else {
        return;
    };
    for (rect, shape, _) in &spots {
        let r = shift_rect(*rect, offset);
        let Some(sk) = SkRect::from_xywh(r.x, r.y, r.width, r.height) else {
            continue;
        };
        let path = match shape {
            SpotlightShape::Rectangle => PathBuilder::from_rect(sk),
            SpotlightShape::Ellipse => match PathBuilder::from_oval(sk) {
                Some(p) => p,
                None => continue,
            },
        };
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    }
    let coverage = mask.data().to_vec();
    let data = pixmap.data_mut();
    for (i, cov) in coverage.iter().enumerate() {
        let keep = 1.0 - dim * (1.0 - *cov as f32 / 255.0);
        if keep >= 1.0 {
            continue;
        }
        // Premultiplied: scaling RGB at fixed alpha darkens toward black.
        for c in &mut data[i * 4..i * 4 + 3] {
            *c = (*c as f32 * keep).round() as u8;
        }
    }
}

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

pub(crate) fn solid_paint(color: Rgba) -> Paint<'static> {
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
        line_cap: if round_caps {
            LineCap::Round
        } else {
            LineCap::Butt
        },
        line_join: LineJoin::Round,
        ..Stroke::default()
    }
}

// ---------------------------------------------------------------------------
//  Per-variant draw fns
// ---------------------------------------------------------------------------

/// Rounded rectangle path; radius 0 is exactly `PathBuilder::from_rect`
/// so unstyled rectangles render byte-identically to before.
pub(crate) fn rounded_rect_path(r: SkRect, radius: f32) -> Option<tiny_skia::Path> {
    let rad = radius.min(r.width() * 0.5).min(r.height() * 0.5).max(0.0);
    if rad <= 0.0 {
        return Some(PathBuilder::from_rect(r));
    }
    const K: f32 = 0.552_284_8;
    let (l, t, rt, b) = (r.left(), r.top(), r.right(), r.bottom());
    let c = rad * K;
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    pb.cubic_to(rt - rad + c, t, rt, t + rad - c, rt, t + rad);
    pb.line_to(rt, b - rad);
    pb.cubic_to(rt, b - rad + c, rt - rad + c, b, rt - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + rad - c, b, l, b - rad + c, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + rad - c, l + rad - c, t, l + rad, t);
    pb.close();
    pb.finish()
}

fn dashed_stroke(line_width: f32, round_caps: bool, dashed: bool) -> Stroke {
    let mut stroke = make_stroke(line_width, round_caps);
    if dashed {
        stroke.dash = StrokeDash::new(line_geom::dash_pattern(line_width).to_vec(), 0.0);
    }
    stroke
}

fn fill_and_stroke(
    pixmap: &mut Pixmap,
    path: &tiny_skia::Path,
    color: Rgba,
    line_width: f32,
    style: ShapeStyle,
) {
    let fill = match style.fill {
        FillMode::Outline => None,
        FillMode::Filled => Some(color),
        FillMode::Both => Some(Rgba::new(
            color.r,
            color.g,
            color.b,
            color.a * BOTH_FILL_ALPHA,
        )),
    };
    if let Some(fill) = fill {
        pixmap.fill_path(
            path,
            &solid_paint(fill),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    if style.fill != FillMode::Filled {
        let stroke = dashed_stroke(line_width, false, style.dashed);
        pixmap.stroke_path(
            path,
            &stroke_paint(color),
            &stroke,
            Transform::identity(),
            None,
        );
    }
}

fn draw_rectangle(
    pixmap: &mut Pixmap,
    rect: RectLike,
    color: Rgba,
    line_width: f32,
    style: ShapeStyle,
    offset: (f32, f32),
) {
    let r = shift_rect(rect, offset);
    let Some(sk_rect) = SkRect::from_xywh(r.x, r.y, r.width, r.height) else {
        return;
    };
    let Some(path) = rounded_rect_path(sk_rect, style.corner_radius) else {
        return;
    };
    fill_and_stroke(pixmap, &path, color, line_width, style);
}

fn draw_ellipse(
    pixmap: &mut Pixmap,
    rect: RectLike,
    color: Rgba,
    line_width: f32,
    style: ShapeStyle,
    offset: (f32, f32),
) {
    let r = shift_rect(rect, offset);
    let Some(sk_rect) = SkRect::from_xywh(r.x, r.y, r.width, r.height) else {
        return;
    };
    let Some(path) = PathBuilder::from_oval(sk_rect) else {
        return;
    };
    fill_and_stroke(pixmap, &path, color, line_width, style);
}

fn draw_polyline(
    pixmap: &mut Pixmap,
    points: &[PointLike],
    color: Rgba,
    line_width: f32,
    offset: (f32, f32),
    round_caps: bool,
) {
    draw_polyline_styled(pixmap, points, color, line_width, offset, round_caps, false);
}

fn draw_polyline_styled(
    pixmap: &mut Pixmap,
    points: &[PointLike],
    color: Rgba,
    line_width: f32,
    offset: (f32, f32),
    round_caps: bool,
    dashed: bool,
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
    let stroke = dashed_stroke(line_width, round_caps, dashed);
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// Line or arrow along its styled path. Arrow shafts stop at each
/// head's base so the filled head fully covers the tip.
#[allow(clippy::too_many_arguments)]
fn draw_line(
    pixmap: &mut Pixmap,
    a: PointLike,
    b: PointLike,
    color: Rgba,
    line_width: f32,
    style: LineStyle,
    offset: (f32, f32),
    arrow: bool,
) {
    let mut shaft = line_geom::path_points(a, b, style.path, line_width);
    if !arrow {
        draw_polyline_styled(
            pixmap,
            &shaft,
            color,
            line_width,
            offset,
            true,
            style.dashed,
        );
        return;
    }
    let (from, to) = line_geom::end_tangent(a, b, style.path);
    let end_head = arrowhead::compute(from, to, line_width);
    if end_head.is_degenerate() {
        return;
    }
    trim_shaft(&mut shaft, end_head, true);
    let start_head = if style.heads == ArrowHeads::Both {
        let (from, to) = line_geom::start_tangent(a, b, style.path);
        Some(arrowhead::compute(from, to, line_width)).filter(|h| !h.is_degenerate())
    } else {
        None
    };
    if let Some(head) = start_head {
        trim_shaft(&mut shaft, head, false);
    }
    draw_polyline_styled(
        pixmap,
        &shaft,
        color,
        line_width,
        offset,
        true,
        style.dashed,
    );
    fill_arrowhead(pixmap, end_head, color, offset);
    if let Some(head) = start_head {
        fill_arrowhead(pixmap, head, color, offset);
    }
}

/// Replace the tip end of `points` with the head's base. `at_end`
/// trims the last point (tip `b`), otherwise the first (tip `a`). For a
/// straight two-point shaft this yields exactly `[a, base]`, matching
/// the pre-style renderer byte for byte.
fn trim_shaft(points: &mut Vec<PointLike>, head: ArrowheadTriangle, at_end: bool) {
    let base = PointLike::new(
        (head.left.x + head.right.x) * 0.5,
        (head.left.y + head.right.y) * 0.5,
    );
    let reach = {
        let (dx, dy) = (head.tip.x - base.x, head.tip.y - base.y);
        (dx * dx + dy * dy).sqrt()
    };
    if !at_end {
        points.reverse();
    }
    points.pop();
    while points.len() > 1 {
        let last = points[points.len() - 1];
        let (dx, dy) = (last.x - head.tip.x, last.y - head.tip.y);
        if (dx * dx + dy * dy).sqrt() < reach {
            points.pop();
        } else {
            break;
        }
    }
    points.push(base);
    if !at_end {
        points.reverse();
    }
}

fn fill_arrowhead(pixmap: &mut Pixmap, head: ArrowheadTriangle, color: Rgba, offset: (f32, f32)) {
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
    pixmap.fill_path(
        &path,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );
}

fn line_advance(scaled: &PxScaleFont<&FontRef<'_>>, line: &str) -> f32 {
    let mut x = 0.0;
    let mut last = None;
    for c in line.chars() {
        let id = scaled.glyph_id(c);
        if let Some(prev) = last {
            x += scaled.kern(prev, id);
        }
        x += scaled.h_advance(id);
        last = Some(id);
    }
    x
}

/// Pill background for pill text, in image coordinates, measured with
/// the bundled font. `render` fills it, and the editor uses it as the
/// text's selection bounds, so the selection box matches the export.
/// `None` for a non-finite or non-positive size.
pub fn text_pill_rect(content: &str, size: f32, origin: PointLike) -> Option<RectLike> {
    if !size.is_finite() || size <= 0.0 {
        return None;
    }
    let font = FontRef::try_from_slice(FONT_DATA).expect("bundled font is valid");
    let scaled = font.as_scaled(PxScale::from(size));
    let normalised = content.replace("\r\n", "\n").replace('\r', "\n");
    let line_height = scaled.height() + scaled.line_gap();
    let widest = normalised
        .split('\n')
        .map(|line| line_advance(&scaled, line))
        .fold(0.0_f32, f32::max);
    let lines = normalised.split('\n').count().max(1) as f32;
    let pad = size * TEXT_PILL_PAD;
    let height = scaled.ascent() - scaled.descent() + (lines - 1.0) * line_height + pad * 2.0;
    Some(RectLike::new(
        origin.x - pad,
        origin.y - scaled.ascent() - pad,
        (widest + pad * 2.0).max(1.0),
        height.max(1.0),
    ))
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    pixmap: &mut Pixmap,
    content: &str,
    origin: PointLike,
    color: Rgba,
    size: f32,
    style: TextStyle,
    offset: (f32, f32),
    font: &FontRef<'static>,
) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    // Clamp the glyph scale before it reaches ab_glyph. `size` can come
    // from a corrupt/hand-edited history sidecar with no upper bound, and
    // ab_glyph allocates a per-glyph coverage bitmap ~size×size — a huge
    // value overflows the raster dimensions and aborts the process (an
    // allocator abort can't even be caught). A glyph larger than the
    // canvas can't display anyway, so cap to the pixmap; non-finite sizes
    // can't render at all, so skip. Legit editor sizes (≤96px) are far
    // below the cap and unaffected.
    let size = if size.is_finite() {
        size.clamp(1.0, pw.max(ph).max(1) as f32)
    } else {
        return;
    };
    let scaled = font.as_scaled(PxScale::from(size));
    let baseline = shift_point(origin, offset);
    // Multi-line text: `content` may contain `\n`. `origin` is the
    // baseline of the first line; each subsequent line drops by the
    // font's line height. Normalise CRLF/CR so a Windows-authored
    // sidecar renders the same as one written on macOS/Linux.
    let line_height = scaled.height() + scaled.line_gap();
    let normalised = content.replace("\r\n", "\n").replace('\r', "\n");

    let mut ink = color;
    if style.pill {
        // Same rect the editor uses for selection bounds (text_pill_rect),
        // shifted into the output pixmap.
        if let Some(r) = text_pill_rect(content, size, origin) {
            let r = shift_rect(r, offset);
            if let Some(rect) = SkRect::from_xywh(r.x, r.y, r.width, r.height) {
                if let Some(path) = rounded_rect_path(rect, (r.height * 0.5).min(size * 0.8)) {
                    pixmap.fill_path(
                        &path,
                        &solid_paint(color),
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                }
            }
        }
        ink = contrast_ink(color);
    }
    // Synthetic bold: a second pass nudged right. Only JetBrains Mono
    // Regular is bundled.
    let passes = if style.bold {
        vec![0.0, (size / 24.0).max(0.6)]
    } else {
        vec![0.0]
    };

    let data = pixmap.data_mut();
    let color_rgba_u8 = [
        (ink.r.clamp(0.0, 1.0) * 255.0) as u8,
        (ink.g.clamp(0.0, 1.0) * 255.0) as u8,
        (ink.b.clamp(0.0, 1.0) * 255.0) as u8,
        (ink.a.clamp(0.0, 1.0) * 255.0) as u8,
    ];

    for (line_idx, line) in normalised.split('\n').enumerate() {
        let line_baseline_y = baseline.y + line_idx as f32 * line_height;
        for &dx in &passes {
            let mut x = baseline.x + dx;
            let mut last_glyph: Option<ab_glyph::GlyphId> = None;
            for c in line.chars() {
                let glyph_id = scaled.glyph_id(c);
                if let Some(prev) = last_glyph {
                    x += scaled.kern(prev, glyph_id);
                }
                let glyph = glyph_id.with_scale_and_position(
                    PxScale::from(size),
                    ab_glyph::point(x, line_baseline_y),
                );
                if let Some(outlined) = font.outline_glyph(glyph) {
                    let bounds = outlined.px_bounds();
                    outlined.draw(|gx, gy, alpha| {
                        let px = bounds.min.x as i32 + gx as i32;
                        let py = bounds.min.y as i32 + gy as i32;
                        if px < 0 || py < 0 || px >= pw || py >= ph {
                            return;
                        }
                        composite_premultiplied(
                            data,
                            (py * pw + px) as usize * 4,
                            color_rgba_u8,
                            alpha,
                        );
                    });
                }
                x += scaled.h_advance(glyph_id);
                last_glyph = Some(glyph_id);
            }
        }
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
    // ≈17 px — fills the circle at default radius.
    draw_centered_label(
        pixmap,
        centre,
        &number.to_string(),
        RADIUS * 1.2,
        [255, 255, 255, 255],
        font,
    );
}

/// Draw `label` centred on `centre` with the bundled font. Shared by
/// numbered pins and the `!` / `?` stamps.
pub(crate) fn draw_centered_label(
    pixmap: &mut Pixmap,
    centre: PointLike,
    label: &str,
    size: f32,
    color: [u8; 4],
    font: &FontRef<'static>,
) {
    let scaled = font.as_scaled(PxScale::from(size));
    let glyph_w: f32 = label
        .chars()
        .map(|c| scaled.h_advance(scaled.glyph_id(c)))
        .sum();
    let baseline_y = centre.y + size * 0.35; // empirical centring offset
    let mut x = centre.x - glyph_w * 0.5;
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let data = pixmap.data_mut();
    let mut last_glyph: Option<ab_glyph::GlyphId> = None;
    for c in label.chars() {
        let glyph_id = scaled.glyph_id(c);
        if let Some(prev) = last_glyph {
            x += scaled.kern(prev, glyph_id);
        }
        let glyph =
            glyph_id.with_scale_and_position(PxScale::from(size), ab_glyph::point(x, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, alpha| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= pw || py >= ph {
                    return;
                }
                composite_premultiplied(data, (py * pw + px) as usize * 4, color, alpha);
            });
        }
        x += scaled.h_advance(glyph_id);
        last_glyph = Some(glyph_id);
    }
}

// ---------------------------------------------------------------------------
//  Premultiplied blend
// ---------------------------------------------------------------------------

/// Source-over blend straight-RGBA `src` through glyph `coverage` onto
/// `dst[idx..idx + 4]`, whose tiny-skia backing store is premultiplied.
pub(crate) fn composite_premultiplied(dst: &mut [u8], idx: usize, src: [u8; 4], coverage: f32) {
    let cov = coverage.clamp(0.0, 1.0);
    let src_a = (src[3] as f32 / 255.0) * cov;
    if src_a <= 0.0 {
        return;
    }
    let inv_a = 1.0 - src_a;
    let sr = (src[0] as f32) * src_a;
    let sg = (src[1] as f32) * src_a;
    let sb = (src[2] as f32) * src_a;
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
    use crate::{
        ArrowHeads, FillMode, LinePath, LineStyle, ShapeStyle, SpotlightShape, StampKind, TextStyle,
    };

    fn solid_base(w: u32, h: u32, rgb: [u8; 3]) -> RgbaImage {
        let mut img = RgbaImage::new(w, h);
        for px in img.pixels_mut() {
            *px = ImgRgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        img
    }

    #[test]
    fn text_blend_premultiplies_source_alpha() {
        let mut dst = [0u8, 0, 0, 0];
        composite_premultiplied(&mut dst, 0, [200, 100, 50, 128], 1.0);

        assert_eq!(dst[3], 128);
        assert!(dst[0] <= dst[3]);
        assert!(dst[1] <= dst[3]);
        assert!(dst[2] <= dst[3]);
    }

    #[test]
    fn spotlight_dims_outside_and_keeps_inside() {
        let base = solid_base(64, 64, [200, 200, 200]);
        let out = render(
            &base,
            &[Annotation::Spotlight {
                rect: RectLike::new(16.0, 16.0, 32.0, 32.0),
                shape: SpotlightShape::Rectangle,
                dim: 0.5,
            }],
        );
        assert_eq!(out.get_pixel(32, 32).0, [200, 200, 200, 255]);
        assert_eq!(out.get_pixel(2, 2).0, [100, 100, 100, 255]);
    }

    #[test]
    fn spotlights_union_and_annotations_stay_bright() {
        let base = solid_base(96, 32, [200, 200, 200]);
        let out = render(
            &base,
            &[
                Annotation::Spotlight {
                    rect: RectLike::new(0.0, 0.0, 32.0, 32.0),
                    shape: SpotlightShape::Rectangle,
                    dim: 0.5,
                },
                Annotation::Spotlight {
                    rect: RectLike::new(64.0, 0.0, 32.0, 32.0),
                    shape: SpotlightShape::Rectangle,
                    dim: 0.3,
                },
                Annotation::Rectangle {
                    rect: RectLike::new(40.0, 8.0, 16.0, 16.0),
                    color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                    line_width: 2.0,
                    style: ShapeStyle {
                        fill: FillMode::Filled,
                        ..ShapeStyle::default()
                    },
                },
            ],
        );
        // Both spotlights stay lit; the gap between them dims by the larger dim.
        assert_eq!(out.get_pixel(16, 16).0, [200, 200, 200, 255]);
        assert_eq!(out.get_pixel(80, 16).0, [200, 200, 200, 255]);
        assert_eq!(out.get_pixel(60, 2).0, [100, 100, 100, 255]);
        // Annotations draw after the dim pass.
        assert_eq!(out.get_pixel(48, 16).0, [255, 0, 0, 255]);
    }

    #[test]
    fn empty_model_returns_base_unchanged() {
        let base = solid_base(16, 16, [200, 100, 50]);
        let out = render(&base, &[]);
        assert_eq!(base.as_raw(), out.as_raw());
    }

    #[test]
    fn text_survives_huge_and_nonfinite_size() {
        // A corrupt/hand-edited history sidecar can carry an absurd or
        // non-finite text size; rendering it must not overflow the glyph
        // raster dimensions or trigger a multi-petabyte allocation abort.
        let base = solid_base(32, 32, [255, 255, 255]);
        for size in [1.0e9_f32, f32::INFINITY, f32::NAN] {
            let out = render(
                &base,
                &[Annotation::Text {
                    content: "hello".into(),
                    origin: PointLike::new(2.0, 16.0),
                    color: Rgba::OPAQUE_BLACK,
                    font_family: "system-ui".into(),
                    size,
                    style: Default::default(),
                }],
            );
            assert_eq!(out.width(), 32);
            assert_eq!(out.height(), 32);
        }
    }

    #[test]
    fn multiline_text_draws_a_second_line_below_the_first() {
        // A `\n` in the content must lay the second line below the
        // first, not overprint it on the same baseline.
        let base = solid_base(96, 96, [255, 255, 255]);

        let ink_y_extent = |img: &RgbaImage| -> Option<(u32, u32)> {
            let mut min_y = None;
            let mut max_y = 0u32;
            for y in 0..img.height() {
                for x in 0..img.width() {
                    let p = img.get_pixel(x, y);
                    // Any non-near-white pixel counts as ink.
                    if p[0] < 200 || p[1] < 200 || p[2] < 200 {
                        min_y.get_or_insert(y);
                        max_y = y;
                        break;
                    }
                }
            }
            min_y.map(|mn| (mn, max_y))
        };

        let single = render(
            &base,
            &[Annotation::Text {
                content: "Eg".into(),
                origin: PointLike::new(4.0, 20.0),
                color: Rgba::OPAQUE_BLACK,
                font_family: "system-ui".into(),
                size: 18.0,
                style: Default::default(),
            }],
        );
        let multi = render(
            &base,
            &[Annotation::Text {
                content: "Eg\nEg".into(),
                origin: PointLike::new(4.0, 20.0),
                color: Rgba::OPAQUE_BLACK,
                font_family: "system-ui".into(),
                size: 18.0,
                style: Default::default(),
            }],
        );

        let (_single_min, single_max) = ink_y_extent(&single).expect("single line should draw ink");
        let (_multi_min, multi_max) = ink_y_extent(&multi).expect("multi line should draw ink");

        // The second line must extend the inked region well below where
        // the single line ends.
        assert!(
            multi_max > single_max + 5,
            "multi-line ink should extend below single-line (single_max={single_max}, multi_max={multi_max})"
        );
    }

    #[test]
    fn multiline_text_normalises_crlf_to_lf() {
        // A Windows-authored sidecar may store CRLF; it must render the
        // same as LF (two lines), never as a stray glyph or one line.
        let base = solid_base(96, 96, [255, 255, 255]);
        let render_content = |content: &str| {
            render(
                &base,
                &[Annotation::Text {
                    content: content.into(),
                    origin: PointLike::new(4.0, 20.0),
                    color: Rgba::OPAQUE_BLACK,
                    font_family: "system-ui".into(),
                    size: 18.0,
                    style: Default::default(),
                }],
            )
        };
        assert_eq!(
            render_content("Eg\r\nEg").as_raw(),
            render_content("Eg\nEg").as_raw(),
            "CRLF content should render identically to LF content"
        );
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
                style: Default::default(),
            }],
        );
        // Sample a pixel on the top edge of the stroke — should have a
        // strong red component now.
        let pixel = out.get_pixel(32, 8);
        assert!(pixel[0] > 200, "expected red dominant, got {pixel:?}");
        assert!(pixel[1] < 100);
        assert!(pixel[2] < 100);
    }

    #[test]
    fn dashed_rectangle_leaves_gaps_along_its_edge() {
        let base = solid_base(120, 40, [255, 255, 255]);
        let out = render(
            &base,
            &[Annotation::Rectangle {
                rect: RectLike::new(10.0, 10.0, 100.0, 20.0),
                color: Rgba::new(1.0, 0.0, 0.0, 1.0),
                line_width: 2.0,
                style: ShapeStyle {
                    dashed: true,
                    ..ShapeStyle::default()
                },
            }],
        );
        let (mut ink, mut gap) = (0, 0);
        for x in 14..106 {
            let p = out.get_pixel(x, 10);
            if p[1] < 128 {
                ink += 1;
            } else {
                gap += 1;
            }
        }
        assert!(ink > 20 && gap > 10, "ink={ink} gap={gap}");
    }

    #[test]
    fn filled_and_both_rectangles_paint_the_interior() {
        let base = solid_base(64, 64, [255, 255, 255]);
        let rect = |fill| Annotation::Rectangle {
            rect: RectLike::new(8.0, 8.0, 48.0, 48.0),
            color: Rgba::new(1.0, 0.0, 0.0, 1.0),
            line_width: 2.0,
            style: ShapeStyle {
                fill,
                ..ShapeStyle::default()
            },
        };
        let filled = render(&base, &[rect(FillMode::Filled)]);
        assert_eq!(filled.get_pixel(32, 32).0, [255, 0, 0, 255]);
        let both = render(&base, &[rect(FillMode::Both)]);
        let p = both.get_pixel(32, 32);
        assert!(p[0] == 255 && (150..240).contains(&p[1]), "{p:?}");
        // Both keeps the full-strength outline.
        assert!(both.get_pixel(32, 8)[1] < 100);
    }

    #[test]
    fn two_headed_arrow_draws_heads_at_both_ends() {
        let base = solid_base(120, 40, [255, 255, 255]);
        let out = render(
            &base,
            &[Annotation::Arrow {
                a: PointLike::new(10.0, 20.0),
                b: PointLike::new(110.0, 20.0),
                color: Rgba::OPAQUE_BLACK,
                line_width: 2.0,
                style: LineStyle {
                    heads: ArrowHeads::Both,
                    ..LineStyle::default()
                },
            }],
        );
        // Heads are 10 px long and 5 px half-wide (lw 2). 8 px from each
        // tip the head spans ±4 px; y = 17 is inside the head but outside
        // the 2 px shaft (y 19–21).
        assert!(out.get_pixel(18, 17)[0] < 128, "start head missing");
        assert!(out.get_pixel(102, 17)[0] < 128, "end head missing");
    }

    #[test]
    fn hand_drawn_line_renders_identically_twice() {
        let base = solid_base(128, 64, [255, 255, 255]);
        let model = [Annotation::Line {
            a: PointLike::new(8.0, 32.0),
            b: PointLike::new(120.0, 30.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 3.0,
            style: LineStyle {
                path: LinePath::HandDrawn { seed: 11 },
                ..LineStyle::default()
            },
        }];
        assert_eq!(
            render(&base, &model).as_raw(),
            render(&base, &model).as_raw()
        );
    }

    #[test]
    fn pill_text_paints_a_background_and_contrasting_ink() {
        let base = solid_base(160, 60, [255, 255, 255]);
        let out = render(
            &base,
            &[Annotation::Text {
                content: "Hi".into(),
                origin: PointLike::new(20.0, 36.0),
                color: Rgba::OPAQUE_BLACK,
                font_family: "system-ui".into(),
                size: 20.0,
                style: TextStyle {
                    pill: true,
                    bold: false,
                },
            }],
        );
        // Inside the pill's left padding at its vertical middle, left of
        // the first glyph (pill spans x ≥ 13, y ≈ 9–49).
        assert_eq!(out.get_pixel(16, 29).0, [0, 0, 0, 255]);
        // Black fill → white ink somewhere in the glyph area.
        let white_ink = (20..44)
            .flat_map(|x| (18..38).map(move |y| (x, y)))
            .any(|(x, y)| out.get_pixel(x, y)[0] > 200);
        assert!(white_ink);
    }

    #[test]
    fn contrast_ink_picks_black_on_light_fills() {
        assert_eq!(
            contrast_ink(Rgba::new(1.0, 0.95, 0.2, 1.0)),
            Rgba::OPAQUE_BLACK
        );
        assert_eq!(
            contrast_ink(Rgba::new(0.1, 0.3, 0.8, 1.0)),
            Rgba::OPAQUE_WHITE
        );
    }

    #[test]
    fn every_stamp_kind_draws_something() {
        let base = solid_base(48, 48, [255, 255, 255]);
        for kind in StampKind::ALL {
            let out = render(
                &base,
                &[Annotation::Stamp {
                    origin: PointLike::new(24.0, 24.0),
                    stamp: kind,
                    color: Rgba::new(0.1, 0.5, 0.2, 1.0),
                }],
            );
            assert_ne!(out.as_raw(), base.as_raw(), "{kind:?} drew nothing");
        }
    }

    #[test]
    fn symbol_stamp_is_a_colored_disc_with_a_white_mark() {
        let base = solid_base(48, 48, [255, 255, 255]);
        let out = render(
            &base,
            &[Annotation::Stamp {
                origin: PointLike::new(24.0, 24.0),
                stamp: StampKind::Cross,
                color: Rgba::new(0.0, 0.0, 1.0, 1.0),
            }],
        );
        assert_eq!(out.get_pixel(24, 33).0, [0, 0, 255, 255], "disc fill");
        let p = out.get_pixel(24, 24);
        assert!(p[0] > 200 && p[1] > 200, "white cross at the centre: {p:?}");
    }
}
