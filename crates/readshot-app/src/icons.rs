//! Canvas-drawn icon set: editor tools plus the line icons for every surface.
//!
//! These replace font-dependent Unicode glyphs with a small, consistent
//! in-app icon set. The icons are intentionally monochrome so the
//! toolbar's active/hover state remains responsible for emphasis.

use iced::widget::canvas::{self, Frame, Path, Stroke, Text as CanvasText};
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};

use crate::app::Message;

#[derive(Clone, Copy, Debug)]
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
    Open,
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
    Chevron,
    Frame,
    SolidLine,
    Dashed,
    LineStraight,
    LineCurved,
    LineSketch,
    ArrowBoth,
}

#[derive(Clone, Copy, Debug)]
struct IconProgram {
    icon: AppIcon,
    color: Color,
    size: f32,
}

pub fn icon<'a>(icon: AppIcon, color: Color, size: f32) -> Element<'a, Message> {
    canvas::Canvas::new(IconProgram { icon, color, size })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

impl canvas::Program<Message, Theme, Renderer> for IconProgram {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        match self.icon {
            AppIcon::Tool(_) | AppIcon::Undo | AppIcon::Redo => {
                frame.scale(self.size / 22.0);
                draw_editor_icon(&mut frame, self.icon, self.color);
            }
            other => draw_shapes(&mut frame, shapes(other), self.color, self.size),
        }
        vec![frame.into_geometry()]
    }
}

fn draw_editor_icon(frame: &mut Frame, icon: AppIcon, color: Color) {
    use readshot_ui::editor::ToolState as T;

    match icon {
        AppIcon::Tool(T::Select) => draw_select_icon(frame, color),
        AppIcon::Tool(T::Rectangle) => draw_rectangle_icon(frame, color),
        AppIcon::Tool(T::Ellipse) => draw_ellipse_icon(frame, color),
        AppIcon::Tool(T::Line) => draw_line_icon(frame, color),
        AppIcon::Tool(T::Arrow) => draw_arrow_icon(frame, color),
        AppIcon::Tool(T::Pen) => draw_pen_icon(frame, color),
        AppIcon::Tool(T::Highlighter) => draw_highlighter_icon(frame, color),
        AppIcon::Tool(T::Text) => draw_text_icon(frame, color),
        AppIcon::Tool(T::Blur) => draw_blur_icon(frame, color),
        AppIcon::Tool(T::Pixelate) => draw_pixelate_icon(frame, color),
        AppIcon::Tool(T::NumberedPin) => draw_numbered_pin_icon(frame, color),
        AppIcon::Tool(T::Crop) => draw_crop_icon(frame, color),
        // Undo is the counter-clockwise ↺ (arrowhead at top-left),
        // redo the clockwise ↻ — they were drawn the other way round.
        AppIcon::Undo => draw_history_icon(frame, color, true),
        AppIcon::Redo => draw_history_icon(frame, color, false),
        _ => {}
    }
}

fn icon_stroke(color: Color, width: f32) -> Stroke<'static> {
    Stroke::default()
        .with_color(color)
        .with_width(width)
        .with_line_cap(canvas::LineCap::Round)
        .with_line_join(canvas::LineJoin::Round)
}

fn stroke_path(frame: &mut Frame, path: &Path, color: Color, width: f32) {
    frame.stroke(path, icon_stroke(color, width));
}

fn line_path(from: Point, to: Point) -> Path {
    Path::line(from, to)
}

fn draw_select_icon(frame: &mut Frame, color: Color) {
    let pointer = Path::new(|p| {
        p.move_to(Point::new(5.0, 3.0));
        p.line_to(Point::new(17.0, 13.0));
        p.line_to(Point::new(12.6, 14.0));
        p.line_to(Point::new(15.7, 20.0));
        p.line_to(Point::new(12.9, 21.4));
        p.line_to(Point::new(9.7, 15.5));
        p.line_to(Point::new(6.5, 18.9));
        p.close();
    });
    frame.fill(&pointer, color);
}

fn draw_rectangle_icon(frame: &mut Frame, color: Color) {
    let path = Path::rectangle(Point::new(4.0, 6.0), iced::Size::new(14.0, 10.0));
    stroke_path(frame, &path, color, 2.0);
}

