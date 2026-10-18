//! Canvas-drawn icon set: editor tools plus the line icons for every surface.
//!
//! These replace font-dependent Unicode glyphs with a small, consistent
//! in-app icon set. The icons are intentionally monochrome so the
//! toolbar's active/hover state remains responsible for emphasis.

use iced::widget::canvas::{self, Frame, Path, Stroke, Text as CanvasText};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};

use crate::app::Message;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppIcon {
    Tool(readshot_ui::editor::ToolState),
    Undo,
    Redo,
    Edit,
    Copy,
    ScanText,
    Save,
    Pin,
    Scroll,
    Close,
    Search,
    Folder,
    Trash,
    Lock,
    Minus,
    Plus,
    CheckCircle,
    Alert,
    Shield,
    Crosshair,
    Image,
    Terminal,
    Power,
    History,
    Gear,
    Sparkle,
    SolidLine,
    Dashed,
    LineStraight,
    LineCurved,
    LineSketch,
    ArrowBoth,
    Dropper,
    Sidebar,
    Chevron,
    RotateLeft,
    RotateRight,
    FlipH,
    FlipV,
    Resize,
    AddBelow,
    AddBeside,
    /// Magnifier bubble shapes: a lens with a centre dot, so they don't
    /// reuse the Ellipse / Rectangle drawing-tool glyphs.
    BubbleCircle,
    BubbleSquare,
    /// Spotlight shapes: the lit shape inside a faint frame (the dimmed
    /// image), echoing the Spotlight tool rather than reusing the
    /// Rectangle / Ellipse drawing-tool glyphs.
    SpotlightRect,
    SpotlightEllipse,
    /// A one-headed arrow, the pair of [`AppIcon::ArrowBoth`] in the
    /// arrow-heads option (it reused the Arrow drawing-tool glyph).
    ArrowOne,
    /// Redaction review: an eye struck through (hide what is sensitive).
    /// The Shield means the Screen Recording permission.
    Redact,
}

/// One glyph per action that more than one surface offers, so the same
/// action always shows the same glyph and different actions never share
/// one (the tests check both). Surfaces use these rather than naming a
/// glyph directly.
pub mod action {
    use super::AppIcon;

    /// Open a capture in the editor: the capture overlay's Edit and
    /// History's Open in Editor.
    pub const OPEN_IN_EDITOR: AppIcon = AppIcon::Edit;
    /// Throw a capture away: History's Delete and the editor's Discard.
    pub const DELETE: AppIcon = AppIcon::Trash;
    /// Dismiss, cancel or close something without losing work.
    pub const DISMISS: AppIcon = AppIcon::Close;
    /// Pick a colour from the image: the colour popover and Beautify.
    pub const PICK_COLOR: AppIcon = AppIcon::Dropper;
    /// The Screen Recording permission: Welcome and the Settings pane.
    pub const SCREEN_RECORDING: AppIcon = AppIcon::Shield;
    /// Review and apply redactions (the Text tab).
    pub const REDACT: AppIcon = AppIcon::Redact;
    /// Open Settings.
    pub const SETTINGS: AppIcon = AppIcon::Gear;
    /// Copy the recognized text.
    pub const COPY_TEXT: AppIcon = AppIcon::ScanText;
    /// Copy the image.
    pub const COPY_IMAGE: AppIcon = AppIcon::Copy;
    /// Pin the capture on screen.
    pub const PIN: AppIcon = AppIcon::Pin;
    /// Save to a file.
    pub const SAVE: AppIcon = AppIcon::Save;

    /// Every action glyph with its name, for the tests.
    #[cfg(test)]
    pub(crate) const ALL: &[(&str, AppIcon)] = &[
        ("open in editor", OPEN_IN_EDITOR),
        ("delete", DELETE),
        ("dismiss", DISMISS),
        ("pick color", PICK_COLOR),
        ("screen recording", SCREEN_RECORDING),
        ("redact", REDACT),
        ("settings", SETTINGS),
        ("copy text", COPY_TEXT),
        ("copy image", COPY_IMAGE),
        ("pin", PIN),
        ("save", SAVE),
    ];
}

#[derive(Clone, Copy, Debug)]
struct IconProgram {
    icon: AppIcon,
    color: Color,
    size: f32,
    /// `(selected, enabled)` when the glyph sits in an icon button: the
    /// colour is then resolved against the live theme at draw time so it
    /// follows the button's selected / disabled state.
    button: Option<(bool, bool)>,
}

pub fn icon<'a>(icon: AppIcon, color: Color, size: f32) -> Element<'a, Message> {
    canvas::Canvas::new(IconProgram {
        icon,
        color,
        size,
        button: None,
    })
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .into()
}

/// Glyph for an icon button: selected draws in `acc_text`, disabled dims,
/// hover brightens like the button's text. The canvas fills the button
/// (the glyph is centred in it) so the cursor test matches the button's
/// own hover area.
pub fn button_icon<'a>(
    icon: AppIcon,
    color: Color,
    size: f32,
    selected: bool,
    enabled: bool,
) -> Element<'a, Message> {
    canvas::Canvas::new(IconProgram {
        icon,
        color,
        size,
        button: Some((selected, enabled)),
    })
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}

impl canvas::Program<Message, Theme, Renderer> for IconProgram {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let color = match self.button {
            Some((selected, enabled)) => {
                // Centre the glyph in the button-sized canvas.
                frame.translate(iced::Vector::new(
                    ((bounds.width - self.size) / 2.0).max(0.0),
                    ((bounds.height - self.size) / 2.0).max(0.0),
                ));
                crate::runtime::icon_button_glyph_color(
                    crate::runtime::palette_for(theme),
                    self.color,
                    selected,
                    enabled,
                    cursor.is_over(bounds),
                )
            }
            None => self.color,
        };
        match self.icon {
            AppIcon::Tool(_) | AppIcon::Undo | AppIcon::Redo => {
                frame.scale(self.size / EDITOR_GRID);
                render_ops(&mut frame, editor_ops(self.icon), color);
            }
            other => draw_shapes(&mut frame, shapes(other), color, self.size),
        }
        vec![frame.into_geometry()]
    }
}

