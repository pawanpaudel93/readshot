//! Undo/redo for the editor's document.
//!
//! Implementation: two stacks (`past`, `future`) plus the current
//! state, capped at [`MAX_HISTORY`] entries on the past stack. Pushing
//! a new state clears the redo stack — the standard "make a change,
//! lose your future" semantics every editor uses.
//!
//! States are [`Snapshot`]s: the annotations plus the document (geometry
//! and Beautify).
//!
//! The history holds owned snapshots rather than diffs because
//! mutations are coarse-grained (one per drag) and the snapshots are
//! small (annotations are values; images inside the geometry are `Arc`s).

use std::sync::Arc;

use readshot_core::beautify::BeautifySettings;
use readshot_core::document::DocGeometry;
use readshot_core::Annotation;

/// Maximum number of past states held on the undo stack. Once
/// exceeded the oldest entry is dropped — a hard memory bound for
/// editors that get heavy use without explicit save.
pub const MAX_HISTORY: usize = 64;

/// The document an annotation list is drawn on. Images inside the
/// geometry are `Arc`s, so a snapshot is cheap to clone.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocState {
    pub geometry: Arc<DocGeometry>,
    pub beautify: BeautifySettings,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub annotations: Vec<Annotation>,
    pub doc: DocState,
}

#[derive(Clone, Debug, Default)]
pub struct History {
    past: Vec<Snapshot>,
    present: Snapshot,
    future: Vec<Snapshot>,
    /// Bumped by every push, undo and redo (never by a preview), so a
    /// caller can tell whether the history moved since it looked.
    generation: u64,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_present(present: Vec<Annotation>) -> Self {
        Self::from_snapshot(Snapshot {
            annotations: present,
            doc: DocState::default(),
        })
    }

    pub fn from_snapshot(present: Snapshot) -> Self {
        Self {
            past: Vec::new(),
            present,
            future: Vec::new(),
            generation: 0,
        }
    }

    /// Changes on every push, undo and redo; never on a preview.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Snapshot accessor — the slice the renderer should render.
    pub fn current(&self) -> &[Annotation] {
        &self.present.annotations
    }

    pub fn doc(&self) -> &DocState {
        &self.present.doc
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.present
    }

    /// Replace the current state without touching undo / redo. Used
    /// for live edit previews that are later committed as one undoable
    /// action when the drag ends.
    pub(crate) fn replace_present(&mut self, present: Vec<Annotation>) {
        self.present.annotations = present;
    }

    pub(crate) fn replace_present_snapshot(&mut self, present: Snapshot) {
        self.present = present;
    }

    /// New annotations on the same document.
    pub fn push(&mut self, new_state: Vec<Annotation>) {
        let doc = self.present.doc.clone();
        self.push_snapshot(Snapshot {
            annotations: new_state,
            doc,
        });
    }

    pub fn push_snapshot(&mut self, next: Snapshot) {
        self.generation += 1;
        let prev = std::mem::replace(&mut self.present, next);
        self.past.push(prev);
        if self.past.len() > MAX_HISTORY {
            let _ = self.past.remove(0);
        }
        self.future.clear();
    }

    /// Move the current state onto the redo stack and pop the most
    /// recent past state into the present. Returns the new present
    /// slice on success or `None` if the undo stack is empty.
    pub fn undo(&mut self) -> Option<&[Annotation]> {
        let prev = self.past.pop()?;
        let current = std::mem::replace(&mut self.present, prev);
        self.future.push(current);
        self.generation += 1;
        Some(&self.present.annotations)
    }

    /// Inverse of [`History::undo`]. Pops from `future`, pushes the current
    /// state to `past`.
    pub fn redo(&mut self) -> Option<&[Annotation]> {
        let next = self.future.pop()?;
        let current = std::mem::replace(&mut self.present, next);
        self.past.push(current);
        self.generation += 1;
        Some(&self.present.annotations)
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

    /// Drop every annotation and both stacks; the document stays.
    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.present.annotations.clear();
        self.generation += 1;
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
            style: Default::default(),
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
    #[test]
    fn clear_bumps_the_generation() {
        let mut h = History::new();
        let g = h.generation();
        h.clear();
        assert_ne!(h.generation(), g);
    }

    #[test]
    fn snapshots_keep_the_document_across_plain_pushes_and_clear() {
        use readshot_core::beautify::BuiltinPreset;
        let mut h = History::new();
        let doc = DocState {
            beautify: BuiltinPreset::Soft.settings(),
            ..DocState::default()
        };
        h.push_snapshot(Snapshot {
            annotations: vec![rect_annotation(0.0)],
            doc: doc.clone(),
        });
        h.push(vec![rect_annotation(1.0)]);
        assert_eq!(h.doc(), &doc, "an annotation push keeps the document");
        h.undo();
        h.undo();
        assert_eq!(h.doc(), &DocState::default());
        h.redo();
        assert_eq!(h.doc(), &doc);
        h.clear();
        assert!(h.current().is_empty());
        assert_eq!(h.doc(), &doc, "clear drops annotations, not the document");
    }
}
