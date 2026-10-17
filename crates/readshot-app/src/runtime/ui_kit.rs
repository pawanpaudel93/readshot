//! Shared components for the redesign. Every style reads the active
//! palette through `palette_for(theme)`, so light/dark is free.

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

/// Width of the `acc_text` outline on a selected icon button or segment:
/// the non-colour cue (WCAG 1.4.1) next to the teal tint.
pub(crate) const SELECTED_OUTLINE: f32 = 1.5;

/// Selected keeps its teal tint and outline, and status layers on top:
/// hover lightens toward t1, pressed deepens the tint, disabled dims.
pub(crate) fn icon_button_style(
    theme: &Theme,
    status: button::Status,
    selected: bool,
) -> button::Style {
    let p = palette_for(theme);
    let (bg, fg, outline) = if selected {
        match status {
            button::Status::Hovered => (
                mix(p.acc_tint, p.acc, 0.10),
                mix(p.acc_text, p.t1, 0.25),
                p.acc_text,
            ),
            button::Status::Pressed => (mix(p.acc_tint, p.acc, 0.25), p.acc_text, p.acc_text),
            button::Status::Disabled => (
                dim(p.acc_tint, 0.45),
                dim(p.acc_text, 0.45),
                dim(p.acc_text, 0.45),
            ),
            button::Status::Active => (p.acc_tint, p.acc_text, p.acc_text),
        }
    } else {
        let (bg, fg) = match status {
            button::Status::Hovered => (p.hover, p.t1),
            button::Status::Pressed => (mix(p.hover, Color::BLACK, 0.2), p.t1),
            button::Status::Disabled => (Color::TRANSPARENT, dim(p.t2, 0.45)),
            button::Status::Active => (Color::TRANSPARENT, p.t2),
        };
        (bg, fg, Color::TRANSPARENT)
    };
    button::Style {
        background: Some(bg.into()),
        text_color: fg,
        border: Border {
            radius: R_BUTTON.into(),
            width: if selected { SELECTED_OUTLINE } else { 0.0 },
            color: outline,
        },
        ..button::Style::default()
    }
}

