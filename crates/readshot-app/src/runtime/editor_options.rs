//! Editor options bar: one floating row under the tool bar
//! showing only the active tool's settings, or the selected
//! annotation's. Hidden for Select with nothing selected. The color and
//! width popovers open from it.

use super::*;

use std::ops::RangeInclusive;

use iced::widget::tooltip::Position;
use iced::widget::{button, column, container, row, text, Space};
use iced::{Alignment, Color, Element, Length, Theme};

use readshot_core::{ArrowHeads, FillMode, Rgba};
use readshot_ui::editor::{
    options_for, LinePathKind, OptionControl, StyleMessage, TextSize, ToolStyle,
};

use crate::app::Message;
use crate::editor::{EditorPopover, EditorSession};
use crate::icons::AppIcon;

/// Top of the options bar: just under the tool bar
/// (design-editor-pro.html places it at 100).
pub(crate) const EDITOR_OPTIONS_TOP: f32 = 100.0;
/// A small icon button plus the bar's 3 px padding above and below.
pub(crate) const EDITOR_OPTIONS_HEIGHT: f32 = ICON_BTN_SM + 6.0;

/// The options bar's controls: `options_for`, minus Snap to text when
/// recognition has no line positions yet (or never will: Windows, Linux).
pub(crate) fn visible_option_controls(ed: &EditorSession) -> Vec<OptionControl> {
    let selected = ed
        .model
        .selected_annotation()
        .and_then(|idx| ed.model.annotations().get(idx));
    let mut controls: Vec<OptionControl> = options_for(ed.model.active_tool(), selected)
        .iter()
        .copied()
        .filter(|c| {
            *c != OptionControl::SnapToText
                || (ed.text_panel.positioned_document().is_some()
                    && !ed.model.geometry().transform.is_sideways())
        })
        .collect();
    if ed.crop_draft.is_some() && ed.model.active_tool() == readshot_ui::editor::ToolState::Crop {
        controls.push(OptionControl::CropApply);
    }
    controls
}

pub(crate) fn editor_options_bar<'a>(
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Option<Element<'a, Message>> {
    let controls = visible_option_controls(ed);
    if controls.is_empty() {
        return None;
    }
    let style = ed.model.effective_style();
    let mut bar = row![].spacing(4).align_y(Alignment::Center);
    for (i, control) in controls.iter().enumerate() {
        if i > 0 && matches!(controls[i - 1], OptionControl::Color | OptionControl::Width) {
            bar = bar.push(vsep());
        }
        bar = bar.push(focus_ring(
            option_control(*control, ed, style, p),
            super::key_focus::is_focused(
                ed,
                crate::editor::FocusZone::OptionsBar,
                crate::editor::FocusItem::Option(*control),
            ),
        ));
    }
    Some(
        container(bar)
            .padding([3, 6])
            .height(Length::Fixed(EDITOR_OPTIONS_HEIGHT))
            .align_y(Alignment::Center)
            .style(floating_bar_style)
            .into(),
    )
}

