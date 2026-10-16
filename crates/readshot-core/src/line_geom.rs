//! Pure geometry for styled lines and arrows (spec §3.3).
//!
//! The renderer, the editor's hit-testing and the canvas preview all
//! call these functions so a curve or a hand-drawn wobble is the same
//! shape everywhere. Hand-drawn jitter comes from a small xorshift PRNG
//! seeded by a value stored in the annotation — no `rand`, and the same
//! seed always produces the same path, so re-renders and moves are
//! stable.

use crate::annotation::LinePath;
use crate::geom::PointLike;

/// Default bend for a newly curved line: the apex sits 15 % of the
/// chord length off the straight line.
pub const DEFAULT_BEND: f32 = 0.3;

/// Samples per curve. Dense enough that the polyline reads as smooth at
/// any stroke width the editor allows.
pub const CURVE_SEGMENTS: usize = 32;

/// FNV-1a over the anchor's coordinate bits. Used once, when a
/// hand-drawn line is created, and stored as its seed.
pub fn seed_for(anchor: PointLike) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for v in [anchor.x, anchor.y] {
        for byte in v.to_bits().to_le_bytes() {
            h ^= byte as u32;
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}

/// Deterministic xorshift32 stream.
pub struct Jitter(u32);

impl Jitter {
    pub fn new(seed: u32) -> Self {
        // xorshift has a fixed point at zero.
        Self(if seed == 0 { 0x6D2B_79F5 } else { seed })
    }

    /// Next value in `[-1, 1]`.
    pub fn next_signed(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
    }
}

/// Left unit normal of `a → b` and the chord length, or `None` for a
/// zero-length chord.
fn normal_and_len(a: PointLike, b: PointLike) -> Option<(f32, f32, f32)> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    (len > f32::EPSILON).then(|| (-dy / len, dx / len, len))
}

