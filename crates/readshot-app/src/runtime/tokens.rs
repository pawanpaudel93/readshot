//! Design tokens for the 2026 redesign — the single source of truth for
//! colors, radii, sizes and type. Mirrors the table in
//! docs/superpowers/specs/2026-10-15-ui-redesign-design.md §2.2.
//! Views never inline colors; style closures call `palette_for(theme)`.

use iced::Color;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Palette {
    pub bg: Color,
    pub surface: Color,
    pub raised: Color,
    pub hover: Color,
    pub border_subtle: Color,
    pub border: Color,
    pub t1: Color,
    pub t2: Color,
    pub t3: Color,
    pub acc: Color,
    pub acc_tint: Color,
    pub acc_text: Color,
    pub pri_bg: Color,
    pub pri_fg: Color,
    pub ok: Color,
    pub warn: Color,
    pub danger: Color,
    /// Foreground on a solid `danger` fill (AA contrast per theme).
    pub danger_fg: Color,
    pub float: Color,
    pub float_border: Color,
    pub tip_bg: Color,
    pub kbd_bg: Color,
    pub shadow: Color,
}

pub(crate) const DARK: Palette = Palette {
    bg: Color::from_rgb8(0x11, 0x11, 0x11),
    surface: Color::from_rgb8(0x19, 0x19, 0x19),
    raised: Color::from_rgb8(0x22, 0x22, 0x22),
    hover: Color::from_rgba(1.0, 1.0, 1.0, 0.06),
    border_subtle: Color::from_rgba(1.0, 1.0, 1.0, 0.06),
    border: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
    t1: Color::from_rgb8(0xED, 0xED, 0xED),
    t2: Color::from_rgb8(0xB4, 0xB4, 0xB4),
    t3: Color::from_rgb8(0x8A, 0x8A, 0x8A),
    acc: Color::from_rgb8(0x14, 0xB8, 0xA6),
    acc_tint: Color::from_rgba(0.078, 0.722, 0.651, 0.14),
    acc_text: Color::from_rgb8(0x5E, 0xEA, 0xD4),
    pri_bg: Color::from_rgb8(0xED, 0xED, 0xED),
    pri_fg: Color::from_rgb8(0x0F, 0x0F, 0x0F),
    ok: Color::from_rgb8(0x3D, 0xD6, 0x8C),
    warn: Color::from_rgb8(0xF5, 0xA5, 0x24),
    danger: Color::from_rgb8(0xFF, 0x63, 0x69),
    danger_fg: Color::from_rgb8(0x0F, 0x0F, 0x0F),
    float: Color::from_rgba(0.110, 0.110, 0.118, 0.96),
    float_border: Color::from_rgba(1.0, 1.0, 1.0, 0.10),
    tip_bg: Color::from_rgb8(0x0F, 0x0F, 0x10),
    kbd_bg: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
    shadow: Color::from_rgba(0.0, 0.0, 0.0, 0.45),
};

pub(crate) const LIGHT: Palette = Palette {
    bg: Color::from_rgb8(0xF4, 0xF4, 0xF5),
    surface: Color::from_rgb8(0xFF, 0xFF, 0xFF),
    raised: Color::from_rgb8(0xFF, 0xFF, 0xFF),
    hover: Color::from_rgba(0.0, 0.0, 0.0, 0.05),
    border_subtle: Color::from_rgba(0.0, 0.0, 0.0, 0.06),
    border: Color::from_rgba(0.0, 0.0, 0.0, 0.11),
    t1: Color::from_rgb8(0x17, 0x17, 0x17),
    t2: Color::from_rgb8(0x52, 0x52, 0x5B),
    t3: Color::from_rgb8(0x8A, 0x8A, 0x93),
    acc: Color::from_rgb8(0x0D, 0x94, 0x88),
    acc_tint: Color::from_rgba(0.051, 0.580, 0.533, 0.12),
    acc_text: Color::from_rgb8(0x0F, 0x76, 0x6E),
    pri_bg: Color::from_rgb8(0x17, 0x17, 0x17),
    pri_fg: Color::from_rgb8(0xFF, 0xFF, 0xFF),
    ok: Color::from_rgb8(0x16, 0xA3, 0x4A),
    warn: Color::from_rgb8(0xD9, 0x77, 0x06),
    danger: Color::from_rgb8(0xDC, 0x26, 0x26),
    danger_fg: Color::from_rgb8(0xFF, 0xFF, 0xFF),
    float: Color::from_rgba(1.0, 1.0, 1.0, 0.97),
    float_border: Color::from_rgba(0.0, 0.0, 0.0, 0.10),
    tip_bg: Color::from_rgb8(0x17, 0x17, 0x17),
    kbd_bg: Color::from_rgba(0.0, 0.0, 0.0, 0.06),
    shadow: Color::from_rgba(0.0, 0.0, 0.0, 0.14),
};