fn draw_ellipse_icon(frame: &mut Frame, color: Color) {
    let path = Path::new(|p| {
        p.move_to(Point::new(18.0, 11.0));
        p.bezier_curve_to(
            Point::new(18.0, 15.4),
            Point::new(14.9, 18.0),
            Point::new(11.0, 18.0),
        );
        p.bezier_curve_to(
            Point::new(7.1, 18.0),
            Point::new(4.0, 15.4),
            Point::new(4.0, 11.0),
        );
        p.bezier_curve_to(
            Point::new(4.0, 6.6),
            Point::new(7.1, 4.0),
            Point::new(11.0, 4.0),
        );
        p.bezier_curve_to(
            Point::new(14.9, 4.0),
            Point::new(18.0, 6.6),
            Point::new(18.0, 11.0),
        );
    });
    stroke_path(frame, &path, color, 2.0);
}

fn draw_line_icon(frame: &mut Frame, color: Color) {
    let path = line_path(Point::new(5.0, 17.0), Point::new(17.0, 5.0));
    stroke_path(frame, &path, color, 2.0);
}

fn draw_arrow_icon(frame: &mut Frame, color: Color) {
    let shaft = line_path(Point::new(4.0, 17.0), Point::new(17.0, 4.0));
    let head = Path::new(|p| {
        p.move_to(Point::new(10.3, 4.5));
        p.line_to(Point::new(17.0, 4.0));
        p.line_to(Point::new(16.4, 10.7));
    });
    stroke_path(frame, &shaft, color, 2.0);
    stroke_path(frame, &head, color, 2.0);
}

fn draw_pen_icon(frame: &mut Frame, color: Color) {
    let body = Path::new(|p| {
        p.move_to(Point::new(5.0, 17.0));
        p.line_to(Point::new(14.4, 7.6));
        p.line_to(Point::new(17.0, 10.2));
        p.line_to(Point::new(7.6, 19.6));
        p.line_to(Point::new(4.6, 20.4));
        p.close();
    });
    let cap = line_path(Point::new(12.7, 6.1), Point::new(18.5, 11.9));
    frame.fill(&body, Color::from_rgba(color.r, color.g, color.b, 0.26));
    stroke_path(frame, &body, color, 1.8);
    stroke_path(frame, &cap, color, 1.8);
}

fn draw_highlighter_icon(frame: &mut Frame, color: Color) {
    let body = Path::new(|p| {
        p.move_to(Point::new(5.0, 14.5));
        p.line_to(Point::new(12.7, 6.8));
        p.line_to(Point::new(17.6, 11.7));
        p.line_to(Point::new(9.9, 19.4));
        p.close();
    });
    let mark = Path::rectangle(Point::new(3.8, 18.0), iced::Size::new(14.4, 2.8));
    frame.fill(&mark, Color::from_rgba(color.r, color.g, color.b, 0.30));
    frame.fill(&body, Color::from_rgba(color.r, color.g, color.b, 0.22));
    stroke_path(frame, &body, color, 1.8);
    stroke_path(
        frame,
        &line_path(Point::new(6.9, 15.2), Point::new(11.2, 19.5)),
        color,
        1.8,
    );
}

fn draw_text_icon(frame: &mut Frame, color: Color) {
    let top = line_path(Point::new(5.0, 5.0), Point::new(17.0, 5.0));
    let stem = line_path(Point::new(11.0, 5.0), Point::new(11.0, 18.0));
    let foot = line_path(Point::new(8.1, 18.0), Point::new(13.9, 18.0));
    stroke_path(frame, &top, color, 2.0);
    stroke_path(frame, &stem, color, 2.0);
    stroke_path(frame, &foot, color, 2.0);
}

fn draw_blur_icon(frame: &mut Frame, color: Color) {
    let droplet = Path::new(|p| {
        p.move_to(Point::new(11.0, 3.7));
        p.bezier_curve_to(
            Point::new(15.4, 8.6),
            Point::new(17.4, 11.9),
            Point::new(17.4, 14.4),
        );
        p.bezier_curve_to(
            Point::new(17.4, 18.0),
            Point::new(14.6, 20.3),
            Point::new(11.0, 20.3),
        );
        p.bezier_curve_to(
            Point::new(7.4, 20.3),
            Point::new(4.6, 18.0),
            Point::new(4.6, 14.4),
        );
        p.bezier_curve_to(
            Point::new(4.6, 11.9),
            Point::new(6.6, 8.6),
            Point::new(11.0, 3.7),
        );
    });
    frame.fill(&droplet, Color::from_rgba(color.r, color.g, color.b, 0.18));
    stroke_path(frame, &droplet, color, 2.0);
    for x in [7.6, 11.0, 14.4] {
        frame.fill(&Path::circle(Point::new(x, 14.8), 1.0), color);
    }
}

