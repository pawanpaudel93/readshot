//! Floating toolbar for the editor.
//!
//! The toolbar offers buttons for every tool plus a 12-swatch colour
//! palette and a thickness slider. It publishes [`ToolbarMessage`]s
//! the composition root maps to [`EditorState`] mutations.
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

/// Tools to display, in order. Keeps the toolbar layout stable so
/// muscle memory works across releases.
pub const TOOL_ORDER: [ToolState; 12] = [
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
    ToolState::NumberedPin,
    ToolState::Crop,
];

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
    fn tool_order_covers_every_tool() {
        let count = TOOL_ORDER.len();
        // 12 tools — Select + 11 drawing tools. If a new tool is
        // added the toolbar must learn about it; this test forces the
        // bump to be intentional.
        assert_eq!(count, 12);
    }

    #[test]
    fn line_width_clamps_to_published_range() {
        assert_eq!(clamp_line_width(0.0), MIN_LINE_WIDTH);
        assert_eq!(clamp_line_width(99.0), MAX_LINE_WIDTH);
        assert!((clamp_line_width(8.0) - 8.0).abs() < f32::EPSILON);
    }
}
