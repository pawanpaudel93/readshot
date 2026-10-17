//! Per-tool drawing options (spec §2.2, §3.3).
//!
//! [`ToolStyle`] is what the next shape gets; [`StyleMessage`] is one
//! change from the options bar. [`options_for`] says which controls the
//! bar shows: the selected annotation's kind wins over the active tool,
//! and Select with nothing selected shows nothing (the bar hides).

use readshot_core::line_geom::{seed_for, DEFAULT_BEND};
use readshot_core::{
    Annotation, ArrowHeads, FillMode, LinePath, LineStyle, MagnifierShape, PointLike, RectLike,
    ShapeStyle, SpotlightShape, StampKind, TextStyle,
};

use super::tool_state::ToolState;

pub const MAX_CORNER_RADIUS: f32 = 32.0;
pub const MIN_DIM: f32 = 0.2;
pub const MAX_DIM: f32 = 0.9;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinePathKind {
    #[default]
    Straight,
    Curved,
    HandDrawn,
}

impl LinePathKind {
    pub fn of(path: LinePath) -> Self {
        match path {
            LinePath::Straight => Self::Straight,
            LinePath::Curved { .. } => Self::Curved,
            LinePath::HandDrawn { .. } => Self::HandDrawn,
        }
    }

    /// Concrete path for a new or restyled line. An unchanged kind keeps
    /// the existing bend / seed so restyling never reshuffles a line.
    pub fn to_path(self, existing: LinePath, anchor: PointLike) -> LinePath {
        match (self, existing) {
            (Self::Straight, _) => LinePath::Straight,
            (Self::Curved, LinePath::Curved { bend }) => LinePath::Curved { bend },
            (Self::Curved, _) => LinePath::Curved { bend: DEFAULT_BEND },
            (Self::HandDrawn, LinePath::HandDrawn { seed }) => LinePath::HandDrawn { seed },
            (Self::HandDrawn, _) => LinePath::HandDrawn {
                seed: seed_for(anchor),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextSize {
    S,
    #[default]
    M,
    L,
    Xl,
}

impl TextSize {
    pub const ALL: [Self; 4] = [Self::S, Self::M, Self::L, Self::Xl];

    pub fn px(self) -> f32 {
        match self {
            Self::S => 16.0,
            Self::M => 24.0,
            Self::L => 36.0,
            Self::Xl => 52.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::S => "S",
            Self::M => "M",
            Self::L => "L",
            Self::Xl => "XL",
        }
    }

    /// The next preset above (`up`) or below an actual pixel size, or
    /// `None` when there is none. Steps from the real size, so an
    /// off-scale legacy text (e.g. 96 px) never jumps the wrong way.
    pub fn step_from_px(size: f32, up: bool) -> Option<Self> {
        const EPS: f32 = 0.01;
        if up {
            Self::ALL.iter().copied().find(|s| s.px() > size + EPS)
        } else {
            Self::ALL
                .iter()
                .rev()
                .copied()
                .find(|s| s.px() < size - EPS)
        }
    }

    pub fn nearest(size: f32) -> Self {
        *Self::ALL
            .iter()
            .min_by(|a, b| (a.px() - size).abs().total_cmp(&(b.px() - size).abs()))
            .expect("ALL is non-empty")
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CropAspect {
    #[default]
    Free,
    Wide,
    Standard,
    Square,
}

impl CropAspect {
    pub const ALL: [Self; 4] = [Self::Free, Self::Wide, Self::Standard, Self::Square];

    pub fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Wide => "16:9",
            Self::Standard => "4:3",
            Self::Square => "1:1",
        }
    }

    pub fn ratio(self) -> Option<f32> {
        match self {
            Self::Free => None,
            Self::Wide => Some(16.0 / 9.0),
            Self::Standard => Some(4.0 / 3.0),
            Self::Square => Some(1.0),
        }
    }
}

/// The largest `ratio` (w / h) rect centred inside `rect`.
pub fn fit_aspect(rect: RectLike, ratio: f32) -> RectLike {
    let (w, h) = if rect.width / rect.height.max(f32::EPSILON) > ratio {
        (rect.height * ratio, rect.height)
    } else {
        (rect.width, rect.width / ratio)
    };
    RectLike::new(
        rect.x + (rect.width - w) * 0.5,
        rect.y + (rect.height - h) * 0.5,
        w,
        h,
    )
}

/// Options applied to the next annotation drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolStyle {
    pub fill: FillMode,
    pub dashed: bool,
    pub corner_radius: f32,
    pub line_path: LinePathKind,
    pub heads: ArrowHeads,
    pub text_size: TextSize,
    pub text_pill: bool,
    pub text_bold: bool,
    pub spotlight_shape: SpotlightShape,
    pub dim: f32,
    pub stamp: StampKind,
    /// Highlighter: snap strokes to recognized text lines (spec §3.6).
    pub snap_to_text: bool,
    /// Crop tool: the aspect preset the drag and draft follow.
    pub crop_aspect: CropAspect,
    /// Magnifier: how much the bubble enlarges its source.
    pub magnifier_zoom: f32,
    /// Magnifier: bubble outline.
    pub magnifier_shape: MagnifierShape,
}

impl Default for ToolStyle {
    fn default() -> Self {
        Self {
            fill: FillMode::default(),
            dashed: false,
            corner_radius: 0.0,
            line_path: LinePathKind::default(),
            heads: ArrowHeads::default(),
            text_size: TextSize::default(),
            text_pill: false,
            text_bold: false,
            spotlight_shape: SpotlightShape::default(),
            dim: readshot_core::annotation::DEFAULT_SPOTLIGHT_DIM,
            stamp: StampKind::default(),
            snap_to_text: false,
            crop_aspect: CropAspect::default(),
            magnifier_zoom: readshot_core::magnifier::DEFAULT_ZOOM,
            magnifier_shape: MagnifierShape::default(),
        }
    }
}

/// One options-bar change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StyleMessage {
    Fill(FillMode),
    Dashed(bool),
    CornerRadius(f32),
    LinePath(LinePathKind),
    Heads(ArrowHeads),
    TextSize(TextSize),
    TextPill(bool),
    TextBold(bool),
    SpotlightShape(SpotlightShape),
    Dim(f32),
    Stamp(StampKind),
    SnapToText(bool),
    CropAspect(CropAspect),
}

/// Controls the options bar can show, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionControl {
    Color,
    Width,
    Fill,
    Dash,
    CornerRadius,
    LinePath,
    Heads,
    TextSize,
    TextPill,
    TextBold,
    Dim,
    SpotlightShape,
    StampPicker,
    SnapToText,
    CropAspect,
    CropApply,
}

/// The tool that draws `annotation` — the options-bar target for a selection.
pub fn tool_for_annotation(annotation: &Annotation) -> ToolState {
    match annotation {
        Annotation::Rectangle { .. } => ToolState::Rectangle,
        Annotation::Ellipse { .. } => ToolState::Ellipse,
        Annotation::Line { .. } => ToolState::Line,
        Annotation::Arrow { .. } => ToolState::Arrow,
        Annotation::Pen { .. } => ToolState::Pen,
        Annotation::Highlighter { .. } => ToolState::Highlighter,
        Annotation::Text { .. } => ToolState::Text,
        Annotation::Blur { .. } => ToolState::Blur,
        Annotation::Pixelate { .. } => ToolState::Pixelate,
        Annotation::Spotlight { .. } => ToolState::Spotlight,
        Annotation::NumberedPin { .. } => ToolState::NumberedPin,
        Annotation::Stamp { .. } => ToolState::Stamp,
        Annotation::Magnifier { .. } => ToolState::Magnifier,
        Annotation::Crop { .. } => ToolState::Crop,
    }
}

pub fn options_for(active: ToolState, selected: Option<&Annotation>) -> &'static [OptionControl] {
    use OptionControl::*;
    match selected.map(tool_for_annotation).unwrap_or(active) {
        ToolState::Select => &[],
        ToolState::Crop => &[CropAspect],
        ToolState::Rectangle => &[Color, Width, Fill, Dash, CornerRadius],
        ToolState::Ellipse => &[Color, Width, Fill, Dash],
        ToolState::Line => &[Color, Width, LinePath, Dash],
        ToolState::Arrow => &[Color, Width, LinePath, Dash, Heads],
        ToolState::Pen => &[Color, Width],
        // Snap to text shapes the next stroke, so a selected highlighter
        // doesn't offer it.
        ToolState::Highlighter if selected.is_none() => &[Color, Width, SnapToText],
        ToolState::Highlighter => &[Color, Width],
        ToolState::Text => &[Color, TextSize, TextPill, TextBold],
        ToolState::Blur | ToolState::Pixelate => &[Width],
        ToolState::Spotlight => &[Dim, SpotlightShape],
        ToolState::NumberedPin => &[Color],
        ToolState::Stamp => &[Color, StampPicker],
        ToolState::Magnifier => &[Color],
    }
}

