//! Undo/redo for the editor's annotation list.
//!
//! Implementation: two stacks (`past`, `future`) plus the current
//! state, capped at [`MAX_HISTORY`] entries on the past stack. Pushing
//! a new state clears the redo stack — the standard "make a change,
//! lose your future" semantics every editor uses.
//!
//! The history holds owned `Vec<Annotation>` snapshots rather than
//! diffs because annotation-tree mutations are coarse-grained (one
//! per drag) and the snapshots are small (annotations are values, no
//! nested trees beyond the pen-points vec).

use readshot_core::Annotation;

/// Maximum number of past states held on the undo stack. Once
/// exceeded the oldest entry is dropped — a hard memory bound for
/// editors that get heavy use without explicit save.
pub const MAX_HISTORY: usize = 64;

#[derive(Clone, Debug, Default)]
pub struct History {
    past: Vec<Vec<Annotation>>,
    present: Vec<Annotation>,
    future: Vec<Vec<Annotation>>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_present(present: Vec<Annotation>) -> Self {
        Self {
            past: Vec::new(),
            present,
            future: Vec::new(),
        }
    }

    /// Snapshot accessor — the slice the renderer should render.
    pub fn current(&self) -> &[Annotation] {
        &self.present
    }

    /// Replace the current state without touching undo / redo. Used
    /// for live edit previews that are later committed as one undoable
    /// action when the drag ends.
    pub(crate) fn replace_present(&mut self, present: Vec<Annotation>) {
        self.present = present;
    }

    /// Push a new state, truncating the redo stack and capping the
    /// undo stack at `MAX_HISTORY`. The previous present moves onto
    /// `past`.
    pub fn push(&mut self, new_state: Vec<Annotation>) {
        let prev = std::mem::replace(&mut self.present, new_state);
        self.past.push(prev);
        if self.past.len() > MAX_HISTORY {
            // Drop the oldest entry. `Vec::remove(0)` is O(n) but the
            // alternative (a `VecDeque`) gives no allocator advantage
            // at this size and complicates the snapshot iteration.
            let _ = self.past.remove(0);
        }
        self.future.clear();
    }

    /// Move the current state onto the redo stack and pop the most
    /// recent past state into the present. Returns the new present
    /// slice on success or `None` if the undo stack is empty.
    pub fn undo(&mut self) -> Option<&[Annotation]> {
        let prev = self.past.pop()?;
        let current_to_redo = std::mem::replace(&mut self.present, prev);
        self.future.push(current_to_redo);
        Some(&self.present)
    }

    /// Inverse of [`undo`]. Pops from `future`, pushes the current
    /// state to `past`.
    pub fn redo(&mut self) -> Option<&[Annotation]> {
        let next = self.future.pop()?;
        let current_to_past = std::mem::replace(&mut self.present, next);
        self.past.push(current_to_past);
        Some(&self.present)
    }

    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// Number of states currently available on the undo stack — used
    /// by the editor toolbar to surface "X actions to undo" in the
    /// button tooltip.
    pub fn past_len(&self) -> usize {
        self.past.len()
    }

    /// Number of states currently available on the redo stack.
    pub fn future_len(&self) -> usize {
        self.future.len()
    }

    /// Drop everything: clears all stacks and resets to an empty
    /// present. Used when the editor opens a new capture.
    pub fn clear(&mut self) {
        self.past.clear();
        self.present.clear();
        self.future.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use readshot_core::{Annotation, RectLike, Rgba};

    fn rect_annotation(x: f32) -> Annotation {
        Annotation::Rectangle {
            rect: RectLike::new(x, 0.0, 10.0, 10.0),
            color: Rgba::OPAQUE_BLACK,
            line_width: 1.0,
        }
    }

    #[test]
    fn new_history_is_empty() {
        let h = History::new();
        assert!(h.current().is_empty());
        assert!(!h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn push_advances_the_present() {
        let mut h = History::new();
        h.push(vec![rect_annotation(0.0)]);
        assert_eq!(h.current().len(), 1);
        assert!(h.can_undo());
        assert!(!h.can_redo());
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut h = History::new();
        h.push(vec![rect_annotation(0.0)]);
        h.push(vec![rect_annotation(0.0), rect_annotation(20.0)]);
        assert_eq!(h.current().len(), 2);

        let after_undo = h.undo().expect("undo succeeds").to_vec();
        assert_eq!(after_undo.len(), 1);
        assert!(h.can_redo());

        let after_redo = h.redo().expect("redo succeeds").to_vec();
        assert_eq!(after_redo.len(), 2);
        assert!(!h.can_redo());
    }

    #[test]
    fn pushing_clears_future() {
        let mut h = History::new();
        h.push(vec![rect_annotation(0.0)]);
        h.push(vec![rect_annotation(0.0), rect_annotation(20.0)]);
        h.undo();
        assert!(h.can_redo());

        // Make a new edit — redo stack must clear.
        h.push(vec![rect_annotation(99.0)]);
        assert!(!h.can_redo());
    }

    #[test]
    fn undo_on_empty_history_returns_none() {
        let mut h = History::new();
        assert!(h.undo().is_none());
        assert!(h.redo().is_none());
    }

    #[test]
    fn history_cap_drops_oldest() {
        let mut h = History::new();
        for i in 0..(MAX_HISTORY + 5) {
            h.push(vec![rect_annotation(i as f32)]);
        }
        // Past stack should be capped at MAX_HISTORY entries.
        assert_eq!(h.past.len(), MAX_HISTORY);
    }

    #[test]
    fn clear_resets_everything() {
        let mut h = History::new();
        h.push(vec![rect_annotation(0.0)]);
        h.push(vec![rect_annotation(20.0)]);
        h.undo();
        h.clear();
        assert!(h.current().is_empty());
        assert!(!h.can_undo());
        assert!(!h.can_redo());
    }
}
