//! Beautify (spec §3.9): the presentation around the editor output —
//! background, padding, corner radius, shadow and canvas size. Pure:
//! settings in, pixels out. The six built-in presets reproduce the
//! retired Frame styles byte for byte (`tests/beautify_parity.rs`), so the
//! pixel helpers below are ported verbatim from the old frame renderer.

use image::imageops::{self, FilterType};
use image::{Rgba, RgbaImage};
use serde::{Deserialize, Serialize};

pub const MAX_PADDING: u32 = 200;
pub const MAX_CORNER_RADIUS: u32 = 32;
pub const MAX_SHADOW_LEVEL: u8 = 4;
pub const SOCIAL_SIZE: (u32, u32) = (1200, 675);
/// Bounds for deserialised shadows (the strongest slider stop is 8 layers, 18 px).
pub const MAX_SHADOW_LAYERS: u32 = 16;
pub const MAX_SHADOW_OFFSET: i64 = 200;
pub const MAX_SAVED_PRESETS: usize = 12;
/// Picking a background while padding is 0 would show nothing; pad first.
pub const PADDING_WHEN_BACKGROUND_PICKED: u32 = 48;

/// Top → bottom linear gradient (the preview uses `Linear::new(Radians(PI))`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Gradient {
    pub top: [u8; 4],
    pub bottom: [u8; 4],
}

pub const GRADIENTS: [Gradient; 5] = [
    Gradient {
        top: [99, 102, 241, 255],
        bottom: [236, 72, 153, 255],
    },
    Gradient {
        top: [14, 165, 233, 255],
        bottom: [16, 185, 129, 255],
    },
    Gradient {
        top: [251, 146, 60, 255],
        bottom: [244, 63, 94, 255],
    },
    Gradient {
        top: [148, 163, 184, 255],
        bottom: [51, 65, 85, 255],
    },
    Gradient {
        top: [253, 224, 71, 255],
        bottom: [132, 204, 22, 255],
    },
];

