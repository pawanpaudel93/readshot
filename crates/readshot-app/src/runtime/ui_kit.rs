//! Shared components for the redesign. Every style reads the active
//! palette through `palette_for(theme)`, so light/dark is free.
#![allow(dead_code)] // removed in Task 14

use std::time::Duration;

use iced::widget::{button, container, row, text, tooltip, Space};
use iced::{Alignment, Border, Color, Element, Length, Shadow, Theme, Vector};

use super::*;
use crate::app::Message;

pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
    DangerSolid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ButtonSize {
    Lg,
    Md,
    Sm,
    Xs,
}

impl ButtonSize {
    pub(crate) fn height(self) -> f32 {
        match self {
            Self::Lg => BTN_LG,
            Self::Md => BTN_MD,
            Self::Sm => BTN_SM,
            Self::Xs => BTN_XS,
        }
    }
    pub(crate) fn padding_x(self) -> f32 {
        match self {
            Self::Lg => 14.0,
            Self::Md => 12.0,
            Self::Sm => 10.0,
            Self::Xs => 8.0,
        }
    }
    pub(crate) fn text_size(self) -> f32 {
        match self {
            Self::Lg | Self::Md => TYPE_BODY,
            Self::Sm => TYPE_CAPTION,
            Self::Xs => TYPE_MICRO,
        }
    }
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgba(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
        a.a + (b.a - a.a) * t,
    )
}

fn dim(c: Color, f: f32) -> Color {
    Color { a: c.a * f, ..c }
}

pub(crate) fn button_style(
    theme: &Theme,
    status: button::Status,
    kind: ButtonKind,
) -> button::Style {
    let p = palette_for(theme);
    let (base, fg, border) = match kind {
        ButtonKind::Primary => (p.pri_bg, p.pri_fg, Color::TRANSPARENT),
        ButtonKind::Secondary => (p.raised, p.t1, p.border),
        ButtonKind::Ghost => (Color::TRANSPARENT, p.t2, Color::TRANSPARENT),
        ButtonKind::Danger => (p.raised, p.danger, p.border),
        ButtonKind::DangerSolid => (p.danger, p.danger_fg, Color::TRANSPARENT),
    };
    let toward = if palette_for(theme) == &LIGHT {
        Color::BLACK
    } else {
        Color::WHITE
    };
    let (bg, fg, border) = match status {
        button::Status::Hovered => match kind {
            ButtonKind::Ghost => (p.hover, p.t1, border),
            _ => (mix(base, toward, 0.08), fg, border),
        },
        button::Status::Pressed => (mix(base, Color::BLACK, 0.06), fg, border),
        button::Status::Disabled => (dim(base, 0.45), dim(fg, 0.5), dim(border, 0.45)),
        button::Status::Active => (base, fg, border),
    };
    button::Style {
        background: Some(bg.into()),
        text_color: fg,
        border: Border {
            radius: R_BUTTON.into(),
            width: if border.a > 0.0 { 1.0 } else { 0.0 },
            color: border,
        },
        ..button::Style::default()
    }
}

pub(crate) fn icon_button_style(
    theme: &Theme,
    status: button::Status,
    selected: bool,
) -> button::Style {
    let p = palette_for(theme);
    let (bg, fg) = if selected {
        (p.acc_tint, p.acc_text)
    } else {
        match status {
            button::Status::Hovered => (p.hover, p.t1),
            button::Status::Pressed => (mix(p.hover, Color::BLACK, 0.2), p.t1),
            button::Status::Disabled => (Color::TRANSPARENT, dim(p.t2, 0.45)),
            button::Status::Active => (Color::TRANSPARENT, p.t2),
        }
    };
    button::Style {
        background: Some(bg.into()),
        text_color: fg,
        border: Border {
            radius: R_BUTTON.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

fn float_shadow(p: &Palette) -> Shadow {
    Shadow {
        color: p.shadow,
        offset: Vector::new(0.0, 12.0),
        blur_radius: 32.0,
    }
}

pub(crate) fn floating_bar_style(theme: &Theme) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: Some(p.float.into()),
        border: Border {
            radius: R_WINDOW.into(),
            width: 1.0,
            color: p.float_border,
        },
        shadow: float_shadow(p),
        ..container::Style::default()
    }
}

pub(crate) fn popover_style(theme: &Theme) -> container::Style {
    floating_bar_style(theme)
}

pub(crate) fn bg_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(palette_for(theme).bg.into()),
        ..container::Style::default()
    }
}

pub(crate) fn surface_style(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(palette_for(theme).surface.into()),
        ..container::Style::default()
    }
}

pub(crate) fn group_style(theme: &Theme) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: Some(p.surface.into()),
        border: Border {
            radius: R_GROUP.into(),
            width: 1.0,
            color: p.border_subtle,
        },
        ..container::Style::default()
    }
}