fn draw_pixelate_icon(frame: &mut Frame, color: Color) {
    for y in [5.0, 10.0, 15.0] {
        for x in [5.0, 10.0, 15.0] {
            let square = Path::rectangle(Point::new(x, y), iced::Size::new(3.5, 3.5));
            frame.fill(&square, Color::from_rgba(color.r, color.g, color.b, 0.72));
        }
    }
}

fn draw_numbered_pin_icon(frame: &mut Frame, color: Color) {
    let pin = Path::new(|p| {
        p.move_to(Point::new(11.0, 20.5));
        p.line_to(Point::new(7.1, 13.6));
        p.bezier_curve_to(
            Point::new(4.7, 9.3),
            Point::new(7.5, 4.2),
            Point::new(11.0, 4.2),
        );
        p.bezier_curve_to(
            Point::new(14.5, 4.2),
            Point::new(17.3, 9.3),
            Point::new(14.9, 13.6),
        );
        p.close();
    });
    frame.fill(&pin, Color::from_rgba(color.r, color.g, color.b, 0.18));
    stroke_path(frame, &pin, color, 2.0);
    frame.fill_text(CanvasText {
        content: "1".into(),
        position: Point::new(8.8, 7.0),
        color,
        size: iced::Pixels(10.5),
        ..Default::default()
    });
}

fn draw_crop_icon(frame: &mut Frame, color: Color) {
    for (from, mid, to) in [
        (
            Point::new(6.0, 3.5),
            Point::new(6.0, 8.0),
            Point::new(3.5, 8.0),
        ),
        (
            Point::new(16.0, 3.5),
            Point::new(16.0, 8.0),
            Point::new(18.5, 8.0),
        ),
        (
            Point::new(6.0, 18.5),
            Point::new(6.0, 14.0),
            Point::new(3.5, 14.0),
        ),
        (
            Point::new(16.0, 18.5),
            Point::new(16.0, 14.0),
            Point::new(18.5, 14.0),
        ),
    ] {
        let corner = Path::new(|p| {
            p.move_to(from);
            p.line_to(mid);
            p.line_to(to);
        });
        stroke_path(frame, &corner, color, 2.0);
    }
}

fn draw_history_icon(frame: &mut Frame, color: Color, mirrored: bool) {
    let arc = Path::new(|p| {
        if mirrored {
            p.move_to(Point::new(7.0, 8.2));
            p.bezier_curve_to(
                Point::new(9.2, 4.8),
                Point::new(15.3, 4.7),
                Point::new(17.6, 9.1),
            );
            p.bezier_curve_to(
                Point::new(19.7, 13.2),
                Point::new(17.4, 18.2),
                Point::new(13.1, 18.9),
            );
        } else {
            p.move_to(Point::new(15.0, 8.2));
            p.bezier_curve_to(
                Point::new(12.8, 4.8),
                Point::new(6.7, 4.7),
                Point::new(4.4, 9.1),
            );
            p.bezier_curve_to(
                Point::new(2.3, 13.2),
                Point::new(4.6, 18.2),
                Point::new(8.9, 18.9),
            );
        }
    });
    let head = Path::new(|p| {
        if mirrored {
            p.move_to(Point::new(7.4, 4.7));
            p.line_to(Point::new(6.9, 8.3));
            p.line_to(Point::new(10.6, 8.4));
        } else {
            p.move_to(Point::new(14.6, 4.7));
            p.line_to(Point::new(15.1, 8.3));
            p.line_to(Point::new(11.4, 8.4));
        }
    });
    stroke_path(frame, &arc, color, 2.0);
    stroke_path(frame, &head, color, 2.0);
}