fn option_control<'a>(
    control: OptionControl,
    ed: &'a EditorSession,
    style: ToolStyle,
    p: &'static Palette,
) -> Element<'a, Message> {
    use OptionControl as O;
    let busy = ed.busy;
    let send = move |m: StyleMessage| (!busy).then_some(Message::Editor(EditorMsg::Style(m)));
    match control {
        O::Color => color_button(
            ed.model.effective_color(),
            ed.popover == Some(EditorPopover::Color),
            busy,
        ),
        O::Width => width_button(
            ed.model.effective_line_width(),
            ed.popover == Some(EditorPopover::Width),
            busy,
        ),
        O::Fill => text_segment(
            &[
                ("Outline", FillMode::Outline),
                ("Filled", FillMode::Filled),
                ("Both", FillMode::Both),
            ],
            style.fill,
            |f| send(StyleMessage::Fill(f)),
        ),
        O::Dash => icon_segment(&DASH_SEGMENTS, style.dashed, busy, p, |d| {
            send(StyleMessage::Dashed(d))
        }),
        O::CornerRadius => labeled_slider(
            "Radius",
            0.0..=readshot_ui::editor::style::MAX_CORNER_RADIUS,
            style.corner_radius,
            1.0,
            format!("{:.0}", style.corner_radius),
            &format!("{:.0}", readshot_ui::editor::style::MAX_CORNER_RADIUS),
            busy,
            StyleMessage::CornerRadius,
        ),
        O::LinePath => icon_segment(&LINE_PATH_SEGMENTS, style.line_path, busy, p, |k| {
            send(StyleMessage::LinePath(k))
        }),
        O::Heads => icon_segment(&ARROW_HEAD_SEGMENTS, style.heads, busy, p, |h| {
            send(StyleMessage::Heads(h))
        }),
        O::TextSize => {
            let items: Vec<(&'static str, TextSize)> =
                TextSize::ALL.iter().map(|s| (s.label(), *s)).collect();
            text_segment(&items, style.text_size, |s| send(StyleMessage::TextSize(s)))
        }
        O::TextPill => text_segment(&[("Plain", false), ("Pill", true)], style.text_pill, |v| {
            send(StyleMessage::TextPill(v))
        }),
        O::TextBold => tooltip_with_key(
            segment_button(
                text("B")
                    .size(ButtonSize::Xs.text_size())
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::DEFAULT
                    })
                    .into(),
                style.text_bold,
                send(StyleMessage::TextBold(!style.text_bold)),
            ),
            "Bold",
            None,
            Position::Bottom,
        ),
        O::Dim => labeled_slider(
            "Dim",
            readshot_ui::editor::style::MIN_DIM..=readshot_ui::editor::style::MAX_DIM,
            style.dim,
            0.05,
            format!("{:.0}%", style.dim * 100.0),
            &format!("{:.0}%", readshot_ui::editor::style::MAX_DIM * 100.0),
            busy,
            StyleMessage::Dim,
        ),
        O::SpotlightShape => icon_segment(
            &SPOTLIGHT_SHAPE_SEGMENTS,
            style.spotlight_shape,
            busy,
            p,
            |s| send(StyleMessage::SpotlightShape(s)),
        ),
        O::SnapToText => text_segment(
            &[("Freehand", false), ("Snap to Text", true)],
            style.snap_to_text,
            |v| send(StyleMessage::SnapToText(v)),
        ),
        O::CropAspect => text_segment(
            &readshot_ui::editor::style::CropAspect::ALL.map(|a| (a.label(), a)),
            style.crop_aspect,
            |a| send(StyleMessage::CropAspect(a)),
        ),
        O::MagnifierZoom => text_segment(
            &[("2×", 2.0_f32), ("3×", 3.0), ("4×", 4.0)],
            style.magnifier_zoom,
            |z| send(StyleMessage::MagnifierZoom(z)),
        ),
        O::MagnifierShape => icon_segment(
            &MAGNIFIER_SHAPE_SEGMENTS,
            style.magnifier_shape,
            busy,
            p,
            |s| send(StyleMessage::MagnifierShape(s)),
        ),
        O::CropApply => row![
            bar_button(
                row![
                    text("Cancel").size(ButtonSize::Xs.text_size()),
                    key_chip(ESC_KEY)
                ]
                .spacing(6)
                .align_y(Alignment::Center)
                .into(),
                ButtonKind::Ghost,
                ButtonSize::Xs,
                (!busy).then_some(Message::Editor(EditorMsg::CropCancel)),
            ),
            bar_button(
                row![
                    text("Apply").size(ButtonSize::Xs.text_size()),
                    key_chip(RETURN_KEY)
                ]
                .spacing(6)
                .align_y(Alignment::Center)
                .into(),
                ButtonKind::Secondary,
                ButtonSize::Xs,
                (!busy).then_some(Message::Editor(EditorMsg::CropApply)),
            ),
        ]
        .spacing(SP1)
        .into(),
        O::StampPicker => {
            let mut r = row![].spacing(1).align_y(Alignment::Center);
            for kind in readshot_core::StampKind::ALL {
                let selected = style.stamp == kind;
                let mut b = button(
                    container(
                        text(kind.glyph())
                            .size(TYPE_BODY)
                            .shaping(iced::widget::text::Shaping::Advanced),
                    )
                    .center_x(Length::Fill)
                    .center_y(Length::Fill),
                )
                .width(Length::Fixed(ICON_BTN_SM))
                .height(Length::Fixed(ICON_BTN_SM))
                .padding(0)
                .style(move |t, s| icon_button_style(t, s, selected));
                if !busy {
                    b = b.on_press(Message::Editor(EditorMsg::Style(StyleMessage::Stamp(kind))));
                }
                r = r.push(tooltip_with_key(b, kind.label(), None, Position::Bottom));
            }
            r.into()
        }
    }
}