/// The editor glyphs (tools, undo, redo) are drawn on a 22-unit grid; the
/// rest of the set ([`shapes`]) on a 24-unit one.
const EDITOR_GRID: f32 = 22.0;

/// One path segment of an editor glyph, in grid units.
#[derive(Clone, Copy, Debug)]
enum Seg {
    M(f32, f32),
    L(f32, f32),
    C(f32, f32, f32, f32, f32, f32),
    Z,
}

/// A closed or open outline of an editor glyph, in grid units.
#[derive(Clone, Copy, Debug)]
enum Geo {
    Path(&'static [Seg]),
    Circle(f32, f32, f32),
    Rect(f32, f32, f32, f32),
    RoundRect(f32, f32, f32, f32, f32),
}

/// One drawing step of an editor glyph. Opacities multiply the glyph
/// colour's own alpha, so a dimmed (disabled) glyph dims as a whole.
#[derive(Clone, Copy, Debug)]
enum Op {
    /// Outline, stroke width in grid units, opacity.
    Stroke(Geo, f32, f32),
    /// Outline, opacity.
    Fill(Geo, f32),
    /// Content, x, y (top-left), size.
    Text(&'static str, f32, f32, f32),
}

use Seg::{C, L, M, Z};

fn geo_path(geo: Geo) -> Path {
    match geo {
        Geo::Path(segs) => Path::new(|p| {
            for seg in segs {
                match *seg {
                    M(x, y) => p.move_to(Point::new(x, y)),
                    L(x, y) => p.line_to(Point::new(x, y)),
                    C(a, b, c, d, x, y) => {
                        p.bezier_curve_to(Point::new(a, b), Point::new(c, d), Point::new(x, y))
                    }
                    Z => p.close(),
                }
            }
        }),
        Geo::Circle(cx, cy, r) => Path::circle(Point::new(cx, cy), r),
        Geo::Rect(x, y, w, h) => Path::rectangle(Point::new(x, y), iced::Size::new(w, h)),
        Geo::RoundRect(x, y, w, h, r) => {
            Path::rounded_rectangle(Point::new(x, y), iced::Size::new(w, h), r.into())
        }
    }
}

fn render_ops(frame: &mut Frame, ops: &[Op], color: Color) {
    let with = |alpha: f32| Color {
        a: color.a * alpha,
        ..color
    };
    for op in ops {
        match *op {
            Op::Stroke(geo, width, alpha) => {
                frame.stroke(&geo_path(geo), icon_stroke(with(alpha), width));
            }
            Op::Fill(geo, alpha) => frame.fill(&geo_path(geo), with(alpha)),
            Op::Text(content, x, y, size) => frame.fill_text(CanvasText {
                content: content.into(),
                position: Point::new(x, y),
                color,
                size: iced::Pixels(size),
                ..Default::default()
            }),
        }
    }
}

fn icon_stroke(color: Color, width: f32) -> Stroke<'static> {
    Stroke::default()
        .with_color(color)
        .with_width(width)
        .with_line_cap(canvas::LineCap::Round)
        .with_line_join(canvas::LineJoin::Round)
}

/// The drawing of every editor glyph. The `ToolState` match has no
/// wildcard, so a new tool fails to compile until it has a glyph (a
/// wildcard silently drew a blank toolbar button).
fn editor_ops(icon: AppIcon) -> &'static [Op] {
    use readshot_ui::editor::ToolState as T;

    match icon {
        AppIcon::Tool(tool) => match tool {
            T::Select => SELECT,
            T::Rectangle => RECTANGLE,
            T::Ellipse => ELLIPSE,
            T::Line => LINE,
            T::Arrow => ARROW,
            T::Pen => PEN,
            T::Highlighter => HIGHLIGHTER,
            T::Text => TEXT,
            T::Blur => BLUR,
            T::Pixelate => PIXELATE,
            T::Spotlight => SPOTLIGHT,
            T::NumberedPin => NUMBERED_PIN,
            T::Stamp => STAMP,
            T::Magnifier => MAGNIFIER,
            T::Crop => CROP,
        },
        // Undo is the counter-clockwise ↺ (arrowhead at top-left), redo
        // the clockwise ↻.
        AppIcon::Undo => UNDO,
        AppIcon::Redo => REDO,
        _ => &[],
    }
}

const SELECT: &[Op] = &[Op::Fill(
    Geo::Path(&[
        M(5.0, 3.0),
        L(17.0, 13.0),
        L(12.6, 14.0),
        L(15.7, 20.0),
        L(12.9, 21.4),
        L(9.7, 15.5),
        L(6.5, 18.9),
        Z,
    ]),
    1.0,
)];

const RECTANGLE: &[Op] = &[Op::Stroke(Geo::Rect(4.0, 6.0, 14.0, 10.0), 2.0, 1.0)];

const ELLIPSE: &[Op] = &[Op::Stroke(
    Geo::Path(&[
        M(18.0, 11.0),
        C(18.0, 15.4, 14.9, 18.0, 11.0, 18.0),
        C(7.1, 18.0, 4.0, 15.4, 4.0, 11.0),
        C(4.0, 6.6, 7.1, 4.0, 11.0, 4.0),
        C(14.9, 4.0, 18.0, 6.6, 18.0, 11.0),
    ]),
    2.0,
    1.0,
)];

const LINE: &[Op] = &[Op::Stroke(
    Geo::Path(&[M(5.0, 17.0), L(17.0, 5.0)]),
    2.0,
    1.0,
)];

const ARROW: &[Op] = &[
    Op::Stroke(Geo::Path(&[M(4.0, 17.0), L(17.0, 4.0)]), 2.0, 1.0),
    Op::Stroke(
        Geo::Path(&[M(10.3, 4.5), L(17.0, 4.0), L(16.4, 10.7)]),
        2.0,
        1.0,
    ),
];

const PEN_BODY: Geo = Geo::Path(&[
    M(5.0, 17.0),
    L(14.4, 7.6),
    L(17.0, 10.2),
    L(7.6, 19.6),
    L(4.6, 20.4),
    Z,
]);

const PEN: &[Op] = &[
    Op::Fill(PEN_BODY, 0.26),
    Op::Stroke(PEN_BODY, 1.8, 1.0),
    Op::Stroke(Geo::Path(&[M(12.7, 6.1), L(18.5, 11.9)]), 1.8, 1.0),
];

