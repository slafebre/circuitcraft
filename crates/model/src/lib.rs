//! The design database.
//!
//! A [`Board`] holds its layer stack, outline, nets and items (footprints with pads, tracks, vias,
//! zones, text and drawings). Items live in copy-on-write [`Items`] collections, so an undo
//! snapshot of a whole board is cheap, and a [`Document`] keeps those snapshots for undo and redo.
//!
//! Edits go through [`Board`] methods that check what they add ([`Board::validate`] checks a whole
//! board, for example one just read from a file). Everything is in whole nanometres.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod document;
mod items;
mod layer;
mod store;

pub use document::{Document, UNDO_LIMIT};
pub use items::{
    Drill, Footprint, Graphic, GraphicShape, ItemId, Justify, MAX_FOOTPRINT_GRAPHICS, MAX_PADS, Net, NetId, Pad, PadKind, PadShape, Rotation, Side,
    Text, Track, TrackPath, Via, Zone,
};
pub use layer::{Layer, LayerId, LayerKind, LayerStack, MAX_LAYERS, ids as layer_ids};
pub use store::{Items, Keyed, MAX_ITEMS};

use circuitcraft_geom::{Contour, Edge, MAX_POINTS};
use circuitcraft_units::{MAX_ABS, Nm, Point};
use serde::{Deserialize, Serialize};

/// The longest name, reference, value or text accepted, in characters.
pub const MAX_NAME_CHARS: usize = 1_024;