/// A tool in `NEW_TOOLS` the user has never selected.
pub(crate) fn tool_shows_new_dot(
    prefs: &readshot_core::Preferences,
    tool: readshot_ui::editor::ToolState,
) -> bool {
    readshot_ui::editor::toolbar::NEW_TOOLS.contains(&tool)
        && !prefs.editor_tools_seen.iter().any(|id| id == tool.id())
}

/// Small static amber dot at the button's top-right (not teal: it is not
/// a selection). No animation, so Reduce Motion needs nothing.
pub(crate) fn with_new_tool_dot<'a>(button: Element<'a, Message>) -> Element<'a, Message> {
    let dot = container(
        Space::new()
            .width(Length::Fixed(6.0))
            .height(Length::Fixed(6.0)),
    )
    .style(|theme: &Theme| container::Style {
        background: Some(palette_for(theme).warn.into()),
        border: iced::Border {
            radius: 3.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    iced::widget::stack![
        button,
        container(dot)
            .width(Length::Fixed(ICON_BTN))
            .height(Length::Fixed(ICON_BTN))
            .padding(4)
            .align_x(Alignment::End)
            .align_y(Alignment::Start),
    ]
    .into()
}

/// Line style: solid or dashed.
pub(crate) const DASH_SEGMENTS: [(AppIcon, &str, bool); 2] = [
    (AppIcon::SolidLine, "Solid", false),
    (AppIcon::Dashed, "Dashed", true),
];

/// Line path: straight, curved or hand-drawn.
pub(crate) const LINE_PATH_SEGMENTS: [(AppIcon, &str, LinePathKind); 3] = [
    (AppIcon::LineStraight, "Straight", LinePathKind::Straight),
    (AppIcon::LineCurved, "Curved", LinePathKind::Curved),
    (AppIcon::LineSketch, "Hand-drawn", LinePathKind::HandDrawn),
];

/// Arrow heads. Their own glyphs: the Arrow tool icon sits in the toolbar
/// at the same time and means "draw an arrow".
pub(crate) const ARROW_HEAD_SEGMENTS: [(AppIcon, &str, ArrowHeads); 2] = [
    (AppIcon::ArrowOne, "One Head", ArrowHeads::End),
    (AppIcon::ArrowBoth, "Both Heads", ArrowHeads::Both),
];

/// Spotlight shapes. Their own glyphs: the Rectangle and Ellipse tool
/// icons sit in the toolbar at the same time and mean "draw".
pub(crate) const SPOTLIGHT_SHAPE_SEGMENTS: [(AppIcon, &str, readshot_core::SpotlightShape); 2] = [
    (
        AppIcon::SpotlightRect,
        "Rectangle Spotlight",
        readshot_core::SpotlightShape::Rectangle,
    ),
    (
        AppIcon::SpotlightEllipse,
        "Ellipse Spotlight",
        readshot_core::SpotlightShape::Ellipse,
    ),
];

/// Magnifier bubble shapes. Their own glyphs: the Ellipse and Rectangle
/// tool icons sit in the toolbar at the same time and mean "draw".
pub(crate) const MAGNIFIER_SHAPE_SEGMENTS: [(AppIcon, &str, readshot_core::MagnifierShape); 2] = [
    (
        AppIcon::BubbleCircle,
        "Circle",
        readshot_core::MagnifierShape::Circle,
    ),
    (
        AppIcon::BubbleSquare,
        "Rounded Square",
        readshot_core::MagnifierShape::RoundedSquare,
    ),
];

/// A text (or swatch + text) button in the bar. `selected` uses the same
/// teal selected state as the icon buttons (teal marks selection),
/// so every current value in the bar reads the same way.
/// `msg: None` renders it disabled.
pub(crate) fn segment_button<'a>(
    content: Element<'a, Message>,
    selected: bool,
    msg: Option<Message>,
) -> iced::widget::Button<'a, Message> {
    let size = ButtonSize::Xs;
    let mut b = button(container(content).center_y(Length::Fill))
        .height(Length::Fixed(size.height()))
        .padding([0.0, size.padding_x()])
        .style(move |t, s| icon_button_style(t, s, selected));
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    b
}