impl ToolStyle {
    pub fn apply(&mut self, msg: StyleMessage) {
        match msg {
            StyleMessage::Fill(f) => self.fill = f,
            StyleMessage::Dashed(d) => self.dashed = d,
            StyleMessage::CornerRadius(r) => self.corner_radius = r.clamp(0.0, MAX_CORNER_RADIUS),
            StyleMessage::LinePath(k) => self.line_path = k,
            StyleMessage::Heads(h) => self.heads = h,
            StyleMessage::TextSize(s) => self.text_size = s,
            StyleMessage::TextPill(p) => self.text_pill = p,
            StyleMessage::TextBold(b) => self.text_bold = b,
            StyleMessage::SpotlightShape(s) => self.spotlight_shape = s,
            StyleMessage::Dim(d) => self.dim = d.clamp(MIN_DIM, MAX_DIM),
            StyleMessage::Stamp(k) => self.stamp = k,
            StyleMessage::SnapToText(v) => self.snap_to_text = v,
            StyleMessage::CropAspect(a) => self.crop_aspect = a,
        }
    }

    pub fn shape_style(&self, tool: ToolState) -> ShapeStyle {
        ShapeStyle {
            fill: self.fill,
            dashed: self.dashed,
            corner_radius: if tool == ToolState::Rectangle {
                self.corner_radius
            } else {
                0.0
            },
        }
    }

