//! Arrowhead geometry helper.
//!
//! [`AnnotationRenderer`](crate::render::render) draws an `Annotation::Arrow`
//! by stroking a line segment for the shaft and filling a triangle for the
//! head. This module owns the head's geometry so the renderer is concerned
//! only with paint and path construction.
//!
//! The head sits at the line's terminus (`b`) and points away from the
//! origin (`a`). Its length and width scale with the shaft's `line_width`,
//! which keeps annotations visually consistent across stroke weights.

use crate::geom::PointLike;

/// Three vertices of an arrowhead triangle in pixel coordinates. `tip` is
/// the head's apex (the line's terminus); `left` and `right` are the
/// barb corners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ArrowheadTriangle {
    pub tip: PointLike,
    pub left: PointLike,
    pub right: PointLike,
}

/// Compute the arrowhead triangle for a line segment.
///
/// `from` and `to` are the shaft's endpoints in pixel space. `line_width`
/// is the shaft's stroke width — the head is sized at `5×` that, which
/// reads as a clear arrow at typical annotation widths (2–8 px) without
/// dwarfing thin strokes.
///
/// If the segment has zero length the function returns a degenerate
/// triangle at `to`; callers can detect this via `is_degenerate` and skip
/// drawing.
pub fn compute(from: PointLike, to: PointLike, line_width: f32) -> ArrowheadTriangle {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = (dx * dx + dy * dy).sqrt();

    if length < f32::EPSILON {
        return ArrowheadTriangle {
            tip: to,
            left: to,
            right: to,
        };
    }

    // Unit vector along the shaft (pointing forward toward the tip).
    let ux = dx / length;
    let uy = dy / length;
    // Perpendicular unit vector (rotated 90° counter-clockwise).
    let px = -uy;
    let py = ux;

    let head_length = (line_width * 5.0).max(8.0);
    let head_half_width = (line_width * 2.5).max(4.0);

    // Base point sits `head_length` back along the shaft from the tip.
    let base_x = to.x - ux * head_length;
    let base_y = to.y - uy * head_length;

    let left = PointLike::new(
        base_x + px * head_half_width,
        base_y + py * head_half_width,
    );
    let right = PointLike::new(
        base_x - px * head_half_width,
        base_y - py * head_half_width,
    );

    ArrowheadTriangle {
        tip: to,
        left,
        right,
    }
}

impl ArrowheadTriangle {
    pub fn is_degenerate(&self) -> bool {
        self.tip == self.left && self.tip == self.right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> PointLike {
        PointLike::new(x, y)
    }

    #[test]
    fn horizontal_arrow_has_symmetric_barbs() {
        let head = compute(pt(0.0, 0.0), pt(100.0, 0.0), 2.0);
        // Tip at (100, 0). Base at (100 - 10, 0) = (90, 0). Half-width 5.
        assert_eq!(head.tip, pt(100.0, 0.0));
        // Left and right barbs are symmetric around the shaft (y=0).
        assert!((head.left.y + head.right.y).abs() < 1e-3);
        assert!((head.left.x - head.right.x).abs() < 1e-3);
    }

    #[test]
    fn diagonal_arrow_orients_correctly() {
        let head = compute(pt(0.0, 0.0), pt(10.0, 10.0), 1.0);
        // Tip at (10, 10). The base direction is normalised (1/√2, 1/√2).
        assert_eq!(head.tip, pt(10.0, 10.0));
        // Base point ≈ (10 - 8/√2, 10 - 8/√2) ≈ (4.34, 4.34) when min head
        // length kicks in (=8). Confirm barb x and y differ from tip.
        assert!(head.left.x < head.tip.x);
        assert!(head.left.y < head.tip.y);
    }

    #[test]
    fn zero_length_returns_degenerate() {
        let head = compute(pt(50.0, 50.0), pt(50.0, 50.0), 2.0);
        assert!(head.is_degenerate());
    }

    #[test]
    fn min_size_floor_protects_thin_strokes() {
        // A 0.5 px stroke would otherwise produce a 2.5 px head; the floor
        // enforces an 8 px head so thin annotations stay readable.
        let head = compute(pt(0.0, 0.0), pt(100.0, 0.0), 0.5);
        let head_length_x = head.tip.x - head.left.x.max(head.right.x);
        assert!(head_length_x >= 7.0, "head_length_x = {head_length_x}");
    }
}
