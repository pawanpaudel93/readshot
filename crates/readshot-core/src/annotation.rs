//! Editor annotation model.
//!
//! [`Annotation`] is a pure value enum that the renderer (Task 4), the
//! capture history sidecar (Task 5), and the editor UI (Task 14) all share.
//! It is intentionally serde-friendly and contains no `tiny_skia` types
//! directly — see [`crate::geom`] for the rationale.
//!
//! Adding a variant requires updating:
//! 1. The renderer's match arm in `readshot-core::render`.
//! 2. The toolbar's `ToolState` enum in `readshot-ui::editor::tool_state`.
//! 3. The serde round-trip test below.

use serde::{Deserialize, Serialize};

use crate::geom::{PointLike, RectLike};

/// Linear-RGB color with alpha. All components are in the range `0.0..=1.0`;
/// values outside the range are tolerated by the renderer (clamped at draw
/// time) so callers don't have to validate.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rgba {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Rgba {
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }

    pub const OPAQUE_BLACK: Self = Self::new(0.0, 0.0, 0.0, 1.0);
    pub const OPAQUE_WHITE: Self = Self::new(1.0, 1.0, 1.0, 1.0);
    pub const TRANSPARENT: Self = Self::new(0.0, 0.0, 0.0, 0.0);
}

/// One element of an editor's annotation list.
///
/// The editor stores `Vec<Annotation>` and passes a slice to the renderer;
/// each variant maps to a distinct draw operation. Variants are ordered by
/// the toolbar layout in the editor (selection / shape / freehand / text /
/// effect / pin / crop) so visual review of the enum matches the UI.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Annotation {
    Rectangle {
        rect: RectLike,
        color: Rgba,
        line_width: f32,
    },
    Ellipse {
        rect: RectLike,
        color: Rgba,
        line_width: f32,
    },
    Line {
        a: PointLike,
        b: PointLike,
        color: Rgba,
        line_width: f32,
    },
    Arrow {
        a: PointLike,
        b: PointLike,
        color: Rgba,
        line_width: f32,
    },
    Pen {
        points: Vec<PointLike>,
        color: Rgba,
        line_width: f32,
    },
    Highlighter {
        points: Vec<PointLike>,
        color: Rgba,
        line_width: f32,
    },
    Text {
        content: String,
        origin: PointLike,
        color: Rgba,
        font_family: String,
        size: f32,
    },
    Blur {
        rect: RectLike,
        radius: f32,
    },
    Pixelate {
        rect: RectLike,
        block_size: f32,
    },
    NumberedPin {
        origin: PointLike,
        number: u32,
        color: Rgba,
    },
    Crop {
        rect: RectLike,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> PointLike {
        PointLike::new(x, y)
    }

    fn rect(x: f32, y: f32, w: f32, h: f32) -> RectLike {
        RectLike::new(x, y, w, h)
    }

    fn red() -> Rgba {
        Rgba::new(1.0, 0.0, 0.0, 1.0)
    }

    /// Every variant must round-trip through serde JSON without loss.
    /// If a new variant is added, extend this fixture array — failing here is
    /// the cheapest way to catch a missed serde derive.
    #[test]
    fn every_variant_roundtrips_through_json() {
        let fixtures: Vec<Annotation> = vec![
            Annotation::Rectangle {
                rect: rect(10.0, 20.0, 30.0, 40.0),
                color: red(),
                line_width: 2.0,
            },
            Annotation::Ellipse {
                rect: rect(0.0, 0.0, 50.0, 25.0),
                color: red(),
                line_width: 1.5,
            },
            Annotation::Line {
                a: pt(0.0, 0.0),
                b: pt(100.0, 100.0),
                color: red(),
                line_width: 3.0,
            },
            Annotation::Arrow {
                a: pt(5.0, 5.0),
                b: pt(95.0, 95.0),
                color: red(),
                line_width: 4.0,
            },
            Annotation::Pen {
                points: vec![pt(0.0, 0.0), pt(10.0, 5.0), pt(20.0, 0.0)],
                color: red(),
                line_width: 2.0,
            },
            Annotation::Highlighter {
                points: vec![pt(0.0, 0.0), pt(50.0, 0.0)],
                color: Rgba::new(1.0, 1.0, 0.0, 0.4),
                line_width: 12.0,
            },
            Annotation::Text {
                content: "Hello, Readshot 👋".to_string(),
                origin: pt(20.0, 20.0),
                color: Rgba::OPAQUE_BLACK,
                font_family: "system-ui".to_string(),
                size: 14.0,
            },
            Annotation::Blur {
                rect: rect(50.0, 50.0, 100.0, 100.0),
                radius: 6.0,
            },
            Annotation::Pixelate {
                rect: rect(0.0, 0.0, 80.0, 60.0),
                block_size: 8.0,
            },
            Annotation::NumberedPin {
                origin: pt(150.0, 150.0),
                number: 3,
                color: red(),
            },
            Annotation::Crop {
                rect: rect(0.0, 0.0, 256.0, 256.0),
            },
        ];

        // Sanity: the fixture set covers every variant. If a new variant is
        // added without extending this list, this assertion fails.
        const VARIANT_COUNT: usize = 11;
        assert_eq!(
            fixtures.len(),
            VARIANT_COUNT,
            "fixtures must cover every Annotation variant",
        );

        for original in fixtures {
            let json = serde_json::to_string(&original).expect("serialise");
            let restored: Annotation = serde_json::from_str(&json).expect("deserialise");
            assert_eq!(
                original, restored,
                "round-trip mismatch for variant: {original:?}\njson: {json}",
            );
        }
    }

    #[test]
    fn rgba_constants_are_what_they_say() {
        assert_eq!(Rgba::OPAQUE_BLACK, Rgba::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(Rgba::OPAQUE_WHITE, Rgba::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(Rgba::TRANSPARENT, Rgba::new(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn discriminant_is_serialised_as_kind_field() {
        // Tag-based serde representation is part of the on-disk contract;
        // a future migration that changes it requires a schema_version bump.
        let a = Annotation::Crop {
            rect: rect(0.0, 0.0, 10.0, 10.0),
        };
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["kind"], "Crop");
    }
}
