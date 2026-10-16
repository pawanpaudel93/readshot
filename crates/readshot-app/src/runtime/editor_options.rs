//! Editor options bar (spec §2.2): one floating row under the tool bar
//! showing only the active tool's settings, or the selected
//! annotation's. Hidden for Select with nothing selected. The color and
//! width popovers open from it.

use super::*;

use std::ops::RangeInclusive;

use iced::widget::tooltip::Position;
use iced::widget::{button, container, row, text, Space};
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

pub(crate) fn editor_options_bar<'a>(
    ed: &'a EditorSession,
    p: &'static Palette,
) -> Option<Element<'a, Message>> {
    let selected = ed
        .model
        .selected_annotation()
        .and_then(|idx| ed.model.annotations().get(idx));
    let controls = options_for(ed.model.active_tool(), selected);
    if controls.is_empty() {
        return None;
    }
    let style = ed.model.effective_style();
    let mut bar = row![].spacing(4).align_y(Alignment::Center);
    for (i, control) in controls.iter().enumerate() {
        if i > 0 && matches!(controls[i - 1], OptionControl::Color | OptionControl::Width) {
            bar = bar.push(vsep());
        }
        bar = bar.push(option_control(*control, ed, style, p));
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
    let send = move |m: StyleMessage| (!busy).then_some(Message::EditorStyle(m));
    match control {
        O::Color => color_button(
            ed.model.effective_color(),
            ed.popover == Some(EditorPopover::Color),
            busy,
        ),
        O::Width => width_button(
            ed.model.current_line_width(),
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
        O::Dash => icon_segment(
            &[
                (AppIcon::SolidLine, "Solid", false),
                (AppIcon::Dashed, "Dashed", true),
            ],
            style.dashed,
            busy,
            p,
            |d| send(StyleMessage::Dashed(d)),
        ),
        O::CornerRadius => labeled_slider(
            "Radius",
            0.0..=readshot_ui::editor::style::MAX_CORNER_RADIUS,
            style.corner_radius,
            1.0,
            format!("{:.0}", style.corner_radius),
            busy,
            StyleMessage::CornerRadius,
        ),
        O::LinePath => icon_segment(
            &[
                (AppIcon::LineStraight, "Straight", LinePathKind::Straight),
                (AppIcon::LineCurved, "Curved", LinePathKind::Curved),
                (AppIcon::LineSketch, "Hand-drawn", LinePathKind::HandDrawn),
            ],
            style.line_path,
            busy,
            p,
            |k| send(StyleMessage::LinePath(k)),
        ),
        O::Heads => icon_segment(
            &[
                (
                    AppIcon::Tool(readshot_ui::editor::ToolState::Arrow),
                    "One head",
                    ArrowHeads::End,
                ),
                (AppIcon::ArrowBoth, "Both heads", ArrowHeads::Both),
            ],
            style.heads,
            busy,
            p,
            |h| send(StyleMessage::Heads(h)),
        ),
        O::TextSize => {
            let items: Vec<(&'static str, TextSize)> =
                TextSize::ALL.iter().map(|s| (s.label(), *s)).collect();
            text_segment(&items, style.text_size, |s| send(StyleMessage::TextSize(s)))
        }
        O::TextPill => text_segment(&[("Plain", false), ("Pill", true)], style.text_pill, |v| {
            send(StyleMessage::TextPill(v))
        }),
        O::TextBold => tooltip_with_key(
            bar_button(
                text("B")
                    .size(ButtonSize::Xs.text_size())
                    .font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::DEFAULT
                    })
                    .into(),
                if style.text_bold {
                    ButtonKind::Secondary
                } else {
                    ButtonKind::Ghost
                },
                ButtonSize::Xs,
                send(StyleMessage::TextBold(!style.text_bold)),
            ),
            "Bold",
            None,
            Position::Bottom,
        ),
    }
}

/// Text segment: the current value reads as Secondary, the rest Ghost.
pub(crate) fn text_segment<'a, T: Copy + PartialEq>(
    items: &[(&'static str, T)],
    current: T,
    msg: impl Fn(T) -> Option<Message>,
) -> Element<'a, Message> {
    let mut r = row![].spacing(1).align_y(Alignment::Center);
    for &(label, value) in items {
        r = r.push(bar_button(
            text(label).size(ButtonSize::Xs.text_size()).into(),
            if value == current {
                ButtonKind::Secondary
            } else {
                ButtonKind::Ghost
            },
            ButtonSize::Xs,
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
pub(crate) fn labeled_slider<'a>(
    label: &'static str,
    range: RangeInclusive<f32>,
    value: f32,
    step: f32,
    readout: String,
    busy: bool,
    make: fn(f32) -> StyleMessage,
) -> Element<'a, Message> {
    let mut r = row![text(label).size(TYPE_CAPTION).style(text_t3)]
        .spacing(6)
        .align_y(Alignment::Center);
    if !busy {
        r = r.push(
            iced::widget::slider(range, value, move |v| Message::EditorStylePreview(make(v)))
                .step(step)
                .on_release(Message::EditorStyleCommit)
                .width(Length::Fixed(72.0)),
        );
    }
    r.push(
        text(readout)
            .font(mono_font())
            .size(TYPE_CAPTION)
            .style(text_t2),
    )
    .into()
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
        b = b.on_press(Message::EditorPopoverToggled(EditorPopover::Color));
    }
    tooltip_with_key(b, "Color", Some(", ."), Position::Bottom)
}

fn width_button<'a>(line_width: f32, open: bool, busy: bool) -> Element<'a, Message> {
    let b = bar_button(
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
            text(format!("{} px", line_width_label(line_width)))
                .font(mono_font())
                .size(TYPE_CAPTION),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        if open {
            ButtonKind::Secondary
        } else {
            ButtonKind::Ghost
        },
        ButtonSize::Xs,
        (!busy).then_some(Message::EditorPopoverToggled(EditorPopover::Width)),
    );
    tooltip_with_key(b, "Line width", Some("[ ]"), Position::Bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn options_bar_shows_for_a_drawing_tool() {
        let mut ed = blank_session();
        ed.model.set_tool(readshot_ui::editor::ToolState::Arrow);
        assert!(editor_options_bar(&ed, &DARK).is_some());
    }
}