/// A chisel-tip marker laying down a translucent band: the band and the
/// solid slanted tip are what read as "highlighter" (the earlier tilted
/// block over a grey bar read as an eraser).
const HIGHLIGHTER_BODY: Geo =
    Geo::Path(&[M(12.6, 3.6), L(18.4, 9.4), L(11.6, 16.2), L(5.8, 10.4), Z]);
const HIGHLIGHTER_TIP: Geo =
    Geo::Path(&[M(7.2, 11.8), L(10.2, 14.8), L(7.0, 18.0), L(4.0, 15.0), Z]);
const HIGHLIGHTER: &[Op] = &[
    Op::Fill(Geo::RoundRect(3.0, 18.4, 16.0, 2.8, 1.0), 0.38),
    Op::Fill(HIGHLIGHTER_BODY, 0.2),
    Op::Stroke(HIGHLIGHTER_BODY, 1.8, 1.0),
    Op::Fill(HIGHLIGHTER_TIP, 1.0),
    Op::Stroke(HIGHLIGHTER_TIP, 1.2, 1.0),
];

const TEXT: &[Op] = &[
    Op::Stroke(Geo::Path(&[M(5.0, 5.0), L(17.0, 5.0)]), 2.0, 1.0),
    Op::Stroke(Geo::Path(&[M(11.0, 5.0), L(11.0, 18.0)]), 2.0, 1.0),
    Op::Stroke(Geo::Path(&[M(8.1, 18.0), L(13.9, 18.0)]), 2.0, 1.0),
];

const DROPLET: Geo = Geo::Path(&[
    M(11.0, 3.7),
    C(15.4, 8.6, 17.4, 11.9, 17.4, 14.4),
    C(17.4, 18.0, 14.6, 20.3, 11.0, 20.3),
    C(7.4, 20.3, 4.6, 18.0, 4.6, 14.4),
    C(4.6, 11.9, 6.6, 8.6, 11.0, 3.7),
]);

const BLUR: &[Op] = &[
    Op::Fill(DROPLET, 0.18),
    Op::Stroke(DROPLET, 2.0, 1.0),
    Op::Fill(Geo::Circle(7.6, 14.8, 1.0), 1.0),
    Op::Fill(Geo::Circle(11.0, 14.8, 1.0), 1.0),
    Op::Fill(Geo::Circle(14.4, 14.8, 1.0), 1.0),
];

const SPOTLIGHT: &[Op] = &[
    Op::Stroke(Geo::Rect(3.0, 4.0, 16.0, 14.0), 1.5, 0.45),
    Op::Fill(Geo::Circle(11.0, 11.0, 4.5), 0.25),
    Op::Stroke(Geo::Circle(11.0, 11.0, 4.5), 2.0, 1.0),
];