pub const SOLIDS: [[u8; 4]; 6] = [
    [255, 255, 255, 255],
    [248, 250, 252, 255],
    [229, 234, 240, 255],
    [17, 24, 39, 255],
    [0, 0, 0, 255],
    [254, 243, 199, 255],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Background {
    Transparent,
    Solid { rgba: [u8; 4] },
    Gradient { gradient: Gradient },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shadow {
    pub color: [u8; 3],
    pub offset: i64,
    pub layers: u32,
    pub alpha: u8,
}

/// The shadow slider's stops: none → strong.
pub const SHADOW_LEVELS: [Option<Shadow>; 5] = [
    None,
    Some(Shadow {
        color: [15, 23, 42],
        offset: 4,
        layers: 3,
        alpha: 16,
    }),
    Some(Shadow {
        color: [15, 23, 42],
        offset: 8,
        layers: 5,
        alpha: 22,
    }),
    Some(Shadow {
        color: [15, 23, 42],
        offset: 12,
        layers: 6,
        alpha: 30,
    }),
    Some(Shadow {
        color: [15, 23, 42],
        offset: 18,
        layers: 8,
        alpha: 42,
    }),
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanvasSize {
    #[default]
    Auto,
    Wide,
    Standard,
    Square,
    Social,
}

impl CanvasSize {
    pub const ALL: [Self; 5] = [
        Self::Auto,
        Self::Wide,
        Self::Standard,
        Self::Square,
        Self::Social,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Wide => "16:9",
            Self::Standard => "4:3",
            Self::Square => "1:1",
            Self::Social => "Social",
        }
    }

    fn ratio(self) -> Option<(u64, u64)> {
        match self {
            Self::Wide => Some((16, 9)),
            Self::Standard => Some((4, 3)),
            Self::Square => Some((1, 1)),
            Self::Auto | Self::Social => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BeautifySettings {
    pub background: Background,
    pub padding: u32,
    pub corner_radius: u32,
    pub shadow: Option<Shadow>,
    /// Opaque card drawn 1 px around the image (legacy frame "mat").
    pub mat: Option<[u8; 4]>,
    /// 1 px stroke around the image (legacy frame border).
    pub border: Option<[u8; 4]>,
    pub canvas: CanvasSize,
    /// Auto Balance: center the image in the canvas (off = top-left at
    /// the padding, extra space right/bottom).
    pub balance: bool,
}

impl Default for BeautifySettings {
    fn default() -> Self {
        BuiltinPreset::None.settings()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinPreset {
    None,
    Soft,
    Light,
    Dark,
    Minimal,
    Transparent,
}

impl BuiltinPreset {
    pub const ALL: [Self; 6] = [
        Self::None,
        Self::Soft,
        Self::Light,
        Self::Dark,
        Self::Minimal,
        Self::Transparent,
    ];

    /// Stable ids: the retired `EditorFrameStyle` variant names.
    pub fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Soft => "soft",
            Self::Light => "light",
            Self::Dark => "dark",
            Self::Minimal => "minimal",
            Self::Transparent => "transparent",
        }
    }

    /// Same labels the Frame popover showed.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "No Frame",
            Self::Soft => "Soft Shadow",
            Self::Light => "Light Card",
            Self::Dark => "Dark Card",
            Self::Minimal => "Minimal Border",
            Self::Transparent => "Transparent",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id)
    }

    /// Exact values of the retired `framed_image_preset`.
    pub fn settings(self) -> BeautifySettings {
        let base = BeautifySettings {
            background: Background::Transparent,
            padding: 0,
            corner_radius: 0,
            shadow: None,
            mat: None,
            border: None,
            canvas: CanvasSize::Auto,
            balance: true,
        };
        match self {
            Self::None => base,
            Self::Soft => BeautifySettings {
                background: Background::Solid {
                    rgba: [229, 234, 240, 255],
                },
                padding: 72,
                corner_radius: 18,
                shadow: Some(Shadow {
                    color: [15, 23, 42],
                    offset: 18,
                    layers: 8,
                    alpha: 30,
                }),
                mat: Some([255, 255, 255, 255]),
                border: Some([148, 163, 184, 180]),
                ..base
            },
            Self::Light => BeautifySettings {
                background: Background::Solid {
                    rgba: [248, 250, 252, 255],
                },
                padding: 56,
                corner_radius: 14,
                shadow: Some(Shadow {
                    color: [71, 85, 105],
                    offset: 12,
                    layers: 5,
                    alpha: 18,
                }),
                mat: Some([255, 255, 255, 255]),
                border: Some([203, 213, 225, 220]),
                ..base
            },
            Self::Dark => BeautifySettings {
                background: Background::Solid {
                    rgba: [17, 24, 39, 255],
                },
                padding: 64,
                corner_radius: 18,
                shadow: Some(Shadow {
                    color: [0, 0, 0],
                    offset: 16,
                    layers: 8,
                    alpha: 42,
                }),
                mat: Some([31, 41, 55, 255]),
                border: Some([94, 234, 212, 180]),
                ..base
            },
            Self::Minimal => BeautifySettings {
                background: Background::Solid {
                    rgba: [255, 255, 255, 255],
                },
                padding: 24,
                corner_radius: 8,
                mat: Some([255, 255, 255, 255]),
                border: Some([203, 213, 225, 255]),
                ..base
            },
            Self::Transparent => BeautifySettings {
                background: Background::Solid { rgba: [0, 0, 0, 0] },
                padding: 32,
                corner_radius: 16,
                border: Some([255, 255, 255, 180]),
                ..base
            },
        }
    }
}

/// One Beautify control change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeautifyEdit {
    Background(Background),
    Padding(u32),
    CornerRadius(u32),
    ShadowLevel(u8),
    Canvas(CanvasSize),
    Balance(bool),
}

/// A user-saved preset (Save Preset), persisted in `Preferences`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SavedBeautifyPreset {
    pub name: String,
    pub settings: BeautifySettings,
}

impl BeautifySettings {
    /// Output == input (No Frame): nothing drawn around the image.
    pub fn is_identity(&self) -> bool {
        self.background == Background::Transparent
            && self.padding == 0
            && self.corner_radius == 0
            && self.shadow.is_none()
            && self.mat.is_none()
            && self.border.is_none()
            && self.canvas == CanvasSize::Auto
    }

    pub fn apply(&mut self, edit: BeautifyEdit) {
        match edit {
            BeautifyEdit::Background(b) => {
                self.background = b;
                if self.padding == 0 && b != Background::Transparent {
                    self.padding = PADDING_WHEN_BACKGROUND_PICKED;
                }
            }
            BeautifyEdit::Padding(p) => self.padding = p.min(MAX_PADDING),
            BeautifyEdit::CornerRadius(r) => self.corner_radius = r.min(MAX_CORNER_RADIUS),
            BeautifyEdit::ShadowLevel(l) => {
                self.shadow = SHADOW_LEVELS[usize::from(l.min(MAX_SHADOW_LEVEL))]
            }
            BeautifyEdit::Canvas(c) => self.canvas = c,
            BeautifyEdit::Balance(b) => self.balance = b,
        }
    }

    /// Slider position for the current shadow: 0 for none, otherwise the
    /// level with the nearest offset (presets carry their own shadows).
    pub fn shadow_level(&self) -> u8 {
        let Some(s) = self.shadow else { return 0 };
        (1..=MAX_SHADOW_LEVEL)
            .min_by_key(|l| {
                let lv = SHADOW_LEVELS[usize::from(*l)].expect("levels 1..=4 are Some");
                (lv.offset - s.offset).unsigned_abs()
            })
            .unwrap_or(1)
    }

    /// Settings with every field forced into the range the UI can produce,
    /// so deserialised (hand-edited / corrupt) values can't panic or exhaust
    /// memory in `render`. A no-op for anything the UI or presets make.
    pub fn clamped(&self) -> Self {
        let mut s = *self;
        s.padding = s.padding.min(MAX_PADDING);
        s.corner_radius = s.corner_radius.min(MAX_CORNER_RADIUS);
        if let Some(sh) = s.shadow.as_mut() {
            sh.layers = sh.layers.min(MAX_SHADOW_LAYERS);
            sh.offset = sh.offset.clamp(-MAX_SHADOW_OFFSET, MAX_SHADOW_OFFSET);
        }
        s
    }

    /// The built-in preset these settings equal, if any (chip highlight).
    pub fn matching_builtin(&self) -> Option<BuiltinPreset> {
        BuiltinPreset::ALL
            .into_iter()
            .find(|p| p.settings() == *self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputGeometry {
    pub width: u32,
    pub height: u32,
    pub image_x: u32,
    pub image_y: u32,
    pub image_w: u32,
    pub image_h: u32,
}

/// Output canvas and image placement for a `w × h` image. `None` only on
/// arithmetic overflow (the caller then outputs the image unchanged).
pub fn output_geometry(w: u32, h: u32, s: &BeautifySettings) -> Option<OutputGeometry> {
    let s = &s.clamped();
    if s.is_identity() {
        return Some(OutputGeometry {
            width: w,
            height: h,
            image_x: 0,
            image_y: 0,
            image_w: w,
            image_h: h,
        });
    }
    let pad = s.padding;
    let pad2 = pad.checked_mul(2)?;
    let (mut iw, mut ih) = (w, h);
    let (ow, oh) = if s.canvas == CanvasSize::Social {
        let (sw, sh) = SOCIAL_SIZE;
        let inner_w = sw.saturating_sub(pad2).max(1) as u64;
        let inner_h = sh.saturating_sub(pad2).max(1) as u64;
        if w > 0 && h > 0 && (w as u64 > inner_w || h as u64 > inner_h) {
            // Downscale only: the smaller ratio wins, integer math.
            if (inner_w * h as u64) <= (inner_h * w as u64) {
                iw = inner_w as u32;
                ih = ((h as u64 * inner_w + w as u64 / 2) / w as u64).max(1) as u32;
            } else {
                ih = inner_h as u32;
                iw = ((w as u64 * inner_h + h as u64 / 2) / h as u64).max(1) as u32;
            }
        }
        (sw, sh)
    } else {
        let mut ow = w.checked_add(pad2)?.max(1) as u64;
        let mut oh = h.checked_add(pad2)?.max(1) as u64;
        if let Some((rw, rh)) = s.canvas.ratio() {
            if ow * rh < oh * rw {
                ow = (oh * rw).div_ceil(rh);
            } else {
                oh = (ow * rh).div_ceil(rw);
            }
        }
        (u32::try_from(ow).ok()?, u32::try_from(oh).ok()?)
    };
    let (x, y) = if s.balance {
        ((ow.saturating_sub(iw)) / 2, (oh.saturating_sub(ih)) / 2)
    } else {
        (
            pad.min(ow.saturating_sub(iw)),
            pad.min(oh.saturating_sub(ih)),
        )
    };
    Some(OutputGeometry {
        width: ow,
        height: oh,
        image_x: x,
        image_y: y,
        image_w: iw,
        image_h: ih,
    })
}

/// The beautified output of `img`.
pub fn render(img: &RgbaImage, s: &BeautifySettings) -> RgbaImage {
    let s = &s.clamped();
    if s.is_identity() {
        return img.clone();
    }
    let Some(g) = output_geometry(img.width(), img.height(), s) else {
        return img.clone();
    };
    let scaled;
    let src: &RgbaImage = if (g.image_w, g.image_h) != img.dimensions() && img.width() > 0 {
        scaled = imageops::resize(img, g.image_w, g.image_h, FilterType::Triangle);
        &scaled
    } else {
        img
    };
    let mut out = RgbaImage::new(g.width, g.height);
    fill_background(&mut out, s.background);
    let (x, y) = (g.image_x as i64, g.image_y as i64);
    if let Some(shadow) = s.shadow {
        for layer in (1..=shadow.layers).rev() {
            let spread = layer * 3;
            let alpha = ((u32::from(shadow.alpha) / layer).max(3)) as u8;
            draw_rounded_rect(
                &mut out,
                x + shadow.offset - spread as i64,
                y + shadow.offset - spread as i64,
                src.width().saturating_add(spread * 2),
                src.height().saturating_add(spread * 2),
                s.corner_radius.saturating_add(spread),
                [shadow.color[0], shadow.color[1], shadow.color[2], alpha],
            );
        }
    }
    if let Some(mat) = s.mat {
        draw_rounded_rect(
            &mut out,
            x - 1,
            y - 1,
            src.width().saturating_add(2),
            src.height().saturating_add(2),
            s.corner_radius.saturating_add(1),
            mat,
        );
    }
    overlay_rounded_image(&mut out, src, g.image_x, g.image_y, s.corner_radius);
    if let Some(border) = s.border {
        draw_rounded_stroke(
            &mut out,
            x - 1,
            y - 1,
            src.width().saturating_add(2),
            src.height().saturating_add(2),
            s.corner_radius.saturating_add(1),
            border,
        );
    }
    out
}

fn fill_background(out: &mut RgbaImage, bg: Background) {
    match bg {
        Background::Transparent => {}
        Background::Solid { rgba } => {
            for px in out.pixels_mut() {
                *px = Rgba(rgba);
            }
        }
        Background::Gradient { gradient } => {
            let h = out.height();
            let span = h.saturating_sub(1).max(1) as f32;
            for y in 0..h {
                let t = y as f32 / span;
                let mut c = [0u8; 4];
                for (i, ch) in c.iter_mut().enumerate() {
                    let a = gradient.top[i] as f32;
                    let b = gradient.bottom[i] as f32;
                    *ch = (a + (b - a) * t).round() as u8;
                }
                for x in 0..out.width() {
                    out.put_pixel(x, y, Rgba(c));
                }
            }
        }
    }
}

fn draw_rounded_rect(
    img: &mut RgbaImage,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
    radius: u32,
    color: [u8; 4],
) {
    if w == 0 || h == 0 {
        return;
    }

    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = (x + w as i64).clamp(0, img.width() as i64) as u32;
    let y1 = (y + h as i64).clamp(0, img.height() as i64) as u32;
    if x0 >= x1 || y0 >= y1 {
        return;
    }

    for py in y0..y1 {
        for px in x0..x1 {
            if point_in_rounded_rect(px as i64, py as i64, x, y, w, h, radius) {
                blend_pixel(img, px, py, color);
            }
        }
    }
}

fn draw_rounded_stroke(
    img: &mut RgbaImage,
    x: i64,
    y: i64,
    w: u32,
    h: u32,
    radius: u32,
    color: [u8; 4],
) {
    if w <= 2 || h <= 2 {
        draw_rounded_rect(img, x, y, w, h, radius, color);
        return;
    }

    for py in y..y + h as i64 {
        for px in x..x + w as i64 {
            if px < 0 || py < 0 || px >= img.width() as i64 || py >= img.height() as i64 {
                continue;
            }
            let outer = point_in_rounded_rect(px, py, x, y, w, h, radius);
            let inner = point_in_rounded_rect(
                px,
                py,
                x + 1,
                y + 1,
                w.saturating_sub(2),
                h.saturating_sub(2),
                radius.saturating_sub(1),
            );
            if outer && !inner {
                blend_pixel(img, px as u32, py as u32, color);
            }
        }
    }
}

fn overlay_rounded_image(out: &mut RgbaImage, src: &RgbaImage, x: u32, y: u32, radius: u32) {
    for sy in 0..src.height() {
        for sx in 0..src.width() {
            let dx = x.saturating_add(sx);
            let dy = y.saturating_add(sy);
            if dx >= out.width() || dy >= out.height() {
                continue;
            }
            if point_in_rounded_rect(
                dx as i64,
                dy as i64,
                x as i64,
                y as i64,
                src.width(),
                src.height(),
                radius,
            ) {
                out.put_pixel(dx, dy, *src.get_pixel(sx, sy));
            }
        }
    }
}

fn point_in_rounded_rect(px: i64, py: i64, x: i64, y: i64, w: u32, h: u32, radius: u32) -> bool {
    if w == 0 || h == 0 {
        return false;
    }

    let right = x + w as i64 - 1;
    let bottom = y + h as i64 - 1;
    if px < x || py < y || px > right || py > bottom {
        return false;
    }

    let r = radius.min(w / 2).min(h / 2) as i64;
    if r <= 0 {
        return true;
    }

    let cx = if px < x + r {
        x + r
    } else if px > right - r {
        right - r
    } else {
        px
    };
    let cy = if py < y + r {
        y + r
    } else if py > bottom - r {
        bottom - r
    } else {
        py
    };

    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}

fn blend_pixel(img: &mut RgbaImage, x: u32, y: u32, src: [u8; 4]) {
    let dst = img.get_pixel_mut(x, y);
    let src_alpha = src[3] as f32 / 255.0;
    let dst_alpha = dst[3] as f32 / 255.0;
    let out_alpha = src_alpha + dst_alpha * (1.0 - src_alpha);
    if out_alpha <= f32::EPSILON {
        dst.0 = [0, 0, 0, 0];
        return;
    }
    let dst_weight = dst_alpha * (1.0 - src_alpha);
    dst.0 = [
        ((src[0] as f32 * src_alpha + dst[0] as f32 * dst_weight) / out_alpha).round() as u8,
        ((src[1] as f32 * src_alpha + dst[1] as f32 * dst_weight) / out_alpha).round() as u8,
        ((src[2] as f32 * src_alpha + dst[2] as f32 * dst_weight) / out_alpha).round() as u8,
        (out_alpha * 255.0).round() as u8,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([10, 20, 30, 255]))
    }

    #[test]
    fn ids_round_trip_and_none_is_default_identity() {
        for p in BuiltinPreset::ALL {
            assert_eq!(BuiltinPreset::from_id(p.id()), Some(p));
        }
        assert!(BeautifySettings::default().is_identity());
        assert_eq!(
            BeautifySettings::default().matching_builtin(),
            Some(BuiltinPreset::None)
        );
        assert!(!BuiltinPreset::Soft.settings().is_identity());
    }

    #[test]
    fn auto_canvas_pads_and_aspect_canvases_grow_the_short_side() {
        let mut s = BuiltinPreset::Minimal.settings(); // pad 24
        let g = output_geometry(100, 50, &s).unwrap();
        assert_eq!((g.width, g.height, g.image_x, g.image_y), (148, 98, 24, 24));
        s.canvas = CanvasSize::Wide;
        let g = output_geometry(100, 50, &s).unwrap();
        assert_eq!((g.width, g.height), (175, 98)); // 98 * 16 / 9 = 174.2 → 175
        assert_eq!(g.image_x, (175 - 100) / 2);
        s.canvas = CanvasSize::Square;
        let g = output_geometry(100, 50, &s).unwrap();
        assert_eq!((g.width, g.height), (148, 148));
    }

    #[test]
    fn balance_off_keeps_the_image_at_the_padding() {
        let mut s = BuiltinPreset::Minimal.settings();
        s.canvas = CanvasSize::Square;
        s.balance = false;
        let g = output_geometry(100, 50, &s).unwrap();
        assert_eq!((g.image_x, g.image_y), (24, 24));
    }

    #[test]
    fn social_is_1200_by_675_and_only_downscales() {
        let mut s = BuiltinPreset::Minimal.settings();
        s.canvas = CanvasSize::Social;
        let small = output_geometry(100, 50, &s).unwrap();
        assert_eq!(
            (small.width, small.height, small.image_w, small.image_h),
            (1200, 675, 100, 50)
        );
        let big = output_geometry(4000, 1000, &s).unwrap();
        assert_eq!((big.width, big.height), (1200, 675));
        assert_eq!(big.image_w, 1200 - 48);
        assert_eq!(big.image_h, 288); // 1000 * 1152 / 4000
        let out = render(&img(4000, 1000), &s);
        assert_eq!(out.dimensions(), (1200, 675));
    }

    #[test]
    fn gradient_runs_top_to_bottom() {
        let mut s = BuiltinPreset::None.settings();
        s.apply(BeautifyEdit::Background(Background::Gradient {
            gradient: GRADIENTS[0],
        }));
        assert_eq!(s.padding, PADDING_WHEN_BACKGROUND_PICKED);
        let out = render(&img(4, 4), &s);
        assert_eq!(out.get_pixel(0, 0).0, GRADIENTS[0].top);
        assert_eq!(out.get_pixel(0, out.height() - 1).0, GRADIENTS[0].bottom);
    }

    #[test]
    fn edits_clamp_and_shadow_levels_round_trip() {
        let mut s = BeautifySettings::default();
        s.apply(BeautifyEdit::Padding(999));
        s.apply(BeautifyEdit::CornerRadius(999));
        assert_eq!(
            (s.padding, s.corner_radius),
            (MAX_PADDING, MAX_CORNER_RADIUS)
        );
        for l in 0..=MAX_SHADOW_LEVEL {
            s.apply(BeautifyEdit::ShadowLevel(l));
            assert_eq!(s.shadow_level(), l);
        }
        assert_eq!(BuiltinPreset::Soft.settings().shadow_level(), 4);
    }

    #[test]
    fn settings_round_trip_through_toml_and_json() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct W {
            p: Vec<SavedBeautifyPreset>,
        }
        for b in BuiltinPreset::ALL {
            let p = SavedBeautifyPreset {
                name: "Mine".into(),
                settings: b.settings(),
            };
            let json = serde_json::to_string(&p).unwrap();
            assert_eq!(
                serde_json::from_str::<SavedBeautifyPreset>(&json).unwrap(),
                p
            );
            let w = W { p: vec![p] };
            assert_eq!(
                toml::from_str::<W>(&toml::to_string(&w).unwrap()).unwrap(),
                w
            );
        }
    }

    #[test]
    fn overflowing_output_geometry_is_none() {
        assert_eq!(
            output_geometry(u32::MAX - 1, 20, &BuiltinPreset::Minimal.settings()),
            None
        );
    }

    #[test]
    fn builtin_labels_are_pinned_and_fit_a_chip() {
        let labels = BuiltinPreset::ALL.map(BuiltinPreset::label);
        assert_eq!(
            labels,
            [
                "No Frame",
                "Soft Shadow",
                "Light Card",
                "Dark Card",
                "Minimal Border",
                "Transparent"
            ]
        );
        assert!(labels.iter().all(|l| l.chars().count() <= 14));
    }

    #[test]
    fn a_saved_preset_with_missing_fields_still_loads() {
        let p: SavedBeautifyPreset = serde_json::from_str(r#"{"name":"X"}"#).unwrap();
        assert_eq!(p.name, "X");
        assert_eq!(p.settings, BeautifySettings::default());
        let p: SavedBeautifyPreset = serde_json::from_str("{}").unwrap();
        assert_eq!(p.settings, BeautifySettings::default());
    }

    #[test]
    fn out_of_range_deserialised_values_render_without_panicking() {
        let mut s = BuiltinPreset::Soft.settings();
        s.padding = 100_000;
        s.corner_radius = u32::MAX;
        s.shadow = Some(Shadow {
            color: [0, 0, 0],
            offset: i64::MAX,
            layers: u32::MAX,
            alpha: 255,
        });
        let json = serde_json::to_string(&s).unwrap();
        let s: BeautifySettings = serde_json::from_str(&json).unwrap();
        let out = render(&img(8, 8), &s);
        assert_eq!(out.width(), 8 + 2 * MAX_PADDING);
        for layers in [255, 256, 257, 512] {
            let mut s = BuiltinPreset::Soft.settings();
            s.shadow = Some(Shadow {
                color: [0, 0, 0],
                offset: 18,
                layers,
                alpha: 42,
            });
            let _ = render(&img(8, 8), &s);
        }
        assert!(output_geometry(8, 8, &s).is_some());
    }
}