/// Text segment: the current value is the (teal) selected state.
pub(crate) fn text_segment<'a, T: Copy + PartialEq>(
    items: &[(&'static str, T)],
    current: T,
    msg: impl Fn(T) -> Option<Message>,
) -> Element<'a, Message> {
    let mut r = row![].spacing(1).align_y(Alignment::Center);
    for &(label, value) in items {
        r = r.push(segment_button(
            text(label).size(ButtonSize::Xs.text_size()).into(),
            value == current,
            msg(value),
        ));
    }
    r.into()
}

/// Icon segment: icon-only buttons with name tooltips; the current value
/// is the (teal) selected state.
pub(crate) fn icon_segment<'a, T: Copy + PartialEq>(
    items: &[(AppIcon, &'static str, T)],
    current: T,
    busy: bool,
    p: &'static Palette,
    msg: impl Fn(T) -> Option<Message>,
) -> Element<'a, Message> {
    let mut r = row![].spacing(1).align_y(Alignment::Center);
    for &(glyph, name, value) in items {
        let selected = value == current;
        let color = if busy {
            Color {
                a: p.t2.a * 0.45,
                ..p.t2
            }
        } else if selected {
            p.acc_text
        } else {
            p.t2
        };
        r = r.push(icon_button(
            glyph,
            color,
            name,
            None,
            msg(value),
            selected,
            ICON_BTN_SM,
            Position::Bottom,
        ));
    }
    r.into()
}

/// Caption + compact slider + mono readout. Drags preview on the
/// selection; release commits one undo step.
#[allow(clippy::too_many_arguments)]
pub(crate) fn labeled_slider<'a>(
    label: &'static str,
    range: RangeInclusive<f32>,
    value: f32,
    step: f32,
    readout: String,
    widest: &str,
    busy: bool,
    make: fn(f32) -> StyleMessage,
) -> Element<'a, Message> {
    let mut r = row![text(label).size(TYPE_CAPTION).style(text_t3)]
        .spacing(SP2)
        .align_y(Alignment::Center);
    if !busy {
        r = r.push(
            iced::widget::slider(range, value, move |v| {
                Message::Editor(EditorMsg::StylePreview(make(v)))
            })
            .step(step)
            .on_release(Message::Editor(EditorMsg::StyleCommit))
            .width(Length::Fixed(72.0)),
        );
    }
    // The bar is centred: a readout that changed width would re-centre
    // it and move the slider under the pointer.
    r.push(mono_readout(readout, widest).style(text_t2)).into()
}

