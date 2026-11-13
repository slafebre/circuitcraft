//! An open board with undo and redo, recorded as change sets.

use crate::change::{BoardEdit, SettingsBefore};
use crate::{Board, ChangeSet, ModelError};
use std::sync::Arc;

/// How many steps of undo are kept. Each step holds what its edit changed (pointers to the items
/// before and after), so its cost follows the size of the edit, not the size of the board.
pub const UNDO_LIMIT: usize = 500;

/// What a step of history did.
#[derive(Debug)]
enum Kind {
    /// An edit: the items and settings it changed. The same in both directions.
    Edit(ChangeSet),
    /// The whole board was replaced (a new or opened board): the board on the other side.
    Replace(Box<Board>),
}

/// One step of history: its label, the board generation on the other side of it, and what it did.
#[derive(Debug)]
struct Step {
    label: String,
    generation: u64,
    kind: Kind,
}

/// The state to return to if the command a guard covers fails.
#[derive(Debug)]
struct Guard {
    /// The steps applied since the guard opened, and in which direction (`true` for an edit or a
    /// redo). For a replacement, the step logged holds the board from before it.
    applied: Vec<(Arc<Step>, bool)>,
    undo: Vec<Arc<Step>>,
    redo: Vec<Arc<Step>>,
    generation: u64,
}

/// A board being edited, with its undo and redo history.
///
/// Each board that [`Document::replace`] brings in (a new or opened board) starts a new
/// *generation*, and undo and redo carry the generation with the board. Callers key per-document
/// facts on it, such as where the board is saved, so that undoing back into an earlier board
/// brings back that board's facts too. Generations are never reused.
///
/// Every change to the board raises the [`Document::revision`].
#[derive(Debug)]
pub struct Document {
    board: Board,
    generation: u64,
    next_generation: u64,
    revision: u64,
    undo: Vec<Arc<Step>>,
    redo: Vec<Arc<Step>>,
    /// Whether an edit is being recorded: true only while [`Document::edit`] runs its closure, or
    /// after that closure panicked, until the recording is rolled back.
    recording: bool,
    /// The settings from before the edit being recorded, once it changes one.
    settings_before: SettingsBefore,
    /// Open guards, innermost last.
    guards: Vec<Guard>,
}

impl Document {
    /// A document for `board`, with no history, in generation 0 and revision 0.
    pub fn new(board: Board) -> Document {
        Document {
            board,
            generation: 0,
            next_generation: 1,
            revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            recording: false,
            settings_before: SettingsBefore::default(),
            guards: Vec::new(),
        }
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
    /// While a guard is open this includes what rolling it back would restore, so facts keyed on
    /// a generation that a command pushes out of the history aren't discarded before the command
    /// has succeeded.
    pub fn generations(&self) -> impl Iterator<Item = u64> + '_ {
        let reachable = |generation: u64, undo: &'_ [Arc<Step>], redo: &'_ [Arc<Step>]| {
            std::iter::once(generation).chain(undo.iter().chain(redo).map(|s| s.generation)).collect::<Vec<_>>()
        };
        reachable(self.generation, &self.undo, &self.redo)
            .into_iter()
            .chain(self.guards.iter().flat_map(move |g| reachable(g.generation, &g.undo, &g.redo)))
    }

    /// A number that goes up with every change to the board (and is never reused), so a reader
    /// can tell whether what it derived from the board is still current.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Makes one undoable change through a [`BoardEdit`]. If `f` fails, the board is left exactly
    /// as it was and no history is recorded; if it succeeds and changed something, the change can
    /// be undone as one step called `label`. Only the items `f` touches are recorded (and the
    /// board settings, if it changes them), so an edit costs what it changes.
    pub fn edit<T>(&mut self, label: &str, f: impl FnOnce(&mut BoardEdit) -> Result<T, ModelError>) -> Result<T, ModelError> {
        self.abandon_recording();
        self.board.begin_recording();
        self.recording = true;
        let result = f(&mut BoardEdit::new(&mut self.board, &mut self.settings_before));
        if result.is_err() {
            self.abandon_recording();
            return result;
        }
        self.recording = false;
        let change = self.board.finish_recording(std::mem::take(&mut self.settings_before));
        if !change.is_empty() {
            let step = Arc::new(Step { label: label.to_owned(), generation: self.generation, kind: Kind::Edit(change) });
            self.push_undo(step.clone());
            self.redo.clear();
            self.changed(step, true);
        }
        result
    }

