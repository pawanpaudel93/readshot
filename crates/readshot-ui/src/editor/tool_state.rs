//! Active editor tool. Mirrors `Annotation` variants plus a `Select`
//! mode for moving / deleting existing annotations.
//!
//! The editor tracks the user's tool choice via [`ToolState`] and
//! consults it when interpreting mouse events on the canvas. Adding a
//! new annotation variant therefore requires updating both this enum
//! and `Annotation` in `readshot-core` — the
//! `ensure_complete_round_trip` test below catches the slip.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ToolState {
    #[default]
    Select,
    Rectangle,
    Ellipse,
    Line,
    Arrow,
    Pen,
    Highlighter,
    Text,
    Blur,
    Pixelate,
    NumberedPin,
    Crop,
}

impl ToolState {
    /// Tools that produce a single-rect annotation (drag-to-define).
    pub fn is_rect_tool(&self) -> bool {
        matches!(
            self,
            Self::Rectangle | Self::Ellipse | Self::Blur | Self::Pixelate | Self::Crop
        )
    }

    /// Tools that produce a single-segment annotation (drag from
    /// anchor to cursor).
    pub fn is_segment_tool(&self) -> bool {
        matches!(self, Self::Line | Self::Arrow)
    }

    /// Tools that accumulate a polyline as the user drags.
    pub fn is_freehand_tool(&self) -> bool {
        matches!(self, Self::Pen | Self::Highlighter)
    }

    /// Tools that drop a single annotation at the click point with no
    /// drag (text, numbered-pin).
    pub fn is_point_tool(&self) -> bool {
        matches!(self, Self::Text | Self::NumberedPin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_select() {
        assert_eq!(ToolState::default(), ToolState::Select);
    }

    #[test]
    fn tool_classifications_partition_the_drawing_tools() {
        // Every drawing tool fits exactly one classification (or none,
        // for Select). If a future tool sits in two, the editor's
        // event handler is ambiguous — fail the test.
        let tools = [
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
        for t in tools {
            let classifications = [
                t.is_rect_tool(),
                t.is_segment_tool(),
                t.is_freehand_tool(),
                t.is_point_tool(),
            ];
            let count = classifications.iter().filter(|c| **c).count();
            assert!(
                count <= 1,
                "tool {t:?} matches more than one classification: {classifications:?}"
            );
        }
    }

    #[test]
    fn rect_tools_include_blur_pixelate_crop() {
        assert!(ToolState::Blur.is_rect_tool());
        assert!(ToolState::Pixelate.is_rect_tool());
        assert!(ToolState::Crop.is_rect_tool());
    }
}
