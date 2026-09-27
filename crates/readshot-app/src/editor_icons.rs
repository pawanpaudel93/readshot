//! Canvas-drawn editor toolbar icons.
//!
//! These replace font-dependent Unicode glyphs with a small, consistent
//! in-app icon set. The icons are intentionally monochrome so the
//! toolbar's active/hover state remains responsible for emphasis.

use iced::widget::canvas::{self, Frame, Path, Stroke, Text as CanvasText};
use iced::widget::container;
use iced::{Color, Element, Length, Point, Rectangle, Renderer, Theme};

use crate::app::Message;

#[derive(Clone, Copy, Debug)]
pub enum EditorIcon {
    Tool(readshot_ui::editor::ToolState),
    Undo,
    Redo,
}

#[derive(Clone, Copy, Debug)]
struct EditorIconProgram {
    icon: EditorIcon,
    color: Color,
}

pub fn editor_icon<'a>(icon: EditorIcon, enabled: bool) -> Element<'a, Message> {
    let color = if enabled {
        Color::WHITE
    } else {
        Color::from_rgba(1.0, 1.0, 1.0, 0.35)
    };
    let canvas = canvas::Canvas::new(EditorIconProgram { icon, color })
        .width(Length::Fixed(22.0))
        .height(Length::Fixed(22.0));

    container(canvas)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

impl canvas::Program<Message, Theme, Renderer> for EditorIconProgram {
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
        draw_editor_icon(&mut frame, self.icon, self.color);
        vec![frame.into_geometry()]
    }
}

fn draw_editor_icon(frame: &mut Frame, icon: EditorIcon, color: Color) {
    use readshot_ui::editor::ToolState as T;

    match icon {
        EditorIcon::Tool(T::Select) => draw_select_icon(frame, color),
        EditorIcon::Tool(T::Rectangle) => draw_rectangle_icon(frame, color),
        EditorIcon::Tool(T::Ellipse) => draw_ellipse_icon(frame, color),
        EditorIcon::Tool(T::Line) => draw_line_icon(frame, color),
        EditorIcon::Tool(T::Arrow) => draw_arrow_icon(frame, color),
        EditorIcon::Tool(T::Pen) => draw_pen_icon(frame, color),
        EditorIcon::Tool(T::Highlighter) => draw_highlighter_icon(frame, color),
        EditorIcon::Tool(T::Text) => draw_text_icon(frame, color),
        EditorIcon::Tool(T::Blur) => draw_blur_icon(frame, color),
        EditorIcon::Tool(T::Pixelate) => draw_pixelate_icon(frame, color),
        EditorIcon::Tool(T::NumberedPin) => draw_numbered_pin_icon(frame, color),
        EditorIcon::Tool(T::Crop) => draw_crop_icon(frame, color),
        // Undo is the counter-clockwise ↺ (arrowhead at top-left),
        // redo the clockwise ↻ — they were drawn the other way round.
        EditorIcon::Undo => draw_history_icon(frame, color, true),
        EditorIcon::Redo => draw_history_icon(frame, color, false),
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
    stroke_path(frame, &path, color, 2.2);
}

fn draw_arrow_icon(frame: &mut Frame, color: Color) {
    let shaft = line_path(Point::new(4.0, 17.0), Point::new(17.0, 4.0));
    let head = Path::new(|p| {
        p.move_to(Point::new(10.3, 4.5));
        p.line_to(Point::new(17.0, 4.0));
        p.line_to(Point::new(16.4, 10.7));
    });
    stroke_path(frame, &shaft, color, 2.2);
    stroke_path(frame, &head, color, 2.2);
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
    stroke_path(frame, &body, color, 1.6);
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
    stroke_path(frame, &body, color, 1.7);
    stroke_path(
        frame,
        &line_path(Point::new(6.9, 15.2), Point::new(11.2, 19.5)),
        color,
        1.7,
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
    stroke_path(frame, &droplet, color, 1.8);
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
    stroke_path(frame, &pin, color, 1.7);
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