    pub fn line_style(&self, anchor: PointLike) -> LineStyle {
        LineStyle {
            path: self.line_path.to_path(LinePath::Straight, anchor),
            dashed: self.dashed,
            heads: self.heads,
        }
    }

    pub fn text_style(&self) -> TextStyle {
        TextStyle {
            pill: self.text_pill,
            bold: self.text_bold,
        }
    }

    /// `annotation`'s settings layered over `self`, so the options bar
    /// shows what the selection uses.
    pub fn with_annotation(mut self, annotation: &Annotation) -> Self {
        match annotation {
            Annotation::Rectangle { style, .. } => {
                self.fill = style.fill;
                self.dashed = style.dashed;
                self.corner_radius = style.corner_radius;
            }
            Annotation::Ellipse { style, .. } => {
                self.fill = style.fill;
                self.dashed = style.dashed;
            }
            Annotation::Line { style, .. } | Annotation::Arrow { style, .. } => {
                self.line_path = LinePathKind::of(style.path);
                self.dashed = style.dashed;
                self.heads = style.heads;
            }
            Annotation::Text { size, style, .. } => {
                self.text_size = TextSize::nearest(*size);
                self.text_pill = style.pill;
                self.text_bold = style.bold;
            }
            Annotation::Spotlight { shape, dim, .. } => {
                self.spotlight_shape = *shape;
                self.dim = *dim;
            }
            Annotation::Stamp { stamp, .. } => self.stamp = *stamp,
            Annotation::Magnifier { zoom, shape, .. } => {
                self.magnifier_zoom = *zoom;
                self.magnifier_shape = *shape;
            }
            _ => {}
        }
        self
    }
}

