//! The editor document: parts laid out with a gap and an
//! alignment, then one rotate/flip/scale transform. Annotations, OCR
//! boxes and History redactions live in *document pixels*: the composed
//! image's pixel grid. Pure; no UI.

use std::sync::Arc;

use image::imageops::{self, FilterType};
use image::RgbaImage;

use crate::annotation::{Annotation, LinePath};
use crate::geom::{PointLike, RectLike};
use crate::line_geom::{bend_for_control, curve_control};
use crate::render::text_bounds;
use crate::text_lines::{TextDocument, TextLine, WordRect};

pub type PartId = u64;
/// The capture the editor opened with.
pub const ORIGINAL_PART: PartId = 0;
/// tiny-skia's pixmap limit; larger documents can't render.
pub const MAX_DOC_SIDE: u32 = 32_767;
pub const MAX_GAP: u32 = 40;
/// Pixel budget of a transformed or combined document, about 1 GB of
/// RGBA. An out-of-memory guard against hand-edited or corrupt sidecars;
/// an untransformed single original part is exempt.
pub const MAX_DOC_PIXELS: u64 = 256_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GeometryError {
    #[error("the image would be {width} × {height} px, larger than Readshot can render")]
    TooLarge { width: u32, height: u32 },
    #[error("the image would be {pixels} px, over the {max} px limit")]
    TooManyPixels { pixels: u64, max: u64 },
    #[error("the document has no parts")]
    Empty,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Join {
    #[default]
    Below,
    Beside,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Join {
    pub const ALL: [Self; 2] = [Self::Below, Self::Beside];
    pub fn label(self) -> &'static str {
        match self {
            Self::Below => "Below",
            Self::Beside => "Beside",
        }
    }
}

impl Align {
    pub const ALL: [Self; 3] = [Self::Start, Self::Center, Self::End];
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Center => "Center",
            Self::End => "End",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipAxis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug)]
pub struct Part {
    pub id: PartId,
    pub image: Arc<RgbaImage>,
    /// How this part joins the parts before it (ignored for the first).
    pub join: Join,
}

impl PartialEq for Part {
    fn eq(&self, o: &Self) -> bool {
        self.id == o.id && self.join == o.join && Arc::ptr_eq(&self.image, &o.image)
    }
}

/// Rotate clockwise `quarter_turns` times, then flip, then scale.
///
/// The fields are public and can come from a sidecar, so every consumer
/// goes through [`Transform::sanitized`] first, and every way of making a
/// new transform from one ([`Transform::scaled`], the rotations, flips and
/// [`DocGeometry::with_transform`]) starts from the sanitized value: a
/// hostile NaN or zero scale reads as 1 everywhere and never persists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub quarter_turns: u8,
    pub flip_h: bool,
    pub flip_v: bool,
    pub scale_x: f32,
    pub scale_y: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            quarter_turns: 0,
            flip_h: false,
            flip_v: false,
            scale_x: 1.0,
            scale_y: 1.0,
        }
    }
}

impl Transform {
    /// Judged on the sanitized value, as the fingerprint and the mapping
    /// are: a NaN scale or four quarter turns is the identity.
    pub fn is_identity(&self) -> bool {
        self.sanitized() == Self::default()
    }

    /// In-range copy: turns reduced mod 4, and a scale that is not finite
    /// or not positive treated as 1 (never a panic, never a zero size).
    pub fn sanitized(&self) -> Self {
        let ok = |s: f32| if s.is_finite() && s > 0.0 { s } else { 1.0 };
        Self {
            quarter_turns: self.quarter_turns % 4,
            scale_x: ok(self.scale_x),
            scale_y: ok(self.scale_y),
            ..*self
        }
    }

    /// Text runs vertically (90° or 270°).
    pub fn is_sideways(&self) -> bool {
        self.quarter_turns % 2 == 1
    }

    /// One more clockwise turn applied in document space.
    pub fn rotated_cw(self) -> Self {
        let t = self.sanitized();
        Self {
            quarter_turns: (t.quarter_turns + 1) % 4,
            flip_h: t.flip_v,
            flip_v: t.flip_h,
            scale_x: t.scale_y,
            scale_y: t.scale_x,
        }
    }

    pub fn rotated_ccw(self) -> Self {
        self.rotated_cw().rotated_cw().rotated_cw()
    }

    pub fn flipped(self, axis: FlipAxis) -> Self {
        let t = self.sanitized();
        match axis {
            FlipAxis::Horizontal => Self {
                flip_h: !t.flip_h,
                ..t
            },
            FlipAxis::Vertical => Self {
                flip_v: !t.flip_v,
                ..t
            },
        }
    }

    /// Scale by `(sx, sy)`. A factor that would leave a scale not
    /// finite or not positive leaves that axis as it was.
    pub fn scaled(self, sx: f32, sy: f32) -> Self {
        let t = self.sanitized();
        let mul = |s: f32, f: f32| {
            let v = s * f;
            if v.is_finite() && v > 0.0 {
                v
            } else {
                s
            }
        };
        Self {
            scale_x: mul(t.scale_x, sx),
            scale_y: mul(t.scale_y, sy),
            ..t
        }
    }

    /// Output size of a `w × h` (already rotated) image.
    pub fn scaled_size(&self, w: u32, h: u32) -> (u32, u32) {
        let t = self.sanitized();
        let sw = (w as f64 * t.scale_x as f64).round().max(1.0);
        let sh = (h as f64 * t.scale_y as f64).round().max(1.0);
        (
            sw.min(u32::MAX as f64) as u32,
            sh.min(u32::MAX as f64) as u32,
        )
    }

    /// Layout px → document px for a `w × h` layout, and the document size.
    pub fn affine(&self, w: u32, h: u32) -> (Affine, (u32, u32)) {
        let t = self.sanitized();
        let (mut m, mut cw, mut ch) = (Affine::IDENTITY, w, h);
        for _ in 0..t.quarter_turns {
            // Clockwise in a y-down frame: (x, y) → (H − y, x).
            m = m.then(&Affine {
                a: 0.0,
                b: 1.0,
                c: -1.0,
                d: 0.0,
                tx: ch as f32,
                ty: 0.0,
            });
            std::mem::swap(&mut cw, &mut ch);
        }
        if t.flip_h {
            m = m.then(&Affine {
                a: -1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                tx: cw as f32,
                ty: 0.0,
            });
        }
        if t.flip_v {
            m = m.then(&Affine {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                tx: 0.0,
                ty: ch as f32,
            });
        }
        let (ow, oh) = t.scaled_size(cw, ch);
        m = m.then(&Affine::scale(
            ow as f32 / cw.max(1) as f32,
            oh as f32 / ch.max(1) as f32,
        ));
        (m, (ow, oh))
    }
}