/// A model change was refused, or a board is inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// A coordinate is outside the ±10 m design range.
    #[error("{0} is outside the design range")]
    OutOfRange(&'static str),
    /// A size that must be positive isn't, or is larger than the design range.
    #[error("{0} must be positive and within the design range")]
    BadSize(&'static str),
    /// A name is empty, too long or already taken.
    #[error("bad name {0:?}: {1}")]
    BadName(String, &'static str),
    /// A layer reference doesn't exist or is the wrong kind.
    #[error("{0}: no such {1} layer")]
    BadLayer(&'static str, &'static str),
    /// A net reference doesn't exist.
    #[error("{0}: no such net")]
    UnknownNet(&'static str),
    /// No item has this id.
    #[error("no item with id {0}")]
    UnknownItem(u64),
    /// Two items share an id, or an id is not below the board's id counter.
    #[error("id {0} is duplicated or out of sequence")]
    BadId(u64),
    /// The board has run out of ids (2⁶⁴ of them).
    #[error("out of item ids")]
    IdsExhausted,
    /// A collection is at its size limit ([`MAX_ITEMS`], [`MAX_PADS`], [`MAX_LAYERS`], ...).
    #[error("too many {0}")]
    TooMany(&'static str),
}

/// A printed circuit board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    /// The board's name, shown in title bars and outputs.
    pub name: String,
    /// Its layers.
    pub stack: LayerStack,
    /// The board outline, if drawn.
    pub outline: Option<Contour>,
    /// Holes and slots cut through the board inside the outline (at most [`MAX_ITEMS`]).
    #[serde(deserialize_with = "cutouts")]
    pub cutouts: Vec<Contour>,
    /// Nets.
    pub nets: Items<Net>,
    /// Placed footprints.
    pub footprints: Items<Footprint>,
    /// Copper tracks.
    pub tracks: Items<Track>,
    /// Vias.
    pub vias: Items<Via>,
    /// Copper zones.
    pub zones: Items<Zone>,
    /// Free text.
    pub texts: Items<Text>,
    /// Free drawings (outline sketches, silkscreen logos, documentation).
    pub graphics: Items<Graphic>,
    /// The next id to hand out; every id on the board is below it.
    pub next_id: u64,
}

fn cutouts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Contour>, D::Error> {
    store::bounded_vec(d, MAX_ITEMS)
}

/// Refuses to add to a collection that is already at [`MAX_ITEMS`].
fn room<T: Keyed>(items: &Items<T>, what: &'static str) -> Result<(), ModelError> {
    if items.len() >= MAX_ITEMS { Err(ModelError::TooMany(what)) } else { Ok(()) }
}

impl Board {
    /// A new, empty two-layer board.
    pub fn new(name: &str) -> Board {
        Board {
            name: name.to_owned(),
            stack: LayerStack::two_layer(),
            outline: None,
            cutouts: Vec::new(),
            nets: Items::default(),
            footprints: Items::default(),
            tracks: Items::default(),
            vias: Items::default(),
            zones: Items::default(),
            texts: Items::default(),
            graphics: Items::default(),
            next_id: 1,
        }
    }

    /// Hands out a fresh id.
    pub fn fresh_id(&mut self) -> Result<u64, ModelError> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(ModelError::IdsExhausted)?;
        Ok(id)
    }

    /// The net with this name.
    pub fn net_by_name(&self, name: &str) -> Option<&Net> {
        self.nets.iter().find(|n| n.name == name)
    }

    /// Adds a net with a unique name.
    pub fn add_net(&mut self, name: &str) -> Result<NetId, ModelError> {
        check_name(name, "net name")?;
        room(&self.nets, "nets")?;
        if self.net_by_name(name).is_some() {
            return Err(ModelError::BadName(name.to_owned(), "a net with this name exists"));
        }
        let id = NetId(self.fresh_id()?);
        self.nets.insert(Net { id, name: name.to_owned() });
        Ok(id)
    }

    /// Adds a track; its id is assigned here.
    pub fn add_track(&mut self, layer: LayerId, width: Nm, path: TrackPath, net: Option<NetId>) -> Result<ItemId, ModelError> {
        room(&self.tracks, "tracks")?;
        let id = ItemId(self.fresh_id()?);
        let track = Track { id, layer, width, path, net };
        self.check_track(&track)?;
        self.tracks.insert(track);
        Ok(id)
    }

    /// Adds a through-board via from the top to the bottom copper layer; its id is assigned here.
    pub fn add_via(&mut self, position: Point, diameter: Nm, drill: Nm, net: Option<NetId>) -> Result<ItemId, ModelError> {
        let (Some(from), Some(to)) = (self.stack.copper().next().map(|l| l.id), self.stack.copper().last().map(|l| l.id)) else {
            return Err(ModelError::BadLayer("via", "copper"));
        };
        room(&self.vias, "vias")?;
        let id = ItemId(self.fresh_id()?);
        let via = Via { id, position, diameter, drill, from, to, net };
        self.check_via(&via)?;
        self.vias.insert(via);
        Ok(id)
    }

    /// Removes any item (track, via, footprint, zone, text, drawing) or net by id. Removing a net
    /// disconnects everything on it.
    pub fn remove(&mut self, id: u64) -> Result<(), ModelError> {
        let item = ItemId(id);
        let removed = self.tracks.remove(item).is_some()
            || self.vias.remove(item).is_some()
            || self.footprints.remove(item).is_some()
            || self.zones.remove(item).is_some()
            || self.texts.remove(item).is_some()
            || self.graphics.remove(item).is_some();
        if removed {
            return Ok(());
        }
        let net = NetId(id);
        if self.nets.remove(net).is_none() {
            return Err(ModelError::UnknownItem(id));
        }
        self.disconnect(net);
        Ok(())
    }

    /// Clears `net` from everything that referred to it.
    fn disconnect(&mut self, net: NetId) {
        let on = |n: &Option<NetId>| *n == Some(net);
        let tracks: Vec<_> = self.tracks.iter().filter(|t| on(&t.net)).map(|t| t.id).collect();
        for id in tracks {
            if let Some(t) = self.tracks.get_mut(id) {
                t.net = None;
            }
        }
        let vias: Vec<_> = self.vias.iter().filter(|v| on(&v.net)).map(|v| v.id).collect();
        for id in vias {
            if let Some(v) = self.vias.get_mut(id) {
                v.net = None;
            }
        }
        let zones: Vec<_> = self.zones.iter().filter(|z| on(&z.net)).map(|z| z.id).collect();
        for id in zones {
            if let Some(z) = self.zones.get_mut(id) {
                z.net = None;
            }
        }
        let fps: Vec<_> = self.footprints.iter().filter(|f| f.pads.iter().any(|p| on(&p.net))).map(|f| f.id).collect();
        for id in fps {
            if let Some(f) = self.footprints.get_mut(id) {
                for p in f.pads.iter_mut().filter(|p| p.net == Some(net)) {
                    p.net = None;
                }
            }
        }
    }

    /// Checks the whole board: the layer stack, names, ids, layer and net references, sizes,
    /// coordinates and collection limits. Run it on any board that didn't come from these edit
    /// methods, such as one read from a file.
    pub fn validate(&self) -> Result<(), ModelError> {
        check_name(&self.name, "board name")?;
        // Layer lookups return the first match, so the stack must be unambiguous before any item's
        // layer is checked against it.
        self.stack.validate()?;
        self.check_collections()?;
        let mut ids = std::collections::BTreeSet::new();
        let mut claim = |id: u64| if id >= self.next_id || !ids.insert(id) { Err(ModelError::BadId(id)) } else { Ok(()) };
        for n in self.nets.iter() {
            claim(n.id.0)?;
            check_name(&n.name, "net name")?;
        }
        let mut names: Vec<&str> = self.nets.iter().map(|n| n.name.as_str()).collect();
        names.sort_unstable();
        if let Some(w) = names.windows(2).find(|w| w.first() == w.get(1)) {
            return Err(ModelError::BadName(w.first().copied().unwrap_or_default().to_owned(), "duplicate net name"));
        }
        if let Some(outline) = &self.outline {
            check_contour(outline, "board outline")?;
        }
        for c in &self.cutouts {
            check_contour(c, "cutout")?;
        }
        for t in self.tracks.iter() {
            claim(t.id.0)?;
            self.check_track(t)?;
        }
        for v in self.vias.iter() {
            claim(v.id.0)?;
            self.check_via(v)?;
        }
        for z in self.zones.iter() {
            claim(z.id.0)?;
            self.check_copper_layer(z.layer, "zone")?;
            self.check_net(z.net, "zone")?;
            check_contour(&z.outline, "zone outline")?;
            check_size(z.clearance, "zone clearance", true)?;
        }
        for t in self.texts.iter() {
            claim(t.id.0)?;
            self.check_text(t)?;
        }
        for g in self.graphics.iter() {
            claim(g.id.0)?;
            self.check_graphic(g)?;
        }
        for f in self.footprints.iter() {
            claim(f.id.0)?;
            check_point(f.position, "footprint position")?;
            check_name(&f.reference, "reference")?;
            check_len(&f.value, "value")?;
            if f.pads.len() > MAX_PADS {
                return Err(ModelError::TooMany("pads"));
            }
            if f.graphics.len() > MAX_FOOTPRINT_GRAPHICS {
                return Err(ModelError::TooMany("footprint drawings"));
            }
            for p in &f.pads {
                check_name(&p.number, "pad number")?;
                check_point(p.position, "pad position")?;
                self.check_net(p.net, "pad")?;
                check_pad_shape(&p.shape)?;
                if let PadKind::ThroughHole { drill } = p.kind {
                    check_size(drill.diameter, "pad drill", false)?;
                }
            }
            for g in &f.graphics {
                claim(g.id.0)?;
                self.check_graphic(g)?;
            }
        }
        Ok(())
    }

    /// Every collection is within [`MAX_ITEMS`] and files each item under its own id (an id
    /// changed through [`Items::get_mut`] would leave it unreachable by its new id).
    fn check_collections(&self) -> Result<(), ModelError> {
        let lens = [
            (self.nets.len(), "nets"),
            (self.footprints.len(), "footprints"),
            (self.tracks.len(), "tracks"),
            (self.vias.len(), "vias"),
            (self.zones.len(), "zones"),
            (self.texts.len(), "texts"),
            (self.graphics.len(), "drawings"),
            (self.cutouts.len(), "cutouts"),
        ];
        if let Some((_, what)) = lens.iter().find(|(n, _)| *n > MAX_ITEMS) {
            return Err(ModelError::TooMany(what));
        }
        let misfiled = (self.nets.mismatched_key().map(|k| k.0))
            .or_else(|| self.footprints.mismatched_key().map(|k| k.0))
            .or_else(|| self.tracks.mismatched_key().map(|k| k.0))
            .or_else(|| self.vias.mismatched_key().map(|k| k.0))
            .or_else(|| self.zones.mismatched_key().map(|k| k.0))
            .or_else(|| self.texts.mismatched_key().map(|k| k.0))
            .or_else(|| self.graphics.mismatched_key().map(|k| k.0));
        match misfiled {
            Some(k) => Err(ModelError::BadId(k)),
            None => Ok(()),
        }
    }

    fn check_net(&self, net: Option<NetId>, what: &'static str) -> Result<(), ModelError> {
        match net {
            Some(n) if !self.nets.contains(n) => Err(ModelError::UnknownNet(what)),
            _ => Ok(()),
        }
    }

    fn check_copper_layer(&self, layer: LayerId, what: &'static str) -> Result<(), ModelError> {
        if self.stack.is_copper(layer) { Ok(()) } else { Err(ModelError::BadLayer(what, "copper")) }
    }

    fn check_track(&self, t: &Track) -> Result<(), ModelError> {
        self.check_copper_layer(t.layer, "track")?;
        self.check_net(t.net, "track")?;
        check_size(t.width, "track width", false)?;
        match t.path {
            TrackPath::Line { from, to } => {
                check_point(from, "track start")?;
                check_point(to, "track end")?;
            }
            TrackPath::Arc { arc } => {
                for p in [arc.start, arc.mid, arc.end] {
                    check_point(p, "track arc")?;
                }
            }
        }
        Ok(())
    }

    fn check_via(&self, v: &Via) -> Result<(), ModelError> {
        self.check_copper_layer(v.from, "via")?;
        self.check_copper_layer(v.to, "via")?;
        self.check_net(v.net, "via")?;
        check_point(v.position, "via position")?;
        check_size(v.diameter, "via diameter", false)?;
        check_size(v.drill, "via drill", false)?;
        if v.drill >= v.diameter {
            return Err(ModelError::BadSize("via drill (smaller than its diameter)"));
        }
        Ok(())
    }

    fn check_text(&self, t: &Text) -> Result<(), ModelError> {
        if self.stack.get(t.layer).is_none() {
            return Err(ModelError::BadLayer("text", "existing"));
        }
        check_len(&t.text, "text")?;
        check_point(t.position, "text position")?;
        check_size(t.height, "text height", false)?;
        check_size(t.stroke, "text stroke", false)
    }

    fn check_graphic(&self, g: &Graphic) -> Result<(), ModelError> {
        if self.stack.get(g.layer).is_none() {
            return Err(ModelError::BadLayer("drawing", "existing"));
        }
        check_size(g.width, "drawing width", true)?;
        match &g.shape {
            GraphicShape::Line { from, to } => {
                check_point(*from, "drawing start")?;
                check_point(*to, "drawing end")
            }
            GraphicShape::Arc { arc } => [arc.start, arc.mid, arc.end].into_iter().try_for_each(|p| check_point(p, "drawing arc")),
            GraphicShape::Circle { center, radius } => {
                check_point(*center, "circle centre")?;
                check_size(*radius, "circle radius", false)
            }
            GraphicShape::Polygon { outline, .. } => check_contour(outline, "drawing outline"),
        }
    }
}

fn check_len(s: &str, what: &'static str) -> Result<(), ModelError> {
    if s.chars().count() > MAX_NAME_CHARS { Err(ModelError::BadName(s.chars().take(40).collect(), what)) } else { Ok(()) }
}

fn check_name(s: &str, what: &'static str) -> Result<(), ModelError> {
    if s.trim().is_empty() {
        return Err(ModelError::BadName(s.to_owned(), what));
    }
    check_len(s, what)
}

fn check_point(p: Point, what: &'static str) -> Result<(), ModelError> {
    let max = MAX_ABS.unsigned_abs();
    if p.x.0.unsigned_abs() > max || p.y.0.unsigned_abs() > max { Err(ModelError::OutOfRange(what)) } else { Ok(()) }
}

/// A size must be positive (or zero, if `zero_ok`) and within the design range.
fn check_size(n: Nm, what: &'static str, zero_ok: bool) -> Result<(), ModelError> {
    let ok = if zero_ok { n.0 >= 0 } else { n.0 > 0 };
    if ok && n.0 <= 2 * MAX_ABS { Ok(()) } else { Err(ModelError::BadSize(what)) }
}

fn check_contour(c: &Contour, what: &'static str) -> Result<(), ModelError> {
    if c.edges.len() > MAX_POINTS {
        return Err(ModelError::TooMany("contour edges"));
    }
    check_point(c.start, what)?;
    for e in &c.edges {
        match *e {
            Edge::Line { to } => check_point(to, what)?,
            Edge::Arc { mid, to } => {
                check_point(mid, what)?;
                check_point(to, what)?;
            }
        }
    }
    Ok(())
}

fn check_pad_shape(s: &PadShape) -> Result<(), ModelError> {
    match s {
        PadShape::Circle { diameter } => check_size(*diameter, "pad diameter", false),
        PadShape::Rect { width, height } | PadShape::Oval { width, height } => {
            check_size(*width, "pad width", false)?;
            check_size(*height, "pad height", false)
        }
        PadShape::RoundRect { width, height, radius } => {
            check_size(*width, "pad width", false)?;
            check_size(*height, "pad height", false)?;
            check_size(*radius, "pad corner radius", true)?;
            // The corners must fit: two radii side by side are no wider than the shorter side.
            if radius.0.saturating_mul(2) > width.0.min(height.0) {
                return Err(ModelError::BadSize("pad corner radius (at most half the shorter side)"));
            }
            Ok(())
        }
        PadShape::Custom { outline } => check_contour(outline, "pad outline"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_ids::{BOTTOM_COPPER, TOP_COPPER, TOP_SILK};

    const MM: i64 = 1_000_000;

    fn p(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    fn line(a: Point, b: Point) -> TrackPath {
        TrackPath::Line { from: a, to: b }
    }

    #[test]
    fn build_a_small_board() {
        let mut b = Board::new("Blinky");
        let gnd = b.add_net("GND").expect("net");
        let t = b.add_track(TOP_COPPER, Nm(250_000), line(p(0, 0), p(10 * MM, 0)), Some(gnd)).expect("track");
        let v = b.add_via(p(10 * MM, 0), Nm(600_000), Nm(300_000), Some(gnd)).expect("via");
        b.add_track(BOTTOM_COPPER, Nm(250_000), line(p(10 * MM, 0), p(10 * MM, 5 * MM)), Some(gnd)).expect("track");
        assert_eq!((b.nets.len(), b.tracks.len(), b.vias.len()), (1, 2, 1));
        assert_ne!(t, v);
        assert_eq!(b.validate(), Ok(()));
    }

    #[test]
    fn edits_check_what_they_add() {
        let mut b = Board::new("x");
        let path = line(p(0, 0), p(MM, 0));
        assert_eq!(b.add_track(TOP_SILK, Nm(1), path, None), Err(ModelError::BadLayer("track", "copper")));
        assert_eq!(b.add_track(TOP_COPPER, Nm(0), path, None), Err(ModelError::BadSize("track width")));
        assert_eq!(b.add_track(TOP_COPPER, Nm(1), path, Some(NetId(99))), Err(ModelError::UnknownNet("track")));
        assert_eq!(b.add_track(TOP_COPPER, Nm(1), line(p(i64::MIN, 0), p(0, 0)), None), Err(ModelError::OutOfRange("track start")));
        assert!(matches!(b.add_via(p(0, 0), Nm(300), Nm(300), None), Err(ModelError::BadSize(_))));
        b.add_net("GND").expect("net");
        assert!(matches!(b.add_net("GND"), Err(ModelError::BadName(..))));
        assert!(matches!(b.add_net("  "), Err(ModelError::BadName(..))));
        // Nothing was added by the refused edits.
        assert_eq!((b.tracks.len(), b.vias.len(), b.nets.len()), (0, 0, 1));
    }

    #[test]
    fn removing_a_net_disconnects_its_items() {
        let mut b = Board::new("x");
        let gnd = b.add_net("GND").expect("net");
        let t = b.add_track(TOP_COPPER, Nm(1), line(p(0, 0), p(1, 0)), Some(gnd)).expect("track");
        b.remove(gnd.0).expect("remove net");
        assert_eq!(b.tracks.get(t).map(|t| t.net), Some(None));
        b.remove(t.0).expect("remove track");
        assert_eq!(b.remove(t.0), Err(ModelError::UnknownItem(t.0)));
        assert_eq!(b.validate(), Ok(()));
    }

    #[test]
    fn validate_catches_inconsistent_boards() {
        let mut b = Board::new("x");
        let gnd = b.add_net("GND").expect("net");
        b.add_track(TOP_COPPER, Nm(1), line(p(0, 0), p(1, 0)), Some(gnd)).expect("track");
        let mut dangling = b.clone();
        dangling.nets.remove(gnd);
        assert_eq!(dangling.validate(), Err(ModelError::UnknownNet("track")));
        let mut stale = b.clone();
        stale.next_id = 1;
        assert!(matches!(stale.validate(), Err(ModelError::BadId(_))));
        let mut unnamed = b;
        unnamed.name = String::new();
        assert!(matches!(unnamed.validate(), Err(ModelError::BadName(..))));
    }

    #[test]
    fn validate_checks_the_layer_stack() {
        let mut b = Board::new("x");
        if let Some(l) = b.stack.layers.get_mut(1) {
            l.name = "Top Paste".to_owned();
        }
        assert!(matches!(b.validate(), Err(ModelError::BadName(..))));
        let mut b = Board::new("x");
        if let Some(l) = b.stack.layers.get_mut(1) {
            l.id = TOP_COPPER;
        }
        assert_eq!(b.validate(), Err(ModelError::BadLayer("layer stack", "uniquely numbered")));
    }

    #[test]
    fn validate_catches_an_id_changed_in_place() {
        let mut b = Board::new("x");
        let t = b.add_track(TOP_COPPER, Nm(1), line(p(0, 0), p(1, 0)), None).expect("track");
        b.next_id = 100;
        if let Some(track) = b.tracks.get_mut(t) {
            track.id = ItemId(50);
        }
        assert!(b.tracks.get(ItemId(50)).is_none(), "still filed under the old id");
        assert_eq!(b.validate(), Err(ModelError::BadId(t.0)));
    }

    #[test]
    fn round_rect_corners_must_fit() {
        let rr = |r: i64| PadShape::RoundRect { width: Nm(100), height: Nm(60), radius: Nm(r) };
        assert_eq!(check_pad_shape(&rr(30)), Ok(()));
        assert!(matches!(check_pad_shape(&rr(31)), Err(ModelError::BadSize(_))));
        assert!(matches!(check_pad_shape(&rr(1_000)), Err(ModelError::BadSize(_))));
    }

    #[test]
    fn json_round_trip_is_stable() {
        let mut b = Board::new("Blinky");
        let gnd = b.add_net("GND").expect("net");
        b.add_track(TOP_COPPER, Nm(250_000), line(p(0, 0), p(MM, 0)), Some(gnd)).expect("track");
        b.add_via(p(MM, 0), Nm(600_000), Nm(300_000), Some(gnd)).expect("via");
        let first = serde_json::to_string_pretty(&b).expect("json");
        let back: Board = serde_json::from_str(&first).expect("parse");
        assert_eq!(back, b);
        assert_eq!(serde_json::to_string_pretty(&back).expect("json"), first, "re-save must be byte-identical");
    }
}