pub(crate) fn tooltip_style(theme: &Theme) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        background: Some(p.tip_bg.into()),
        text_color: Some(Color::from_rgb8(0xED, 0xED, 0xED)),
        border: Border {
            radius: R_TIP.into(),
            width: 1.0,
            color: Color::from_rgba(1.0, 1.0, 1.0, 0.12),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, 0.35),
            offset: Vector::new(0.0, 8.0),
            blur_radius: 20.0,
        },
        ..container::Style::default()
    }
}

macro_rules! text_role {
    ($name:ident, $field:ident) => {
        pub(crate) fn $name(theme: &Theme) -> text::Style {
            text::Style {
                color: Some(palette_for(theme).$field),
            }
        }
    };
}
text_role!(text_t1, t1);
text_role!(text_t2, t2);
text_role!(text_t3, t3);
text_role!(text_ok, ok);
text_role!(text_warn, warn);
text_role!(text_danger, danger);
text_role!(text_acc, acc_text);

/// Key chip on a dark surface (tooltips, overlay). Fixed colors: tooltips
/// are dark in both themes.
fn key_chip_on_dark<'a>(label: &'a str) -> Element<'a, Message> {
    container(
        text(label)
            .size(TYPE_MICRO)
            .color(Color::from_rgb8(0xE5, 0xE5, 0xE5)),
    )
    .padding([3, 6])
    .style(|_| container::Style {
        background: Some(Color::from_rgba(1.0, 1.0, 1.0, 0.12).into()),
        border: Border {
            radius: R_CHIP.into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

/// Key chip that follows the theme (legends, footers, hints).
pub(crate) fn key_chip<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    container(text(label).size(TYPE_MICRO).style(text_t2))
        .padding([3, 6])
        .style(|theme| container::Style {
            background: Some(palette_for(theme).kbd_bg.into()),
            border: Border {
                radius: R_CHIP.into(),
                ..Border::default()
            },
            ..container::Style::default()
        })
        .into()
}

/// Every icon-only control goes through this: name + optional key chip,
/// 400 ms hover delay. (iced shows tooltips on hover only; keyboard-focus
/// tooltips are not available in iced 0.14 — documented limitation.)
pub(crate) fn tooltip_with_key<'a>(
    content: impl Into<Element<'a, Message>>,
    name: impl Into<String>,
    key: Option<&'static str>,
    position: tooltip::Position,
) -> Element<'a, Message> {
    let mut r = row![text(name.into()).size(TYPE_CAPTION)]
        .spacing(SP2)
        .align_y(Alignment::Center);
    if let Some(k) = key {
        r = r.push(key_chip_on_dark(k));
    }
    let tip = container(r).padding([5, 8]).style(tooltip_style);
    tooltip::Tooltip::new(content.into(), tip, position)
        .gap(SP2)
        .delay(TOOLTIP_DELAY)
        .into()
}

pub(crate) fn vsep() -> Element<'static, Message> {
    container(
        container(
            Space::new()
                .width(Length::Fixed(1.0))
                .height(Length::Fixed(18.0)),
        )
        .style(|theme| container::Style {
            background: Some(palette_for(theme).border.into()),
            ..container::Style::default()
        }),
    )
    .padding([0, 4])
    .into()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Ok,
    Info,
    Warn,
    Error,
}

pub(crate) fn toast<'a>(
    kind: ToastKind,
    message: String,
    action: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let dot_style = match kind {
        ToastKind::Ok => text_ok,
        ToastKind::Info => text_t2,
        ToastKind::Warn => text_warn,
        ToastKind::Error => text_danger,
    };
    let glyph = match kind {
        ToastKind::Ok => "✓",
        ToastKind::Info => "•",
        ToastKind::Warn | ToastKind::Error => "!",
    };
    let mut r = row![
        text(glyph).size(TYPE_BODY).style(dot_style),
        text(message)
            .size(TYPE_BODY)
            .style(text_t1)
            .wrapping(text::Wrapping::Word),
    ]
    .spacing(SP2)
    .align_y(Alignment::Center);
    if let Some(a) = action {
        r = r.push(a);
    }
    container(r)
        .padding([8, 12])
        .max_width(460)
        .style(|theme| container::Style {
            border: Border {
                radius: R_GROUP.into(),
                ..floating_bar_style(theme).border
            },
            ..floating_bar_style(theme)
        })
        .into()
}

/// Text fields: raised fill, 1 px border, 2 px accent ring when focused (spec §2.3 focus ring).
pub(crate) fn text_input_style(
    theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    use iced::widget::text_input::Status;
    let p = palette_for(theme);
    let focused = matches!(status, Status::Focused { .. });
    iced::widget::text_input::Style {
        background: p.raised.into(),
        border: Border {
            radius: R_BUTTON.into(),
            width: if focused { 2.0 } else { 1.0 },
            color: if focused { p.acc } else { p.border },
        },
        icon: p.t3,
        placeholder: p.t3,
        value: p.t1,
        selection: p.acc_tint,
    }
}

