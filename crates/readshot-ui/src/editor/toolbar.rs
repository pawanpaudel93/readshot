//! Floating toolbar for the editor.
//!
//! The toolbar offers buttons for every tool plus a 12-swatch colour
//! palette and a thickness slider. It publishes [`ToolbarMessage`]s
//! the composition root maps to [`super::EditorState`] mutations.
//!
//! Task 14 ships the model + tested helpers; the iced view function
//! that materialises the buttons lives here as well, but we leave the
//! actual layout call (placement on screen, styling) to Task 16's
//! editor window because the toolbar is a *secondary* iced window
//! anchored to the selection.

use readshot_core::Rgba;

use super::tool_state::ToolState;

/// A 12-colour palette chosen to cover the most-used annotation
/// colours on a typical screenshot. Order is preserved between
/// renders so swatch positions are stable.
pub const PALETTE: [Rgba; 12] = [
    // Reds → useful for highlighting errors / call-outs.
    Rgba::new(0.85, 0.10, 0.10, 1.0),
    Rgba::new(1.00, 0.40, 0.20, 1.0),
    // Yellows / oranges → highlighter, pin colours.
    Rgba::new(1.00, 0.80, 0.10, 1.0),
    Rgba::new(1.00, 0.95, 0.20, 1.0),
    // Greens → check marks, success indicators.
    Rgba::new(0.20, 0.75, 0.30, 1.0),
    Rgba::new(0.10, 0.50, 0.20, 1.0),
    // Blues → links, references.
    Rgba::new(0.10, 0.55, 1.00, 1.0),
    Rgba::new(0.10, 0.30, 0.80, 1.0),
    // Purples / pinks → notes.
    Rgba::new(0.65, 0.30, 0.85, 1.0),
    Rgba::new(0.95, 0.40, 0.75, 1.0),
    // Greys → subtle markings.
    Rgba::new(0.40, 0.40, 0.40, 1.0),
    // Black anchor.
    Rgba::OPAQUE_BLACK,
];

/// Default thickness slider range (logical pixels).
pub const MIN_LINE_WIDTH: f32 = 0.5;
pub const MAX_LINE_WIDTH: f32 = 32.0;

/// Messages the toolbar publishes. The composition root forwards each
/// to the matching `EditorState::set_*` method.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToolbarMessage {
    SelectTool(ToolState),
    SelectColor(Rgba),
    SetLineWidth(f32),
    Undo,
    Redo,
}

/// Tool bar groups (spec §2.1): Select | shapes | freehand + text |
/// effects | marks | crop. Phase C adds `Magnifier` after `Stamp`: extend the group, `TOOL_ORDER`, and the
/// `slot` match in the test — nothing else needs to know the layout.
pub const TOOL_GROUPS: [&[ToolState]; 6] = [
    &[ToolState::Select],
    &[
        ToolState::Rectangle,
        ToolState::Ellipse,
        ToolState::Line,
        ToolState::Arrow,
    ],
    &[ToolState::Pen, ToolState::Highlighter, ToolState::Text],
    &[ToolState::Blur, ToolState::Pixelate, ToolState::Spotlight],
    &[ToolState::NumberedPin, ToolState::Stamp],
    &[ToolState::Crop],
];

/// Tools in display order (`TOOL_GROUPS` flattened).
pub const TOOL_ORDER: [ToolState; 14] = [
    ToolState::Select,
    ToolState::Rectangle,
    ToolState::Ellipse,
    ToolState::Line,
    ToolState::Arrow,
    ToolState::Pen,
    ToolState::Highlighter,
    ToolState::Text,
    ToolState::Blur,
    ToolState::Pixelate,
    ToolState::Spotlight,
    ToolState::NumberedPin,
    ToolState::Stamp,
    ToolState::Crop,
];

/// Tools that show a "new" dot until first selected.
pub const NEW_TOOLS: &[ToolState] = &[ToolState::Spotlight, ToolState::Stamp];

/// Clamp a slider raw value to the published thickness range.
pub fn clamp_line_width(raw: f32) -> f32 {
    raw.clamp(MIN_LINE_WIDTH, MAX_LINE_WIDTH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_has_twelve_swatches_with_full_alpha() {
        assert_eq!(PALETTE.len(), 12);
        for c in PALETTE {
            assert!(
                (c.a - 1.0).abs() < f32::EPSILON,
                "palette swatches should be opaque, got alpha {}",
                c.a
            );
        }
    }

    #[test]
    fn pill_ink_has_the_higher_wcag_contrast_on_every_swatch() {
        fn lum(c: Rgba) -> f32 {
            let lin = |v: f32| {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
        }
        fn ratio(a: Rgba, b: Rgba) -> f32 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        for c in PALETTE {
            let ink = readshot_core::render::contrast_ink(c);
            let other = if ink == Rgba::OPAQUE_BLACK {
                Rgba::OPAQUE_WHITE
            } else {
                Rgba::OPAQUE_BLACK
            };
            assert!(
                ratio(c, ink) >= ratio(c, other),
                "{c:?}: chose {ink:?} ({}) over {other:?} ({})",
                ratio(c, ink),
                ratio(c, other)
            );
        }
    }

    #[test]
    fn tool_order_covers_every_tool() {
        // Exhaustive on purpose: a new ToolState variant fails to compile
        // here until it has a slot in the tool bar.
        fn slot(t: ToolState) -> usize {
            match t {
                ToolState::Select => 0,
                ToolState::Rectangle => 1,
                ToolState::Ellipse => 2,
                ToolState::Line => 3,
                ToolState::Arrow => 4,
                ToolState::Pen => 5,
                ToolState::Highlighter => 6,
                ToolState::Text => 7,
                ToolState::Blur => 8,
                ToolState::Pixelate => 9,
                ToolState::Spotlight => 10,
                ToolState::NumberedPin => 11,
                ToolState::Stamp => 12,
                ToolState::Crop => 13,
            }
        }
        assert_eq!(TOOL_ORDER.len(), 14);
        for (i, t) in TOOL_ORDER.iter().enumerate() {
            assert_eq!(slot(*t), i, "{t:?} is out of place");
        }
    }

    #[test]
    fn tool_groups_flatten_to_tool_order() {
        let flat: Vec<ToolState> = TOOL_GROUPS.iter().flat_map(|g| g.iter().copied()).collect();
        assert_eq!(flat, TOOL_ORDER.to_vec());
    }

    #[test]
    fn new_tools_are_in_the_tool_bar() {
        assert!(NEW_TOOLS.iter().all(|t| TOOL_ORDER.contains(t)));
    }

    #[test]
    fn tool_ids_are_unique() {
        let ids: std::collections::HashSet<&str> = TOOL_ORDER.iter().map(|t| t.id()).collect();
        assert_eq!(ids.len(), TOOL_ORDER.len());
    }

    #[test]
    fn line_width_clamps_to_published_range() {
        assert_eq!(clamp_line_width(0.0), MIN_LINE_WIDTH);
        assert_eq!(clamp_line_width(99.0), MAX_LINE_WIDTH);
        assert!((clamp_line_width(8.0) - 8.0).abs() < f32::EPSILON);
    }
}