/// The widest line width the Width button can show (half steps up to
/// the maximum), so its label never changes the bar's width.
pub(crate) fn widest_line_width_label() -> String {
    format!(
        "{}\u{a0}px",
        line_width_label(readshot_ui::editor::toolbar::MAX_LINE_WIDTH - 0.5)
    )
}

/// Color popover: 12 presets, the Recent row (last 6
/// custom colors), and a hex field.
pub(crate) fn color_popover<'a>(
    current: Rgba,
    recent: &'a [String],
    hex_draft: &'a str,
    busy: bool,
    p: &'static Palette,
) -> Element<'a, Message> {
    use readshot_ui::editor::toolbar::PALETTE;
    // 7 columns: 7 presets, then 5 presets plus one free cell, where the
    // mockup puts the eyedropper.
    let rows: Vec<&[Rgba]> = PALETTE.chunks(SWATCH_COLUMNS).collect();
    let mut grid = column![].spacing(8);
    for (i, chunk) in rows.iter().enumerate() {
        let mut r = swatch_row(chunk.iter().copied(), current, busy);
        if i + 1 == rows.len() {
            r = r.push(
                container(icon_button(
                    crate::icons::action::PICK_COLOR,
                    if busy {
                        Color {
                            a: p.t2.a * 0.45,
                            ..p.t2
                        }
                    } else {
                        p.t2
                    },
                    "Eyedropper",
                    Some("I"),
                    (!busy).then_some(Message::Editor(EditorMsg::EyedropperToggled)),
                    false,
                    22.0,
                    Position::Bottom,
                ))
                .width(Length::Fixed(22.0))
                .height(Length::Fixed(22.0)),
            );
        }
        grid = grid.push(r);
    }
    let mut body = column![popover_heading("Color"), grid].spacing(8);
    // A hand-edited preferences file may hold duplicates or too many.
    let mut seen: Vec<String> = Vec::new();
    let recent: Vec<Rgba> = recent
        .iter()
        .filter_map(|h| Rgba::from_hex(h))
        .filter(|c| {
            let hex = c.to_hex();
            let fresh = !seen.contains(&hex);
            seen.push(hex);
            fresh
        })
        .take(readshot_core::preferences::RECENT_COLORS_MAX)
        .collect();
    if !recent.is_empty() {
        body = body.push(text("Recent").size(TYPE_MICRO).style(text_t3));
        body = body.push(swatch_row(recent.into_iter(), current, busy));
    }
    let preview = swatch_color(current);
    let mut field = iced::widget::text_input("#RRGGBB", hex_draft)
        .id(iced::widget::Id::new(super::key_focus::COLOR_HEX_ID))
        .font(mono_font())
        .size(TYPE_CAPTION)
        .padding([4, 8])
        .width(Length::Fill)
        .style(text_input_style);
    if !busy {
        field = field
            .on_input(|a| Message::Editor(EditorMsg::HexChanged(a)))
            .on_submit(Message::Editor(EditorMsg::HexSubmitted));
    }
    let hex_row = row![
        container(
            Space::new()
                .width(Length::Fixed(14.0))
                .height(Length::Fixed(14.0))
        )
        .style(move |theme: &Theme| container::Style {
            background: Some(preview.into()),
            border: iced::Border {
                radius: 7.0.into(),
                width: 1.0,
                color: palette_for(theme).border,
            },
            ..Default::default()
        }),
        field,
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .width(Length::Fixed(SWATCH_GRID_WIDTH));
    body.push(hex_row).into()
}

/// Swatch grid columns and width (7 × 22 px swatches, 8 px gaps).
const SWATCH_COLUMNS: usize = 7;
const SWATCH_GRID_WIDTH: f32 = 7.0 * 22.0 + 6.0 * 8.0;

