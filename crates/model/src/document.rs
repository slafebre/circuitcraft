//! An open board with undo and redo.

use crate::{Board, ModelError};

/// How many steps of undo are kept. Snapshots share unchanged items, so each costs memory in
/// proportion to the number of items on the board (pointers), not their content.
pub const UNDO_LIMIT: usize = 500;

/// A board being edited, with its undo and redo history.
#[derive(Debug, Clone)]
pub struct Document {
    board: Board,
    undo: Vec<(String, Board)>,
    redo: Vec<(String, Board)>,
}

impl Document {
    /// A document for `board`, with no history.
    pub fn new(board: Board) -> Document {
        Document { board, undo: Vec::new(), redo: Vec::new() }
    }

    /// The board.
    pub fn board(&self) -> &Board {
        &self.board
    }

    /// Makes one undoable change. If `f` fails, the board is left exactly as it was and no history
    /// is recorded; if it succeeds, the change can be undone as one step called `label`.
    pub fn edit<T>(&mut self, label: &str, f: impl FnOnce(&mut Board) -> Result<T, ModelError>) -> Result<T, ModelError> {
        let before = self.board.clone();
        match f(&mut self.board) {
            Ok(v) => {
                self.push_undo(label, before);
                self.redo.clear();
                Ok(v)
            }
            Err(e) => {
                self.board = before;
                Err(e)
            }
        }
    }

    /// Replaces the board (a new or opened board); undoable like any edit.
    pub fn replace(&mut self, label: &str, board: Board) {
        let before = std::mem::replace(&mut self.board, board);
        self.push_undo(label, before);
        self.redo.clear();
    }

    fn push_undo(&mut self, label: &str, board: Board) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push((label.to_owned(), board));
    }

    /// Undoes the last change; returns its label, or `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Option<String> {
        let (label, board) = self.undo.pop()?;
        let current = std::mem::replace(&mut self.board, board);
        self.redo.push((label.clone(), current));
        Some(label)
    }

    /// Redoes the last undone change; returns its label, or `None` when there is nothing to redo.
    pub fn redo(&mut self) -> Option<String> {
        let (label, board) = self.redo.pop()?;
        let current = std::mem::replace(&mut self.board, board);
        self.push_undo(&label, current);
        Some(label)
    }

    /// The label of the change [`Document::undo`] would undo.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|(l, _)| l.as_str())
    }

    /// The label of the change [`Document::redo`] would redo.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|(l, _)| l.as_str())
    }

    /// A cheap copy of the current state, for restoring after a failure outside [`Document::edit`]
    /// (the engine's panic guard).
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint { board: self.board.clone(), undo: self.undo.len(), redo: self.redo.len() }
    }

    /// Puts the board back to a checkpoint and drops history recorded since.
    pub fn restore(&mut self, c: Checkpoint) {
        self.board = c.board;
        self.undo.truncate(c.undo);
        self.redo.truncate(c.redo);
    }
}

/// A saved state from [`Document::checkpoint`].
#[derive(Debug, Clone)]
pub struct Checkpoint {
    board: Board,
    undo: usize,
    redo: usize,
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
    fn checkpoint_restores_board_and_history() {
        let mut d = Document::new(Board::new("x"));
        track(&mut d, 1).expect("1");
        let c = d.checkpoint();
        track(&mut d, 2).expect("2");
        d.restore(c);
        assert_eq!(d.board().tracks.len(), 1);
        assert_eq!(d.undo().as_deref(), Some("Add track"));
        assert!(d.undo().is_none());
    }
}
