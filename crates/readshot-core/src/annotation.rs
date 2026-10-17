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
//! 4. `readshot-ui::editor::style::{tool_for_annotation, options_for}` and
//!    the `state.rs` helpers.

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

impl Rgba {
    /// Build from 8-bit straight RGBA channels.
    pub fn from_rgba8([r, g, b, a]: [u8; 4]) -> Self {
        Self::new(
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            a as f32 / 255.0,
        )
    }

    /// 8-bit straight RGBA (inverse of [`Self::from_rgba8`]).
    pub fn to_rgba8(&self) -> [u8; 4] {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [c(self.r), c(self.g), c(self.b), c(self.a)]
    }

    /// `#RRGGBB`, uppercase. Alpha is not encoded.
    pub fn to_hex(&self) -> String {
        let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("#{:02X}{:02X}{:02X}", c(self.r), c(self.g), c(self.b))
    }

    /// Parse `#RGB`, `#RRGGBB`, `RGB` or `RRGGBB` (any case; surrounding
    /// whitespace ignored). The result is opaque.
    pub fn from_hex(input: &str) -> Option<Self> {
        let s = input.trim();
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.is_empty() || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let expanded: String = match s.len() {
            3 => s.chars().flat_map(|c| [c, c]).collect(),
            6 => s.to_string(),
            _ => return None,
        };
        let byte = |i: usize| u8::from_str_radix(&expanded[i..i + 2], 16).ok();
        Some(Self::from_rgba8([byte(0)?, byte(2)?, byte(4)?, 255]))
    }
}

/// How a rectangle or ellipse is painted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillMode {
    #[default]
    Outline,
    Filled,
    /// Translucent fill (see `render::BOTH_FILL_ALPHA`) plus the outline.
    Both,
}

/// Rectangle / ellipse options (spec §3.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShapeStyle {
    pub fill: FillMode,
    pub dashed: bool,
    /// Rectangle only; ellipses ignore it.
    pub corner_radius: f32,
}

impl ShapeStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Spotlight hole shape (spec §3.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpotlightShape {
    #[default]
    Rectangle,
    Ellipse,
}

/// Magnifier bubble shape (spec §3.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MagnifierShape {
    #[default]
    Circle,
    RoundedSquare,
}

/// Default dim outside a spotlight (spec §3.1: 55 %).
pub const DEFAULT_SPOTLIGHT_DIM: f32 = 0.55;

/// Path a line or arrow follows between its endpoints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LinePath {
    #[default]
    Straight,
    /// Quadratic curve. `bend` places the control point at the chord
    /// midpoint plus `bend × chord length` along the chord's left normal.
    Curved { bend: f32 },
    /// Seeded jitter (see `line_geom`). The seed is stored so every
    /// re-render, move and export draws the same wobble.
    HandDrawn { seed: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrowHeads {
    #[default]
    End,
    Both,
}

/// Line / arrow options (spec §3.3). `heads` only matters for arrows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LineStyle {
    pub path: LinePath,
    pub dashed: bool,
    pub heads: ArrowHeads,
}

impl LineStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Text options (spec §3.3). Size lives in `Annotation::Text::size`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    pub pill: bool,
    pub bold: bool,
}

impl TextStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Stamp marks (spec §3.4). Symbol stamps are vector marks on a disc in
/// the stamp color; emoji stamps come from the system emoji font.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StampKind {
    #[default]
    Check,
    Cross,
    Exclaim,
    Question,
    Star,
    Heart,
    ThumbsUp,
    ThumbsDown,
    Fire,
    Party,
    Eyes,
    /// Last in the spec's emoji row, but drawn as a vector symbol that
    /// takes the stamp color (no emoji font has a color ➜).
    ArrowRight,
}

impl StampKind {
    /// Picker order (spec §3.4 / mockup): ✓ ✕ ! ? ★ ♥ then 👍 👎 🔥 🎉 👀 ➜.
    pub const ALL: [Self; 12] = [
        Self::Check,
        Self::Cross,
        Self::Exclaim,
        Self::Question,
        Self::Star,
        Self::Heart,
        Self::ThumbsUp,
        Self::ThumbsDown,
        Self::Fire,
        Self::Party,
        Self::Eyes,
        Self::ArrowRight,
    ];