fn lerp(a: PointLike, b: PointLike, t: f32) -> PointLike {
    PointLike::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn quad_point(a: PointLike, c: PointLike, b: PointLike, t: f32) -> PointLike {
    let u = 1.0 - t;
    PointLike::new(
        u * u * a.x + 2.0 * u * t * c.x + t * t * b.x,
        u * u * a.y + 2.0 * u * t * c.y + t * t * b.y,
    )
}

/// Quadratic control point for `bend`.
pub fn curve_control(a: PointLike, b: PointLike, bend: f32) -> PointLike {
    let mid = lerp(a, b, 0.5);
    match normal_and_len(a, b) {
        Some((nx, ny, len)) => PointLike::new(mid.x + nx * bend * len, mid.y + ny * bend * len),
        None => mid,
    }
}

/// The `bend` whose control point is `c` (inverse of [`curve_control`]).
/// Used when an affine map moves a curve's endpoints and control point.
pub fn bend_for_control(a: PointLike, b: PointLike, c: PointLike) -> f32 {
    let mid = lerp(a, b, 0.5);
    match normal_and_len(a, b) {
        Some((nx, ny, len)) => ((c.x - mid.x) * nx + (c.y - mid.y) * ny) / len,
        None => 0.0,
    }
}

/// Point of the curve at t = 0.5 — where the Bend handle sits.
pub fn curve_apex(a: PointLike, b: PointLike, bend: f32) -> PointLike {
    quad_point(a, curve_control(a, b, bend), b, 0.5)
}

/// New bend after the apex handle is dragged by `(dx, dy)`. The apex
/// moves by half the control point's offset, so the bend changes by
/// twice the drag's normal component over the chord length.
pub fn bend_after_drag(a: PointLike, b: PointLike, bend: f32, dx: f32, dy: f32) -> f32 {
    match normal_and_len(a, b) {
        Some((nx, ny, len)) => (bend + 2.0 * (dx * nx + dy * ny) / len).clamp(-2.0, 2.0),
        None => bend,
    }
}

/// Polyline that strokes `path` from `a` to `b`. Endpoints are exact.
pub fn path_points(a: PointLike, b: PointLike, path: LinePath, line_width: f32) -> Vec<PointLike> {
    match path {
        LinePath::Straight => vec![a, b],
        LinePath::Curved { bend } => {
            let c = curve_control(a, b, bend);
            (0..=CURVE_SEGMENTS)
                .map(|i| quad_point(a, c, b, i as f32 / CURVE_SEGMENTS as f32))
                .collect()
        }
        LinePath::HandDrawn { seed } => hand_drawn_points(a, b, seed, line_width),
    }
}

fn hand_drawn_points(a: PointLike, b: PointLike, seed: u32, line_width: f32) -> Vec<PointLike> {
    let Some((nx, ny, len)) = normal_and_len(a, b) else {
        return vec![a, b];
    };
    let n = ((len / 14.0).round() as usize).clamp(4, 64);
    let amp = (line_width * 0.7).clamp(1.0, 4.0);
    let mut rng = Jitter::new(seed);
    (0..=n)
        .map(|i| {
            let base = lerp(a, b, i as f32 / n as f32);
            if i == 0 || i == n {
                base
            } else {
                let off = rng.next_signed() * amp;
                PointLike::new(base.x + nx * off, base.y + ny * off)
            }
        })
        .collect()
}

/// `(from, to)` for the head at `b`. Curves aim along the end tangent
/// (control → b); straight and hand-drawn lines aim along the chord.
pub fn end_tangent(a: PointLike, b: PointLike, path: LinePath) -> (PointLike, PointLike) {
    match path {
        LinePath::Curved { bend } => (curve_control(a, b, bend), b),
        _ => (a, b),
    }
}

/// `(from, to)` for the head at `a` (two-headed arrows).
pub fn start_tangent(a: PointLike, b: PointLike, path: LinePath) -> (PointLike, PointLike) {
    match path {
        LinePath::Curved { bend } => (curve_control(a, b, bend), a),
        _ => (b, a),
    }
}

/// `[on, off]` dash lengths in image pixels.
pub fn dash_pattern(line_width: f32) -> [f32; 2] {
    [(line_width * 3.0).max(6.0), (line_width * 2.0).max(4.0)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> PointLike {
        PointLike::new(x, y)
    }

    fn close(a: PointLike, b: PointLike) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn straight_path_is_the_two_endpoints() {
        let pts = path_points(pt(0.0, 0.0), pt(10.0, 0.0), LinePath::Straight, 2.0);
        assert_eq!(pts, vec![pt(0.0, 0.0), pt(10.0, 0.0)]);
    }

    #[test]
    fn curved_path_peaks_at_the_apex() {
        let (a, b) = (pt(0.0, 0.0), pt(100.0, 0.0));
        let pts = path_points(a, b, LinePath::Curved { bend: 0.4 }, 2.0);
        assert!(close(pts[0], a) && close(*pts.last().unwrap(), b));
        let apex = curve_apex(a, b, 0.4);
        assert!(close(pts[CURVE_SEGMENTS / 2], apex));
        // Left normal of +x is +y: control at y = 40, apex at y = 20.
        assert!((apex.y - 20.0).abs() < 1e-3, "{apex:?}");
    }

    #[test]
    fn hand_drawn_is_deterministic_per_seed() {
        let (a, b) = (pt(0.0, 0.0), pt(200.0, 50.0));
        let one = path_points(a, b, LinePath::HandDrawn { seed: 7 }, 3.0);
        let again = path_points(a, b, LinePath::HandDrawn { seed: 7 }, 3.0);
        let other = path_points(a, b, LinePath::HandDrawn { seed: 8 }, 3.0);
        assert_eq!(one, again);
        assert_ne!(one, other);
        assert!(one.len() > 2);
        assert!(close(one[0], a) && close(*one.last().unwrap(), b));
    }

    #[test]
    fn hand_drawn_moves_rigidly_with_its_endpoints() {
        let seed = LinePath::HandDrawn { seed: 42 };
        let pts = path_points(pt(0.0, 0.0), pt(120.0, 40.0), seed, 2.0);
        let moved = path_points(pt(10.0, -5.0), pt(130.0, 35.0), seed, 2.0);
        assert_eq!(pts.len(), moved.len());
        for (p, q) in pts.iter().zip(&moved) {
            assert!(close(pt(p.x + 10.0, p.y - 5.0), *q), "{p:?} vs {q:?}");
        }
    }

    #[test]
    fn hand_drawn_jitter_stays_near_the_chord() {
        let pts = path_points(
            pt(0.0, 0.0),
            pt(300.0, 0.0),
            LinePath::HandDrawn { seed: 3 },
            8.0,
        );
        assert!(pts.iter().all(|p| p.y.abs() <= 4.0 + 1e-3));
    }

    #[test]
    fn bend_drag_tracks_the_pointer() {
        let (a, b) = (pt(0.0, 0.0), pt(100.0, 0.0));
        let bend = bend_after_drag(a, b, 0.4, 0.0, 10.0);
        assert!((curve_apex(a, b, bend).y - 30.0).abs() < 1e-3);
        // A drag along the chord does not change the bend.
        assert!((bend_after_drag(a, b, 0.4, 25.0, 0.0) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn seed_for_is_stable_and_position_sensitive() {
        assert_eq!(seed_for(pt(1.5, 2.5)), seed_for(pt(1.5, 2.5)));
        assert_ne!(seed_for(pt(1.5, 2.5)), seed_for(pt(2.5, 1.5)));
    }

    #[test]
    fn jitter_stays_in_range() {
        let mut j = Jitter::new(0);
        for _ in 0..1000 {
            let v = j.next_signed();
            assert!((-1.0..=1.0).contains(&v));
        }
    }

    #[test]
    fn dash_pattern_scales_with_width_and_has_floors() {
        assert_eq!(dash_pattern(1.0), [6.0, 4.0]);
        assert_eq!(dash_pattern(4.0), [12.0, 8.0]);
    }
}