pub(crate) fn palette(mode: ThemeMode) -> &'static Palette {
    match mode {
        ThemeMode::Dark => &DARK,
        ThemeMode::Light => &LIGHT,
    }
}

// Radius (spec §2.2): nested corners shrink inward.
pub(crate) const R_CHIP: f32 = 5.0;
pub(crate) const R_TIP: f32 = 7.0;
pub(crate) const R_BUTTON: f32 = 8.0;
pub(crate) const R_GROUP: f32 = 10.0;
pub(crate) const R_WINDOW: f32 = 12.0;

// Control heights.
pub(crate) const BTN_LG: f32 = 36.0;
pub(crate) const BTN_MD: f32 = 32.0;
pub(crate) const BTN_SM: f32 = 28.0;
pub(crate) const BTN_XS: f32 = 24.0;
pub(crate) const ICON_BTN: f32 = 32.0;
pub(crate) const ICON_BTN_SM: f32 = 28.0;
pub(crate) const ICON_BTN_XS: f32 = 24.0;
pub(crate) const ICON_SIZE: f32 = 16.0;

// Type scale.
pub(crate) const TYPE_DISPLAY: f32 = 26.0;
pub(crate) const TYPE_TITLE: f32 = 22.0;
pub(crate) const TYPE_HEADING: f32 = 15.0;
pub(crate) const TYPE_BODY: f32 = 13.0;
pub(crate) const TYPE_CAPTION: f32 = 12.0;
pub(crate) const TYPE_MICRO: f32 = 11.0;

// 4 px spacing grid.
pub(crate) const SP1: f32 = 4.0;
pub(crate) const SP2: f32 = 8.0;
pub(crate) const SP3: f32 = 12.0;
pub(crate) const SP4: f32 = 16.0;

/// Composite `fg` (possibly translucent) over an opaque `bg`.
#[cfg(test)]
pub(crate) fn over(fg: Color, bg: Color) -> Color {
    let a = fg.a;
    Color::from_rgb(
        fg.r * a + bg.r * (1.0 - a),
        fg.g * a + bg.g * (1.0 - a),
        fg.b * a + bg.b * (1.0 - a),
    )
}

#[cfg(test)]
fn channel(c: f32) -> f32 {
    if c <= 0.039_28 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
fn luminance(c: Color) -> f32 {
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

/// WCAG 2.x contrast ratio. Translucent `fg` is composited over `bg`.
#[cfg(test)]
pub(crate) fn contrast_ratio(fg: Color, bg: Color) -> f32 {
    let fg = over(fg, bg);
    let (a, b) = (luminance(fg), luminance(bg));
    let (hi, lo) = if a > b { (a, b) } else { (b, a) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both() -> [&'static Palette; 2] {
        [palette(ThemeMode::Dark), palette(ThemeMode::Light)]
    }

    #[test]
    fn body_text_meets_aa_on_every_surface() {
        for p in both() {
            for bg in [p.bg, p.surface, p.raised] {
                assert!(contrast_ratio(p.t1, bg) >= 4.5, "t1 on {bg:?}");
                assert!(contrast_ratio(p.t2, bg) >= 4.5, "t2 on {bg:?}");
                assert!(contrast_ratio(p.t3, bg) >= 3.0, "t3 on {bg:?}");
            }
        }
    }

    #[test]
    fn primary_button_and_status_colors_are_readable() {
        for p in both() {
            assert!(contrast_ratio(p.pri_fg, p.pri_bg) >= 7.0);
            assert!(contrast_ratio(p.danger_fg, p.danger) >= 4.5);
            assert!(contrast_ratio(p.danger, p.raised) >= 4.5);
            let selected = over(p.acc_tint, p.surface);
            assert!(contrast_ratio(p.acc_text, selected) >= 4.5);
        }
    }

    #[test]
    fn accent_is_the_brand_teal_in_dark_mode() {
        assert_eq!(DARK.acc, Color::from_rgb8(0x14, 0xB8, 0xA6));
    }

    #[test]
    fn contrast_of_black_on_white_is_21() {
        let r = contrast_ratio(Color::BLACK, Color::WHITE);
        assert!((r - 21.0).abs() < 0.01, "{r}");
    }
}