/// Apply `msg` to `annotation` when it applies to that kind. Returns
/// whether anything changed.
pub fn apply_style_to_annotation(annotation: &mut Annotation, msg: StyleMessage) -> bool {
    let before = annotation.clone();
    match (&mut *annotation, msg) {
        (
            Annotation::Rectangle { style, .. } | Annotation::Ellipse { style, .. },
            StyleMessage::Fill(f),
        ) => style.fill = f,
        (
            Annotation::Rectangle { style, .. } | Annotation::Ellipse { style, .. },
            StyleMessage::Dashed(d),
        ) => style.dashed = d,
        (Annotation::Rectangle { style, .. }, StyleMessage::CornerRadius(r)) => {
            style.corner_radius = r.clamp(0.0, MAX_CORNER_RADIUS)
        }
        (
            Annotation::Line { a, style, .. } | Annotation::Arrow { a, style, .. },
            StyleMessage::LinePath(kind),
        ) => style.path = kind.to_path(style.path, *a),
        (
            Annotation::Line { style, .. } | Annotation::Arrow { style, .. },
            StyleMessage::Dashed(d),
        ) => style.dashed = d,
        (Annotation::Arrow { style, .. }, StyleMessage::Heads(h)) => style.heads = h,
        (Annotation::Text { size, .. }, StyleMessage::TextSize(s)) => *size = s.px(),
        (Annotation::Text { style, .. }, StyleMessage::TextPill(p)) => style.pill = p,
        (Annotation::Text { style, .. }, StyleMessage::TextBold(b)) => style.bold = b,
        (Annotation::Spotlight { shape, .. }, StyleMessage::SpotlightShape(s)) => *shape = s,
        (Annotation::Spotlight { dim, .. }, StyleMessage::Dim(d)) => {
            *dim = d.clamp(MIN_DIM, MAX_DIM)
        }
        (Annotation::Stamp { stamp, .. }, StyleMessage::Stamp(k)) => *stamp = k,
        (Annotation::Crop { rect }, StyleMessage::CropAspect(a)) => {
            if let Some(r) = a.ratio() {
                *rect = fit_aspect(*rect, r);
            }
        }
        _ => return false,
    }
    *annotation != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use readshot_core::{RectLike, Rgba};

    #[test]
    fn step_from_px_moves_relative_to_the_actual_size() {
        // Legacy off-scale sizes never jump the wrong way.
        assert_eq!(TextSize::step_from_px(96.0, true), None);
        assert_eq!(TextSize::step_from_px(12.0, false), None);
        assert_eq!(TextSize::step_from_px(20.0, true), Some(TextSize::M));
        assert_eq!(TextSize::step_from_px(20.0, false), Some(TextSize::S));
        assert_eq!(TextSize::step_from_px(30.0, false), Some(TextSize::M));
        // On-scale sizes step one preset; the ends clamp to no-op.
        assert_eq!(TextSize::step_from_px(24.0, true), Some(TextSize::L));
        assert_eq!(TextSize::step_from_px(24.0, false), Some(TextSize::S));
        assert_eq!(TextSize::step_from_px(52.0, true), None);
        assert_eq!(TextSize::step_from_px(16.0, false), None);
    }

    fn rect() -> Annotation {
        Annotation::Rectangle {
            rect: RectLike::new(0.0, 0.0, 10.0, 10.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: ShapeStyle::default(),
        }
    }

    fn arrow() -> Annotation {
        Annotation::Arrow {
            a: PointLike::new(0.0, 0.0),
            b: PointLike::new(40.0, 0.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 2.0,
            style: LineStyle::default(),
        }
    }

    #[test]
    fn options_bar_is_hidden_for_select_without_selection() {
        assert!(options_for(ToolState::Select, None).is_empty());
    }

    #[test]
    fn options_follow_the_selection_over_the_active_tool() {
        let a = arrow();
        assert_eq!(
            options_for(ToolState::Select, Some(&a)),
            options_for(ToolState::Arrow, None)
        );
        assert_eq!(
            options_for(ToolState::Text, Some(&a)),
            options_for(ToolState::Arrow, None)
        );
    }

    #[test]
    fn options_per_tool_match_the_spec() {
        use OptionControl::*;
        assert_eq!(
            options_for(ToolState::Rectangle, None),
            &[Color, Width, Fill, Dash, CornerRadius]
        );
        assert_eq!(
            options_for(ToolState::Arrow, None),
            &[Color, Width, LinePath, Dash, Heads]
        );
        assert_eq!(
            options_for(ToolState::Text, None),
            &[Color, TextSize, TextPill, TextBold]
        );
        assert_eq!(options_for(ToolState::Blur, None), &[Width]);
    }

    #[test]
    fn spotlight_shows_dim_and_shape_and_defaults_to_55_percent() {
        assert_eq!(
            options_for(ToolState::Spotlight, None),
            &[OptionControl::Dim, OptionControl::SpotlightShape]
        );
        assert_eq!(ToolStyle::default().dim, 0.55);
        let mut s = ToolStyle::default();
        s.apply(StyleMessage::Dim(2.0));
        assert_eq!(s.dim, MAX_DIM);
    }

    #[test]
    fn line_path_kind_keeps_existing_bend_and_seed() {
        let anchor = PointLike::new(3.0, 4.0);
        let curved = LinePath::Curved { bend: -0.7 };
        assert_eq!(LinePathKind::Curved.to_path(curved, anchor), curved);
        assert_eq!(
            LinePathKind::Curved.to_path(LinePath::Straight, anchor),
            LinePath::Curved { bend: DEFAULT_BEND }
        );
        assert_eq!(
            LinePathKind::HandDrawn.to_path(LinePath::Straight, anchor),
            LinePath::HandDrawn {
                seed: seed_for(anchor)
            }
        );
    }

    #[test]
    fn apply_style_only_touches_matching_kinds() {
        let mut r = rect();
        assert!(apply_style_to_annotation(
            &mut r,
            StyleMessage::Fill(FillMode::Filled)
        ));
        assert!(!apply_style_to_annotation(
            &mut r,
            StyleMessage::Fill(FillMode::Filled)
        ));
        assert!(!apply_style_to_annotation(
            &mut r,
            StyleMessage::Heads(ArrowHeads::Both)
        ));
        let mut a = arrow();
        assert!(apply_style_to_annotation(
            &mut a,
            StyleMessage::Heads(ArrowHeads::Both)
        ));
        assert!(!apply_style_to_annotation(
            &mut a,
            StyleMessage::CornerRadius(8.0)
        ));
    }

    #[test]
    fn text_size_maps_to_px_and_back() {
        assert_eq!(TextSize::L.px(), 36.0);
        assert_eq!(TextSize::nearest(20.0), TextSize::S);
        assert_eq!(TextSize::nearest(23.0), TextSize::M);
        assert_eq!(TextSize::nearest(80.0), TextSize::Xl);
    }

    #[test]
    fn ellipses_never_get_a_corner_radius() {
        let style = ToolStyle {
            corner_radius: 12.0,
            ..ToolStyle::default()
        };
        assert_eq!(style.shape_style(ToolState::Ellipse).corner_radius, 0.0);
        assert_eq!(style.shape_style(ToolState::Rectangle).corner_radius, 12.0);
    }

    #[test]
    fn snap_to_text_is_a_highlighter_drawing_option_off_by_default() {
        use OptionControl::*;
        assert_eq!(
            options_for(ToolState::Highlighter, None),
            &[Color, Width, SnapToText]
        );
        let h = Annotation::Highlighter {
            points: vec![
                readshot_core::geom::PointLike::new(0.0, 0.0),
                readshot_core::geom::PointLike::new(9.0, 0.0),
            ],
            color: Rgba::OPAQUE_BLACK,
            line_width: 12.0,
        };
        // A selected highlighter has nothing to snap.
        assert_eq!(options_for(ToolState::Select, Some(&h)), &[Color, Width]);
        assert_eq!(options_for(ToolState::Pen, None), &[Color, Width]);
        let mut s = ToolStyle::default();
        assert!(!s.snap_to_text);
        s.apply(StyleMessage::SnapToText(true));
        assert!(s.snap_to_text);
        let mut h2 = h.clone();
        assert!(!apply_style_to_annotation(
            &mut h2,
            StyleMessage::SnapToText(true)
        ));
    }

    #[test]
    fn fit_aspect_centres_the_largest_rect_of_the_ratio() {
        let r = RectLike::new(0.0, 0.0, 200.0, 100.0);
        assert_eq!(fit_aspect(r, 1.0), RectLike::new(50.0, 0.0, 100.0, 100.0));
        let tall = RectLike::new(10.0, 10.0, 90.0, 160.0);
        assert_eq!(
            fit_aspect(tall, 16.0 / 9.0),
            RectLike::new(10.0, 64.6875, 90.0, 50.625)
        );
        assert_eq!(CropAspect::Free.ratio(), None);
        assert_eq!(
            options_for(ToolState::Crop, None),
            &[OptionControl::CropAspect]
        );
        let mut crop = Annotation::Crop { rect: r };
        assert!(apply_style_to_annotation(
            &mut crop,
            StyleMessage::CropAspect(CropAspect::Square)
        ));
        assert_eq!(
            crop,
            Annotation::Crop {
                rect: RectLike::new(50.0, 0.0, 100.0, 100.0)
            }
        );
    }
}