fn swatch_row<'a>(
    colors: impl Iterator<Item = Rgba>,
    current: Rgba,
    busy: bool,
) -> iced::widget::Row<'a, Message> {
    let mut r = row![].spacing(8);
    for swatch in colors {
        let is_current = swatch_eq(swatch, current);
        let fill = swatch_color(swatch);
        let mut b = button(
            Space::new()
                .width(Length::Fixed(22.0))
                .height(Length::Fixed(22.0)),
        )
        .padding(0)
        .style(move |theme: &Theme, status| {
            let p = palette_for(theme);
            let (width, color) = if is_current {
                (2.0, p.t1)
            } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                (1.5, p.t2)
            } else {
                (1.0, p.border)
            };
            button::Style {
                background: Some(fade(fill, if busy { 0.45 } else { 1.0 }).into()),
                border: iced::Border {
                    radius: 11.0.into(),
                    width,
                    color,
                },
                ..Default::default()
            }
        });
        if !busy {
            b = b.on_press(Message::Editor(EditorMsg::Toolbar(
                readshot_ui::ToolbarMessage::SelectColor(swatch),
            )));
        }
        r = r.push(tooltip_with_key(b, swatch.to_hex(), None, Position::Bottom));
    }
    r
}

fn color_button<'a>(color: Rgba, open: bool, busy: bool) -> Element<'a, Message> {
    let fill = swatch_color(color);
    let dot = container(
        container(
            Space::new()
                .width(Length::Fixed(18.0))
                .height(Length::Fixed(18.0)),
        )
        .style(move |theme: &Theme| container::Style {
            background: Some(fade(fill, if busy { 0.45 } else { 1.0 }).into()),
            border: iced::Border {
                radius: 9.0.into(),
                width: 2.0,
                color: palette_for(theme).float,
            },
            ..Default::default()
        }),
    )
    .padding(1.5)
    .style(|theme: &Theme| container::Style {
        background: Some(palette_for(theme).t3.into()),
        border: iced::Border {
            radius: 10.5.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    let mut b = button(container(dot).center_x(Length::Fill).center_y(Length::Fill))
        .width(Length::Fixed(ICON_BTN_SM))
        .height(Length::Fixed(ICON_BTN_SM))
        .padding(0)
        .style(move |t, s| icon_button_style(t, s, open));
    if !busy {
        b = b.on_press(Message::Editor(EditorMsg::PopoverToggled(
            EditorPopover::Color,
        )));
    }
    tooltip_with_key(b, "Color", Some(", ."), Position::Bottom)
}

fn width_button<'a>(line_width: f32, open: bool, busy: bool) -> Element<'a, Message> {
    let b = segment_button(
        row![
            container(
                Space::new()
                    .width(Length::Fixed(16.0))
                    .height(Length::Fixed(3.0))
            )
            .style(|theme: &Theme| container::Style {
                background: Some(palette_for(theme).t1.into()),
                border: iced::Border {
                    radius: 2.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
            mono_readout(
                format!("{}\u{a0}px", line_width_label(line_width)),
                &widest_line_width_label(),
            ),
        ]
        .spacing(SP2)
        .align_y(Alignment::Center)
        .into(),
        open,
        (!busy).then_some(Message::Editor(EditorMsg::PopoverToggled(
            EditorPopover::Width,
        ))),
    );
    tooltip_with_key(b, "Line Width", Some("[ ]"), Position::Bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value a readout can show fits the width reserved for its
    /// widest one, so dragging never resizes it (or moves the bar).
    #[test]
    fn readouts_reserve_room_for_their_widest_value() {
        let fits = |shown: String, widest: &str| {
            mono_readout_width(&shown, TYPE_CAPTION) <= mono_readout_width(widest, TYPE_CAPTION)
        };
        let max_gap = readshot_core::document::MAX_GAP;
        for gap in 0..=max_gap {
            assert!(fits(
                format!("{gap}\u{a0}px"),
                &format!("{max_gap}\u{a0}px")
            ));
        }
        let max_r = readshot_ui::editor::style::MAX_CORNER_RADIUS;
        for r in 0..=max_r as u32 {
            assert!(fits(format!("{r}"), &format!("{max_r:.0}")));
        }
        let widest = widest_line_width_label();
        let mut w = readshot_ui::editor::toolbar::MIN_LINE_WIDTH;
        while w <= readshot_ui::editor::toolbar::MAX_LINE_WIDTH {
            assert!(
                fits(format!("{}\u{a0}px", line_width_label(w)), &widest),
                "{w}"
            );
            w += 0.5;
        }
        for pct in [5, 25, 100, 400, 800] {
            assert!(fits(format!("{pct}%"), "800%"));
        }
        assert_eq!(
            mono_readout_width("40\u{a0}px", 12.0),
            5.0 * 12.0 * 0.6 + 2.0
        );
    }

    fn blank_session() -> crate::editor::EditorSession {
        crate::editor::EditorSession::new(image::RgbaImage::from_pixel(
            32,
            32,
            image::Rgba([255, 255, 255, 255]),
        ))
    }

    #[test]
    fn options_bar_hides_for_select_with_nothing_selected() {
        let ed = blank_session();
        assert!(editor_options_bar(&ed, &DARK).is_none());
    }

    #[test]
    fn new_tools_show_a_dot_until_seen() {
        use readshot_ui::editor::ToolState;
        let mut prefs = readshot_core::Preferences::default();
        assert!(tool_shows_new_dot(&prefs, ToolState::Spotlight));
        assert!(!tool_shows_new_dot(&prefs, ToolState::Rectangle));
        prefs.mark_tool_seen("spotlight");
        assert!(!tool_shows_new_dot(&prefs, ToolState::Spotlight));
    }

    #[test]
    fn options_bar_shows_for_a_drawing_tool() {
        let mut ed = blank_session();
        ed.model.set_tool(readshot_ui::editor::ToolState::Arrow);
        assert!(editor_options_bar(&ed, &DARK).is_some());
    }

    #[test]
    fn snap_to_text_shows_only_with_line_positions() {
        use readshot_core::text_lines::TextDocument;
        let mut ed = blank_session();
        ed.model
            .set_tool(readshot_ui::editor::ToolState::Highlighter);
        assert!(!visible_option_controls(&ed).contains(&OptionControl::SnapToText));
        let positioned = TextDocument::from_recognized(
            &[readshot_core::ocr_layout::RecognizedLine {
                text: "hi".into(),
                x: 0.1,
                y: 0.1,
                w: 0.5,
                h: 0.2,
                words: Vec::new(),
            }],
            32,
            32,
        );
        ed.text_panel.recognition =
            crate::editor::TextRecognition::Ready(std::sync::Arc::new(positioned));
        assert!(visible_option_controls(&ed).contains(&OptionControl::SnapToText));
        ed.text_panel.recognition = crate::editor::TextRecognition::Ready(std::sync::Arc::new(
            TextDocument::from_plain("hi"),
        ));
        assert!(!visible_option_controls(&ed).contains(&OptionControl::SnapToText));
        // Sideways, text runs vertically: no snap.
        ed.text_panel.recognition =
            crate::editor::TextRecognition::Ready(std::sync::Arc::new(positioned_hi()));
        ed.model.rotate(true).unwrap();
        assert!(!visible_option_controls(&ed).contains(&OptionControl::SnapToText));
        ed.model.rotate(true).unwrap();
        assert!(visible_option_controls(&ed).contains(&OptionControl::SnapToText));
    }

    fn positioned_hi() -> readshot_core::text_lines::TextDocument {
        readshot_core::text_lines::TextDocument::from_recognized(
            &[readshot_core::ocr_layout::RecognizedLine {
                text: "hi".into(),
                x: 0.1,
                y: 0.1,
                w: 0.5,
                h: 0.2,
                words: Vec::new(),
            }],
            32,
            32,
        )
    }
}
