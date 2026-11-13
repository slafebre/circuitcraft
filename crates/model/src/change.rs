//! What an edit changed, and the handle an edit changes the board through.
//!
//! A [`ChangeSet`] is how undo and redo work (an undo step stores one, not a copy of the board),
//! and how readers that keep something derived from the board (the renderer, a spatial index,
//! incremental checks) will learn what to update.

use crate::store::Change;
use crate::{
    Assembled, Board, BoardHole, DesignSettings, Footprint, Graphic, Hole, ItemId, Keepout, LayerId, ModelError, Net, NetId, Text, Track, TrackPath,
    Via, Zone,
};
use circuitcraft_geom::Contour;
use circuitcraft_units::{Nm, Point};

/// A value before and after an edit.
type BeforeAfter<T> = Option<Box<(T, T)>>;

/// Board settings from before the edit being recorded, each kept by the first setter that
/// changes it, so an edit copies only the settings it changes.
#[derive(Debug, Default)]
pub(crate) struct SettingsBefore {
    name: Option<String>,
    design: Option<DesignSettings>,
    outline: Option<Option<Contour>>,
    cutouts: Option<Vec<Contour>>,
}

/// What one edit changed, item by item, in id order within each kind.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    /// Nets added, removed or changed.
    pub nets: Vec<Change<Net>>,
    /// Footprints added, removed or changed.
    pub footprints: Vec<Change<Footprint>>,
    /// Tracks added, removed or changed.
    pub tracks: Vec<Change<Track>>,
    /// Vias added, removed or changed.
    pub vias: Vec<Change<Via>>,
    /// Zones added, removed or changed.
    pub zones: Vec<Change<Zone>>,
    /// Keepouts added, removed or changed.
    pub keepouts: Vec<Change<Keepout>>,
    /// Holes outside a footprint added, removed or changed.
    pub holes: Vec<Change<BoardHole>>,
    /// Texts added, removed or changed.
    pub texts: Vec<Change<Text>>,
    /// Drawings added, removed or changed.
    pub graphics: Vec<Change<Graphic>>,
    /// The board name before and after, if it changed.
    name: BeforeAfter<String>,
    /// The design settings before and after, if they changed.
    design: BeforeAfter<DesignSettings>,
    /// The board outline before and after, if it changed.
    outline: BeforeAfter<Option<Contour>>,
    /// The cutouts before and after, if they changed.
    cutouts: BeforeAfter<Vec<Contour>>,
}

impl ChangeSet {
    /// How many things changed: each item, the name, the design settings, the outline and the
    /// cutouts count as one.
    pub fn len(&self) -> usize {
        self.nets.len()
            + self.footprints.len()
            + self.tracks.len()
            + self.vias.len()
            + self.zones.len()
            + self.keepouts.len()
            + self.holes.len()
            + self.texts.len()
            + self.graphics.len()
            + usize::from(self.name.is_some())
            + usize::from(self.design.is_some())
            + usize::from(self.outline.is_some())
            + usize::from(self.cutouts.is_some())
    }

    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the board's name, design settings, outline or cutouts changed.
    pub fn settings_changed(&self) -> bool {
        self.name.is_some() || self.design.is_some() || self.outline.is_some() || self.cutouts.is_some()
    }
}

/// The board as an edit sees it ([`crate::Document::edit`]): everything can be read (it derefs to
/// [`Board`]), and changes go only through these methods, which record what they change. An edit
/// can't swap out a collection or the whole board behind the recording's back.
#[derive(Debug)]
pub struct BoardEdit<'a> {
    pub(crate) board: &'a mut Board,
    before: &'a mut SettingsBefore,
}

impl std::ops::Deref for BoardEdit<'_> {
    type Target = Board;
    fn deref(&self) -> &Board {
        self.board
    }
}

