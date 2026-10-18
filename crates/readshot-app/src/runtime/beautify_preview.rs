//! Live canvas preview of Beautify. The export is
//! `readshot_core::beautify::render`; the preview approximates it with
//! iced containers (padding, background, radius, shadow) so slider drags
//! never re-render pixels. Geometry comes from the same
//! `beautify::output_geometry`, so sizes and placement match the export.

use iced::widget::container;
use iced::{Color, Shadow, Vector};
use readshot_core::beautify::{output_geometry, Background, BeautifySettings};

pub(crate) fn rgba_color(rgba: [u8; 4]) -> Color {
    Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3] as f32 / 255.0)
}

pub(crate) fn rgb_shadow_color(rgb: [u8; 3], alpha: u8) -> Color {
    Color::from_rgba8(rgb[0], rgb[1], rgb[2], alpha as f32 / 255.0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PreviewMetrics {
    /// Displayed px per image px (what the annotation canvas maps with).
    pub scale: f32,
    /// Displayed px per output px.
    pub out_scale: f32,
    pub output_w: f32,
    pub output_h: f32,
    /// Space around the image inside the output, displayed px.
    pub pad: iced::Padding,
}

/// Fit scales the whole output into `available`; an explicit zoom is in
/// image pixels (100 % = one image px per screen px), as before.
pub(crate) fn preview_metrics(
    image_w: u32,
    image_h: u32,
    s: &BeautifySettings,
    available: iced::Size,
    explicit_zoom: Option<f32>,
) -> PreviewMetrics {
    let g =
        output_geometry(image_w, image_h, s).unwrap_or(readshot_core::beautify::OutputGeometry {
            width: image_w,
            height: image_h,
            image_x: 0,
            image_y: 0,
            image_w,
            image_h,
        });
    // Social may show the image smaller than its own pixels.
    let inner = g.image_w as f32 / image_w.max(1) as f32;
    let out_scale = match explicit_zoom {
        Some(z) => z / inner.max(f32::EPSILON),
        None => super::editor_fit_scale(available, g.width, g.height),
    };
    let right = g.width.saturating_sub(g.image_x + g.image_w) as f32;
    let bottom = g.height.saturating_sub(g.image_y + g.image_h) as f32;
    PreviewMetrics {
        scale: out_scale * inner,
        out_scale,
        output_w: (g.width as f32 * out_scale).max(1.0),
        output_h: (g.height as f32 * out_scale).max(1.0),
        pad: iced::Padding {
            top: g.image_y as f32 * out_scale,
            left: g.image_x as f32 * out_scale,
            right: right * out_scale,
            bottom: bottom * out_scale,
        },
    }
}

pub(crate) fn background_color_or_gradient(bg: Background) -> Option<iced::Background> {
    match bg {
        Background::Transparent => None,
        Background::Solid { rgba } => Some(rgba_color(rgba).into()),
        Background::Gradient { gradient } => Some(
            iced::gradient::Linear::new(iced::Radians(std::f32::consts::PI))
                .add_stop(0.0, rgba_color(gradient.top))
                .add_stop(1.0, rgba_color(gradient.bottom))
                .into(),
        ),
    }
}

/// The output canvas: background, rounded outer edge, shadow.
pub(crate) fn beautify_canvas_style(s: &BeautifySettings, out_scale: f32) -> container::Style {
    let shadow = s.shadow.map_or_else(Shadow::default, |sh| Shadow {
        color: rgb_shadow_color(sh.color, sh.alpha),
        offset: Vector::new(sh.offset as f32 * out_scale, sh.offset as f32 * out_scale),
        blur_radius: sh.layers as f32 * 5.0 * out_scale,
    });
    container::Style {
        background: background_color_or_gradient(s.background),
        border: iced::Border {
            radius: ((s.corner_radius + s.padding / 3) as f32 * out_scale).into(),
            ..Default::default()
        },
        shadow,
        ..Default::default()
    }
}

/// The image card: mat, rounded corners, 1 px border.
pub(crate) fn beautify_image_style(s: &BeautifySettings, out_scale: f32) -> container::Style {
    container::Style {
        background: s.mat.map(|rgba| rgba_color(rgba).into()),
        border: iced::Border {
            radius: (s.corner_radius as f32 * out_scale).into(),
            color: s.border.map(rgba_color).unwrap_or(Color::TRANSPARENT),
            width: if s.border.is_some() { 1.0 } else { 0.0 },
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use readshot_core::beautify::{BuiltinPreset, CanvasSize};

    #[test]
    fn no_frame_preview_is_just_the_image() {
        let m = preview_metrics(
            400,
            200,
            &BuiltinPreset::None.settings(),
            iced::Size::new(800.0, 800.0),
            None,
        );
        assert_eq!(m.scale, 2.0);
        assert_eq!((m.output_w, m.output_h), (800.0, 400.0));
        assert_eq!(m.pad, iced::Padding::ZERO);
    }

    #[test]
    fn fit_includes_the_frame_and_explicit_zoom_is_image_pixels() {
        let s = BuiltinPreset::Minimal.settings(); // pad 24 → 448 × 248 output
        let fit = preview_metrics(400, 200, &s, iced::Size::new(448.0, 448.0), None);
        assert_eq!(fit.out_scale, 1.0);
        assert_eq!(fit.pad.left, 24.0);
        let zoomed = preview_metrics(400, 200, &s, iced::Size::new(100.0, 100.0), Some(2.0));
        assert_eq!(zoomed.scale, 2.0);
        assert_eq!(zoomed.output_w, 896.0);
    }

    #[test]
    fn social_downscale_keeps_the_canvas_mapping_on_the_image() {
        let mut s = BuiltinPreset::Minimal.settings();
        s.canvas = CanvasSize::Social;
        let m = preview_metrics(4000, 1000, &s, iced::Size::new(1200.0, 675.0), None);
        // The image is shown at its exported size (1152 px wide at out_scale 1).
        assert!((m.out_scale - 1.0).abs() < 1e-6);
        assert!((m.scale - 1152.0 / 4000.0).abs() < 1e-6);
    }
}