    /// Replaces the board (a new or opened board) and starts a new generation; undoable like any
    /// edit, so the replaced board is never lost.
    pub fn replace(&mut self, label: &str, board: Board) {
        self.abandon_recording();
        let before = std::mem::replace(&mut self.board, board);
        let step = Arc::new(Step { label: label.to_owned(), generation: self.generation, kind: Kind::Replace(Box::new(before)) });
        self.push_undo(step.clone());
        self.redo.clear();
        self.generation = self.next_generation;
        // 2^64 replacements won't happen; saturating keeps this panic-free regardless.
        self.next_generation = self.next_generation.saturating_add(1);
        self.changed(step, true);
    }

    /// Undoes the last change; returns its label, or `None` when there is nothing to undo.
    pub fn undo(&mut self) -> Option<String> {
        self.abandon_recording();
        let step = self.undo.pop()?;
        let back = self.cross(step, false);
        let label = back.label.clone();
        self.redo.push(back);
        Some(label)
    }

    /// Redoes the last undone change; returns its label, or `None` when there is nothing to redo.
    pub fn redo(&mut self) -> Option<String> {
        self.abandon_recording();
        let step = self.redo.pop()?;
        let back = self.cross(step, true);
        let label = back.label.clone();
        self.push_undo(back);
        Some(label)
    }

    /// The label of the change [`Document::undo`] would undo.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    /// The label of the change [`Document::redo`] would redo.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Starts covering a command: until the matching [`Document::close_guard`], everything done to
    /// the document can be reversed by [`Document::rollback_guard`]. The cost is a copy of the
    /// history's pointers (at most a thousand), not of the board. Guards nest: a command run by
    /// another command gets its own.
    pub fn open_guard(&mut self) {
        self.guards.push(Guard { applied: Vec::new(), undo: self.undo.clone(), redo: self.redo.clone(), generation: self.generation });
    }

    /// Stops covering a command that finished; what it did stays, and an enclosing guard can
    /// still reverse it.
    pub fn close_guard(&mut self) {
        if let Some(inner) = self.guards.pop()
            && let Some(outer) = self.guards.last_mut()
        {
            outer.applied.extend(inner.applied);
        }
    }

    /// Puts the board, its history and generation back as they were at the matching
    /// [`Document::open_guard`], including an edit that was interrupted halfway (its closure
    /// panicked), and closes that guard. The revision goes up: readers see a change. Without an
    /// open guard, only such an interrupted edit is rolled back.
    pub fn rollback_guard(&mut self) {
        self.abandon_recording();
        let Some(Guard { applied, undo, redo, generation }) = self.guards.pop() else { return };
        if applied.is_empty() {
            return;
        }
        for (step, forward) in applied.into_iter().rev() {
            match &step.kind {
                Kind::Edit(change) => self.board.apply(change, !forward),
                Kind::Replace(before) => self.board = Board::clone(before),
            }
        }
        (self.undo, self.redo, self.generation) = (undo, redo, generation);
        self.revision = self.revision.saturating_add(1);
    }

    /// Rolls back an edit whose closure never returned (it panicked), if there is one.
    fn abandon_recording(&mut self) {
        if std::mem::take(&mut self.recording) {
            self.board.rollback_recording(std::mem::take(&mut self.settings_before));
        }
    }

    /// Applies a history step in one direction and returns the step that leads back: the same
    /// step for an edit, a new one holding the board it replaced for a replacement. The change is
    /// logged before an edit is applied, so a guard can complete its reversal even if applying
    /// stops halfway; a replacement is a single swap, logged after it.
    fn cross(&mut self, step: Arc<Step>, forward: bool) -> Arc<Step> {
        let generation = std::mem::replace(&mut self.generation, step.generation);
        match &step.kind {
            Kind::Edit(change) => {
                self.changed(step.clone(), forward);
                self.board.apply(change, forward);
                step
            }
            Kind::Replace(other) => {
                // Copies the other board's item maps (pointers): only undo and redo of a new or
                // opened board pay this.
                let current = std::mem::replace(&mut self.board, Board::clone(other));
                let back = Arc::new(Step { label: step.label.clone(), generation, kind: Kind::Replace(Box::new(current)) });
                self.changed(back.clone(), forward);
                back
            }
        }
    }

