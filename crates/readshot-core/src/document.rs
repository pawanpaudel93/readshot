//! The editor document (spec §5): parts laid out with a gap and an
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
/// Pixel budget of a document (and of its pre-transform layout), about
/// 512 MB of RGBA. Guards allocation against hand-edited or corrupt
/// sidecars.
pub const MAX_DOC_PIXELS: u64 = 128_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GeometryError {
    #[error("the image would be {width} × {height} px, larger than Readshot can render")]
    TooLarge { width: u32, height: u32 },
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
/// goes through [`Transform::sanitized`] first.
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
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
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
        Self {
            quarter_turns: (self.quarter_turns % 4 + 1) % 4,
            flip_h: self.flip_v,
            flip_v: self.flip_h,
            scale_x: self.scale_y,
            scale_y: self.scale_x,
        }
    }

    pub fn rotated_ccw(self) -> Self {
        self.rotated_cw().rotated_cw().rotated_cw()
    }

    pub fn flipped(self, axis: FlipAxis) -> Self {
        match axis {
            FlipAxis::Horizontal => Self {
                flip_h: !self.flip_h,
                ..self
            },
            FlipAxis::Vertical => Self {
                flip_v: !self.flip_v,
                ..self
            },
        }
    }

    pub fn scaled(self, sx: f32, sy: f32) -> Self {
        Self {
            scale_x: self.scale_x * sx,
            scale_y: self.scale_y * sy,
            ..self
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
}

fn clamp_u32(v: i64) -> u32 {
    u32::try_from(v.max(0)).unwrap_or(u32::MAX)
}

fn within_limits(w: u32, h: u32) -> bool {
    w <= MAX_DOC_SIDE && h <= MAX_DOC_SIDE && u64::from(w) * u64::from(h) <= MAX_DOC_PIXELS
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

    /// The document image. The original geometry hands out the part's
    /// own `Arc` (no copy).
    pub fn compose(&self) -> Arc<RgbaImage> {
        if self.parts.len() == 1 && self.transform.is_identity() {
            return Arc::clone(&self.parts[0].image);
        }
        // Never allocate past the limits (a sidecar can carry any scale):
        // an invalid document composes to one transparent pixel.
        let l = self.layout();
        if self.parts.is_empty() || !within_limits(l.width, l.height) || self.validate().is_err() {
            return Arc::new(RgbaImage::new(1, 1));
        }
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
        Arc::new(img)
    }

    pub fn validate(&self) -> Result<(), GeometryError> {
        if self.parts.is_empty() {
            return Err(GeometryError::Empty);
        }
        let l = self.layout();
        if !within_limits(l.width, l.height) {
            return Err(GeometryError::TooLarge {
                width: l.width,
                height: l.height,
            });
        }
        let (width, height) = self.size();
        if !within_limits(width, height) {
            return Err(GeometryError::TooLarge { width, height });
        }
        Ok(())
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
            "parts={};gap={};align={:?};rot={};fh={};fv={};sx={:.6};sy={:.6}",
            parts.join(","),
            self.gap.min(MAX_GAP),
            self.align,
            t.quarter_turns,
            t.flip_h,
            t.flip_v,
            t.scale_x,
            t.scale_y
        ))
    }

    pub fn with_transform(&self, transform: Transform) -> Self {
        Self {
            transform,
            ..self.clone()
        }
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
    /// before a part is added to a transformed document (decision D4).
    pub fn baked(&self, id: PartId) -> Self {
        Self {
            parts: vec![Part {
                id,
                image: self.compose(),
                join: Join::Below,
            }],
            gap: self.gap,
            align: self.align,
            transform: Transform::default(),
        }
    }
}

fn center(r: RectLike) -> PointLike {
    PointLike::new(r.x + r.width * 0.5, r.y + r.height * 0.5)
}

fn dist2(a: PointLike, b: PointLike) -> f32 {
    (a.x - b.x) * (a.x - b.x) + (a.y - b.y) * (a.y - b.y)
}

/// Outward to whole pixels, so an anti-aliased edge never lets a partly
/// covered pixel through (same rule as `redact::redaction_rect`).
fn snap_out(r: RectLike) -> RectLike {
    let (x0, y0) = (r.x.floor(), r.y.floor());
    let (x1, y1) = ((r.x + r.width).ceil(), (r.y + r.height).ceil());
    RectLike::new(x0, y0, x1 - x0, y1 - y0)
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
    }
}

/// `a` moved by `m`. Lengths (stroke widths, radii, text size) scale by
/// `m.length_scale()`; pins and stamps keep their fixed size; text stays
/// upright, centred where its box went. Redactions snap outward.
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
            *rect = snap_out(m.map_rect(*rect));
            *radius = (*radius * ls).max(2.0);
        }
        Annotation::Pixelate { rect, block_size } => {
            *rect = snap_out(m.map_rect(*rect));
            *block_size = (*block_size * ls).max(4.0);
        }
        Annotation::Spotlight { rect, .. } | Annotation::Crop { rect } => *rect = m.map_rect(*rect),
        Annotation::NumberedPin { origin, .. } | Annotation::Stamp { origin, .. } => {
            *origin = m.apply(*origin)
        }
    }
    if crate::scrub::is_redaction(&out) {
        if let Annotation::Rectangle { rect, .. } = &mut out {
            *rect = snap_out(*rect);
        }
    }
    out
}

/// `anns` (document px of `old`) in document px of `new`. Each annotation
/// moves with the part under its anchor; parts missing from either side
/// leave it where it is.
pub fn remap_annotations(
    old: &DocGeometry,
    new: &DocGeometry,
    anns: &[Annotation],
) -> Vec<Annotation> {
    anns.iter()
        .map(|a| {
            let m = old
                .part_at(annotation_anchor(a))
                .and_then(|id| Some((old.part_affine(id)?, new.part_affine(id)?)))
                .and_then(|(o, n)| Some(o.inverse()?.then(&n)))
                .unwrap_or(Affine::IDENTITY);
            if m == Affine::IDENTITY {
                a.clone()
            } else {
                map_annotation(a, &m)
            }
        })
        .collect()
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
    fn resize_scales_geometry_widths_and_snaps_redactions_outward() {
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
        // 0.3..1.2 → snapped out to 0..2.
        assert_eq!(rect_of(&block[0]), RectLike::new(0.0, 0.0, 2.0, 2.0));
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
        let baked = r.baked(7);
        assert_eq!(baked.parts.len(), 1);
        assert!(baked.transform.is_identity());
        assert!(!baked.is_original());
        assert_eq!(*baked.compose(), *r.compose());
        assert_ne!(baked.fingerprint(), None);
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
            let out = g.compose();
            assert!(u64::from(out.width()) * u64::from(out.height()) <= MAX_DOC_PIXELS);
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
        assert_eq!(big.compose().dimensions(), (1, 1));
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
