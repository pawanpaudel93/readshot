//! Magnifier callout geometry (spec §3.2). Pure; `render` draws it and
//! the editor hit-tests with it.

use crate::annotation::MagnifierShape;
use crate::geom::{PointLike, RectLike};

pub const MAGNIFIER_ZOOMS: [f32; 3] = [2.0, 3.0, 4.0];
pub const DEFAULT_ZOOM: f32 = 2.0;
pub const MIN_SOURCE_RADIUS: f32 = 8.0;
/// Space between the source ring and the bubble when placed.
pub const BUBBLE_MARGIN: f32 = 24.0;

pub fn bubble_radius(radius: f32, zoom: f32) -> f32 {
    radius * zoom
}

/// Corner radius of a rounded-square bubble.
pub fn corner_radius(bubble_r: f32) -> f32 {
    bubble_r * 0.25
}

fn fits(c: PointLike, r: f32, v: RectLike) -> bool {
    c.x - r >= v.x && c.y - r >= v.y && c.x + r <= v.x + v.width && c.y + r <= v.y + v.height
}

/// Where a new bubble goes: diagonally up-right of the source, clear of
/// it; else up-left, down-right, down-left — the first that fits inside
/// `visible`; else the first, clamped inside.
pub fn default_bubble(source: PointLike, radius: f32, zoom: f32, visible: RectLike) -> PointLike {
    let br = bubble_radius(radius, zoom);
    let d = (radius + br + BUBBLE_MARGIN) / std::f32::consts::SQRT_2;
    let candidates = [(d, -d), (-d, -d), (d, d), (-d, d)];
    for (dx, dy) in candidates {
        let c = PointLike::new(source.x + dx, source.y + dy);
        if fits(c, br, visible) {
            return c;
        }
    }
    let c = PointLike::new(source.x + d, source.y - d);
    let clamp = |v: f32, lo: f32, hi: f32| {
        if lo > hi {
            (lo + hi) * 0.5
        } else {
            v.clamp(lo, hi)
        }
    };
    PointLike::new(
        clamp(c.x, visible.x + br, visible.x + visible.width - br),
        clamp(c.y, visible.y + br, visible.y + visible.height - br),
    )
}

/// The dashed leader, source rim → bubble rim; `None` when they overlap.
pub fn leader_endpoints(
    source: PointLike,
    radius: f32,
    bubble: PointLike,
    bubble_r: f32,
) -> Option<(PointLike, PointLike)> {
    let (dx, dy) = (bubble.x - source.x, bubble.y - source.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len <= radius + bubble_r {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    Some((
        PointLike::new(source.x + ux * radius, source.y + uy * radius),
        PointLike::new(bubble.x - ux * bubble_r, bubble.y - uy * bubble_r),
    ))
}

pub fn bounds(source: PointLike, radius: f32, bubble: PointLike, zoom: f32) -> RectLike {
    let br = bubble_radius(radius, zoom);
    let x0 = (source.x - radius).min(bubble.x - br);
    let y0 = (source.y - radius).min(bubble.y - br);
    let x1 = (source.x + radius).max(bubble.x + br);
    let y1 = (source.y + radius).max(bubble.y + br);
    RectLike::new(x0, y0, x1 - x0, y1 - y0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MagnifierHit {
    /// On the source ring: drag to change the radius.
    SourceRim,
    /// Inside the source: drag to move what is magnified.
    Source,
    /// Inside the bubble: drag to move the bubble.
    Bubble,
}

/// Whether the offset `(dx, dy)` from the bubble centre lies inside a
/// bubble of radius `br`.
pub fn in_bubble(shape: MagnifierShape, dx: f32, dy: f32, br: f32) -> bool {
    match shape {
        MagnifierShape::Circle => dx * dx + dy * dy <= br * br,
        MagnifierShape::RoundedSquare => {
            let (ax, ay) = (dx.abs(), dy.abs());
            if ax > br || ay > br {
                return false;
            }
            let r = corner_radius(br);
            let (cx, cy) = (ax - (br - r), ay - (br - r));
            cx <= 0.0 || cy <= 0.0 || cx * cx + cy * cy <= r * r
        }
    }
}

pub fn hit(
    source: PointLike,
    radius: f32,
    bubble: PointLike,
    zoom: f32,
    shape: MagnifierShape,
    p: PointLike,
    tol: f32,
) -> Option<MagnifierHit> {
    let (dx, dy) = (p.x - source.x, p.y - source.y);
    let d = (dx * dx + dy * dy).sqrt();
    if (d - radius).abs() <= tol {
        return Some(MagnifierHit::SourceRim);
    }
    if d < radius {
        return Some(MagnifierHit::Source);
    }
    in_bubble(
        shape,
        p.x - bubble.x,
        p.y - bubble.y,
        bubble_radius(radius, zoom),
    )
    .then_some(MagnifierHit::Bubble)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bubble_sits_up_right_and_stays_inside_when_it_can() {
        let visible = RectLike::new(0.0, 0.0, 400.0, 300.0);
        let b = default_bubble(PointLike::new(100.0, 200.0), 10.0, 2.0, visible);
        assert!(b.x > 100.0 && b.y < 200.0, "{b:?}");
        // Near the top-right corner it flips to a side that fits.
        let b = default_bubble(PointLike::new(390.0, 10.0), 10.0, 2.0, visible);
        assert!(
            b.x - 20.0 >= 0.0 && b.y + 20.0 <= 300.0 && b.x < 390.0 && b.y > 10.0,
            "{b:?}"
        );
    }

    #[test]
    fn leader_runs_rim_to_rim_and_vanishes_on_overlap() {
        let (a, b) = leader_endpoints(
            PointLike::new(0.0, 0.0),
            2.0,
            PointLike::new(10.0, 0.0),
            3.0,
        )
        .unwrap();
        assert_eq!((a, b), (PointLike::new(2.0, 0.0), PointLike::new(7.0, 0.0)));
        assert!(leader_endpoints(
            PointLike::new(0.0, 0.0),
            6.0,
            PointLike::new(10.0, 0.0),
            6.0
        )
        .is_none());
    }

    #[test]
    fn hits_rim_source_and_bubble() {
        let (s, b) = (PointLike::new(10.0, 10.0), PointLike::new(50.0, 10.0));
        let hit = |x, y| {
            hit(
                s,
                5.0,
                b,
                2.0,
                MagnifierShape::Circle,
                PointLike::new(x, y),
                1.0,
            )
        };
        assert_eq!(hit(15.0, 10.0), Some(MagnifierHit::SourceRim));
        assert_eq!(hit(10.0, 10.0), Some(MagnifierHit::Source));
        assert_eq!(hit(55.0, 12.0), Some(MagnifierHit::Bubble));
        assert_eq!(hit(30.0, 30.0), None);
    }
}