    fn push_undo(&mut self, step: Arc<Step>) {
        if self.undo.len() >= UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(step);
    }

    /// Records that the board changed by `step`: a new revision, and an entry in the innermost
    /// guard.
    fn changed(&mut self, step: Arc<Step>, forward: bool) {
        self.revision = self.revision.saturating_add(1);
        if let Some(guard) = self.guards.last_mut() {
            guard.applied.push((step, forward));
        }
    }

    /// How many item versions the history holds, a measure of its memory (for tests).
    #[cfg(test)]
    fn history_items(&self) -> usize {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .map(|s| match &s.kind {
                Kind::Edit(c) => c.len(),
                Kind::Replace(b) => {
                    b.nets.len() + b.footprints.len() + b.tracks.len() + b.vias.len() + b.zones.len() + b.texts.len() + b.graphics.len()
                }
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer_ids::TOP_COPPER;
    use crate::{ItemId, ModelError, TrackPath};
    use circuitcraft_units::{Nm, Point};

    fn track(d: &mut Document, x: i64) -> Result<ItemId, ModelError> {
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
            b.set_name("renamed")?;
            b.add_net("GND")?;
            b.add_net("GND") // refused: the rename and the first net must not survive either
        });
        assert!(r.is_err());
        assert!(d.board().nets.is_empty());
        assert_eq!(d.board().name, "x");
        assert!(d.undo_label().is_none());
        assert_eq!(d.revision(), 0);
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

    #[test]
    fn history_holds_what_edits_changed_not_whole_boards() {
        let mut d = Document::new(Board::new("x"));
        for i in 0..1_000 {
            track(&mut d, i + 1).expect("track");
        }
        // One track per step: with whole-board snapshots this would be about 375,000.
        assert_eq!(d.history_items(), UNDO_LIMIT);
    }

    #[test]
    fn an_edit_records_items_and_settings_before_and_after() {
        let mut d = Document::new(Board::new("x"));
        let t = track(&mut d, 1).expect("track");
        let gnd = d.edit("Add net", |b| b.add_net("GND")).expect("net");
        d.edit("Change", |b| {
            if let Some(track) = b.board.tracks.get_mut(t) {
                track.net = Some(gnd);
                track.width = Nm(250);
            }
            b.set_name("renamed")
        })
        .expect("edit");
        let change = match &d.undo.last().expect("a step").kind {
            Kind::Edit(c) => c,
            Kind::Replace(_) => panic!("an edit step"),
        };
        assert_eq!(change.len(), 2, "one track and the settings");
        assert!(change.settings_changed());
        let c = change.tracks.first().expect("the track");
        assert_eq!((c.id, c.before.as_ref().map(|t| t.width), c.after.as_ref().map(|t| t.width)), (t, Some(Nm(100)), Some(Nm(250))));
        d.undo();
        assert_eq!((d.board().name.as_str(), d.board().tracks.get(t).map(|t| (t.width, t.net))), ("x", Some((Nm(100), None))));
        d.redo();
        assert_eq!((d.board().name.as_str(), d.board().tracks.get(t).map(|t| (t.width, t.net))), ("renamed", Some((Nm(250), Some(gnd)))));
    }

    #[test]
    fn an_edit_that_changes_nothing_records_nothing() {
        let mut d = Document::new(Board::new("x"));
        let t = track(&mut d, 1).expect("track");
        let (before, item) = (d.revision(), d.board().tracks.get(t).map(|t| t as *const _));
        d.edit("Touch", |b| {
            let _ = b.board.tracks.get_mut(t); // touched, not changed
            b.set_name("x") // the same name
        })
        .expect("edit");
        assert_eq!((d.revision(), d.undo_label()), (before, Some("Add track")));
        assert_eq!(d.board().tracks.get(t).map(|t| t as *const _), item, "the original item is kept, not a copy");
    }

    #[test]
    fn every_change_raises_the_revision_and_rollback_does_not_reuse_one() {
        let mut d = Document::new(Board::new("x"));
        track(&mut d, 1).expect("track");
        assert_eq!(d.revision(), 1);
        d.undo();
        assert_eq!(d.revision(), 2);
        d.redo();
        assert_eq!(d.revision(), 3);
        d.replace("New Board", Board::new("y"));
        assert_eq!(d.revision(), 4);
        d.open_guard();
        track(&mut d, 2).expect("track");
        d.rollback_guard();
        assert_eq!(d.revision(), 6, "the rollback is a change too");
        let generation = d.generation();
        d.open_guard();
        d.replace("New Board", Board::new("z"));
        let discarded = d.generation();
        d.rollback_guard();
        d.replace("New Board", Board::new("w"));
        assert!(d.generation() != discarded && d.generation() != generation, "generations are never reused");
    }

    #[test]
    fn rollback_guard_reverses_everything_a_command_did() {
        let mut d = Document::new(Board::new("x"));
        track(&mut d, 1).expect("track");
        track(&mut d, 2).expect("track");
        d.undo();
        let board = d.board().clone();
        d.open_guard();
        track(&mut d, 3).expect("track"); // clears redo
        d.undo();
        d.undo();
        d.replace("New Board", Board::new("y"));
        track(&mut d, 4).expect("track");
        d.undo();
        d.undo(); // undo of the replacement
        d.redo();
        d.rollback_guard();
        assert_eq!(d.board(), &board);
        assert_eq!(d.generation(), 0);
        assert_eq!(d.redo().as_deref(), Some("Add track"), "the redo step the edit cleared is back");
        assert_eq!(d.board().tracks.len(), 2);
    }

    #[test]
    fn rollback_guard_undoes_an_edit_whose_closure_panicked() {
        let mut d = Document::new(Board::new("x"));
        d.open_guard();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = d.edit("Boom", |b| -> Result<(), ModelError> {
                b.set_name("renamed")?;
                b.add_net("GND")?;
                panic!("in the middle of an edit");
            });
        }));
        assert!(r.is_err());
        d.rollback_guard();
        assert!(d.board().nets.is_empty());
        assert_eq!(d.board().name, "x");
        assert!(d.undo_label().is_none());
        track(&mut d, 1).expect("the document works afterwards");
        assert_eq!(d.board().tracks.len(), 1);
    }

    #[test]
    fn generations_a_rollback_could_restore_stay_listed_while_a_guard_is_open() {
        let mut d = Document::new(Board::new("A"));
        d.replace("Open", Board::new("B"));
        let b = d.generation();
        d.undo(); // redo now leads back to B
        d.open_guard();
        d.replace("Open", Board::new("C")); // clears redo: B is out of the history for now
        assert!(d.generations().any(|g| g == b), "B could come back if the command fails");
        d.rollback_guard();
        assert_eq!(d.redo_label(), Some("Open"));
        d.open_guard();
        d.replace("Open", Board::new("C"));
        d.close_guard();
        assert!(!d.generations().any(|g| g == b), "once the command succeeded, B is gone for good");
    }

    #[test]
    fn guards_nest() {
        let mut d = Document::new(Board::new("x"));
        d.open_guard();
        track(&mut d, 1).expect("outer");
        d.open_guard();
        track(&mut d, 2).expect("inner, finished");
        d.close_guard();
        d.open_guard();
        track(&mut d, 3).expect("inner, failed");
        d.rollback_guard();
        assert_eq!(d.board().tracks.len(), 2, "only the failed inner command is reversed");
        d.rollback_guard();
        assert!(d.board().tracks.is_empty(), "the outer guard reverses the finished inner command too");
    }

    #[test]
    fn close_guard_keeps_what_the_command_did() {
        let mut d = Document::new(Board::new("x"));
        d.open_guard();
        track(&mut d, 1).expect("track");
        d.close_guard();
        d.rollback_guard();
        assert_eq!(d.board().tracks.len(), 1);
    }
}