pub(crate) fn group_row<'a>(
    label: &'a str,
    help: impl Into<String>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let help = help.into();
    let mut left = iced::widget::column![text(label).size(TYPE_BODY).style(text_t1)].spacing(2);
    if !help.is_empty() {
        left = left.push(
            text(help)
                .size(TYPE_CAPTION)
                .style(text_t3)
                .wrapping(text::Wrapping::Word),
        );
    }
    row![left.width(Length::Fill), control]
        .spacing(SP3)
        .align_y(Alignment::Center)
        .padding([10, 14])
        .into()
}

#[allow(clippy::too_many_arguments)] // signature fixed by the redesign plan
pub(crate) fn icon_button<'a>(
    glyph: crate::icons::AppIcon,
    color: Color,
    name: &'static str,
    key: Option<&'static str>,
    msg: Option<Message>,
    selected: bool,
    size: f32,
    position: tooltip::Position,
) -> Element<'a, Message> {
    let mut b = button(
        container(crate::icons::icon(glyph, color, ICON_SIZE))
            .center_x(Length::Fill)
            .center_y(Length::Fill),
    )
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .padding(0)
    .style(move |t, s| icon_button_style(t, s, selected));
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    tooltip_with_key(b, name, key, position)
}

pub(crate) fn empty_state<'a>(
    glyph: crate::icons::AppIcon,
    p: &'static Palette,
    title: &'a str,
    body: &'a str,
    action: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let tile = container(crate::icons::icon(glyph, p.t2, 22.0))
        .width(Length::Fixed(44.0))
        .height(Length::Fixed(44.0))
        .center_x(Length::Fixed(44.0))
        .center_y(Length::Fixed(44.0))
        .style(|theme| container::Style {
            background: Some(palette_for(theme).hover.into()),
            border: Border {
                radius: 12.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });
    let mut col = iced::widget::column![
        tile,
        text(title)
            .size(TYPE_BODY)
            .font(iced::Font {
                weight: iced::font::Weight::Semibold,
                ..iced::Font::DEFAULT
            })
            .style(text_t1),
        text(body)
            .size(TYPE_CAPTION)
            .style(text_t2)
            .align_x(iced::alignment::Horizontal::Center),
    ]
    .spacing(SP2)
    .align_x(Alignment::Center)
    .max_width(320);
    if let Some(a) = action {
        col = col.push(a);
    }
    container(col)
        .center_x(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::widget::button::Status;

    fn dark() -> Theme {
        readshot_theme_for(ThemeMode::Dark)
    }
    fn light() -> Theme {
        readshot_theme_for(ThemeMode::Light)
    }

    fn bg(s: &button::Style) -> Color {
        match s.background {
            Some(iced::Background::Color(c)) => c,
            _ => Color::TRANSPARENT,
        }
    }

    #[test]
    fn primary_is_the_inverted_surface() {
        assert_eq!(
            bg(&button_style(&dark(), Status::Active, ButtonKind::Primary)),
            DARK.pri_bg
        );
        assert_eq!(
            bg(&button_style(&light(), Status::Active, ButtonKind::Primary)),
            LIGHT.pri_bg
        );
        assert_eq!(
            button_style(&dark(), Status::Active, ButtonKind::Primary).text_color,
            DARK.pri_fg
        );
    }

    #[test]
    fn every_button_uses_the_button_radius() {
        for k in [
            ButtonKind::Primary,
            ButtonKind::Secondary,
            ButtonKind::Ghost,
            ButtonKind::Danger,
            ButtonKind::DangerSolid,
        ] {
            assert_eq!(
                button_style(&dark(), Status::Active, k).border.radius,
                R_BUTTON.into()
            );
        }
    }

    #[test]
    fn pressed_differs_from_hover_and_disabled_dims() {
        let h = button_style(&dark(), Status::Hovered, ButtonKind::Secondary);
        let p = button_style(&dark(), Status::Pressed, ButtonKind::Secondary);
        assert_ne!(bg(&h), bg(&p));
        let d = button_style(&dark(), Status::Disabled, ButtonKind::Primary);
        assert!(d.text_color.a < 0.6);
    }

    #[test]
    fn selected_icon_button_uses_accent_tint() {
        let s = icon_button_style(&dark(), Status::Active, true);
        assert_eq!(bg(&s), DARK.acc_tint);
        assert_eq!(s.text_color, DARK.acc_text);
    }

    #[test]
    fn button_sizes_match_spec() {
        assert_eq!(ButtonSize::Lg.height(), 36.0);
        assert_eq!(ButtonSize::Md.height(), 32.0);
        assert_eq!(ButtonSize::Sm.height(), 28.0);
        assert_eq!(ButtonSize::Xs.height(), 24.0);
    }

    #[test]
    fn danger_solid_uses_the_palette_foreground() {
        assert_eq!(
            button_style(&dark(), Status::Active, ButtonKind::DangerSolid).text_color,
            DARK.danger_fg
        );
        assert_eq!(
            button_style(&light(), Status::Active, ButtonKind::DangerSolid).text_color,
            LIGHT.danger_fg
        );
    }
}