const PIXELATE: &[Op] = &[
    Op::Fill(Geo::Rect(5.0, 5.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(10.0, 5.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(15.0, 5.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(5.0, 10.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(10.0, 10.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(15.0, 10.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(5.0, 15.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(10.0, 15.0, 3.5, 3.5), 0.72),
    Op::Fill(Geo::Rect(15.0, 15.0, 3.5, 3.5), 0.72),
];

/// A loupe with a "+": magnify (two joined circles read as a chain link).
/// The "+" keeps it apart from the plain Search glass.
const MAGNIFIER: &[Op] = &[
    Op::Fill(Geo::Circle(9.5, 9.5, 5.8), 0.16),
    Op::Stroke(Geo::Circle(9.5, 9.5, 5.8), 2.0, 1.0),
    Op::Stroke(Geo::Path(&[M(13.9, 13.9), L(18.6, 18.6)]), 2.8, 1.0),
    Op::Stroke(Geo::Path(&[M(9.5, 7.0), L(9.5, 12.0)]), 1.7, 1.0),
    Op::Stroke(Geo::Path(&[M(7.0, 9.5), L(12.0, 9.5)]), 1.7, 1.0),
];

const STAMP: &[Op] = &[
    Op::Stroke(Geo::Circle(11.0, 5.5, 2.5), 2.0, 1.0),
    Op::Stroke(Geo::Path(&[M(11.0, 8.0), L(11.0, 12.0)]), 2.0, 1.0),
    Op::Stroke(Geo::RoundRect(5.0, 12.0, 12.0, 4.0, 1.0), 2.0, 1.0),
    Op::Stroke(Geo::Path(&[M(4.0, 19.0), L(18.0, 19.0)]), 2.0, 1.0),
];

const PIN_OUTLINE: Geo = Geo::Path(&[
    M(11.0, 20.5),
    L(7.1, 13.6),
    C(4.7, 9.3, 7.5, 4.2, 11.0, 4.2),
    C(14.5, 4.2, 17.3, 9.3, 14.9, 13.6),
    Z,
]);

const NUMBERED_PIN: &[Op] = &[
    Op::Fill(PIN_OUTLINE, 0.18),
    Op::Stroke(PIN_OUTLINE, 2.0, 1.0),
    Op::Text("1", 8.8, 7.0, 10.5),
];

/// The standard crop glyph: two interlocking L's (four outward corner
/// ticks read as "exit full screen" and echoed the brand brackets).
const CROP: &[Op] = &[
    Op::Stroke(
        Geo::Path(&[M(6.0, 2.5), L(6.0, 16.0), L(19.5, 16.0)]),
        2.0,
        1.0,
    ),
    Op::Stroke(
        Geo::Path(&[M(2.5, 6.0), L(16.0, 6.0), L(16.0, 19.5)]),
        2.0,
        1.0,
    ),
];

const UNDO: &[Op] = &[
    Op::Stroke(
        Geo::Path(&[
            M(7.0, 8.2),
            C(9.2, 4.8, 15.3, 4.7, 17.6, 9.1),
            C(19.7, 13.2, 17.4, 18.2, 13.1, 18.9),
        ]),
        2.0,
        1.0,
    ),
    Op::Stroke(
        Geo::Path(&[M(7.4, 4.7), L(6.9, 8.3), L(10.6, 8.4)]),
        2.0,
        1.0,
    ),
];

const REDO: &[Op] = &[
    Op::Stroke(
        Geo::Path(&[
            M(15.0, 8.2),
            C(12.8, 4.8, 6.7, 4.7, 4.4, 9.1),
            C(2.3, 13.2, 4.6, 18.2, 8.9, 18.9),
        ]),
        2.0,
        1.0,
    ),
    Op::Stroke(
        Geo::Path(&[M(14.6, 4.7), L(15.1, 8.3), L(11.4, 8.4)]),
        2.0,
        1.0,
    ),
];

#[derive(Clone, Copy, Debug)]
enum Shape {
    Line(f32, f32, f32, f32),
    Poly(&'static [(f32, f32)], bool), // points, closed
    Rect(f32, f32, f32, f32, f32),     // x, y, w, h, radius
    Circle(f32, f32, f32),
    Arc(f32, f32, f32, f32, f32), // cx, cy, r, start_deg, end_deg (clockwise, 0 deg = east)
    Dot(f32, f32),
    /// A [`Shape::Rect`] stroked at reduced opacity (a backdrop).
    FaintRect(f32, f32, f32, f32, f32),
}

/// Opacity of a [`Shape::FaintRect`], as the Spotlight tool's frame.
const FAINT: f32 = 0.45;

impl Shape {
    #[cfg(test)]
    fn points(&self) -> Vec<(f32, f32)> {
        match *self {
            Shape::Line(a, b, c, d) => vec![(a, b), (c, d)],
            Shape::Poly(p, _) => p.to_vec(),
            Shape::Rect(x, y, w, h, _) | Shape::FaintRect(x, y, w, h, _) => {
                vec![(x, y), (x + w, y + h)]
            }
            Shape::Circle(cx, cy, r) | Shape::Arc(cx, cy, r, _, _) => {
                vec![(cx - r, cy - r), (cx + r, cy + r)]
            }
            Shape::Dot(x, y) => vec![(x, y)],
        }
    }
}

use Shape::*;

fn shapes(icon: AppIcon) -> &'static [Shape] {
    match icon {
        AppIcon::Edit => &[
            Poly(
                &[
                    (16.5, 3.5),
                    (19.5, 6.5),
                    (7.0, 19.0),
                    (3.0, 20.0),
                    (4.0, 16.0),
                ],
                true,
            ),
            Line(12.0, 20.0, 21.0, 20.0),
        ],
        // The back sheet's visible edges, with the front sheet's rounded
        // corners (they were chamfered, which looked octagonal).
        AppIcon::Copy => &[
            Rect(8.0, 8.0, 14.0, 14.0, 2.0),
            Arc(4.0, 14.0, 2.0, 90.0, 180.0),
            Line(2.0, 14.0, 2.0, 4.0),
            Arc(4.0, 4.0, 2.0, 180.0, 270.0),
            Line(4.0, 2.0, 14.0, 2.0),
            Arc(14.0, 4.0, 2.0, 270.0, 360.0),
        ],
        AppIcon::ScanText => &[
            Poly(&[(3.0, 7.0), (3.0, 3.0), (7.0, 3.0)], false),
            Poly(&[(17.0, 3.0), (21.0, 3.0), (21.0, 7.0)], false),
            Poly(&[(21.0, 17.0), (21.0, 21.0), (17.0, 21.0)], false),
            Poly(&[(7.0, 21.0), (3.0, 21.0), (3.0, 17.0)], false),
            Line(7.0, 8.0, 15.0, 8.0),
            Line(7.0, 12.0, 17.0, 12.0),
            Line(7.0, 16.0, 13.0, 16.0),
        ],
        AppIcon::Save => &[
            Poly(
                &[(3.0, 15.0), (3.0, 21.0), (21.0, 21.0), (21.0, 15.0)],
                false,
            ),
            Poly(&[(7.0, 10.0), (12.0, 15.0), (17.0, 10.0)], false),
            Line(12.0, 15.0, 12.0, 3.0),
        ],
        AppIcon::Pin => &[
            Line(12.0, 17.0, 12.0, 22.0),
            Poly(
                &[
                    (8.0, 2.0),
                    (16.0, 2.0),
                    (16.0, 4.0),
                    (15.0, 4.0),
                    (15.0, 10.0),
                    (19.0, 15.0),
                    (19.0, 17.0),
                    (5.0, 17.0),
                    (5.0, 15.0),
                    (9.0, 10.0),
                    (9.0, 4.0),
                    (8.0, 4.0),
                ],
                true,
            ),
        ],
        AppIcon::Scroll => &[
            Rect(5.0, 2.0, 14.0, 20.0, 3.0),
            Line(12.0, 7.0, 12.0, 14.0),
            Poly(&[(9.0, 11.0), (12.0, 14.0), (15.0, 11.0)], false),
        ],
        AppIcon::Close => &[Line(18.0, 6.0, 6.0, 18.0), Line(6.0, 6.0, 18.0, 18.0)],
        AppIcon::Search => &[Circle(11.0, 11.0, 7.0), Line(21.0, 21.0, 16.0, 16.0)],
        AppIcon::Folder => &[Poly(
            &[
                (2.0, 5.0),
                (9.0, 5.0),
                (11.0, 7.0),
                (22.0, 7.0),
                (22.0, 20.0),
                (2.0, 20.0),
            ],
            true,
        )],
        AppIcon::Trash => &[
            Line(3.0, 6.0, 21.0, 6.0),
            Poly(&[(19.0, 6.0), (19.0, 22.0), (5.0, 22.0), (5.0, 6.0)], false),
            Poly(&[(8.0, 6.0), (8.0, 2.0), (16.0, 2.0), (16.0, 6.0)], false),
        ],
        AppIcon::Lock => &[
            Rect(4.0, 11.0, 16.0, 10.0, 2.0),
            Arc(12.0, 7.0, 4.0, 180.0, 360.0),
            Line(8.0, 7.0, 8.0, 11.0),
            Line(16.0, 7.0, 16.0, 11.0),
        ],
        AppIcon::Minus => &[Line(5.0, 12.0, 19.0, 12.0)],
        AppIcon::Plus => &[Line(5.0, 12.0, 19.0, 12.0), Line(12.0, 5.0, 12.0, 19.0)],
        AppIcon::CheckCircle => &[
            Circle(12.0, 12.0, 9.0),
            Poly(&[(8.5, 12.0), (11.0, 14.5), (15.5, 9.5)], false),
        ],
        AppIcon::Alert => &[
            Circle(12.0, 12.0, 9.0),
            Line(12.0, 8.0, 12.0, 13.0),
            Dot(12.0, 16.5),
        ],
        AppIcon::Shield => &[Poly(
            &[
                (12.0, 2.0),
                (20.0, 5.0),
                (20.0, 12.0),
                (16.0, 18.0),
                (12.0, 22.0),
                (8.0, 18.0),
                (4.0, 12.0),
                (4.0, 5.0),
            ],
            true,
        )],
        AppIcon::Crosshair => &[
            Circle(12.0, 12.0, 9.0),
            Line(12.0, 3.0, 12.0, 7.0),
            Line(12.0, 17.0, 12.0, 21.0),
            Line(3.0, 12.0, 7.0, 12.0),
            Line(17.0, 12.0, 21.0, 12.0),
        ],
        AppIcon::Image => &[
            Rect(3.0, 3.0, 18.0, 18.0, 2.0),
            Circle(9.0, 9.0, 2.0),
            Poly(&[(21.0, 15.0), (16.0, 10.0), (6.0, 21.0)], false),
        ],
        AppIcon::Terminal => &[
            Poly(&[(4.0, 17.0), (10.0, 11.0), (4.0, 5.0)], false),
            Line(12.0, 19.0, 20.0, 19.0),
        ],
        AppIcon::Power => &[
            Line(12.0, 2.0, 12.0, 12.0),
            Arc(12.0, 13.0, 8.0, -50.0, 230.0),
        ],
        AppIcon::History => &[
            Arc(12.0, 12.0, 9.0, 200.0, 500.0),
            Poly(&[(3.0, 4.0), (3.0, 9.0), (8.0, 9.0)], false),
            Poly(&[(12.0, 7.0), (12.0, 12.0), (15.0, 14.0)], false),
        ],
        // A toothed gear (a ring with detached spokes read as a sun).
        AppIcon::Gear => &[
            Poly(
                &[
                    (9.8, 4.9),
                    (10.4, 2.1),
                    (13.6, 2.1),
                    (14.2, 4.9),
                    (15.4, 5.4),
                    (17.9, 3.9),
                    (20.1, 6.1),
                    (18.6, 8.6),
                    (19.1, 9.8),
                    (21.9, 10.4),
                    (21.9, 13.6),
                    (19.1, 14.2),
                    (18.6, 15.4),
                    (20.1, 17.9),
                    (17.9, 20.1),
                    (15.4, 18.6),
                    (14.2, 19.1),
                    (13.6, 21.9),
                    (10.4, 21.9),
                    (9.8, 19.1),
                    (8.6, 18.6),
                    (6.1, 20.1),
                    (3.9, 17.9),
                    (5.4, 15.4),
                    (4.9, 14.2),
                    (2.1, 13.6),
                    (2.1, 10.4),
                    (4.9, 9.8),
                    (5.4, 8.6),
                    (3.9, 6.1),
                    (6.1, 3.9),
                    (8.6, 5.4),
                ],
                true,
            ),
            Circle(12.0, 12.0, 3.0),
        ],
        AppIcon::Sparkle => &[Poly(
            &[
                (12.0, 3.0),
                (14.0, 10.0),
                (21.0, 12.0),
                (14.0, 14.0),
                (12.0, 21.0),
                (10.0, 14.0),
                (3.0, 12.0),
                (10.0, 10.0),
            ],
            true,
        )],
        AppIcon::SolidLine => &[Line(4.0, 12.0, 20.0, 12.0)],
        // Short dashes with wide gaps: the round caps eat a unit at each
        // end, and 3-unit gaps closed to 1 unit and read as a solid line.
        AppIcon::Dashed => &[
            Line(3.0, 12.0, 5.5, 12.0),
            Line(10.75, 12.0, 13.25, 12.0),
            Line(18.5, 12.0, 21.0, 12.0),
        ],
        AppIcon::LineStraight => &[Line(5.0, 19.0, 19.0, 5.0)],
        AppIcon::LineCurved => &[Poly(
            &[
                (4.0, 19.0),
                (6.0, 14.0),
                (9.0, 10.0),
                (13.0, 7.5),
                (17.0, 6.0),
                (20.0, 5.5),
            ],
            false,
        )],
        AppIcon::LineSketch => &[Poly(
            &[
                (4.0, 18.0),
                (7.0, 15.0),
                (9.0, 16.0),
                (12.0, 12.0),
                (14.0, 13.0),
                (17.0, 8.0),
                (20.0, 6.0),
            ],
            false,
        )],
        // An eyedropper: squeeze bulb, collar, and a glass tube tapering
        // to the tip (a stick with a ring read as a pin or a match).
        AppIcon::Dropper => &[
            Circle(18.0, 6.0, 3.0),
            Line(10.5, 7.5, 16.5, 13.5),
            Poly(&[(14.0, 9.5), (5.0, 18.5), (4.0, 20.0)], false),
            Poly(&[(14.5, 10.0), (5.5, 19.0), (4.0, 20.0)], false),
            Line(4.0, 20.0, 2.5, 21.5),
        ],
        AppIcon::ArrowBoth => &[
            Line(5.0, 12.0, 19.0, 12.0),
            Poly(&[(8.0, 8.0), (4.0, 12.0), (8.0, 16.0)], false),
            Poly(&[(16.0, 8.0), (20.0, 12.0), (16.0, 16.0)], false),
        ],
        AppIcon::Sidebar => &[Rect(3.0, 4.0, 18.0, 16.0, 2.0), Line(14.0, 4.0, 14.0, 20.0)],
        AppIcon::Chevron => &[Poly(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)], false)],
        AppIcon::RotateLeft => &[
            Arc(12.0, 13.0, 7.0, 200.0, 470.0),
            Poly(&[(3.0, 6.0), (5.0, 11.0), (10.0, 9.0)], false),
        ],
        AppIcon::RotateRight => &[
            Arc(12.0, 13.0, 7.0, -290.0, -20.0),
            Poly(&[(21.0, 6.0), (19.0, 11.0), (14.0, 9.0)], false),
        ],
        AppIcon::FlipH => &[
            Line(12.0, 3.0, 12.0, 21.0),
            Poly(&[(9.0, 6.0), (3.0, 18.0), (9.0, 18.0)], true),
            Poly(&[(15.0, 6.0), (21.0, 18.0), (15.0, 18.0)], false),
        ],
        AppIcon::FlipV => &[
            Line(3.0, 12.0, 21.0, 12.0),
            Poly(&[(6.0, 9.0), (18.0, 3.0), (18.0, 9.0)], true),
            Poly(&[(6.0, 15.0), (18.0, 21.0), (18.0, 15.0)], false),
        ],
        AppIcon::Resize => &[
            Rect(3.0, 3.0, 18.0, 18.0, 2.0),
            Poly(&[(14.0, 7.0), (17.0, 7.0), (17.0, 10.0)], false),
            Line(17.0, 7.0, 11.0, 13.0),
            Poly(&[(7.0, 14.0), (7.0, 17.0), (10.0, 17.0)], false),
        ],
        AppIcon::AddBelow => &[
            Rect(4.0, 3.0, 16.0, 8.0, 1.5),
            Rect(4.0, 14.0, 16.0, 7.0, 1.5),
            Line(12.0, 15.5, 12.0, 19.5),
            Line(10.0, 17.5, 14.0, 17.5),
        ],
        AppIcon::AddBeside => &[
            Rect(3.0, 4.0, 8.0, 16.0, 1.5),
            Rect(14.0, 4.0, 7.0, 16.0, 1.5),
            Line(17.5, 10.0, 17.5, 14.0),
            Line(15.5, 12.0, 19.5, 12.0),
        ],
        AppIcon::BubbleCircle => &[Circle(12.0, 12.0, 8.0), Dot(12.0, 12.0)],
        AppIcon::BubbleSquare => &[Rect(4.0, 4.0, 16.0, 16.0, 5.0), Dot(12.0, 12.0)],
        AppIcon::SpotlightRect => &[
            FaintRect(2.0, 4.0, 20.0, 16.0, 2.0),
            Rect(7.0, 8.5, 10.0, 7.0, 1.5),
        ],
        AppIcon::SpotlightEllipse => &[
            FaintRect(2.0, 4.0, 20.0, 16.0, 2.0),
            Circle(12.0, 12.0, 4.5),
        ],
        AppIcon::ArrowOne => &[
            Line(5.0, 12.0, 19.0, 12.0),
            Poly(&[(15.0, 8.0), (19.0, 12.0), (15.0, 16.0)], false),
        ],
        AppIcon::Redact => &[
            Poly(
                &[
                    (2.5, 12.0),
                    (5.5, 8.6),
                    (9.0, 6.8),
                    (12.0, 6.4),
                    (15.0, 6.8),
                    (18.5, 8.6),
                    (21.5, 12.0),
                    (18.5, 15.4),
                    (15.0, 17.2),
                    (12.0, 17.6),
                    (9.0, 17.2),
                    (5.5, 15.4),
                ],
                true,
            ),
            Circle(12.0, 12.0, 2.8),
            Line(4.0, 3.5, 20.0, 20.5),
        ],
        AppIcon::Tool(_) | AppIcon::Undo | AppIcon::Redo => &[],
    }
}

/// `AppIcon::SHAPED`: every icon drawn from [`shapes`], for the grid
/// test. The list also generates an exhaustive `match` (no wildcard), so
/// a new `AppIcon` variant fails to compile here until it is classified:
/// listed, or added to the not-shape-drawn arm. The grid test can then
/// never silently skip a shape-drawn icon.
macro_rules! shaped_icons {
    ($($icon:ident),* $(,)?) => {
        #[cfg(test)]
        impl AppIcon {
            pub const SHAPED: &'static [AppIcon] = &[$(AppIcon::$icon),*];

            /// Whether `self` is in [`Self::SHAPED`]; exhaustive by
            /// construction.
            fn is_shape_drawn(self) -> bool {
                match self {
                    $(AppIcon::$icon)|* => true,
                    AppIcon::Tool(_) | AppIcon::Undo | AppIcon::Redo => false,
                }
            }
        }
    };
}

shaped_icons![
    Edit,
    Copy,
    ScanText,
    Save,
    Pin,
    Scroll,
    Close,
    Search,
    Folder,
    Trash,
    Lock,
    Minus,
    Plus,
    CheckCircle,
    Alert,
    Shield,
    Crosshair,
    Image,
    Terminal,
    Power,
    History,
    Gear,
    Sparkle,
    SolidLine,
    Dashed,
    LineStraight,
    LineCurved,
    LineSketch,
    ArrowBoth,
    Dropper,
    Sidebar,
    Chevron,
    RotateLeft,
    RotateRight,
    FlipH,
    FlipV,
    Resize,
    AddBelow,
    AddBeside,
    BubbleCircle,
    BubbleSquare,
    SpotlightRect,
    SpotlightEllipse,
    ArrowOne,
    Redact,
];

fn draw_shapes(frame: &mut Frame, list: &[Shape], color: Color, size: f32) {
    let k = size / 24.0;
    let w = (2.0 * k).max(1.5);
    let p = |x: f32, y: f32| Point::new(x * k, y * k);
    for s in list {
        let path = match *s {
            Line(a, b, c, d) => Path::line(p(a, b), p(c, d)),
            Poly(pts, closed) => Path::new(|bld| {
                bld.move_to(p(pts[0].0, pts[0].1));
                for &(x, y) in &pts[1..] {
                    bld.line_to(p(x, y));
                }
                if closed {
                    bld.close();
                }
            }),
            Rect(x, y, rw, rh, r) => {
                Path::rounded_rectangle(p(x, y), iced::Size::new(rw * k, rh * k), (r * k).into())
            }
            FaintRect(x, y, rw, rh, r) => {
                let path = Path::rounded_rectangle(
                    p(x, y),
                    iced::Size::new(rw * k, rh * k),
                    (r * k).into(),
                );
                let faint = Color {
                    a: color.a * FAINT,
                    ..color
                };
                frame.stroke(&path, icon_stroke(faint, w));
                continue;
            }
            Circle(cx, cy, r) => Path::circle(p(cx, cy), r * k),
            Arc(cx, cy, r, a0, a1) => Path::new(|bld| {
                bld.arc(canvas::path::Arc {
                    center: p(cx, cy),
                    radius: r * k,
                    start_angle: iced::Radians(a0.to_radians()),
                    end_angle: iced::Radians(a1.to_radians()),
                })
            }),
            Dot(x, y) => {
                frame.fill(&Path::circle(p(x, y), w * 0.75), color);
                continue;
            }
        };
        frame.stroke(&path, icon_stroke(color, w));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use readshot_ui::editor::ToolState as T;
    use tiny_skia::{
        FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke as SkStroke, Transform,
    };

    const TOOLS: [T; 15] = [
        T::Select,
        T::Rectangle,
        T::Ellipse,
        T::Line,
        T::Arrow,
        T::Pen,
        T::Highlighter,
        T::Text,
        T::Blur,
        T::Pixelate,
        T::Spotlight,
        T::NumberedPin,
        T::Stamp,
        T::Magnifier,
        T::Crop,
    ];

    /// Every glyph in the set: the tools, undo and redo, then the shapes.
    fn every_icon() -> Vec<AppIcon> {
        let mut all: Vec<AppIcon> = TOOLS.iter().map(|&t| AppIcon::Tool(t)).collect();
        all.extend([AppIcon::Undo, AppIcon::Redo]);
        all.extend_from_slice(AppIcon::SHAPED);
        all
    }

    fn name(icon: AppIcon) -> String {
        match icon {
            AppIcon::Tool(t) => format!("Tool-{t:?}"),
            other => format!("{other:?}"),
        }
    }

    fn paint(alpha: f32) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color_rgba8(0, 0, 0, (alpha * 255.0).round() as u8);
        paint.anti_alias = true;
        paint
    }

    fn sk_stroke(width: f32) -> SkStroke {
        SkStroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..SkStroke::default()
        }
    }

    fn push_round_rect(pb: &mut PathBuilder, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let r = r.min(w / 2.0).min(h / 2.0);
        let k = r * 0.552_284_8;
        pb.move_to(x + r, y);
        pb.line_to(x + w - r, y);
        pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
        pb.line_to(x + w, y + h - r);
        pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
        pb.line_to(x + r, y + h);
        pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
        pb.line_to(x, y + r);
        pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
        pb.close();
    }

    fn geo_sk_path(geo: Geo) -> Option<tiny_skia::Path> {
        let mut pb = PathBuilder::new();
        match geo {
            Geo::Path(segs) => {
                for seg in segs {
                    match *seg {
                        M(x, y) => pb.move_to(x, y),
                        L(x, y) => pb.line_to(x, y),
                        C(a, b, c, d, x, y) => pb.cubic_to(a, b, c, d, x, y),
                        Z => pb.close(),
                    }
                }
            }
            Geo::Circle(cx, cy, r) => pb.push_circle(cx, cy, r),
            Geo::Rect(x, y, w, h) => push_round_rect(&mut pb, x, y, w, h, 0.0),
            Geo::RoundRect(x, y, w, h, r) => push_round_rect(&mut pb, x, y, w, h, r),
        }
        pb.finish()
    }

    /// `icon` rasterised black on transparent at `px` pixels, through the
    /// same op lists and shape tables (and the same stroke rules) the
    /// canvas uses. Text (the pin's "1") is not drawn.
    fn rasterize(icon: AppIcon, px: u32) -> Pixmap {
        let mut pm = Pixmap::new(px, px).unwrap();
        let ops = editor_ops(icon);
        if !ops.is_empty() {
            let t = Transform::from_scale(px as f32 / EDITOR_GRID, px as f32 / EDITOR_GRID);
            for op in ops {
                match *op {
                    Op::Stroke(geo, width, alpha) => {
                        let path = geo_sk_path(geo).unwrap();
                        pm.stroke_path(&path, &paint(alpha), &sk_stroke(width), t, None);
                    }
                    Op::Fill(geo, alpha) => {
                        let path = geo_sk_path(geo).unwrap();
                        pm.fill_path(&path, &paint(alpha), FillRule::Winding, t, None);
                    }
                    Op::Text(..) => {}
                }
            }
            return pm;
        }
        let k = px as f32 / 24.0;
        let w = (2.0 * k).max(1.5);
        for shape in shapes(icon) {
            let mut pb = PathBuilder::new();
            let mut alpha = 1.0;
            match *shape {
                Line(a, b, c, d) => {
                    pb.move_to(a * k, b * k);
                    pb.line_to(c * k, d * k);
                }
                Poly(pts, closed) => {
                    pb.move_to(pts[0].0 * k, pts[0].1 * k);
                    for &(x, y) in &pts[1..] {
                        pb.line_to(x * k, y * k);
                    }
                    if closed {
                        pb.close();
                    }
                }
                Rect(x, y, rw, rh, r) => {
                    push_round_rect(&mut pb, x * k, y * k, rw * k, rh * k, r * k)
                }
                FaintRect(x, y, rw, rh, r) => {
                    alpha = FAINT;
                    push_round_rect(&mut pb, x * k, y * k, rw * k, rh * k, r * k)
                }
                Circle(cx, cy, r) => pb.push_circle(cx * k, cy * k, r * k),
                Arc(cx, cy, r, a0, a1) => {
                    let steps = 48;
                    for i in 0..=steps {
                        let a = (a0 + (a1 - a0) * i as f32 / steps as f32).to_radians();
                        let (x, y) = ((cx + r * a.cos()) * k, (cy + r * a.sin()) * k);
                        if i == 0 {
                            pb.move_to(x, y);
                        } else {
                            pb.line_to(x, y);
                        }
                    }
                }
                Dot(x, y) => {
                    pb.push_circle(x * k, y * k, w * 0.75);
                    let path = pb.finish().unwrap();
                    pm.fill_path(
                        &path,
                        &paint(1.0),
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                    continue;
                }
            }
            let path = pb.finish().unwrap();
            pm.stroke_path(
                &path,
                &paint(alpha),
                &sk_stroke(w),
                Transform::identity(),
                None,
            );
        }
        pm
    }

    /// Pixels at least half covered.
    fn ink(pm: &Pixmap) -> Vec<bool> {
        pm.pixels().iter().map(|p| p.alpha() >= 128).collect()
    }

    /// Pixels inked in one glyph but not the other: how much of the picture
    /// differs. Shared frames (a ring, a box) cancel out, so a check in a
    /// circle and an exclamation mark in a circle still differ by their
    /// insides.
    fn difference(a: &[bool], b: &[bool]) -> usize {
        a.iter().zip(b).filter(|(x, y)| x != y).count()
    }

    /// Glyph pairs that are deliberately the same picture: a horizontal
    /// rule is both "solid line" (line style) and "minus" (zoom out), and
    /// the straight line path is the Line tool's stroke. Each pair sits on
    /// a different surface with its own tooltip.
    const SAME_PICTURE: &[(&str, &str)] = &[("SolidLine", "Minus"), ("Tool-Line", "LineStraight")];

    /// Fewest differing pixels (at 24 px) for two glyphs to count as
    /// different pictures: a feature about as big as a short stroke.
    const MIN_DIFFERENCE: usize = 16;

    #[test]
    fn every_glyph_is_its_own_picture() {
        let icons = every_icon();
        let masks: Vec<Vec<bool>> = icons.iter().map(|&i| ink(&rasterize(i, 24))).collect();
        let mut clashes = Vec::new();
        for i in 0..icons.len() {
            for j in i + 1..icons.len() {
                let (a, b) = (name(icons[i]), name(icons[j]));
                let allowed = SAME_PICTURE
                    .iter()
                    .any(|&(x, y)| (a == x && b == y) || (a == y && b == x));
                let d = difference(&masks[i], &masks[j]);
                if !allowed && d < MIN_DIFFERENCE {
                    clashes.push(format!("{a} ~ {b}: {d} px"));
                }
            }
        }
        assert!(clashes.is_empty(), "near-identical glyphs: {clashes:#?}");
    }

    #[test]
    fn every_glyph_draws_something() {
        for icon in every_icon() {
            let inked = ink(&rasterize(icon, 24)).iter().filter(|&&p| p).count();
            assert!(inked >= 12, "{} draws only {inked} pixels", name(icon));
        }
    }

    #[test]
    fn redrawn_glyphs_no_longer_look_like_what_they_were_mistaken_for() {
        let mask = |i| ink(&rasterize(i, 24));
        // Magnifier vs the plain Search glass: the "+" sets it apart, but
        // both are a loupe, so they overlap more than unrelated glyphs.
        let magnifier = mask(AppIcon::Tool(T::Magnifier));
        let search = mask(AppIcon::Search);
        assert!(difference(&magnifier, &search) >= 2 * MIN_DIFFERENCE);
        // Crop vs the four corner ticks of Copy Text / the brand brackets.
        let crop = mask(AppIcon::Tool(T::Crop));
        assert!(difference(&crop, &mask(AppIcon::ScanText)) >= 4 * MIN_DIFFERENCE);
        // The colour-picker dropper vs the push-pin.
        assert!(difference(&mask(AppIcon::Dropper), &mask(AppIcon::Pin)) >= 4 * MIN_DIFFERENCE);
        // The gear has teeth: ink reaches its outer radius at the tooth
        // tips but not between them (the sun had detached spokes and a
        // ring, a different silhouette).
        let gear = rasterize(AppIcon::Gear, 48);
        let alpha = |x: u32, y: u32| gear.pixel(x, y).unwrap().alpha();
        assert!(alpha(24, 5) > 128, "tooth tip at the top");
        assert!(alpha(31, 6) < 64, "gap between teeth");
    }

    #[test]
    fn each_action_has_one_glyph_and_actions_do_not_share() {
        let all = action::ALL;
        for (i, &(a, ga)) in all.iter().enumerate() {
            for &(b, gb) in &all[i + 1..] {
                assert_ne!(ga, gb, "{a} and {b} share a glyph");
            }
        }
        // The overlay's Edit and History's Open in Editor open the same
        // editor: the pencil, never the external-link box.
        assert_eq!(action::OPEN_IN_EDITOR, AppIcon::Edit);
        // Discarding the editor's capture is destructive: the trash.
        assert_eq!(action::DELETE, AppIcon::Trash);
        assert_ne!(action::REDACT, action::SCREEN_RECORDING);
    }

    #[test]
    fn icons_outside_the_shaped_list_draw_no_shapes() {
        let others = [
            AppIcon::Tool(readshot_ui::editor::ToolState::default()),
            AppIcon::Undo,
            AppIcon::Redo,
        ];
        for icon in others {
            assert!(!icon.is_shape_drawn(), "{icon:?}");
            assert!(shapes(icon).is_empty(), "{icon:?} draws shapes: list it");
        }
        for &icon in AppIcon::SHAPED {
            assert!(editor_ops(icon).is_empty(), "{icon:?} is drawn twice");
        }
    }

    #[test]
    fn every_shape_icon_stays_on_the_24_grid() {
        for &icon in AppIcon::SHAPED {
            assert!(icon.is_shape_drawn());
            let shapes = shapes(icon);
            assert!(!shapes.is_empty(), "{icon:?} has no shapes");
            for s in shapes {
                for (x, y) in s.points() {
                    assert!(
                        (0.0..=24.0).contains(&x) && (0.0..=24.0).contains(&y),
                        "{icon:?} {x},{y}"
                    );
                }
            }
        }
    }

    /// Writes every glyph as `<name>.png` (48 px) and `<name>@16.png` into
    /// `$READSHOT_ICON_SHEET_DIR`, for reviewing the set by eye; a no-op
    /// otherwise.
    #[test]
    fn write_glyph_pngs_when_asked() {
        let Ok(dir) = std::env::var("READSHOT_ICON_SHEET_DIR") else {
            return;
        };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for icon in every_icon() {
            for (px, suffix) in [(48, ""), (16, "@16")] {
                let path = dir.join(format!("{}{suffix}.png", name(icon)));
                rasterize(icon, px).save_png(path).unwrap();
            }
        }
    }
}