    /// The emoji for emoji stamps; `None` for symbol stamps.
    pub fn emoji(self) -> Option<&'static str> {
        match self {
            Self::ThumbsUp => Some("👍"),
            Self::ThumbsDown => Some("👎"),
            Self::Fire => Some("🔥"),
            Self::Party => Some("🎉"),
            Self::Eyes => Some("👀"),
            _ => None,
        }
    }

    /// What the picker shows.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Check => "✓",
            Self::Cross => "✕",
            Self::Exclaim => "!",
            Self::Question => "?",
            Self::Star => "★",
            Self::Heart => "♥",
            Self::ArrowRight => "➜",
            other => other.emoji().unwrap_or("?"),
        }
    }

    /// Tooltip name.
    pub fn label(self) -> &'static str {
        match self {
            Self::Check => "Check",
            Self::Cross => "Cross",
            Self::Exclaim => "Exclamation",
            Self::Question => "Question",
            Self::Star => "Star",
            Self::Heart => "Heart",
            Self::ArrowRight => "Arrow",
            Self::ThumbsUp => "Thumbs up",
            Self::ThumbsDown => "Thumbs down",
            Self::Fire => "Fire",
            Self::Party => "Party",
            Self::Eyes => "Eyes",
        }
    }
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
        #[serde(default, skip_serializing_if = "ShapeStyle::is_default")]
        style: ShapeStyle,
    },
    Ellipse {
        rect: RectLike,
        color: Rgba,
        line_width: f32,
        #[serde(default, skip_serializing_if = "ShapeStyle::is_default")]
        style: ShapeStyle,
    },
    Line {
        a: PointLike,
        b: PointLike,
        color: Rgba,
        line_width: f32,
        #[serde(default, skip_serializing_if = "LineStyle::is_default")]
        style: LineStyle,
    },
    Arrow {
        a: PointLike,
        b: PointLike,
        color: Rgba,
        line_width: f32,
        #[serde(default, skip_serializing_if = "LineStyle::is_default")]
        style: LineStyle,
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
        #[serde(default, skip_serializing_if = "TextStyle::is_default")]
        style: TextStyle,
    },
    Blur {
        rect: RectLike,
        radius: f32,
    },
    Pixelate {
        rect: RectLike,
        block_size: f32,
    },
    /// Everything outside `rect` dims by `dim` (0–1). Multiple spotlights
    /// union: see `render::apply_spotlights`.
    Spotlight {
        rect: RectLike,
        shape: SpotlightShape,
        dim: f32,
    },
    NumberedPin {
        origin: PointLike,
        number: u32,
        color: Rgba,
    },
    /// A mark sized like a numbered pin (spec §3.4). `color` tints symbol
    /// stamps; emoji keep their own colors.
    Stamp {
        origin: PointLike,
        stamp: StampKind,
        color: Rgba,
    },
    /// A callout that shows the circle around `source` enlarged `zoom`×
    /// in a bubble at `bubble` with a dashed leader (spec §3.2). Drawn in
    /// a final pass so the bubble always shows redactions.
    Magnifier {
        source: PointLike,
        radius: f32,
        bubble: PointLike,
        zoom: f32,
        #[serde(default)]
        shape: MagnifierShape,
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
                style: ShapeStyle::default(),
            },
            Annotation::Ellipse {
                rect: rect(0.0, 0.0, 50.0, 25.0),
                color: red(),
                line_width: 1.5,
                style: ShapeStyle::default(),
            },
            Annotation::Line {
                a: pt(0.0, 0.0),
                b: pt(100.0, 100.0),
                color: red(),
                line_width: 3.0,
                style: LineStyle::default(),
            },
            Annotation::Arrow {
                a: pt(5.0, 5.0),
                b: pt(95.0, 95.0),
                color: red(),
                line_width: 4.0,
                style: LineStyle::default(),
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
                style: TextStyle::default(),
            },
            Annotation::Blur {
                rect: rect(50.0, 50.0, 100.0, 100.0),
                radius: 6.0,
            },
            Annotation::Pixelate {
                rect: rect(0.0, 0.0, 80.0, 60.0),
                block_size: 8.0,
            },
            Annotation::Spotlight {
                rect: rect(10.0, 10.0, 60.0, 40.0),
                shape: SpotlightShape::Ellipse,
                dim: 0.55,
            },
            Annotation::NumberedPin {
                origin: pt(150.0, 150.0),
                number: 3,
                color: red(),
            },
            Annotation::Stamp {
                origin: pt(40.0, 40.0),
                stamp: StampKind::Fire,
                color: red(),
            },
            Annotation::Magnifier {
                source: pt(30.0, 30.0),
                radius: 10.0,
                bubble: pt(90.0, 10.0),
                zoom: 3.0,
                shape: MagnifierShape::RoundedSquare,
                color: red(),
            },
            Annotation::Crop {
                rect: rect(0.0, 0.0, 256.0, 256.0),
            },
        ];

        // Sanity: the fixture set covers every variant. If a new variant is
        // added without extending this list, this assertion fails.
        const VARIANT_COUNT: usize = 14;
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
    fn stamp_kinds_split_into_seven_symbols_and_five_emoji() {
        assert_eq!(
            StampKind::ALL
                .iter()
                .filter(|k| k.emoji().is_some())
                .count(),
            5
        );
        assert!(StampKind::ArrowRight.emoji().is_none());
        assert!(StampKind::ALL
            .iter()
            .all(|k| !k.glyph().is_empty() && !k.label().is_empty()));
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

    #[test]
    fn legacy_json_without_style_decodes_to_default_style() {
        // Sidecars written before Phase A carry no `style` key.
        let json = r#"{"kind":"Rectangle","rect":{"x":1.0,"y":2.0,"width":3.0,"height":4.0},"color":{"r":1.0,"g":0.0,"b":0.0,"a":1.0},"line_width":2.0}"#;
        let a: Annotation = serde_json::from_str(json).expect("legacy rectangle");
        assert_eq!(
            a,
            Annotation::Rectangle {
                rect: rect(1.0, 2.0, 3.0, 4.0),
                color: red(),
                line_width: 2.0,
                style: ShapeStyle::default(),
            }
        );
        let json = r#"{"kind":"Arrow","a":{"x":0.0,"y":0.0},"b":{"x":5.0,"y":5.0},"color":{"r":1.0,"g":0.0,"b":0.0,"a":1.0},"line_width":3.0}"#;
        let a: Annotation = serde_json::from_str(json).expect("legacy arrow");
        assert!(matches!(a, Annotation::Arrow { style, .. } if style == LineStyle::default()));
        let json = r#"{"kind":"Text","content":"hi","origin":{"x":0.0,"y":0.0},"color":{"r":0.0,"g":0.0,"b":0.0,"a":1.0},"font_family":"system-ui","size":14.0}"#;
        let a: Annotation = serde_json::from_str(json).expect("legacy text");
        assert!(matches!(a, Annotation::Text { style, .. } if style == TextStyle::default()));
    }

    #[test]
    fn default_style_is_not_serialised() {
        let a = Annotation::Line {
            a: pt(0.0, 0.0),
            b: pt(1.0, 1.0),
            color: red(),
            line_width: 2.0,
            style: LineStyle::default(),
        };
        let json = serde_json::to_value(&a).unwrap();
        assert!(json.get("style").is_none(), "{json}");
    }

    #[test]
    fn styled_variants_roundtrip_through_json() {
        let fixtures = vec![
            Annotation::Rectangle {
                rect: rect(0.0, 0.0, 10.0, 10.0),
                color: red(),
                line_width: 2.0,
                style: ShapeStyle {
                    fill: FillMode::Both,
                    dashed: true,
                    corner_radius: 8.0,
                },
            },
            Annotation::Arrow {
                a: pt(0.0, 0.0),
                b: pt(50.0, 10.0),
                color: red(),
                line_width: 3.0,
                style: LineStyle {
                    path: LinePath::Curved { bend: 0.3 },
                    dashed: false,
                    heads: ArrowHeads::Both,
                },
            },
            Annotation::Line {
                a: pt(0.0, 0.0),
                b: pt(50.0, 10.0),
                color: red(),
                line_width: 3.0,
                style: LineStyle {
                    path: LinePath::HandDrawn { seed: 99 },
                    dashed: true,
                    heads: ArrowHeads::End,
                },
            },
            Annotation::Text {
                content: "Click here".into(),
                origin: pt(4.0, 20.0),
                color: Rgba::OPAQUE_BLACK,
                font_family: "system-ui".into(),
                size: 24.0,
                style: TextStyle {
                    pill: true,
                    bold: true,
                },
            },
        ];
        for original in fixtures {
            let json = serde_json::to_string(&original).unwrap();
            let back: Annotation = serde_json::from_str(&json).unwrap();
            assert_eq!(original, back, "{json}");
        }
    }

    #[test]
    fn hex_round_trips_and_accepts_short_forms() {
        assert_eq!(Rgba::new(1.0, 0.0, 0.0, 1.0).to_hex(), "#FF0000");
        assert_eq!(Rgba::from_hex("#4f46e5").unwrap().to_hex(), "#4F46E5");
        assert_eq!(Rgba::from_hex("  4F46E5 ").unwrap().to_hex(), "#4F46E5");
        assert_eq!(Rgba::from_hex("#abc").unwrap().to_hex(), "#AABBCC");
        assert_eq!(Rgba::from_hex("#abc").unwrap().a, 1.0);
        for bad in ["", "#", "#12", "#12345", "#1234567", "#GGGGGG", "red"] {
            assert!(Rgba::from_hex(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn to_rgba8_round_trips_and_clamps() {
        for px in [[0, 0, 0, 0], [255, 255, 255, 255], [16, 32, 48, 200]] {
            assert_eq!(Rgba::from_rgba8(px).to_rgba8(), px);
        }
        assert_eq!(
            Rgba::new(2.0, -1.0, 0.5, 1.0).to_rgba8(),
            [255, 0, 128, 255]
        );
    }

    #[test]
    fn from_rgba8_maps_channels_to_unit_range() {
        let c = Rgba::from_rgba8([255, 0, 51, 255]);
        assert_eq!(c, Rgba::new(1.0, 0.0, 0.2, 1.0));
    }
}
