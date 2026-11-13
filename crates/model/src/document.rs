//! An open board with undo and redo.

use crate::{Board, ModelError};
use std::sync::Arc;

/// How many steps of undo are kept. Snapshots share unchanged items, so each costs memory in
/// proportion to the number of items on the board (pointers), not their content.
pub const UNDO_LIMIT: usize = 500;

/// One step of history: its label, the board on the other side of it and that board's
/// generation. Shared, so cloning a document (the engine's panic guard) copies pointers.
type Step = Arc<(String, Board, u64)>;

/// A board being edited, with its undo and redo history. Cloning it is cheap: the board shares
/// its items and the history shares its steps.
///
/// Each board that [`Document::replace`] brings in (a new or opened board) starts a new
/// *generation*, and undo and redo carry the generation with the board. Callers key per-document
/// facts on it, such as where the board is saved, so that undoing back into an earlier board
/// brings back that board's facts too.
#[derive(Debug, Clone)]
pub struct Document {
    board: Board,
    generation: u64,
    next_generation: u64,
    undo: Vec<Step>,
    redo: Vec<Step>,
}

impl Document {
    /// A document for `board`, with no history, in generation 0.
    pub fn new(board: Board) -> Document {
        Document { board, generation: 0, next_generation: 1, undo: Vec::new(), redo: Vec::new() }
    }

    /// The board.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// The current board's generation: unchanged by edits, new after each [`Document::replace`].
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Every generation still reachable: the current one and those in the undo and redo history.
    pub fn generations(&self) -> impl Iterator<Item = u64> + '_ {
        std::iter::once(self.generation).chain(self.undo.iter().chain(self.redo.iter()).map(|s| s.2))
    }

    /// Makes one undoable change. If `f` fails, the board is left exactly as it was and no history
    /// is recorded; if it succeeds, the change can be undone as one step called `label`.
    pub fn edit<T>(&mut self, label: &str, f: impl FnOnce(&mut Board) -> Result<T, ModelError>) -> Result<T, ModelError> {
        let before = self.board.clone();
        match f(&mut self.board) {
            Ok(v) => {
                self.push_undo(label, before, self.generation);
                self.redo.clear();
                Ok(v)
            }
            Err(e) => {
                self.board = before;
                Err(e)
            }
        }
    }

    /// Replaces the board (a new or opened board) and starts a new generation; undoable like any
    /// edit, so the replaced board is never lost.
    pub fn replace(&mut self, label: &str, board: Board) {
        let before = std::mem::replace(&mut self.board, board);
        self.push_undo(label, before, self.generation);
        self.redo.clear();
        self.generation = self.next_generation;
        // 2^64 replacements won't happen; saturating keeps this panic-free regardless.
        self.next_generation = self.next_generation.saturating_add(1);
    }

    fn push_undo(&mut self, label: &str, board: Board, generation: u64) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(Arc::new((label.to_owned(), board, generation)));
    }

    /// Undoes the last change; returns its label, or `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Option<String> {
        let (label, board, generation) = Arc::unwrap_or_clone(self.undo.pop()?);
        let current = std::mem::replace(&mut self.board, board);
        let current_generation = std::mem::replace(&mut self.generation, generation);
        self.redo.push(Arc::new((label.clone(), current, current_generation)));
        Some(label)
    }

    /// Redoes the last undone change; returns its label, or `None` when there is nothing to redo.
    pub fn redo(&mut self) -> Option<String> {
        let (label, board, generation) = Arc::unwrap_or_clone(self.redo.pop()?);
        let current = std::mem::replace(&mut self.board, board);
        let current_generation = std::mem::replace(&mut self.generation, generation);
        self.push_undo(&label, current, current_generation);
        Some(label)
    }

    /// The label of the change [`Document::undo`] would undo.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.0.as_str())
    }

    /// The label of the change [`Document::redo`] would redo.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.0.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer_ids::TOP_COPPER;
    use crate::{ModelError, TrackPath};
    use circuitcraft_units::{Nm, Point};

    fn track(d: &mut Document, x: i64) -> Result<crate::ItemId, ModelError> {
        d.edit("Add track", |b| {
            let path = TrackPath::Line { from: Point::new(Nm(0), Nm(0)), to: Point::new(Nm(x), Nm(0)) };
            b.add_track(TOP_COPPER, Nm(100), path, None)
        })
    }

    #[test]
    fn undo_and_redo() {
        let mut d = Document::new(Board::new("x"));
        track(&mut d, 1).expect("1");
        track(&mut d, 2).expect("2");
        assert_eq!(d.board().tracks.len(), 2);
        assert_eq!(d.undo_label(), Some("Add track"));
        assert_eq!(d.undo().as_deref(), Some("Add track"));
        assert_eq!(d.board().tracks.len(), 1);
        assert_eq!(d.redo().as_deref(), Some("Add track"));
        assert_eq!(d.board().tracks.len(), 2);
        assert!(d.redo().is_none());
        d.undo();
        d.undo();
        assert_eq!(d.board().tracks.len(), 0);
        assert!(d.undo().is_none());
    }

    #[test]
    fn a_new_edit_clears_redo() {
        let mut d = Document::new(Board::new("x"));
        track(&mut d, 1).expect("1");
        d.undo();
        track(&mut d, 2).expect("2");
        assert!(d.redo().is_none());
    }

    #[test]
    fn failed_edit_leaves_no_trace() {
        let mut d = Document::new(Board::new("x"));
        let r = d.edit("Half done", |b| {
            b.add_net("GND")?;
            b.add_net("GND") // refused: the first net must not survive either
        });
        assert!(r.is_err());
        assert!(d.board().nets.is_empty());
        assert!(d.undo_label().is_none());
    }

    #[test]
    fn history_is_bounded() {
        let mut d = Document::new(Board::new("x"));
        for i in 0..(UNDO_LIMIT as i64 + 10) {
            track(&mut d, i + 1).expect("track");
        }
        let mut steps = 0;
        while d.undo().is_some() {
            steps += 1;
        }
        assert_eq!(steps, UNDO_LIMIT);
        assert_eq!(d.board().tracks.len(), 10);
    }

    #[test]
    fn generations_follow_replaced_boards_through_undo_and_redo() {
        let mut d = Document::new(Board::new("A"));
        track(&mut d, 1).expect("edit keeps the generation");
        assert_eq!(d.generation(), 0);
        d.replace("New Board", Board::new("B"));
        let b = d.generation();
        assert_ne!(b, 0);
        track(&mut d, 2).expect("edit");
        assert_eq!(d.generation(), b);
        d.undo();
        assert_eq!(d.generation(), b, "undoing an edit stays in B");
        d.undo();
        assert_eq!((d.board().name.as_str(), d.generation()), ("A", 0), "undoing the replace goes back to A");
        assert_eq!(d.board().tracks.len(), 1, "A's work is still there");
        d.redo();
        assert_eq!((d.board().name.as_str(), d.generation()), ("B", b));
        let mut all: Vec<_> = d.generations().collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all, [0, b]);
    }
}