#[derive(Clone, Copy, Debug)]
enum Shape {
    Line(f32, f32, f32, f32),
    Poly(&'static [(f32, f32)], bool), // points, closed
    Rect(f32, f32, f32, f32, f32),     // x, y, w, h, radius
    Circle(f32, f32, f32),
    Arc(f32, f32, f32, f32, f32), // cx, cy, r, start_deg, end_deg (clockwise, 0 deg = east)
    Dot(f32, f32),
}

impl Shape {
    #[cfg(test)]
    fn points(&self) -> Vec<(f32, f32)> {
        match *self {
            Shape::Line(a, b, c, d) => vec![(a, b), (c, d)],
            Shape::Poly(p, _) => p.to_vec(),
            Shape::Rect(x, y, w, h, _) => vec![(x, y), (x + w, y + h)],
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
        AppIcon::Copy => &[
            Rect(8.0, 8.0, 14.0, 14.0, 2.0),
            Poly(
                &[
                    (4.0, 16.0),
                    (2.0, 14.0),
                    (2.0, 4.0),
                    (4.0, 2.0),
                    (14.0, 2.0),
                    (16.0, 4.0),
                ],
                false,
            ),
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
        AppIcon::Open => &[
            Poly(&[(15.0, 3.0), (21.0, 3.0), (21.0, 9.0)], false),
            Line(10.0, 14.0, 21.0, 3.0),
            Poly(
                &[
                    (18.0, 13.0),
                    (18.0, 21.0),
                    (3.0, 21.0),
                    (3.0, 6.0),
                    (11.0, 6.0),
                ],
                false,
            ),
        ],
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
        AppIcon::Gear => &[
            Circle(12.0, 12.0, 3.0),
            Circle(12.0, 12.0, 7.5),
            Line(12.0, 2.0, 12.0, 4.5),
            Line(12.0, 19.5, 12.0, 22.0),
            Line(2.0, 12.0, 4.5, 12.0),
            Line(19.5, 12.0, 22.0, 12.0),
            Line(4.9, 4.9, 6.7, 6.7),
            Line(17.3, 17.3, 19.1, 19.1),
            Line(4.9, 19.1, 6.7, 17.3),
            Line(17.3, 6.7, 19.1, 4.9),
        ],
        AppIcon::Chevron => &[Poly(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)], false)],
        AppIcon::Frame => &[
            Rect(2.0, 2.0, 20.0, 20.0, 3.0),
            Rect(6.0, 6.0, 12.0, 12.0, 1.0),
        ],
        AppIcon::SolidLine => &[Line(4.0, 12.0, 20.0, 12.0)],
        AppIcon::Dashed => &[
            Line(3.0, 12.0, 7.0, 12.0),
            Line(10.0, 12.0, 14.0, 12.0),
            Line(17.0, 12.0, 21.0, 12.0),
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
        AppIcon::ArrowBoth => &[
            Line(5.0, 12.0, 19.0, 12.0),
            Poly(&[(8.0, 8.0), (4.0, 12.0), (8.0, 16.0)], false),
            Poly(&[(16.0, 8.0), (20.0, 12.0), (16.0, 16.0)], false),
        ],
        AppIcon::Tool(_) | AppIcon::Undo | AppIcon::Redo => &[],
    }
}

impl AppIcon {
    #[cfg(test)]
    pub const SHAPED: [AppIcon; 31] = [
        AppIcon::Edit,
        AppIcon::Copy,
        AppIcon::ScanText,
        AppIcon::Save,
        AppIcon::Pin,
        AppIcon::Scroll,
        AppIcon::Close,
        AppIcon::Search,
        AppIcon::Folder,
        AppIcon::Open,
        AppIcon::Trash,
        AppIcon::Lock,
        AppIcon::Minus,
        AppIcon::Plus,
        AppIcon::CheckCircle,
        AppIcon::Alert,
        AppIcon::Shield,
        AppIcon::Crosshair,
        AppIcon::Image,
        AppIcon::Terminal,
        AppIcon::Power,
        AppIcon::History,
        AppIcon::Gear,
        AppIcon::Chevron,
        AppIcon::Frame,
        AppIcon::SolidLine,
        AppIcon::Dashed,
        AppIcon::LineStraight,
        AppIcon::LineCurved,
        AppIcon::LineSketch,
        AppIcon::ArrowBoth,
    ];
}

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

    #[test]
    fn every_shape_icon_stays_on_the_24_grid() {
        for icon in AppIcon::SHAPED {
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
}