/// Colour of an icon button's canvas glyph. The glyph is a canvas, so
/// the button's `text_color` never reaches it; this mirrors the style:
/// selected draws in `acc_text`, hover brightens (any non-accent glyph
/// to t1, selected toward t1), disabled at most 45 % alpha (callers that already dimmed
/// stay as they are).
pub(crate) fn icon_button_glyph_color(
    p: &Palette,
    color: Color,
    selected: bool,
    enabled: bool,
    hovered: bool,
) -> Color {
    let base = if selected { p.acc_text } else { color };
    if enabled && hovered {
        if selected {
            mix(p.acc_text, p.t1, 0.25)
        } else if color == p.acc || color == p.acc_text {
            base
        } else {
            // Any neutral glyph (t2, t3, …) brightens to t1, as the
            // button's hover text colour does.
            p.t1
        }
    } else if enabled {
        base
    } else {
        Color {
            a: base.a.min(0.45),
            ..base
        }
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
        text_color: Some(p.tip_fg),
        border: Border {
            radius: R_TIP.into(),
            width: 1.0,
            color: p.tip_border,
        },
        shadow: Shadow {
            color: p.tip_shadow,
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

/// Key chip on a dark surface (tooltips, overlay): the `tip_kbd_*`
/// tokens, which are dark in both themes.
fn key_chip_on_dark<'a>(label: &'a str) -> Element<'a, Message> {
    container(text(label).size(TYPE_MICRO).style(|theme| text::Style {
        color: Some(palette_for(theme).tip_kbd_fg),
    }))
    .padding([3, 6])
    .style(|theme| container::Style {
        background: Some(palette_for(theme).tip_kbd_bg.into()),
        border: Border {
            radius: R_CHIP.into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

/// (background, text) of a key chip sitting on a Primary button, whose
/// fill is inverted relative to the theme.
pub(crate) fn key_chip_on_primary_colors(p: &Palette) -> (Color, Color) {
    (
        Color {
            a: 0.14,
            ..p.pri_fg
        },
        p.pri_fg,
    )
}

/// Key chip for use inside a Primary button.
pub(crate) fn key_chip_on_primary<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    container(text(label).size(TYPE_MICRO).style(|theme| text::Style {
        color: Some(key_chip_on_primary_colors(palette_for(theme)).1),
    }))
    .padding([3, 6])
    .style(|theme| container::Style {
        background: Some(key_chip_on_primary_colors(palette_for(theme)).0.into()),
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
    key_chip_enabled(label, true)
}

/// (background, text) of a theme key chip; a disabled row's chip dims
/// with it so it does not advertise a key that does nothing.
pub(crate) fn key_chip_colors(p: &Palette, enabled: bool) -> (Color, Color) {
    if enabled {
        (p.kbd_bg, p.t2)
    } else {
        (dim(p.kbd_bg, 0.45), dim(p.t2, 0.45))
    }
}

/// [`key_chip`] that dims when `enabled` is false.
pub(crate) fn key_chip_enabled<'a>(
    label: impl text::IntoFragment<'a>,
    enabled: bool,
) -> Element<'a, Message> {
    container(
        text(label)
            .size(TYPE_MICRO)
            .style(move |theme| text::Style {
                color: Some(key_chip_colors(palette_for(theme), enabled).1),
            }),
    )
    .padding([3, 6])
    .style(move |theme| container::Style {
        background: Some(key_chip_colors(palette_for(theme), enabled).0.into()),
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
    tooltip_with_hint(content, name, key, None, position)
}

/// Widest a tooltip's how-to line runs before it wraps.
pub(crate) const TOOLTIP_HINT_MAX_W: f32 = 240.0;

/// [`tooltip_with_key`] plus an optional how-to line under the name (the
/// tool bar's per-tool hints).
pub(crate) fn tooltip_with_hint<'a>(
    content: impl Into<Element<'a, Message>>,
    name: impl Into<String>,
    key: Option<&'static str>,
    hint: Option<String>,
    position: tooltip::Position,
) -> Element<'a, Message> {
    let mut r = row![text(name.into()).size(TYPE_CAPTION)]
        .spacing(SP2)
        .align_y(Alignment::Center);
    if let Some(k) = key {
        r = r.push(key_chip_on_dark(k));
    }
    let body: Element<'a, Message> = match hint {
        Some(h) => iced::widget::column![
            r,
            container(
                text(h)
                    .size(TYPE_CAPTION)
                    .wrapping(iced::widget::text::Wrapping::Word)
            )
            .max_width(TOOLTIP_HINT_MAX_W),
        ]
        .spacing(SP1)
        .into(),
        None => r.into(),
    };
    let tip = container(body).padding([5, 8]).style(tooltip_style);
    tooltip::Tooltip::new(content.into(), tip, position)
        .gap(SP2)
        .delay(TOOLTIP_DELAY)
        .into()
}

/// The fixed width of a mono readout whose widest value is `widest` at
/// `size`: exact for JetBrains Mono, plus a pixel either side so rounding
/// never wraps or clips it. A changing value then never moves its
/// neighbours (a slider track mid-drag, a centred bar).
pub(crate) fn mono_readout_width(widest: &str, size: f32) -> f32 {
    widest.chars().count() as f32 * size * MONO_ADVANCE + 2.0
}

/// A right-aligned mono readout with room for `widest`: the digits and
/// unit hold their place as the value changes.
pub(crate) fn mono_readout<'a>(value: String, widest: &str) -> iced::widget::Text<'a> {
    text(value)
        .font(crate::runtime::mono_font())
        .size(TYPE_CAPTION)
        .width(Length::Fixed(mono_readout_width(widest, TYPE_CAPTION)))
        .align_x(iced::alignment::Horizontal::Right)
        .wrapping(iced::widget::text::Wrapping::None)
}

/// A pixel size as every readout writes it: "1200 × 800 px".
pub(crate) fn size_px(w: impl std::fmt::Display, h: impl std::fmt::Display) -> String {
    format!("{w} × {h} px")
}

/// "1 capture" / "3 captures": a count with the whole singular or plural
/// word, never a suffix pasted on, so each form can be translated.
pub(crate) fn counted(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// A key label for this platform: the macOS glyph form (`mac`), or the
/// Ctrl form (`other`) elsewhere. Every new key chip and tooltip goes
/// through it so Windows and Linux never advertise a ⌘ chord.
pub(crate) const fn platform_key(mac: &'static str, other: &'static str) -> &'static str {
    if cfg!(target_os = "macos") {
        mac
    } else {
        other
    }
}

/// The keyboard focus ring around `content` (`on`), drawn as a layer over
/// it so focusing never moves anything. The layer is always present, so
/// the widget tree (and any text field's own focus state) is the same
/// with and without the ring. It takes no pointer input.
pub(crate) fn focus_ring<'a>(
    content: impl Into<Element<'a, Message>>,
    on: bool,
) -> Element<'a, Message> {
    let content = content.into();
    // The stack takes the control's own sizing, so a Fill button in a row
    // still shares the row as before.
    let size = content.as_widget().size();
    let ring = container(Space::new().width(Length::Fill).height(Length::Fill))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(move |theme| focus_ring_style(theme, on));
    iced::widget::Stack::with_children([content, ring.into()])
        .width(size.width)
        .height(size.height)
        .into()
}

pub(crate) fn focus_ring_style(theme: &Theme, on: bool) -> container::Style {
    let p = palette_for(theme);
    container::Style {
        border: Border {
            radius: R_BUTTON.into(),
            width: if on { FOCUS_RING_WIDTH } else { 0.0 },
            color: if on {
                focus_ring_color(p)
            } else {
                Color::TRANSPARENT
            },
        },
        ..container::Style::default()
    }
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

/// Each kind has its own glyph, so severity is not carried by colour
/// alone (WCAG 1.4.1): a warning and an error differ in shape too.
pub(crate) fn toast_glyph(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::Ok => "✓",
        ToastKind::Info => "•",
        ToastKind::Warn => "!",
        ToastKind::Error => "✕",
    }
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
    let mut r = row![
        text(toast_glyph(kind)).size(TYPE_BODY).style(dot_style),
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
    icon_button_hinted(glyph, color, name, key, None, msg, selected, size, position)
}

/// [`icon_button`] whose tooltip also carries a how-to line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn icon_button_hinted<'a>(
    glyph: crate::icons::AppIcon,
    color: Color,
    name: &'static str,
    key: Option<&'static str>,
    hint: Option<String>,
    msg: Option<Message>,
    selected: bool,
    size: f32,
    position: tooltip::Position,
) -> Element<'a, Message> {
    let enabled = msg.is_some();
    let mut b = button(crate::icons::button_icon(
        glyph, color, ICON_SIZE, selected, enabled,
    ))
    .width(Length::Fixed(size))
    .height(Length::Fixed(size))
    .padding(0)
    .style(move |t, s| icon_button_style(t, s, selected));
    if let Some(m) = msg {
        b = b.on_press(m);
    }
    tooltip_with_hint(b, name, key, hint, position)
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
                radius: R_WINDOW.into(),
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
    fn selected_is_marked_by_an_outline_not_colour_alone() {
        for t in [dark(), light()] {
            let p = palette_for(&t);
            let s = icon_button_style(&t, Status::Active, true);
            assert_eq!(s.border.width, SELECTED_OUTLINE);
            assert_eq!(s.border.color, p.acc_text);
            let u = icon_button_style(&t, Status::Active, false);
            assert_eq!(u.border.width, 0.0);
        }
    }

    #[test]
    fn selected_icon_button_shows_hover_pressed_and_disabled() {
        for t in [dark(), light()] {
            let active = icon_button_style(&t, Status::Active, true);
            let hovered = icon_button_style(&t, Status::Hovered, true);
            let pressed = icon_button_style(&t, Status::Pressed, true);
            let disabled = icon_button_style(&t, Status::Disabled, true);
            assert_ne!(bg(&active), bg(&hovered));
            assert_ne!(bg(&hovered), bg(&pressed));
            assert_ne!(bg(&active), bg(&pressed));
            assert!(disabled.text_color.a < 0.5);
            assert!(disabled.border.color.a < 0.5);
            assert!(bg(&disabled).a < bg(&active).a);
        }
    }

    #[test]
    fn icon_glyph_follows_the_button_state() {
        let p = &LIGHT;
        assert_eq!(icon_button_glyph_color(p, p.t2, false, true, false), p.t2);
        assert_eq!(
            icon_button_glyph_color(p, p.t1, true, true, false),
            p.acc_text
        );
        let off = icon_button_glyph_color(p, p.t2, false, false, false);
        assert!(off.a <= 0.45);
        // Already-dimmed colours are not dimmed twice.
        let pre = Color { a: 0.45, ..p.t2 };
        assert_eq!(icon_button_glyph_color(p, pre, false, false, false), pre);
        let sel_off = icon_button_glyph_color(p, p.t2, true, false, false);
        assert_eq!((sel_off.r, sel_off.a), (p.acc_text.r, 0.45));
        // Hover follows the style's text colour; a disabled button ignores it.
        assert_eq!(icon_button_glyph_color(p, p.t2, false, true, true), p.t1);
        // Any neutral glyph brightens (the Remove-preset ✕ is t3); an
        // accent glyph keeps its colour.
        assert_eq!(icon_button_glyph_color(p, p.t3, false, true, true), p.t1);
        assert_eq!(
            icon_button_glyph_color(p, p.acc_text, false, true, true),
            p.acc_text
        );
        assert_ne!(
            icon_button_glyph_color(p, p.t2, true, true, true),
            p.acc_text
        );
        assert!(icon_button_glyph_color(p, p.t2, false, false, true).a <= 0.45);
    }

    #[test]
    fn a_disabled_key_chip_dims() {
        for p in [&DARK, &LIGHT] {
            assert_eq!(key_chip_colors(p, true), (p.kbd_bg, p.t2));
            let (bg, fg) = key_chip_colors(p, false);
            assert!(fg.a <= 0.5 && bg.a < p.kbd_bg.a);
        }
    }

    /// Outlined Danger (Settings Reset / Uninstall, History Clear All)
    /// keeps its red label at AA in every state that is not disabled.
    #[test]
    fn outlined_danger_label_stays_readable_on_hover_and_press() {
        for t in [dark(), light()] {
            for s in [Status::Active, Status::Hovered, Status::Pressed] {
                let st = button_style(&t, s, ButtonKind::Danger);
                let r = contrast_ratio(st.text_color, bg(&st));
                assert!(r >= 4.5, "{s:?}: {r:.2}:1");
            }
        }
    }

    #[test]
    fn every_toast_kind_has_its_own_glyph() {
        let kinds = [
            ToastKind::Ok,
            ToastKind::Info,
            ToastKind::Warn,
            ToastKind::Error,
        ];
        for (i, a) in kinds.iter().enumerate() {
            for b in &kinds[i + 1..] {
                assert_ne!(toast_glyph(*a), toast_glyph(*b), "{a:?} vs {b:?}");
            }
        }
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

#[cfg(test)]
mod chip_tests {
    use super::*;

    #[test]
    fn on_primary_chip_uses_pri_fg_and_is_readable_in_both_palettes() {
        for p in [&DARK, &LIGHT] {
            let (bg, fg) = key_chip_on_primary_colors(p);
            assert_eq!(fg, p.pri_fg);
            // Chip fill composited over the primary button fill.
            let over = |a: f32| Color {
                r: bg.r * a + p.pri_bg.r * (1.0 - a),
                g: bg.g * a + p.pri_bg.g * (1.0 - a),
                b: bg.b * a + p.pri_bg.b * (1.0 - a),
                a: 1.0,
            };
            let fill = over(bg.a);
            assert!(contrast_ratio(fg, fill) >= 4.5);
        }
    }
}

#[cfg(test)]
mod focus_ring_tests {
    use super::*;

    #[test]
    fn the_focus_ring_shows_only_when_on_and_uses_the_ring_token() {
        for mode in [ThemeMode::Dark, ThemeMode::Light] {
            let theme = readshot_theme_for(mode);
            let p = palette(mode);
            let on = focus_ring_style(&theme, true);
            assert_eq!(on.border.width, FOCUS_RING_WIDTH);
            assert_eq!(on.border.color, focus_ring_color(p));
            assert!(on.background.is_none(), "the ring never hides the control");
            let off = focus_ring_style(&theme, false);
            assert_eq!(off.border.width, 0.0);
        }
    }

    /// Wrapping a control in the ring never changes how it is laid out:
    /// a Fill button still fills, a fixed one keeps its size.
    #[test]
    fn the_focus_ring_keeps_the_controls_sizing() {
        let fill = focus_ring(
            bar_button(
                text("Copy").into(),
                ButtonKind::Secondary,
                ButtonSize::Sm,
                None,
            )
            .width(Length::Fill),
            true,
        );
        assert_eq!(fill.as_widget().size().width, Length::Fill);
        let fixed = focus_ring(
            container(Space::new())
                .width(Length::Fixed(84.0))
                .height(Length::Fixed(24.0)),
            false,
        );
        let size = fixed.as_widget().size();
        assert_eq!(
            (size.width, size.height),
            (Length::Fixed(84.0), Length::Fixed(24.0))
        );
    }

    #[test]
    fn platform_key_picks_the_glyph_form_on_macos_only() {
        let k = platform_key("⇧⌘I", "Ctrl+Shift+I");
        if cfg!(target_os = "macos") {
            assert_eq!(k, "⇧⌘I");
        } else {
            assert_eq!(k, "Ctrl+Shift+I");
        }
    }
}