impl<'a> BoardEdit<'a> {
    pub(crate) fn new(board: &'a mut Board, before: &'a mut SettingsBefore) -> BoardEdit<'a> {
        BoardEdit { board, before }
    }

    /// Renames the board.
    pub fn set_name(&mut self, name: &str) -> Result<(), ModelError> {
        crate::check_name(name, "board name")?;
        if self.before.name.is_none() {
            self.before.name = Some(self.board.name.clone());
        }
        self.board.name = name.to_owned();
        Ok(())
    }

    /// Sets or clears the board outline.
    pub fn set_outline(&mut self, outline: Option<Contour>) -> Result<(), ModelError> {
        if let Some(c) = &outline {
            crate::check_contour(c, "board outline")?;
        }
        if self.before.outline.is_none() {
            self.before.outline = Some(self.board.outline.clone());
        }
        self.board.outline = outline;
        Ok(())
    }

    /// Replaces the design settings.
    pub fn set_design(&mut self, design: DesignSettings) -> Result<(), ModelError> {
        crate::check_design(&design)?;
        self.before.design.get_or_insert(self.board.design);
        self.board.design = design;
        Ok(())
    }

    /// See [`Board::assemble_outline`].
    pub fn assemble_outline(&mut self, gap: Nm) -> Result<Assembled, ModelError> {
        if self.before.outline.is_none() {
            self.before.outline = Some(self.board.outline.clone());
        }
        if self.before.cutouts.is_none() {
            self.before.cutouts = Some(self.board.cutouts.clone());
        }
        self.board.assemble_outline(gap)
    }

    /// See [`Board::add_hole`].
    pub fn add_hole(&mut self, position: Point, hole: Hole) -> Result<ItemId, ModelError> {
        self.board.add_hole(position, hole)
    }

    /// See [`Board::add_net`].
    pub fn add_net(&mut self, name: &str) -> Result<NetId, ModelError> {
        self.board.add_net(name)
    }

    /// See [`Board::add_track`].
    pub fn add_track(&mut self, layer: LayerId, width: Nm, path: TrackPath, net: Option<NetId>) -> Result<ItemId, ModelError> {
        self.board.add_track(layer, width, path, net)
    }

    /// See [`Board::add_via`].
    pub fn add_via(&mut self, position: Point, diameter: Nm, drill: Nm, net: Option<NetId>) -> Result<ItemId, ModelError> {
        self.board.add_via(position, diameter, drill, net)
    }

    /// See [`Board::remove`].
    pub fn remove(&mut self, id: ItemId) -> Result<(), ModelError> {
        self.board.remove(id)
    }
}

/// Before and after, if `before` was kept and differs from `now`.
fn changed<T: PartialEq + Clone>(before: Option<T>, now: &T) -> BeforeAfter<T> {
    before.filter(|b| b != now).map(|b| Box::new((b, now.clone())))
}

/// The value before (`forward == false`) or after a change.
fn pick<T: Clone>(c: &(T, T), forward: bool) -> T {
    if forward { c.1.clone() } else { c.0.clone() }
}

impl Board {
    /// Starts recording what happens to the items.
    pub(crate) fn begin_recording(&mut self) {
        self.nets.begin();
        self.footprints.begin();
        self.tracks.begin();
        self.vias.begin();
        self.zones.begin();
        self.keepouts.begin();
        self.holes.begin();
        self.texts.begin();
        self.graphics.begin();
    }

    /// Stops recording and returns what changed; `before` is what [`BoardEdit`] kept.
    pub(crate) fn finish_recording(&mut self, before: SettingsBefore) -> ChangeSet {
        ChangeSet {
            nets: self.nets.finish(),
            footprints: self.footprints.finish(),
            tracks: self.tracks.finish(),
            vias: self.vias.finish(),
            zones: self.zones.finish(),
            keepouts: self.keepouts.finish(),
            holes: self.holes.finish(),
            texts: self.texts.finish(),
            graphics: self.graphics.finish(),
            name: changed(before.name, &self.name),
            design: changed(before.design, &self.design),
            outline: changed(before.outline, &self.outline),
            cutouts: changed(before.cutouts, &self.cutouts),
        }
    }

    /// Stops recording and puts everything back as it was when recording began.
    pub(crate) fn rollback_recording(&mut self, before: SettingsBefore) {
        self.nets.rollback();
        self.footprints.rollback();
        self.tracks.rollback();
        self.vias.rollback();
        self.zones.rollback();
        self.keepouts.rollback();
        self.holes.rollback();
        self.texts.rollback();
        self.graphics.rollback();
        if let Some(name) = before.name {
            self.name = name;
        }
        if let Some(design) = before.design {
            self.design = design;
        }
        if let Some(outline) = before.outline {
            self.outline = outline;
        }
        if let Some(cutouts) = before.cutouts {
            self.cutouts = cutouts;
        }
    }

    /// Redoes (`forward`) or undoes a change set. It writes the recorded values, not differences,
    /// so it also completes a change set that was applied halfway.
    pub(crate) fn apply(&mut self, c: &ChangeSet, forward: bool) {
        self.nets.apply(&c.nets, forward);
        self.footprints.apply(&c.footprints, forward);
        self.tracks.apply(&c.tracks, forward);
        self.vias.apply(&c.vias, forward);
        self.zones.apply(&c.zones, forward);
        self.keepouts.apply(&c.keepouts, forward);
        self.holes.apply(&c.holes, forward);
        self.texts.apply(&c.texts, forward);
        self.graphics.apply(&c.graphics, forward);
        if let Some(name) = &c.name {
            self.name = pick(name, forward);
        }
        if let Some(design) = &c.design {
            self.design = pick(design, forward);
        }
        if let Some(outline) = &c.outline {
            self.outline = pick(outline, forward);
        }
        if let Some(cutouts) = &c.cutouts {
            self.cutouts = pick(cutouts, forward);
        }
    }
}