/// `x' = a·x + c·y + tx`, `y' = b·x + d·y + ty`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine {
    pub const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        tx: 0.0,
        ty: 0.0,
    };

    pub fn translate(x: f32, y: f32) -> Self {
        Self {
            tx: x,
            ty: y,
            ..Self::IDENTITY
        }
    }

    pub fn scale(sx: f32, sy: f32) -> Self {
        Self {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }

    pub fn apply(&self, p: PointLike) -> PointLike {
        PointLike::new(
            self.a * p.x + self.c * p.y + self.tx,
            self.b * p.x + self.d * p.y + self.ty,
        )
    }

    /// `self`, then `n`.
    pub fn then(&self, n: &Affine) -> Affine {
        Affine {
            a: n.a * self.a + n.c * self.b,
            b: n.b * self.a + n.d * self.b,
            c: n.a * self.c + n.c * self.d,
            d: n.b * self.c + n.d * self.d,
            tx: n.a * self.tx + n.c * self.ty + n.tx,
            ty: n.b * self.tx + n.d * self.ty + n.ty,
        }
    }

    pub fn det(&self) -> f32 {
        self.a * self.d - self.b * self.c
    }

    pub fn inverse(&self) -> Option<Affine> {
        let det = self.det();
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine {
            a,
            b,
            c,
            d,
            tx: -(a * self.tx + c * self.ty),
            ty: -(b * self.tx + d * self.ty),
        })
    }

    /// How lengths scale (geometric mean of the axis scales).
    pub fn length_scale(&self) -> f32 {
        self.det().abs().sqrt()
    }

    /// Axis-aligned bounds of the mapped rect (exact for quarter turns).
    pub fn map_rect(&self, r: RectLike) -> RectLike {
        let pts = [
            self.apply(PointLike::new(r.x, r.y)),
            self.apply(PointLike::new(r.x + r.width, r.y)),
            self.apply(PointLike::new(r.x, r.y + r.height)),
            self.apply(PointLike::new(r.x + r.width, r.y + r.height)),
        ];
        let x0 = pts.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let y0 = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let x1 = pts.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
        let y1 = pts.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        RectLike::new(x0, y0, x1 - x0, y1 - y0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    pub id: PartId,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub width: u32,
    pub height: u32,
    pub placements: Vec<Placement>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocGeometry {
    pub parts: Vec<Part>,
    pub gap: u32,
    pub align: Align,
    pub transform: Transform,
    /// Fingerprint of the frame this geometry was baked from, so two bakes
    /// of one original (different pixels, same fresh part id) never share a
    /// fingerprint.
    pub baked_from: Option<String>,
}

fn clamp_u32(v: i64) -> u32 {
    u32::try_from(v.max(0)).unwrap_or(u32::MAX)
}

/// The document limits: no side over `MAX_DOC_SIDE`, no more than
/// `MAX_DOC_PIXELS` pixels. Every newly composed image (combine,
/// transform, Beautify output) must pass it before it is allocated.
pub fn check_size(w: u32, h: u32) -> Result<(), GeometryError> {
    if w > MAX_DOC_SIDE || h > MAX_DOC_SIDE {
        return Err(GeometryError::TooLarge {
            width: w,
            height: h,
        });
    }
    let pixels = u64::from(w) * u64::from(h);
    if pixels > MAX_DOC_PIXELS {
        return Err(GeometryError::TooManyPixels {
            pixels,
            max: MAX_DOC_PIXELS,
        });
    }
    Ok(())
}

fn align_offset(start: i64, span: i64, len: i64, align: Align) -> i64 {
    match align {
        Align::Start => start,
        Align::Center => start + (span - len) / 2,
        Align::End => start + span - len,
    }
}

impl DocGeometry {
    pub fn original(image: Arc<RgbaImage>) -> Self {
        Self {
            parts: vec![Part {
                id: ORIGINAL_PART,
                image,
                join: Join::Below,
            }],
            ..Self::default()
        }
    }

    /// The capture as opened: one original part, no transform.
    pub fn is_original(&self) -> bool {
        self.parts.len() == 1 && self.parts[0].id == ORIGINAL_PART && self.transform.is_identity()
    }

    pub fn layout(&self) -> Layout {
        let gap = i64::from(self.gap.min(MAX_GAP));
        let mut placed: Vec<(PartId, i64, i64, u32, u32)> = Vec::with_capacity(self.parts.len());
        let (mut x0, mut y0, mut x1, mut y1) = (0i64, 0i64, 0i64, 0i64);
        for (i, p) in self.parts.iter().enumerate() {
            let (w, h) = (i64::from(p.image.width()), i64::from(p.image.height()));
            let (x, y) = if i == 0 {
                (0, 0)
            } else {
                match p.join {
                    Join::Below => (align_offset(x0, x1 - x0, w, self.align), y1 + gap),
                    Join::Beside => (x1 + gap, align_offset(y0, y1 - y0, h, self.align)),
                }
            };
            placed.push((p.id, x, y, p.image.width(), p.image.height()));
            if i == 0 {
                (x0, y0, x1, y1) = (x, y, x + w, y + h);
            } else {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + w);
                y1 = y1.max(y + h);
            }
        }
        Layout {
            width: clamp_u32(x1 - x0),
            height: clamp_u32(y1 - y0),
            placements: placed
                .into_iter()
                .map(|(id, x, y, width, height)| Placement {
                    id,
                    x: clamp_u32(x - x0),
                    y: clamp_u32(y - y0),
                    width,
                    height,
                })
                .collect(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        let l = self.layout();
        self.transform.affine(l.width.max(1), l.height.max(1)).1
    }

    /// Part px → document px.
    pub fn part_affine(&self, id: PartId) -> Option<Affine> {
        let l = self.layout();
        let p = l.placements.iter().find(|p| p.id == id)?;
        let (t, _) = self.transform.affine(l.width.max(1), l.height.max(1));
        Some(Affine::translate(p.x as f32, p.y as f32).then(&t))
    }

    /// Where part `id` sits in the document.
    pub fn part_rect(&self, id: PartId) -> Option<RectLike> {
        let p = self.parts.iter().find(|p| p.id == id)?;
        let r = RectLike::new(0.0, 0.0, p.image.width() as f32, p.image.height() as f32);
        Some(self.part_affine(id)?.map_rect(r))
    }

    /// The part under `p` (document px), else the nearest one.
    pub fn part_at(&self, p: PointLike) -> Option<PartId> {
        let rects: Vec<(PartId, RectLike)> = self
            .parts
            .iter()
            .filter_map(|part| Some((part.id, self.part_rect(part.id)?)))
            .collect();
        if let Some((id, _)) = rects
            .iter()
            .find(|(_, r)| p.x >= r.x && p.y >= r.y && p.x < r.x + r.width && p.y < r.y + r.height)
        {
            return Some(*id);
        }
        rects
            .iter()
            .min_by(|(_, a), (_, b)| dist2(center(*a), p).total_cmp(&dist2(center(*b), p)))
            .map(|(id, _)| *id)
    }

    /// The document image. Callers must `validate()` first: an invalid
    /// geometry trips a debug assertion and, in release, composes to one
    /// transparent pixel (never an unbounded allocation). The original
    /// geometry hands out the part's own `Arc` (no copy).
    pub fn compose(&self) -> Arc<RgbaImage> {
        match self.try_compose() {
            Ok(img) => img,
            Err(e) => {
                debug_assert!(false, "compose on an invalid geometry: {e}");
                Arc::new(RgbaImage::new(1, 1))
            }
        }
    }

    /// [`Self::compose`] that reports an invalid geometry instead.
    pub fn try_compose(&self) -> Result<Arc<RgbaImage>, GeometryError> {
        if self.parts.len() == 1 && self.transform.is_identity() {
            return Ok(Arc::clone(&self.parts[0].image));
        }
        self.validate()?;
        let l = self.layout();
        let mut img = RgbaImage::new(l.width.max(1), l.height.max(1));
        for pl in &l.placements {
            if let Some(part) = self.parts.iter().find(|p| p.id == pl.id) {
                imageops::replace(&mut img, &*part.image, i64::from(pl.x), i64::from(pl.y));
            }
        }
        let t = self.transform.sanitized();
        let mut img = match t.quarter_turns {
            1 => imageops::rotate90(&img),
            2 => imageops::rotate180(&img),
            3 => imageops::rotate270(&img),
            _ => img,
        };
        if t.flip_h {
            imageops::flip_horizontal_in_place(&mut img);
        }
        if t.flip_v {
            imageops::flip_vertical_in_place(&mut img);
        }
        let (ow, oh) = t.scaled_size(img.width(), img.height());
        if (ow, oh) != img.dimensions() {
            img = imageops::resize(&img, ow, oh, FilterType::Triangle);
        }
        Ok(Arc::new(img))
    }

    /// Whether the document can be rendered. The output size is what is
    /// limited (a combine that a downscale makes fit is fine); the
    /// pre-scale layout only has a coarse memory guard. An untransformed
    /// single original part is exempt: it is the capture as taken.
    pub fn validate(&self) -> Result<(), GeometryError> {
        if self.parts.is_empty() {
            return Err(GeometryError::Empty);
        }
        if self.is_original() {
            return Ok(());
        }
        let l = self.layout();
        if l.width > MAX_DOC_SIDE.saturating_mul(4) || l.height > MAX_DOC_SIDE.saturating_mul(4) {
            return Err(GeometryError::TooLarge {
                width: l.width,
                height: l.height,
            });
        }
        let layout_px = u64::from(l.width) * u64::from(l.height);
        if layout_px > MAX_DOC_PIXELS.saturating_mul(4) {
            return Err(GeometryError::TooManyPixels {
                pixels: layout_px,
                max: MAX_DOC_PIXELS.saturating_mul(4),
            });
        }
        let (width, height) = self.size();
        check_size(width, height)
    }

    /// The pre-resize size (the laid-out, rotated parts) and the document
    /// size, when composing resamples ([`Self::try_compose`]'s resize);
    /// `None` when it doesn't.
    fn resample_dims(&self) -> Option<((u32, u32), (u32, u32))> {
        let l = self.layout();
        let t = self.transform.sanitized();
        let (w, h) = (l.width.max(1), l.height.max(1));
        let (cw, ch) = if t.is_sideways() { (h, w) } else { (w, h) };
        let (ow, oh) = t.scaled_size(cw, ch);
        ((ow, oh) != (cw, ch)).then_some(((cw, ch), (ow, oh)))
    }

    /// The document pixels a redaction over `r` (document px, exact) must
    /// hide: `r` snapped outward to whole pixels
    /// ([`crate::render::redaction_pixels`]) and, when composing resamples
    /// the document (a resize), widened to every pixel the resize computed
    /// from a source pixel under it. The resize's Triangle filter spreads a
    /// source pixel up to about half the scale (or about a pixel when
    /// shrinking) past the rect's edge; left there, that fringe would carry
    /// the hidden content's colours outside the redaction.
    pub fn redaction_cover(&self, r: RectLike) -> RectLike {
        let px = crate::render::redaction_pixels(r);
        let finite = [px.x, px.y, px.width, px.height]
            .iter()
            .all(|v| v.is_finite());
        let Some(((cw, ch), (ow, oh))) = self.resample_dims().filter(|_| finite) else {
            return px;
        };
        let (rx0, rx1) = (r.x.min(r.x + r.width), r.x.max(r.x + r.width));
        let (ry0, ry1) = (r.y.min(r.y + r.height), r.y.max(r.y + r.height));
        let (x0, x1) = cover_axis((px.x, px.x + px.width), (rx0, rx1), cw, ow);
        let (y0, y1) = cover_axis((px.y, px.y + px.height), (ry0, ry1), ch, oh);
        RectLike::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// `anns` as they are drawn over this document's composed image: every
    /// redaction ([`Annotation::redaction_rect`]) widened to
    /// [`Self::redaction_cover`], everything else as is. What every
    /// render of the document, its History bake and its text masking use;
    /// the editor keeps the exact rects (so mapping never accumulates).
    pub fn cover_redactions(&self, anns: &[Annotation]) -> Vec<Annotation> {
        anns.iter()
            .map(|a| {
                a.redaction_rect()
                    .and_then(|r| a.with_redaction_rect(self.redaction_cover(r)))
                    .unwrap_or_else(|| a.clone())
            })
            .collect()
    }

    /// `None` for the original frame; otherwise a stable id of this frame.
    pub fn fingerprint(&self) -> Option<String> {
        if self.is_original() {
            return None;
        }
        let parts: Vec<String> = self
            .parts
            .iter()
            .map(|p| format!("{}{:?}", p.id, p.join))
            .collect();
        let t = self.transform.sanitized();
        Some(format!(
            "parts={};gap={};align={:?};rot={};fh={};fv={};sx={:.6};sy={:.6};from=[{}]",
            parts.join(","),
            self.gap.min(MAX_GAP),
            self.align,
            t.quarter_turns,
            t.flip_h,
            t.flip_v,
            t.scale_x,
            t.scale_y,
            self.baked_from.as_deref().unwrap_or("")
        ))
    }

    pub fn with_transform(&self, transform: Transform) -> Self {
        Self {
            transform: transform.sanitized(),
            ..self.clone()
        }
    }

    /// This geometry turned a quarter, clockwise or not.
    pub fn rotated(&self, clockwise: bool) -> Self {
        let t = if clockwise {
            self.transform.rotated_cw()
        } else {
            self.transform.rotated_ccw()
        };
        self.with_transform(t)
    }

    /// This geometry mirrored along `axis`.
    pub fn flipped(&self, axis: FlipAxis) -> Self {
        self.with_transform(self.transform.flipped(axis))
    }

    pub fn with_layout(&self, gap: u32, align: Align) -> Self {
        Self {
            gap: gap.min(MAX_GAP),
            align,
            ..self.clone()
        }
    }

    pub fn with_part(&self, part: Part) -> Self {
        let mut next = self.clone();
        next.parts.push(part);
        next
    }

    /// One part holding the current document pixels, no transform. Used
    /// before a part is added to a transformed document: a part is never
    /// joined to turned pixels.
    /// Fails when the document is invalid. `id` must be fresh: asking for
    /// [`ORIGINAL_PART`] gets the next unused id instead, so cached OCR of
    /// the untouched capture is never matched to baked pixels.
    pub fn baked(&self, id: PartId) -> Result<Self, GeometryError> {
        self.validate()?;
        let image = self.try_compose()?;
        self.baked_from_image(id, image)
    }

    /// [`Self::baked`] with the document already composed (`image`, which
    /// must be [`Self::compose`]'s result), so nothing is composed here.
    /// An image of another size is not this document's: composed instead.
    pub fn baked_from_image(
        &self,
        id: PartId,
        image: Arc<RgbaImage>,
    ) -> Result<Self, GeometryError> {
        self.validate()?;
        let image = if image.dimensions() == self.size() {
            image
        } else {
            self.try_compose()?
        };
        let id = if id == ORIGINAL_PART {
            self.parts
                .iter()
                .map(|p| p.id)
                .max()
                .map_or(1, |m| m.saturating_add(1))
                .max(1)
        } else {
            id
        };
        Ok(Self {
            parts: vec![Part {
                id,
                image,
                join: Join::Below,
            }],
            gap: self.gap,
            align: self.align,
            transform: Transform::default(),
            baked_from: Some(baked_from_id(self.fingerprint().as_deref())),
        })
    }
}

/// The `baked_from` of a bake of the frame with fingerprint `prior`
/// (`None`: the original frame). A fixed-size hash of the prior
/// fingerprint, so repeated bakes never grow the fingerprint (each used to
/// nest the whole previous one). Fingerprints are only ever compared for
/// equality, so an id persisted by an older build simply matches nothing
/// and its text is dropped (fail closed).
fn baked_from_id(prior: Option<&str>) -> String {
    let mut h = Fnv1a64::new();
    h.write(b"bake-v1\0");
    match prior {
        None => h.write(b"original"),
        Some(f) => {
            h.write(b"frame\0");
            h.write(f.as_bytes());
        }
    }
    format!("fnv:{:016x}", h.finish())
}

/// 64-bit FNV-1a. Implemented here rather than with std's
/// `DefaultHasher`, whose output may change between Rust releases: these
/// hashes are persisted and must stay the same across builds (here and
/// in the app's History frame ids).
#[derive(Clone, Debug)]
pub struct Fnv1a64(u64);

impl Default for Fnv1a64 {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv1a64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new() -> Self {
        Self(Self::OFFSET)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(Self::PRIME);
        }
    }

    pub fn finish(&self) -> u64 {
        self.0
    }
}

fn center(r: RectLike) -> PointLike {
    PointLike::new(r.x + r.width * 0.5, r.y + r.height * 0.5)
}

fn dist2(a: PointLike, b: PointLike) -> f32 {
    (a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)
}

/// Whole document pixels `(lo, hi)` (the snapped `exact` span) widened,
/// on one axis resampled from `src` to `dst` px by `image`'s Triangle
/// resize, to every output pixel that read a source pixel under `exact`. That resize computes output pixel
/// `x` from source pixels `i` with `|i + 0.5 − (x + 0.5)·ratio| < max(ratio, 1)`
/// (`ratio = src / dst`); the bounds are rounded outward with a little
/// slack for its f32 arithmetic.
fn cover_axis((lo, hi): (f32, f32), exact: (f32, f32), src: u32, dst: u32) -> (f32, f32) {
    if src == dst || src == 0 || dst == 0 || hi <= lo {
        return (lo, hi);
    }
    let ratio = src as f32 / dst as f32;
    let support = ratio.max(1.0);
    // Source pixels holding anything under the exact span.
    let first_src = (exact.0 * ratio).floor();
    let end_src = (exact.1 * ratio).ceil();
    const SLACK: f32 = 1e-3;
    let first = ((first_src + 0.5 - support) / ratio - 0.5 - SLACK).floor();
    let end = ((end_src - 0.5 + support) / ratio - 0.5 + SLACK).ceil();
    (lo.min(first), hi.max(end))
}

/// The point that decides which part an annotation belongs to.
pub fn annotation_anchor(a: &Annotation) -> PointLike {
    match a {
        Annotation::Rectangle { rect, .. }
        | Annotation::Ellipse { rect, .. }
        | Annotation::Blur { rect, .. }
        | Annotation::Pixelate { rect, .. }
        | Annotation::Spotlight { rect, .. }
        | Annotation::Crop { rect } => center(*rect),
        Annotation::Line { a, b, .. } | Annotation::Arrow { a, b, .. } => {
            PointLike::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
        }
        Annotation::Pen { points, .. } | Annotation::Highlighter { points, .. } => {
            points.first().copied().unwrap_or(PointLike::new(0.0, 0.0))
        }
        Annotation::Text { origin, .. }
        | Annotation::NumberedPin { origin, .. }
        | Annotation::Stamp { origin, .. } => *origin,
        // The source decides the part: that is what is magnified.
        Annotation::Magnifier { source, .. } => *source,
    }
}

/// `a` moved by `m`. Lengths (stroke widths, radii, text size) scale by
/// `m.length_scale()`; pins and stamps keep their fixed size; text stays
/// upright, centred where its box went. A redaction's rect is mapped
/// exactly (never snapped here: snapping after every mapping grew it on
/// each resize round trip); it becomes whole pixels where it is drawn
/// ([`crate::render::redaction_pixels`], [`DocGeometry::cover_redactions`]).
pub fn map_annotation(a: &Annotation, m: &Affine) -> Annotation {
    let mut out = a.clone();
    let finite = [m.a, m.b, m.c, m.d, m.tx, m.ty]
        .iter()
        .all(|v| v.is_finite());
    if !finite {
        return out;
    }
    let ls = m.length_scale();
    match &mut out {
        Annotation::Rectangle {
            rect,
            line_width,
            style,
            ..
        } => {
            *rect = m.map_rect(*rect);
            *line_width *= ls;
            style.corner_radius *= ls;
        }
        Annotation::Ellipse {
            rect, line_width, ..
        } => {
            *rect = m.map_rect(*rect);
            *line_width *= ls;
        }
        Annotation::Line {
            a: p0,
            b: p1,
            line_width,
            style,
            ..
        }
        | Annotation::Arrow {
            a: p0,
            b: p1,
            line_width,
            style,
            ..
        } => {
            let (na, nb) = (m.apply(*p0), m.apply(*p1));
            if let LinePath::Curved { bend } = &mut style.path {
                let c = m.apply(curve_control(*p0, *p1, *bend));
                let nb_bend = bend_for_control(na, nb, c);
                *bend = if nb_bend.is_finite() {
                    nb_bend.clamp(-2.0, 2.0)
                } else {
                    0.0
                };
            }
            (*p0, *p1) = (na, nb);
            *line_width *= ls;
        }
        Annotation::Pen {
            points, line_width, ..
        }
        | Annotation::Highlighter {
            points, line_width, ..
        } => {
            for p in points.iter_mut() {
                *p = m.apply(*p);
            }
            *line_width *= ls;
        }
        Annotation::Text {
            content,
            origin,
            size,
            style,
            ..
        } => {
            let new_size = (*size * ls).max(1.0);
            match (
                text_bounds(content, *size, *origin, *style),
                text_bounds(content, new_size, *origin, *style),
            ) {
                (Some(old), Some(new)) => {
                    let to = m.apply(center(old));
                    let at = center(new);
                    *origin = PointLike::new(origin.x + to.x - at.x, origin.y + to.y - at.y);
                }
                _ => *origin = m.apply(*origin),
            }
            *size = new_size;
        }
        Annotation::Blur { rect, radius } => {
            *rect = m.map_rect(*rect);
            *radius = (*radius * ls).max(2.0);
        }
        Annotation::Pixelate { rect, block_size } => {
            *rect = m.map_rect(*rect);
            *block_size = (*block_size * ls).max(4.0);
        }
        Annotation::Spotlight { rect, .. } | Annotation::Crop { rect } => *rect = m.map_rect(*rect),
        Annotation::NumberedPin { origin, .. } | Annotation::Stamp { origin, .. } => {
            *origin = m.apply(*origin)
        }
        Annotation::Magnifier {
            source,
            radius,
            bubble,
            ..
        } => {
            *source = m.apply(*source);
            *bubble = m.apply(*bubble);
            *radius = (*radius * ls).max(1.0);
        }
    }
    out
}

/// Old document px to new document px for the pixels of part `id`.
fn part_map(old: &DocGeometry, new: &DocGeometry, id: PartId) -> Option<Affine> {
    Some(old.part_affine(id)?.inverse()?.then(&new.part_affine(id)?))
}

/// The map from document px of `old` to document px of `new` at `p` (a
/// point of `old`): the affine of the part under `p`. `None` unless the
/// change is an affine of that frame: `old` and `new` must have the same
/// parts with the same pixels (a part added, removed or baked is not),
/// and `p` must be on a part (not in a gutter or outside), so the point
/// means the same pixel of the same part on both sides.
pub fn frame_map_at(old: &DocGeometry, new: &DocGeometry, p: PointLike) -> Option<Affine> {
    if old.parts != new.parts || !(p.x.is_finite() && p.y.is_finite()) {
        return None;
    }
    let id = old.parts.iter().map(|part| part.id).find(|&id| {
        old.part_rect(id).is_some_and(|r| {
            p.x >= r.x && p.y >= r.y && p.x <= r.x + r.width && p.y <= r.y + r.height
        })
    })?;
    part_map(old, new, id)
}

/// `p` (document px of `old`) in document px of `new`; see
/// [`frame_map_at`].
pub fn map_point(old: &DocGeometry, new: &DocGeometry, p: PointLike) -> Option<PointLike> {
    Some(frame_map_at(old, new, p)?.apply(p))
}

/// `anns` (document px of `old`) in document px of `new`. Each annotation
/// moves with the part under its anchor; parts missing from either side
/// leave it where it is. A redaction (see [`Annotation::redaction_rect`]) that
/// overlaps a part moves with that part; one that overlaps two or more is split
/// along the parts, and each piece
/// moves with its own part (exactly; each piece covers its whole pixels
/// where it is drawn), so no covered pixel is lost when the parts move
/// apart.
pub fn remap_annotations(
    old: &DocGeometry,
    new: &DocGeometry,
    anns: &[Annotation],
) -> Vec<Annotation> {
    let mut out = Vec::with_capacity(anns.len());
    for a in anns {
        if let Some(r) = a.redaction_rect() {
            let pieces: Vec<(PartId, RectLike)> = old
                .parts
                .iter()
                .filter_map(|p| Some((p.id, r.intersect(old.part_rect(p.id)?)?)))
                .collect();
            if !pieces.is_empty() {
                // One overlapped part: its affine maps the whole rect.
                // Several: split along the parts. Only a redaction
                // entirely in a gutter falls to the center rule.
                let single = pieces.len() == 1;
                for (id, piece) in pieces {
                    let moved = if single {
                        a.clone()
                    } else {
                        a.with_redaction_rect(piece).unwrap_or_else(|| a.clone())
                    };
                    out.push(match part_map(old, new, id) {
                        Some(m) if m != Affine::IDENTITY => map_annotation(&moved, &m),
                        _ => moved,
                    });
                }
                continue;
            }
        }
        let m = old
            .part_at(annotation_anchor(a))
            .and_then(|id| part_map(old, new, id))
            .unwrap_or(Affine::IDENTITY);
        out.push(if m == Affine::IDENTITY {
            a.clone()
        } else {
            map_annotation(a, &m)
        });
    }
    out
}

/// `doc`'s boxes moved by `m` (lines and words stay in order).
pub fn map_text_document(doc: &TextDocument, m: &Affine) -> TextDocument {
    TextDocument {
        lines: doc
            .lines
            .iter()
            .map(|l| TextLine {
                text: l.text.clone(),
                rect: l.rect.map(|r| m.map_rect(r)),
                words: l
                    .words
                    .iter()
                    .map(|w| WordRect {
                        start: w.start,
                        end: w.end,
                        rect: m.map_rect(w.rect),
                    })
                    .collect(),
            })
            .collect(),
        positioned: doc.positioned,
        layout_text: doc.layout_text.clone(),
    }
}

/// Parts' documents in part order: lines concatenated, texts joined by a
/// blank line; positioned only when every part is.
pub fn merge_text_documents(docs: &[TextDocument]) -> TextDocument {
    if docs.len() == 1 {
        return docs[0].clone();
    }
    TextDocument {
        lines: docs.iter().flat_map(|d| d.lines.iter().cloned()).collect(),
        positioned: !docs.is_empty() && docs.iter().all(|d| d.positioned),
        layout_text: docs
            .iter()
            .map(|d| d.layout_text.as_str())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{FillMode, LineStyle, ShapeStyle};
    use crate::{RectLike, Rgba};
    use image::Rgba as Px;

    #[test]
    fn a_point_maps_with_its_part_only_when_the_frame_is_an_affine_of_it() {
        let g = DocGeometry::original(grid(40, 20));
        // Resized 2×: (8, 4) is (16, 8).
        let big = g.with_transform(g.transform.scaled(2.0, 2.0));
        assert_eq!(
            map_point(&g, &big, PointLike::new(8.0, 4.0)),
            Some(PointLike::new(16.0, 8.0))
        );
        // Rotated clockwise: the same pixel, turned.
        let turned = g.with_transform(g.transform.rotated_cw());
        let q = map_point(&g, &turned, PointLike::new(8.0, 4.0)).unwrap();
        assert!(
            (q.x - 16.0).abs() < 1e-4 && (q.y - 8.0).abs() < 1e-4,
            "{q:?}"
        );
        // Outside the image: no part under it.
        assert_eq!(map_point(&g, &big, PointLike::new(41.0, 4.0)), None);
        // A part added: not an affine of the shown frame.
        let combined = g.with_part(Part {
            id: 7,
            image: grid(10, 10),
            join: Join::Below,
        });
        assert_eq!(map_point(&g, &combined, PointLike::new(8.0, 4.0)), None);
        // Two parts moved apart by the gap: each point with its own part,
        // a gutter point with none.
        let wide = combined.with_layout(10, combined.align);
        let lower = combined.part_rect(7).unwrap();
        let in_lower = PointLike::new(lower.x + 2.0, lower.y + 2.0);
        let moved = map_point(&combined, &wide, in_lower).unwrap();
        let lower_after = wide.part_rect(7).unwrap();
        assert_eq!(
            moved,
            PointLike::new(lower_after.x + 2.0, lower_after.y + 2.0)
        );
        let gutter = PointLike::new(2.0, 25.0);
        assert_eq!(map_point(&wide, &combined, gutter), None);
    }

    /// 3×2 image whose every pixel is unique: value = x + 10·y.
    fn grid(w: u32, h: u32) -> Arc<RgbaImage> {
        Arc::new(RgbaImage::from_fn(w, h, |x, y| {
            Px([(x + 10 * y) as u8, 0, 0, 255])
        }))
    }

    fn rect_ann(r: RectLike) -> Annotation {
        Annotation::Rectangle {
            rect: r,
            color: Rgba::OPAQUE_BLACK,
            line_width: 4.0,
            style: ShapeStyle::default(),
        }
    }

    fn solid_block(r: RectLike) -> Annotation {
        Annotation::Rectangle {
            rect: r,
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
            style: ShapeStyle {
                fill: FillMode::Filled,
                ..ShapeStyle::default()
            },
        }
    }

    fn rect_of(a: &Annotation) -> RectLike {
        match a {
            Annotation::Rectangle { rect, .. } => *rect,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn magnifiers_move_and_scale_with_the_document() {
        let g = DocGeometry::original(grid(10, 10));
        let half = g.with_transform(g.transform.scaled(0.5, 0.5));
        let m = Annotation::Magnifier {
            source: PointLike::new(2.0, 2.0),
            radius: 4.0,
            bubble: PointLike::new(8.0, 6.0),
            zoom: 3.0,
            shape: crate::MagnifierShape::RoundedSquare,
            color: Rgba::OPAQUE_BLACK,
        };
        let out = remap_annotations(&g, &half, &[m]);
        let Annotation::Magnifier {
            source,
            radius,
            bubble,
            zoom,
            ..
        } = &out[0]
        else {
            unreachable!()
        };
        assert_eq!(
            (*source, *bubble, *radius, *zoom),
            (PointLike::new(1.0, 1.0), PointLike::new(4.0, 3.0), 2.0, 3.0)
        );
    }

    #[test]
    fn hostile_transforms_read_alike_everywhere_and_never_keep_a_nan() {
        let g = DocGeometry::original(grid(3, 2));
        let a = rect_ann(RectLike::new(1.0, 0.0, 1.0, 1.0));
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -2.0] {
            let t = Transform {
                scale_x: bad,
                scale_y: bad,
                ..Transform::default()
            };
            // The raw value, as a sidecar could hand it over.
            let raw = DocGeometry {
                transform: t,
                ..g.clone()
            };
            assert!(t.is_identity(), "{bad}");
            assert!(raw.is_original(), "{bad}");
            assert_eq!(raw.fingerprint(), g.fingerprint(), "{bad}");
            assert_eq!(raw.size(), g.size(), "{bad}");
            assert_eq!(
                remap_annotations(&g, &raw, std::slice::from_ref(&a)),
                vec![a.clone()],
                "{bad}"
            );
            // Nothing made from it keeps the bad value.
            let made = [
                g.with_transform(t).transform,
                t.scaled(0.5, 0.5),
                t.rotated_cw(),
                t.rotated_ccw(),
                t.flipped(FlipAxis::Horizontal),
            ];
            for m in made {
                assert!(m.scale_x.is_finite() && m.scale_x > 0.0, "{bad}: {m:?}");
                assert!(m.scale_y.is_finite() && m.scale_y > 0.0, "{bad}: {m:?}");
            }
            assert_eq!(t.scaled(0.5, 0.5).scale_x, 0.5, "{bad}");
        }
        // A bad factor leaves the scale as it was.
        let half = Transform::default().scaled(0.5, 0.5);
        for bad in [f32::NAN, f32::INFINITY, 0.0, -1.0] {
            assert_eq!(half.scaled(bad, bad), half, "{bad}");
        }
        // Four quarter turns are no turn.
        let t = Transform {
            quarter_turns: 4,
            ..Transform::default()
        };
        assert!(t.is_identity());
        assert_eq!(t.rotated_cw().quarter_turns, 1);
    }

    #[test]
    fn affine_then_and_inverse_round_trip() {
        let m = Affine::translate(3.0, -2.0).then(&Affine::scale(2.0, 0.5));
        let p = PointLike::new(1.0, 4.0);
        let q = m.apply(p);
        assert_eq!(q, PointLike::new(8.0, 1.0));
        let back = m.inverse().unwrap().apply(q);
        assert!((back.x - p.x).abs() < 1e-5 && (back.y - p.y).abs() < 1e-5);
    }

    #[test]
    fn original_geometry_is_the_part_itself_without_copying() {
        let img = grid(3, 2);
        let g = DocGeometry::original(Arc::clone(&img));
        assert!(g.is_original());
        assert_eq!(g.fingerprint(), None);
        assert!(Arc::ptr_eq(&g.compose(), &img));
        assert_eq!(g.size(), (3, 2));
    }

    #[test]
    fn rotation_and_flips_match_imageops_and_four_turns_are_identity() {
        let img = grid(3, 2);
        let g = DocGeometry::original(Arc::clone(&img));
        let cw = g.with_transform(g.transform.rotated_cw());
        assert_eq!(*cw.compose(), image::imageops::rotate90(&*img));
        let cw_fh = cw.with_transform(cw.transform.flipped(FlipAxis::Horizontal));
        assert_eq!(
            *cw_fh.compose(),
            image::imageops::flip_horizontal(&image::imageops::rotate90(&*img))
        );
        // Flip then rotate equals rotate then the other flip.
        let fh_cw = g.with_transform(g.transform.flipped(FlipAxis::Horizontal).rotated_cw());
        assert_eq!(
            *fh_cw.compose(),
            image::imageops::rotate90(&image::imageops::flip_horizontal(&*img))
        );
        let t = g
            .transform
            .rotated_cw()
            .rotated_cw()
            .rotated_cw()
            .rotated_cw();
        assert!(t.is_identity());
        assert!(g.transform.rotated_cw().rotated_ccw().is_identity());
    }

    #[test]
    fn annotations_follow_their_pixels_through_every_transform() {
        let img = grid(3, 2);
        let g = DocGeometry::original(Arc::clone(&img));
        // Pixel (2, 0) has value 2. Track it through each transform.
        let ann = rect_ann(RectLike::new(2.0, 0.0, 1.0, 1.0));
        for t in [
            g.transform.rotated_cw(),
            g.transform.rotated_ccw(),
            g.transform.rotated_cw().rotated_cw(),
            g.transform.flipped(FlipAxis::Horizontal),
            g.transform.flipped(FlipAxis::Vertical),
            g.transform.rotated_cw().flipped(FlipAxis::Vertical),
        ] {
            let ng = g.with_transform(t);
            let moved = remap_annotations(&g, &ng, std::slice::from_ref(&ann));
            let r = rect_of(&moved[0]);
            let out = ng.compose();
            assert_eq!((r.width, r.height), (1.0, 1.0), "{t:?}");
            assert_eq!(out.get_pixel(r.x as u32, r.y as u32).0[0], 2, "{t:?}");
        }
    }

    #[test]
    fn resize_scales_geometry_widths_and_maps_redactions_exactly() {
        let g = DocGeometry::original(grid(10, 10));
        let half = g.with_transform(g.transform.scaled(0.5, 0.5));
        assert_eq!(half.size(), (5, 5));
        let moved = remap_annotations(&g, &half, &[rect_ann(RectLike::new(2.0, 2.0, 4.0, 4.0))]);
        assert_eq!(rect_of(&moved[0]), RectLike::new(1.0, 1.0, 2.0, 2.0));
        match &moved[0] {
            Annotation::Rectangle { line_width, .. } => assert_eq!(*line_width, 2.0),
            _ => unreachable!(),
        }
        let small = g.with_transform(g.transform.scaled(0.3, 0.3));
        let block = remap_annotations(
            &g,
            &small,
            &[solid_block(RectLike::new(1.0, 1.0, 3.0, 3.0))],
        );
        // Mapped exactly (0.3..1.2); its pixels snap out to 0..2, and the
        // resize's fringe widens what it covers.
        let r = rect_of(&block[0]);
        assert!(
            (r.x - 0.3).abs() < 1e-5 && (r.width - 0.9).abs() < 1e-5,
            "{r:?}"
        );
        let px = crate::render::redaction_pixels(r);
        assert_eq!(px, RectLike::new(0.0, 0.0, 2.0, 2.0));
        assert!(small.redaction_cover(r).contains(px));
    }

    /// `r`'s pixels with no slack at all: every pixel it touches.
    fn touched(r: RectLike) -> RectLike {
        let (x0, y0) = (r.x.floor(), r.y.floor());
        let (x1, y1) = ((r.x + r.width).ceil(), (r.y + r.height).ceil());
        RectLike::new(x0, y0, x1 - x0, y1 - y0)
    }

    fn scaled(g: &DocGeometry, sx: f32, sy: f32) -> DocGeometry {
        g.with_transform(g.transform.scaled(sx, sy))
    }

    fn redactions_at(r: RectLike) -> Vec<Annotation> {
        vec![
            solid_block(r),
            Annotation::Blur {
                rect: r,
                radius: 3.0,
            },
            Annotation::Pixelate {
                rect: r,
                block_size: 4.0,
            },
        ]
    }

    fn redaction_rect_of(a: &Annotation) -> RectLike {
        a.redaction_rect().expect("a redaction")
    }

    #[test]
    fn resize_round_trips_give_back_the_original_redaction_pixels() {
        for (w, h) in [(10, 10), (101, 57), (333, 1000), (1920, 1080)] {
            let g = DocGeometry::original(grid(w, h));
            let original = RectLike::new(3.0, 5.0, 7.0, 4.0);
            for (s, back) in [
                (0.5, 2.0),
                (2.0, 0.5),
                (0.37, 1.0 / 0.37),
                (3.0, 1.0 / 3.0),
                (0.9, 1.0 / 0.9),
            ] {
                let mut anns = redactions_at(original);
                let mut at = g.clone();
                // Five round trips: nothing accumulates.
                for _ in 0..5 {
                    let mid = scaled(&at, s, s);
                    let there = remap_annotations(&at, &mid, &anns);
                    // Never smaller than the original pixels, there.
                    let m = mid.part_affine(ORIGINAL_PART).unwrap();
                    for a in &there {
                        let cover = mid.redaction_cover(redaction_rect_of(a));
                        assert!(
                            cover.contains(touched(m.map_rect(original))),
                            "{w}x{h} ×{s}: {cover:?}"
                        );
                    }
                    let home = scaled(&mid, back, back);
                    anns = remap_annotations(&mid, &home, &there);
                    at = home;
                }
                assert_eq!(at.size(), (w, h), "×{s}");
                for a in &anns {
                    let r = redaction_rect_of(a);
                    assert_eq!(
                        crate::render::redaction_pixels(r),
                        original,
                        "{w}x{h} ×{s} then ×{back}: {r:?}"
                    );
                    assert_eq!(at.redaction_cover(r), original, "{w}x{h} ×{s}");
                }
            }
        }
    }

    #[test]
    fn rotation_and_flip_round_trips_give_back_the_original_redaction() {
        let g = DocGeometry::original(grid(30, 20));
        let original = RectLike::new(3.0, 5.0, 7.0, 4.0);
        let turn = |g: &DocGeometry, cw: bool| {
            g.with_transform(if cw {
                g.transform.rotated_cw()
            } else {
                g.transform.rotated_ccw()
            })
        };
        let flip = |g: &DocGeometry, axis| g.with_transform(g.transform.flipped(axis));
        type Step = Box<dyn Fn(&DocGeometry) -> DocGeometry>;
        let steps: Vec<Step> = vec![
            Box::new(move |g| turn(g, true)),
            Box::new(move |g| turn(g, true)),
            Box::new(move |g| flip(g, FlipAxis::Horizontal)),
            Box::new(move |g| scaled(g, 0.5, 0.5)),
            Box::new(move |g| flip(g, FlipAxis::Horizontal)),
            Box::new(move |g| scaled(g, 2.0, 2.0)),
            Box::new(move |g| turn(g, false)),
            Box::new(move |g| turn(g, false)),
        ];
        let mut at = g.clone();
        let mut anns = redactions_at(original);
        for step in &steps {
            let next = step(&at);
            anns = remap_annotations(&at, &next, &anns);
            at = next;
        }
        assert!(at.is_original(), "{:?}", at.transform);
        for a in &anns {
            assert_eq!(at.redaction_cover(redaction_rect_of(a)), original);
        }
    }

    #[test]
    fn redactions_split_at_a_seam_round_trip_without_growing() {
        let a = Part {
            id: ORIGINAL_PART,
            image: grid(10, 10),
            join: Join::Below,
        };
        let b = Part {
            id: 1,
            image: grid(10, 10),
            join: Join::Below,
        };
        let g = DocGeometry {
            parts: vec![a, b],
            ..DocGeometry::default()
        };
        let original = RectLike::new(1.0, 7.0, 6.0, 5.0);
        // Gap 40 splits the redaction at the seam; then a resize there and
        // back, then the gap closes again.
        let wide = g.with_layout(40, Align::Start);
        let pieces = remap_annotations(&g, &wide, &[solid_block(original)]);
        assert_eq!(pieces.len(), 2);
        let half = scaled(&wide, 0.5, 0.5);
        let there = remap_annotations(&wide, &half, &pieces);
        // Each piece still covers its part's original pixels there.
        for (piece, id) in there.iter().zip([ORIGINAL_PART, 1]) {
            let want = half.part_affine(id).unwrap().map_rect(
                wide.part_affine(id)
                    .unwrap()
                    .inverse()
                    .unwrap()
                    .map_rect(rect_of(&pieces[if id == ORIGINAL_PART { 0 } else { 1 }])),
            );
            assert!(half.redaction_cover(rect_of(piece)).contains(touched(want)));
        }
        let back = scaled(&half, 2.0, 2.0);
        let home = remap_annotations(&half, &back, &there);
        let closed = back.with_layout(0, Align::Start);
        let home = remap_annotations(&back, &closed, &home);
        let px: Vec<RectLike> = home
            .iter()
            .map(|a| closed.redaction_cover(rect_of(a)))
            .collect();
        assert_eq!(
            px,
            vec![
                RectLike::new(1.0, 7.0, 6.0, 3.0),
                RectLike::new(1.0, 10.0, 6.0, 2.0)
            ],
            "the two pieces are exactly the original's pixels"
        );
    }

    /// A 40 × 30 gradient with a high-contrast checkerboard (the secret)
    /// at x 12–22, y 10–18 — or, `secret: false`, the gradient there too.
    fn secret_image(secret: bool) -> Arc<RgbaImage> {
        Arc::new(RgbaImage::from_fn(40, 30, |x, y| {
            let inside = (12..22).contains(&x) && (10..18).contains(&y);
            if secret && inside {
                let on = (x + y) % 2 == 0;
                let v = if on { 255 } else { 0 };
                Px([v, 255 - v, v, 255])
            } else {
                Px([(x * 6) as u8, (y * 8) as u8, 100, 255])
            }
        }))
    }

    #[test]
    fn no_pixel_outside_a_resized_redaction_carries_what_it_hides() {
        let secret_rect = RectLike::new(12.0, 10.0, 10.0, 8.0);
        for (sx, sy) in [(3.0, 3.0), (0.4, 0.4), (2.5, 0.6), (0.5, 1.7), (1.3, 1.3)] {
            for redaction in redactions_at(secret_rect) {
                let render = |secret: bool, cover: bool| {
                    let g = DocGeometry::original(secret_image(secret));
                    let s = scaled(&g, sx, sy);
                    let anns = remap_annotations(&g, &s, std::slice::from_ref(&redaction));
                    let drawn = if cover {
                        s.cover_redactions(&anns)
                    } else {
                        anns.clone()
                    };
                    (crate::render::render(&s.compose(), &drawn), s, anns)
                };
                let (with, s, anns) = render(true, true);
                let (without, _, _) = render(false, true);
                let exact = redaction_rect_of(&anns[0]);
                let cover = s.redaction_cover(exact);
                // The cover stays within the resize's reach of the rect.
                let reach_x = 0.5 * sx.max(1.0) + 3.0;
                let reach_y = 0.5 * sy.max(1.0) + 3.0;
                assert!(cover.x >= exact.x - reach_x && cover.y >= exact.y - reach_y);
                assert!(cover.x + cover.width <= exact.x + exact.width + reach_x);
                assert!(cover.y + cover.height <= exact.y + exact.height + reach_y);
                for (x, y, px) in with.enumerate_pixels() {
                    let (fx, fy) = (x as f32, y as f32);
                    let inside = fx >= cover.x
                        && fy >= cover.y
                        && fx < cover.x + cover.width
                        && fy < cover.y + cover.height;
                    if !inside || matches!(redaction, Annotation::Rectangle { .. }) {
                        assert_eq!(
                            px,
                            without.get_pixel(x, y),
                            "×({sx}, {sy}) {redaction:?} at ({x}, {y})"
                        );
                    }
                }
                // Drawn at the exact rect only (no fringe cover), the
                // resize's fringe shows: what this guards against.
                if sx > 1.0 && matches!(redaction, Annotation::Rectangle { .. }) {
                    let (bare_with, _, _) = render(true, false);
                    let (bare_without, _, _) = render(false, false);
                    assert_ne!(bare_with, bare_without, "×({sx}, {sy})");
                }
            }
        }
    }

    #[test]
    fn curved_lines_keep_their_apex_under_a_flip() {
        let g = DocGeometry::original(grid(10, 10));
        let line = Annotation::Line {
            a: PointLike::new(0.0, 0.0),
            b: PointLike::new(10.0, 0.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: LineStyle {
                path: crate::LinePath::Curved { bend: 0.3 },
                ..LineStyle::default()
            },
        };
        let flipped = g.with_transform(g.transform.flipped(FlipAxis::Horizontal));
        let m = remap_annotations(&g, &flipped, &[line]);
        let Annotation::Line { a, b, style, .. } = &m[0] else {
            unreachable!()
        };
        let crate::LinePath::Curved { bend } = style.path else {
            unreachable!()
        };
        let apex = crate::line_geom::curve_apex(*a, *b, bend);
        // Original apex (5, 1.5) mirrored about x = 5 is itself.
        assert!(
            (apex.x - 5.0).abs() < 1e-4 && (apex.y - 1.5).abs() < 1e-4,
            "{apex:?}"
        );
    }

    #[test]
    fn combine_layout_aligns_and_gaps() {
        let a = Part {
            id: ORIGINAL_PART,
            image: grid(4, 2),
            join: Join::Below,
        };
        let b = Part {
            id: 1,
            image: grid(2, 3),
            join: Join::Below,
        };
        let g = DocGeometry {
            parts: vec![a.clone(), b],
            gap: 1,
            align: Align::Center,
            ..DocGeometry::default()
        };
        let l = g.layout();
        assert_eq!((l.width, l.height), (4, 6));
        assert_eq!((l.placements[1].x, l.placements[1].y), (1, 3));
        let c = Part {
            id: 2,
            image: grid(2, 3),
            join: Join::Beside,
        };
        let g2 = DocGeometry {
            parts: vec![a, c],
            gap: 2,
            align: Align::End,
            ..DocGeometry::default()
        };
        let l2 = g2.layout();
        assert_eq!((l2.width, l2.height), (8, 3));
        assert_eq!((l2.placements[0].x, l2.placements[0].y), (0, 1));
        assert_eq!((l2.placements[1].x, l2.placements[1].y), (6, 0));
        // Gutters are transparent; parts keep their pixels.
        let img = g2.compose();
        assert_eq!(img.get_pixel(4, 0).0[3], 0);
        assert_eq!(img.get_pixel(6, 0).0, [0, 0, 0, 255]);
    }

    #[test]
    fn gap_changes_move_only_the_annotations_on_the_moved_part() {
        let a = Part {
            id: ORIGINAL_PART,
            image: grid(4, 2),
            join: Join::Below,
        };
        let b = Part {
            id: 1,
            image: grid(4, 2),
            join: Join::Below,
        };
        let g = DocGeometry {
            parts: vec![a, b],
            gap: 0,
            ..DocGeometry::default()
        };
        let wide = g.with_layout(10, Align::Start);
        let on_a = rect_ann(RectLike::new(1.0, 0.0, 1.0, 1.0));
        let on_b = rect_ann(RectLike::new(1.0, 3.0, 1.0, 1.0));
        let m = remap_annotations(&g, &wide, &[on_a.clone(), on_b]);
        assert_eq!(rect_of(&m[0]), rect_of(&on_a));
        assert_eq!(rect_of(&m[1]), RectLike::new(1.0, 13.0, 1.0, 1.0));
        assert_eq!(g.part_at(PointLike::new(1.5, 3.5)), Some(1));
    }

    #[test]
    fn fingerprints_tell_frames_apart_and_bake_keeps_pixels() {
        let g = DocGeometry::original(grid(3, 2));
        let r = g.with_transform(g.transform.rotated_cw());
        let f = g.with_transform(g.transform.flipped(FlipAxis::Horizontal));
        assert!(r.fingerprint().is_some());
        assert_ne!(r.fingerprint(), f.fingerprint());
        let baked = r.baked(7).unwrap();
        assert_eq!(baked.parts.len(), 1);
        assert!(baked.transform.is_identity());
        assert!(!baked.is_original());
        assert_eq!(*baked.compose(), *r.compose());
        assert_ne!(baked.fingerprint(), None);
    }

    #[test]
    fn fnv1a64_matches_the_published_test_vectors() {
        let hash = |s: &[u8]| {
            let mut h = Fnv1a64::new();
            h.write(s);
            h.finish()
        };
        assert_eq!(hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn repeated_bakes_keep_the_fingerprint_bounded_and_history_specific() {
        // 50 bake/rotate rounds: the fingerprint must not grow with the
        // number of bakes behind it.
        let bake_chain = |turns: &[bool]| {
            let mut g = DocGeometry::original(grid(3, 2));
            let mut lens = Vec::new();
            for (i, &cw) in turns.iter().enumerate() {
                let t = if cw {
                    g.transform.rotated_cw()
                } else {
                    g.transform.flipped(FlipAxis::Horizontal)
                };
                g = g.with_transform(t).baked(i as PartId + 1).unwrap();
                lens.push(g.fingerprint().unwrap().len());
            }
            (g.fingerprint().unwrap(), lens)
        };
        let ops: Vec<bool> = (0..50).map(|i| i % 3 != 0).collect();
        let (fp, lens) = bake_chain(&ops);
        let max = *lens.iter().max().unwrap();
        assert!(max <= 160, "fingerprint grew to {max} bytes: {fp}");
        assert!(
            lens[10..].iter().all(|&l| l == lens[10]),
            "length still changes with bake count: {lens:?}"
        );
        // Same history, same id; a history differing in one early step
        // (same part ids, same final transform kind), a different id.
        assert_eq!(bake_chain(&ops).0, fp);
        let mut other = ops.clone();
        other[1] = !other[1];
        assert_ne!(bake_chain(&other).0, fp);
        // Two bakes of one original with different pixels never share a
        // fingerprint even with the same fresh part id.
        let g = DocGeometry::original(grid(3, 2));
        let a = g.with_transform(g.transform.rotated_cw()).baked(9).unwrap();
        let b = g
            .with_transform(g.transform.flipped(FlipAxis::Vertical))
            .baked(9)
            .unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint());
        // A bake of the untouched original differs from a bake of any
        // transformed frame.
        let plain = g.baked(9).unwrap();
        assert_ne!(plain.fingerprint(), a.fingerprint());
    }

    #[test]
    fn oversized_documents_are_rejected() {
        let g = DocGeometry::original(grid(3, 2));
        let huge = g.with_transform(g.transform.scaled(20_000.0, 1.0));
        assert_eq!(
            huge.validate(),
            Err(GeometryError::TooLarge {
                width: 60_000,
                height: 2
            })
        );
        assert_eq!(DocGeometry::default().validate(), Err(GeometryError::Empty));
    }

    #[test]
    fn text_boxes_map_and_parts_merge_in_order() {
        use crate::text_lines::{TextLine, WordRect};
        let doc = TextDocument {
            lines: vec![TextLine {
                text: "ab".into(),
                rect: Some(RectLike::new(0.0, 0.0, 2.0, 1.0)),
                words: vec![WordRect {
                    start: 0,
                    end: 2,
                    rect: RectLike::new(0.0, 0.0, 2.0, 1.0),
                }],
            }],
            positioned: true,
            layout_text: "ab".into(),
        };
        let g = DocGeometry::original(grid(3, 2));
        let r = g.with_transform(g.transform.rotated_cw());
        let mapped = map_text_document(&doc, &r.part_affine(ORIGINAL_PART).unwrap());
        assert_eq!(
            mapped.lines[0].rect,
            Some(RectLike::new(1.0, 0.0, 1.0, 2.0))
        );
        assert_eq!(
            mapped.lines[0].words[0].rect,
            RectLike::new(1.0, 0.0, 1.0, 2.0)
        );
        let merged = merge_text_documents(&[doc.clone(), doc]);
        assert_eq!(merged.lines.len(), 2);
        assert_eq!(merged.layout_text, "ab\n\nab");
        assert!(merged.positioned);
    }

    #[test]
    fn pixel_budget_applies_to_the_output_and_exempts_the_untouched_original() {
        // A huge untouched capture opens.
        let huge = Arc::new(RgbaImage::new(12_000, 12_000));
        assert_eq!(DocGeometry::original(huge).validate(), Ok(()));
        // Seven 6016x3384 captures joined Below are 6016x23688 and valid.
        let part = |id| Part {
            id,
            image: Arc::new(RgbaImage::new(6016, 3384)),
            join: Join::Below,
        };
        let seven = DocGeometry {
            parts: (0..7).map(part).collect(),
            ..DocGeometry::default()
        };
        assert_eq!(seven.size(), (6016, 23688));
        assert_eq!(seven.validate(), Ok(()));
        // A layout over the pixel budget (and a side) fits once downscaled.
        let shared = Arc::new(RgbaImage::new(6016, 3384));
        let fourteen = DocGeometry {
            parts: (0..14)
                .map(|id| Part {
                    id,
                    image: Arc::clone(&shared),
                    join: Join::Below,
                })
                .collect(),
            ..DocGeometry::default()
        };
        let l = fourteen.layout();
        assert!(u64::from(l.width) * u64::from(l.height) > MAX_DOC_PIXELS);
        assert!(fourteen.validate().is_err());
        let half = fourteen.with_transform(fourteen.transform.scaled(0.5, 0.5));
        assert_eq!(half.size(), (3008, 23688));
        assert_eq!(half.validate(), Ok(()));
        // Layout sides are capped too.
        let px = Arc::new(RgbaImage::new(1, 1));
        let long = DocGeometry {
            parts: (0..140_000)
                .map(|id| Part {
                    id,
                    image: Arc::clone(&px),
                    join: Join::Beside,
                })
                .collect(),
            ..DocGeometry::default()
        };
        assert!(matches!(
            long.with_transform(long.transform.scaled(0.001, 1.0))
                .validate(),
            Err(GeometryError::TooLarge { .. })
        ));
        // Over the pixel budget gets its own error; over a side keeps TooLarge.
        let g = DocGeometry::original(grid(3, 2));
        let dense = g.with_transform(g.transform.scaled(10_000.0, 10_000.0));
        assert_eq!(
            dense.validate(),
            Err(GeometryError::TooManyPixels {
                pixels: 30_000 * 20_000,
                max: MAX_DOC_PIXELS
            })
        );
    }

    #[test]
    fn baking_an_invalid_document_fails_and_never_reuses_the_original_id() {
        let g = DocGeometry::original(grid(3, 2));
        let bad = g.with_transform(g.transform.scaled(20_000.0, 1.0));
        assert!(matches!(bad.baked(1), Err(GeometryError::TooLarge { .. })));
        assert!(matches!(
            DocGeometry::default().baked(1),
            Err(GeometryError::Empty)
        ));
        let r = g.with_transform(g.transform.rotated_cw());
        let baked = r.baked(ORIGINAL_PART).unwrap();
        assert_ne!(baked.parts[0].id, ORIGINAL_PART);
        assert!(!baked.is_original());
        assert!(baked.fingerprint().is_some());
        assert_eq!(*baked.compose(), *r.compose());
    }

    #[test]
    fn different_bakes_of_one_original_fingerprint_differently() {
        let g = DocGeometry::original(grid(3, 2));
        let r = g.with_transform(g.transform.rotated_cw()).baked(1).unwrap();
        let f = g
            .with_transform(g.transform.flipped(FlipAxis::Horizontal))
            .baked(1)
            .unwrap();
        assert_ne!(r.fingerprint(), f.fingerprint());
        assert_ne!(r.fingerprint(), None);
    }

    #[test]
    fn a_redaction_touching_one_part_follows_that_part_not_the_nearest_center() {
        let big = Part {
            id: ORIGINAL_PART,
            image: grid(100, 100),
            join: Join::Below,
        };
        let small = Part {
            id: 1,
            image: grid(10, 10),
            join: Join::Beside,
        };
        let g = DocGeometry {
            parts: vec![big, small],
            ..DocGeometry::default()
        };
        // Overlaps only part 0 (cols 90..100, rows 50..60); its center is nearer part 1.
        let block = solid_block(RectLike::new(90.0, 50.0, 70.0, 10.0));
        assert_eq!(g.part_at(PointLike::new(125.0, 55.0)), Some(1));
        for new in [
            g.with_layout(40, Align::Start),
            g.with_layout(0, Align::End),
            g.with_layout(0, Align::Center),
        ] {
            let moved = remap_annotations(&g, &new, std::slice::from_ref(&block));
            let want = new
                .part_affine(ORIGINAL_PART)
                .unwrap()
                .map_rect(RectLike::new(90.0, 50.0, 10.0, 10.0));
            assert!(
                moved.iter().any(|m| {
                    let r = rect_of(m);
                    r.x <= want.x
                        && r.y <= want.y
                        && r.x + r.width >= want.x + want.width
                        && r.y + r.height >= want.y + want.height
                }),
                "{new:?} {moved:?}"
            );
        }
    }

    #[test]
    fn a_redaction_across_parts_is_split_and_keeps_every_covered_pixel() {
        let a = Part {
            id: ORIGINAL_PART,
            image: grid(10, 10),
            join: Join::Below,
        };
        let b = Part {
            id: 1,
            image: grid(10, 10),
            join: Join::Below,
        };
        let g = DocGeometry {
            parts: vec![a, b],
            gap: 0,
            ..DocGeometry::default()
        };
        let wide = g.with_layout(40, Align::Start);
        let blocks = remap_annotations(
            &g,
            &wide,
            &[solid_block(RectLike::new(0.0, 8.0, 10.0, 4.0))],
        );
        assert_eq!(blocks.len(), 2);
        assert_eq!(rect_of(&blocks[0]), RectLike::new(0.0, 8.0, 10.0, 2.0));
        assert_eq!(rect_of(&blocks[1]), RectLike::new(0.0, 50.0, 10.0, 2.0));
        // Blur and pixelate redactions split too; a non-redaction keeps the center rule.
        let blur = Annotation::Blur {
            rect: RectLike::new(0.0, 8.0, 10.0, 4.0),
            radius: 5.0,
        };
        assert_eq!(remap_annotations(&g, &wide, &[blur]).len(), 2);
        let stroke = rect_ann(RectLike::new(0.0, 8.0, 10.0, 4.0));
        assert_eq!(remap_annotations(&g, &wide, &[stroke]).len(), 1);
        // A redaction inside one part stays one piece.
        let inside = solid_block(RectLike::new(1.0, 11.0, 2.0, 2.0));
        let m = remap_annotations(&g, &wide, &[inside]);
        assert_eq!(m.len(), 1);
        assert_eq!(rect_of(&m[0]), RectLike::new(1.0, 51.0, 2.0, 2.0));
    }

    #[test]
    fn hostile_transforms_and_gaps_never_panic_or_over_allocate() {
        let img = grid(3, 2);
        let base = DocGeometry::original(Arc::clone(&img));
        for (sx, sy) in [
            (f32::NAN, 1.0),
            (f32::INFINITY, f32::INFINITY),
            (-3.0, 0.0),
            (1e30, 1e30),
        ] {
            let t = Transform {
                quarter_turns: 255,
                scale_x: sx,
                scale_y: sy,
                ..Transform::default()
            };
            let g = base.with_transform(t);
            let _ = g.size();
            let _ = g.layout();
            let _ = g.fingerprint();
            let _ = g.part_rect(ORIGINAL_PART);
            if let Ok(out) = g.try_compose() {
                assert!(u64::from(out.width()) * u64::from(out.height()) <= MAX_DOC_PIXELS);
            }
        }
        let t = Transform {
            quarter_turns: 255,
            ..Transform::default()
        };
        assert_eq!(t.rotated_cw().quarter_turns, 0);
        // Non-finite scale reads as 1.
        let nan = base.with_transform(Transform {
            scale_x: f32::NAN,
            ..Transform::default()
        });
        assert_eq!(nan.size(), (3, 2));
        // A scale that fits one side but not the pixel budget is rejected and composes tiny.
        let big = base.with_transform(Transform::default().scaled(20_000.0, 20_000.0));
        assert!(matches!(
            big.validate(),
            Err(GeometryError::TooLarge { .. })
        ));
        assert!(big.try_compose().is_err());
        // The gap is clamped wherever it came from.
        let a = Part {
            id: 0,
            image: grid(2, 2),
            join: Join::Below,
        };
        let b = Part {
            id: 1,
            image: grid(2, 2),
            join: Join::Below,
        };
        let g = DocGeometry {
            parts: vec![a, b],
            gap: u32::MAX,
            ..DocGeometry::default()
        };
        assert_eq!(g.layout().height, 4 + MAX_GAP);
        // Non-finite affines leave an annotation alone.
        let ann = rect_ann(RectLike::new(1.0, 1.0, 2.0, 2.0));
        let bad = Affine {
            a: f32::NAN,
            ..Affine::IDENTITY
        };
        assert_eq!(map_annotation(&ann, &bad), ann);
    }
}
