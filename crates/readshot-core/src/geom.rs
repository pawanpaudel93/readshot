//! Geometry primitives used throughout Readshot.
//!
//! Two parallel type families live here:
//!
//! * [`Point`] / [`Rect`] are aliases over `tiny_skia` so the renderer can
//!   construct paths and paints without conversion overhead.
//! * [`PointLike`] / [`RectLike`] are serde-friendly value types used by the
//!   `Annotation` enum so capture-history sidecar JSON and TOML preferences
//!   round-trip cleanly. `tiny_skia` types do not implement `serde`.
//!
//! Conversions between the two families are infallible for points and
//! fallible for rectangles (`tiny_skia::Rect` rejects zero / non-finite
//! dimensions).

use serde::{Deserialize, Serialize};

/// Renderer-level point type — alias of `tiny_skia::Point`.
pub type Point = tiny_skia::Point;

/// Renderer-level rectangle — alias of `tiny_skia::Rect`.
pub type Rect = tiny_skia::Rect;

/// Serde-compatible point. Use this in any value that round-trips through
/// JSON or TOML.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PointLike {
    pub x: f32,
    pub y: f32,
}

impl PointLike {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// Serde-compatible rectangle. Always stores `x`, `y` (top-left corner) and
/// `width`, `height`. Negative or non-finite dimensions are rejected at
/// conversion time.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RectLike {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl RectLike {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self { x, y, width, height }
    }
}

impl From<PointLike> for Point {
    fn from(p: PointLike) -> Self {
        Point::from_xy(p.x, p.y)
    }
}

impl From<Point> for PointLike {
    fn from(p: Point) -> Self {
        Self { x: p.x, y: p.y }
    }
}

impl TryFrom<RectLike> for Rect {
    type Error = InvalidRect;

    fn try_from(r: RectLike) -> Result<Self, Self::Error> {
        Rect::from_xywh(r.x, r.y, r.width, r.height).ok_or(InvalidRect {
            x: r.x,
            y: r.y,
            width: r.width,
            height: r.height,
        })
    }
}

impl From<Rect> for RectLike {
    fn from(r: Rect) -> Self {
        Self {
            x: r.x(),
            y: r.y(),
            width: r.width(),
            height: r.height(),
        }
    }
}

/// Returned when a [`RectLike`] cannot be converted to a `tiny_skia::Rect`
/// because its dimensions are not strictly positive or are non-finite.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("invalid rect: x={x} y={y} width={width} height={height}")]
pub struct InvalidRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Intersect `rect` with `bounds` and return the clipped rectangle. Returns
/// `None` if the two do not overlap or the resulting region is degenerate.
///
/// Used at every entry point that takes a user-supplied selection (capture
/// overlay → Capturer, CLI `--rect` argument, MCP `capture_region` tool) so
/// downstream code never sees a region that escapes the display.
pub fn clamp_rect(rect: Rect, bounds: Rect) -> Option<Rect> {
    let x0 = rect.x().max(bounds.x());
    let y0 = rect.y().max(bounds.y());
    let x1 = rect.right().min(bounds.right());
    let y1 = rect.bottom().min(bounds.bottom());
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Rect::from_ltrb(x0, y0, x1, y1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_xywh(x, y, w, h).expect("test rect must be valid")
    }

    #[test]
    fn point_roundtrip_through_tiny_skia() {
        let pl = PointLike::new(3.5, -2.0);
        let ts: Point = pl.into();
        let back: PointLike = ts.into();
        assert_eq!(pl, back);
    }

    #[test]
    fn rect_roundtrip_through_tiny_skia() {
        let rl = RectLike::new(10.0, 20.0, 30.0, 40.0);
        let ts: Rect = rl.try_into().unwrap();
        let back: RectLike = ts.into();
        assert_eq!(rl, back);
    }

    #[test]
    fn rect_with_negative_dimensions_rejected() {
        // Negative width inverts the L/R relationship, which `tiny_skia::Rect`
        // refuses. (Zero-width is a degenerate-but-accepted edge case in
        // tiny-skia 0.11; we don't test it here so a future tiny-skia change
        // doesn't churn the test.)
        let rl = RectLike::new(0.0, 0.0, -10.0, 10.0);
        let result: Result<Rect, _> = rl.try_into();
        assert!(result.is_err(), "negative width must be rejected");
    }

    #[test]
    fn rect_with_nan_rejected() {
        let rl = RectLike::new(f32::NAN, 0.0, 10.0, 10.0);
        let result: Result<Rect, _> = rl.try_into();
        assert!(result.is_err(), "non-finite coordinates must be rejected");
    }

    #[test]
    fn clamp_fully_inside_returns_unchanged() {
        let bounds = r(0.0, 0.0, 100.0, 100.0);
        let rect = r(10.0, 10.0, 20.0, 20.0);
        assert_eq!(clamp_rect(rect, bounds), Some(rect));
    }

    #[test]
    fn clamp_partial_overlap_returns_intersection() {
        let bounds = r(0.0, 0.0, 100.0, 100.0);
        let rect = r(80.0, 80.0, 50.0, 50.0);
        assert_eq!(clamp_rect(rect, bounds), Some(r(80.0, 80.0, 20.0, 20.0)));
    }

    #[test]
    fn clamp_no_overlap_returns_none() {
        let bounds = r(0.0, 0.0, 100.0, 100.0);
        let rect = r(200.0, 200.0, 50.0, 50.0);
        assert_eq!(clamp_rect(rect, bounds), None);
    }

    #[test]
    fn clamp_touching_edge_returns_none() {
        // Boundary at x=100; rect starts at x=100 — zero-width intersection.
        let bounds = r(0.0, 0.0, 100.0, 100.0);
        let rect = r(100.0, 0.0, 50.0, 50.0);
        assert_eq!(clamp_rect(rect, bounds), None);
    }
}
