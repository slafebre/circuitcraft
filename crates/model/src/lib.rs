//! The design database.
//!
//! A [`Board`] holds its design settings, layer stack, outline, nets and items (footprints with
//! pads, tracks, vias, zones, keepouts, holes, text and drawings). Items live in copy-on-write [`Items`] collections that record what
//! an edit touches, so a [`Document`] keeps each edit's [`ChangeSet`] for undo and redo, at a cost
//! that follows the size of the edit, not the size of the board.
//!
//! An edit runs inside [`Document::edit`] and changes the board through a [`BoardEdit`], whose
//! methods check what they add and record what they change ([`Board::validate`] checks a whole
//! board, for example one just read from a file). Everything is in whole nanometres.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod change;
mod document;
mod items;
mod layer;
mod outline;
mod store;

pub use change::{BoardEdit, ChangeSet};
pub use document::{Document, UNDO_LIMIT};
pub use items::{
    Assembly, BoardHole, DesignSettings, Footprint, FootprintText, Graphic, GraphicShape, Hole, ItemId, Justify, Keepout, LibraryLink,
    MAX_FOOTPRINT_GRAPHICS, MAX_FOOTPRINT_TEXTS, MAX_PADS, MAX_ZONE_HOLES, MAX_ZONE_POLYGONS, Mounting, Net, NetId, Opening, Pad, PadConnection,
    PadFace, PadShape, Rotation, Side, Slot, Text, TextContent, Track, TrackPath, Via, Zone, ZoneFill,
};
pub use layer::{Layer, LayerId, LayerKind, LayerStack, MAX_LAYERS, ids as layer_ids};
pub use outline::{Assembled, MAX_OUTLINE_GAP};
pub use store::{Change, Items, Keyed, MAX_ITEMS};

use circuitcraft_geom::{Contour, Edge, MAX_POINTS, Polygon};
use circuitcraft_units::{MAX_ABS, Nm, Point};
use serde::{Deserialize, Serialize};

/// The longest name, reference, value or text accepted, in characters.
pub const MAX_NAME_CHARS: usize = 1_024;

/// A model change was refused, or a board is inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// A coordinate or an expansion is outside the ±10 m design range, or a rotation is outside
    /// the turn it may make.
    #[error("{0} is out of range")]
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
    UnknownItem(ItemId),
    /// An id is malformed, all zeros, used twice, or doesn't match where its item is filed.
    #[error("bad id {0:?}: malformed, nil, duplicated or misfiled")]
    BadId(String),
    /// The system gave no random numbers for a new id.
    #[error("no random numbers for a new id: {0}")]
    NoRandomness(String),
    /// A layer's side or pair doesn't fit the layer stack.
    #[error("layer {0:?}: {1}")]
    BadStack(String, &'static str),
    /// A pad's copper, hole and net don't fit together.
    #[error("pad {0:?}: {1}")]
    BadPad(String, &'static str),
    /// An outline, a zone or a keepout that can't do what it is for.
    #[error("{0}: {1}")]
    BadArea(&'static str, &'static str),
    /// The drawings on the outline layer don't make one board outline.
    #[error("board outline: {0}")]
    BadOutline(&'static str),
    /// A collection is at its size limit ([`MAX_ITEMS`], [`MAX_PADS`], [`MAX_LAYERS`], ...).
    #[error("too many {0}")]
    TooMany(&'static str),
}

/// A printed circuit board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Board {
    /// The board's name, shown in title bars and outputs.
    pub name: String,
    /// The values the whole board shares.
    pub design: DesignSettings,
    /// Its layers.
    pub stack: LayerStack,
    /// The board outline, if it has one. Loose lines and arcs on the outline layer become one
    /// with [`BoardEdit::assemble_outline`].
    #[serde(deserialize_with = "items::required")]
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
    /// Areas on a copper layer that tracks, vias or zones stay out of.
    pub keepouts: Items<Keepout>,
    /// Holes that belong to no footprint.
    pub holes: Items<BoardHole>,
    /// Free text.
    pub texts: Items<Text>,
    /// Free drawings (outline sketches, silkscreen logos, documentation).
    pub graphics: Items<Graphic>,
}

fn cutouts<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Contour>, D::Error> {
    store::bounded_vec(d, MAX_ITEMS)
}

/// Refuses to add to a collection that is already at [`MAX_ITEMS`], before an id is spent and with
/// a message that names the collection ([`Items::insert`] enforces the same limit).
fn room<T: Keyed>(items: &Items<T>, what: &'static str) -> Result<(), ModelError> {
    if items.len() >= MAX_ITEMS { Err(ModelError::TooMany(what)) } else { Ok(()) }
}

impl Board {
    /// A new, empty two-layer board.
    pub fn new(name: &str) -> Board {
        Board {
            name: name.to_owned(),
            design: DesignSettings::default(),
            stack: LayerStack::two_layer(),
            outline: None,
            cutouts: Vec::new(),
            nets: Items::default(),
            footprints: Items::default(),
            tracks: Items::default(),
            vias: Items::default(),
            zones: Items::default(),
            keepouts: Items::default(),
            holes: Items::default(),
            texts: Items::default(),
            graphics: Items::default(),
        }
    }

    /// A new random id that no net or top-level item on the board has. A clash is about as
    /// likely as guessing a 122-bit number, so the retries only guard against a broken random
    /// source (footprint drawings and texts aren't searched; [`Board::validate`] checks every id).
    pub fn fresh_id(&self) -> Result<ItemId, ModelError> {
        for _ in 0..8 {
            let id = ItemId::random()?;
            if !self.id_in_use(id) {
                return Ok(id);
            }
        }
        Err(ModelError::NoRandomness("the random source keeps repeating itself".to_owned()))
    }

    fn id_in_use(&self, id: ItemId) -> bool {
        self.nets.contains(NetId::from_u128(id.as_u128()))
            || self.tracks.contains(id)
            || self.vias.contains(id)
            || self.footprints.contains(id)
            || self.zones.contains(id)
            || self.keepouts.contains(id)
            || self.holes.contains(id)
            || self.texts.contains(id)
            || self.graphics.contains(id)
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
        let id = NetId::from_u128(self.fresh_id()?.as_u128());
        self.nets.insert(Net { id, name: name.to_owned() })?;
        Ok(id)
    }

    /// Adds a track; its id is assigned here.
    pub fn add_track(&mut self, layer: LayerId, width: Nm, path: TrackPath, net: Option<NetId>) -> Result<ItemId, ModelError> {
        room(&self.tracks, "tracks")?;
        let id = self.fresh_id()?;
        let track = Track { id, layer, width, path, net };
        self.check_track(&track)?;
        self.tracks.insert(track)?;
        Ok(id)
    }

    /// Adds a through-board via from the top to the bottom copper layer, with solder mask openings
    /// from the design rules; its id is assigned here.
    pub fn add_via(&mut self, position: Point, diameter: Nm, drill: Nm, net: Option<NetId>) -> Result<ItemId, ModelError> {
        let (Some(from), Some(to)) = (self.stack.copper().next().map(|l| l.id), self.stack.copper().last().map(|l| l.id)) else {
            return Err(ModelError::BadLayer("via", "copper"));
        };
        room(&self.vias, "vias")?;
        let id = self.fresh_id()?;
        let via = Via { id, position, diameter, drill, from, to, top_mask: Opening::Rules, bottom_mask: Opening::Rules, net };
        self.check_via(&via)?;
        self.vias.insert(via)?;
        Ok(id)
    }

    /// Adds a hole that belongs to no footprint; its id is assigned here.
    pub fn add_hole(&mut self, position: Point, hole: Hole) -> Result<ItemId, ModelError> {
        room(&self.holes, "holes")?;
        let hole = BoardHole { id: self.fresh_id()?, position, hole };
        check_board_hole(&hole)?;
        self.holes.insert(hole)?;
        Ok(hole.id)
    }

    /// Removes any item (track, via, footprint, zone, keepout, hole, text, drawing) or net by id.
    /// Removing a net disconnects everything on it.
    pub fn remove(&mut self, id: ItemId) -> Result<(), ModelError> {
        let item = id;
        let removed = self.tracks.remove(item).is_some()
            || self.vias.remove(item).is_some()
            || self.footprints.remove(item).is_some()
            || self.zones.remove(item).is_some()
            || self.keepouts.remove(item).is_some()
            || self.holes.remove(item).is_some()
            || self.texts.remove(item).is_some()
            || self.graphics.remove(item).is_some();
        if removed {
            return Ok(());
        }
        let net = NetId::from_u128(id.as_u128());
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

    /// Checks the whole board: the design settings, the layer stack, names, ids, layer and net
    /// references, sizes, coordinates and collection limits. Run it on any board that didn't come from these edit
    /// methods, such as one read from a file.
    pub fn validate(&self) -> Result<(), ModelError> {
        check_name(&self.name, "board name")?;
        // Layer lookups return the first match, so the stack must be unambiguous before any item's
        // layer is checked against it.
        self.stack.validate()?;
        check_design(&self.design)?;
        self.check_collections()?;
        let mut ids = std::collections::BTreeSet::new();
        // Nets and items share one id space, so `remove` and references are never ambiguous.
        let mut claim = |id: u128| {
            if id == 0 || !ids.insert(id) { Err(ModelError::BadId(ItemId::from_u128(id).to_string())) } else { Ok(()) }
        };
        for n in self.nets.iter() {
            claim(n.id.as_u128())?;
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
            claim(t.id.as_u128())?;
            self.check_track(t)?;
        }
        for v in self.vias.iter() {
            claim(v.id.as_u128())?;
            self.check_via(v)?;
        }
        for z in self.zones.iter() {
            claim(z.id.as_u128())?;
            self.check_zone(z)?;
        }
        for k in self.keepouts.iter() {
            claim(k.id.as_u128())?;
            self.check_copper_layer(k.layer, "keepout")?;
            check_contour(&k.outline, "keepout outline")?;
            if !(k.tracks || k.vias || k.zones) {
                return Err(ModelError::BadArea("keepout", "it must keep tracks, vias or zones out"));
            }
        }
        for h in self.holes.iter() {
            claim(h.id.as_u128())?;
            check_board_hole(h)?;
        }
        for t in self.texts.iter() {
            claim(t.id.as_u128())?;
            self.check_text(t)?;
        }
        for g in self.graphics.iter() {
            claim(g.id.as_u128())?;
            self.check_graphic(g)?;
        }
        for f in self.footprints.iter() {
            claim(f.id.as_u128())?;
            check_point(f.position, "footprint position")?;
            check_rotation(f.rotation, "footprint rotation")?;
            check_name(&f.reference, "reference")?;
            check_len(&f.value, "value")?;
            if let Some(link) = &f.link {
                // A link that names no library says so with an empty name: empty, never blank.
                if !link.library.is_empty() {
                    check_name(&link.library, "library name")?;
                }
                check_name(&link.footprint, "library footprint name")?;
            }
            if f.pads.len() > MAX_PADS {
                return Err(ModelError::TooMany("pads"));
            }
            if f.graphics.len() > MAX_FOOTPRINT_GRAPHICS {
                return Err(ModelError::TooMany("footprint drawings"));
            }
            if f.texts.len() > MAX_FOOTPRINT_TEXTS {
                return Err(ModelError::TooMany("footprint texts"));
            }
            for p in &f.pads {
                self.check_pad(p)?;
            }
            for g in &f.graphics {
                claim(g.id.as_u128())?;
                self.check_graphic(g)?;
            }
            for t in &f.texts {
                claim(t.id.as_u128())?;
                self.check_footprint_text(t)?;
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
            (self.keepouts.len(), "keepouts"),
            (self.holes.len(), "holes"),
            (self.texts.len(), "texts"),
            (self.graphics.len(), "drawings"),
            (self.cutouts.len(), "cutouts"),
        ];
        if let Some((_, what)) = lens.iter().find(|(n, _)| *n > MAX_ITEMS) {
            return Err(ModelError::TooMany(what));
        }
        let misfiled = (self.nets.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.footprints.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.tracks.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.vias.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.zones.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.keepouts.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.holes.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.texts.mismatched_key().map(|k| k.to_string()))
            .or_else(|| self.graphics.mismatched_key().map(|k| k.to_string()));
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
        check_opening(v.top_mask, "via mask expansion")?;
        check_opening(v.bottom_mask, "via mask expansion")
    }

    fn check_zone(&self, z: &Zone) -> Result<(), ModelError> {
        self.check_copper_layer(z.layer, "zone")?;
        self.check_net(z.net, "zone")?;
        check_contour(&z.outline, "zone outline")?;
        if z.holes.len() > MAX_ZONE_HOLES {
            return Err(ModelError::TooMany("zone holes"));
        }
        z.holes.iter().try_for_each(|h| check_contour(h, "zone hole"))?;
        check_size(z.clearance, "zone clearance", true)?;
        check_size(z.min_width, "zone minimum width", true)?;
        if let ZoneFill::Hatched { width, gap } = z.fill {
            check_size(width, "zone hatch width", false)?;
            check_size(gap, "zone hatch gap", false)?;
        }
        if let PadConnection::Relief { gap, spoke } = z.pads {
            check_size(gap, "thermal relief gap", false)?;
            check_size(spoke, "thermal relief spoke", false)?;
        }
        check_poured(&z.filled)
    }

    fn check_pad(&self, p: &Pad) -> Result<(), ModelError> {
        // A pad that belongs to no pin (a mounting hole) has no number: empty, never blank.
        if !p.number.is_empty() {
            check_name(&p.number, "pad number")?;
        }
        check_point(p.position, "pad position")?;
        check_rotation(p.rotation, "pad rotation")?;
        check_point(p.copper_offset, "pad copper offset")?;
        self.check_net(p.net, "pad")?;
        let bad = |why| Err(ModelError::BadPad(p.number.chars().take(40).collect(), why));
        // Copper on inner layers alone is allowed (a buried pad), and so is copper no larger than
        // its hole, which importers meet on unplated holes: drilling removes it.
        let copper = p.front.is_some() || p.inner.is_some() || p.back.is_some();
        if !copper && p.hole.is_none() {
            return bad("it needs copper or a hole");
        }
        // Without both, `position` already says where the pad is, in one way.
        if p.copper_offset != Point::ORIGIN && !(copper && p.hole.is_some()) {
            return bad("only copper around a hole can be off centre");
        }
        // A hole alone, unplated, has nothing a net could connect to.
        if p.net.is_some() && !copper && !p.hole.is_some_and(|h| h.plated) {
            return bad("a net needs copper or a plated hole");
        }
        for face in [&p.front, &p.back].into_iter().flatten() {
            check_pad_shape(&face.shape)?;
            check_opening(face.mask, "pad mask expansion")?;
            check_opening(face.paste, "pad paste expansion")?;
        }
        if let Some(shape) = &p.inner {
            check_pad_shape(shape)?;
        }
        match &p.hole {
            Some(hole) => check_hole(hole),
            None => Ok(()),
        }
    }

    fn check_text(&self, t: &Text) -> Result<(), ModelError> {
        self.check_lettering(t.layer, t.position, t.rotation, t.height, t.stroke)?;
        check_len(&t.text, "text")
    }

    fn check_footprint_text(&self, t: &FootprintText) -> Result<(), ModelError> {
        self.check_lettering(t.layer, t.position, t.rotation, t.height, t.stroke)?;
        match &t.content {
            // Blank words would be a second way to show nothing, next to `visible`.
            TextContent::Fixed(words) => check_name(words, "footprint text"),
            // The footprint's own reference and value are checked with the footprint.
            TextContent::Reference | TextContent::Value => Ok(()),
        }
    }

    /// Where and how large a text is drawn, for text on the board and text in a footprint.
    fn check_lettering(&self, layer: LayerId, position: Point, rotation: Rotation, height: Nm, stroke: Nm) -> Result<(), ModelError> {
        if self.stack.get(layer).is_none() {
            return Err(ModelError::BadLayer("text", "existing"));
        }
        check_point(position, "text position")?;
        check_rotation(rotation, "text rotation")?;
        check_size(height, "text height", false)?;
        check_size(stroke, "text stroke", false)
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
    // A point alone is no outline; the smallest one is a single arc that is a full circle.
    if c.edges.is_empty() {
        return Err(ModelError::BadArea(what, "an outline needs an edge"));
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

/// An expansion may be negative (the opening is smaller than the copper), within the design range.
fn check_opening(o: Opening, what: &'static str) -> Result<(), ModelError> {
    match o {
        Opening::Open { by: Some(by) } if by.0.unsigned_abs() > MAX_ABS.unsigned_abs() => Err(ModelError::OutOfRange(what)),
        _ => Ok(()),
    }
}

/// The copper of a poured zone: a bounded number of areas and points, every ring with at least
/// the three corners an area needs, and every point within the design range.
fn check_poured(filled: &[Polygon]) -> Result<(), ModelError> {
    if filled.len() > MAX_ZONE_POLYGONS {
        return Err(ModelError::TooMany("zone fill areas"));
    }
    let mut points = 0_usize;
    for ring in filled.iter().flat_map(|area| std::iter::once(&area.outer).chain(&area.holes)) {
        if ring.len() < 3 {
            return Err(ModelError::BadArea("zone fill", items::FEW_CORNERS));
        }
        points = points.saturating_add(ring.len());
        if points > MAX_POINTS {
            return Err(ModelError::TooMany("zone fill points"));
        }
        ring.iter().try_for_each(|&p| check_point(p, "zone fill"))?;
    }
    Ok(())
}

fn check_board_hole(h: &BoardHole) -> Result<(), ModelError> {
    check_point(h.position, "hole position")?;
    check_hole(&h.hole)
}

/// Expansions may be negative; sizes are positive, and a via's drill is smaller than its copper,
/// as every via must be.
pub(crate) fn check_design(d: &DesignSettings) -> Result<(), ModelError> {
    let max = MAX_ABS.unsigned_abs();
    if d.mask_expansion.0.unsigned_abs() > max {
        return Err(ModelError::OutOfRange("solder mask expansion"));
    }
    if d.paste_expansion.0.unsigned_abs() > max {
        return Err(ModelError::OutOfRange("paste expansion"));
    }
    check_size(d.clearance, "clearance", true)?;
    check_size(d.track_width, "track width", false)?;
    check_size(d.via_diameter, "via diameter", false)?;
    check_size(d.via_drill, "via drill", false)?;
    if d.via_drill >= d.via_diameter {
        return Err(ModelError::BadSize("via drill (smaller than its diameter)"));
    }
    check_point(d.origin, "origin")
}

fn check_hole(h: &Hole) -> Result<(), ModelError> {
    check_size(h.diameter, "hole diameter", false)?;
    if let Some(slot) = h.slot {
        check_size(slot.length, "slot length", false)?;
        // A slot no longer than it is wide is a round hole, and a slot turned half a turn is the
        // same slot: each has one way to be written.
        if slot.length <= h.diameter {
            return Err(ModelError::BadSize("slot length (longer than the slot is wide)"));
        }
        if !(0..180_000).contains(&slot.rotation.0) {
            return Err(ModelError::OutOfRange("slot rotation (below half a turn)"));
        }
    }
    Ok(())
}

/// A rotation is within one turn, as [`Rotation`] promises; a file can hold any number.
fn check_rotation(r: Rotation, what: &'static str) -> Result<(), ModelError> {
    if (0..360_000).contains(&r.0) { Ok(()) } else { Err(ModelError::OutOfRange(what)) }
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
        assert_eq!(b.add_track(TOP_COPPER, Nm(1), path, Some(NetId::from_u128(99))), Err(ModelError::UnknownNet("track")));
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
        b.remove(ItemId::from_u128(gnd.as_u128())).expect("remove net");
        assert_eq!(b.tracks.get(t).map(|t| t.net), Some(None));
        b.remove(t).expect("remove track");
        assert_eq!(b.remove(t), Err(ModelError::UnknownItem(t)));
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
        // A via filed under the track's id: two items, one id.
        let mut twice = b.clone();
        let track_id = b.tracks.iter().next().map(|t| t.id).expect("track");
        let via = Via {
            id: track_id,
            position: p(0, 0),
            diameter: Nm(600),
            drill: Nm(300),
            from: TOP_COPPER,
            to: BOTTOM_COPPER,
            top_mask: Opening::Rules,
            bottom_mask: Opening::Rules,
            net: None,
        };
        twice.vias.insert(via.clone()).expect("room");
        assert_eq!(twice.validate(), Err(ModelError::BadId(track_id.to_string())));
        let mut nil = b.clone();
        nil.vias.insert(Via { id: ItemId::from_u128(0), ..via }).expect("room");
        assert!(matches!(nil.validate(), Err(ModelError::BadId(_))));
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
        let new = ItemId::from_u128(50);
        if let Some(track) = b.tracks.get_mut(t) {
            track.id = new;
        }
        assert!(b.tracks.get(new).is_none(), "still filed under the old id");
        assert_eq!(b.validate(), Err(ModelError::BadId(t.to_string())));
        // A save and reload files it under its new id, which then validates.
        let back: Board = serde_json::from_str(&serde_json::to_string(&b).expect("json")).expect("parse");
        assert!(back.tracks.get(new).is_some() && back.tracks.get(t).is_none());
        assert_eq!(back.validate(), Ok(()));
        // Removal goes by the key it is filed under, not the changed id.
        assert_eq!(b.remove(new), Err(ModelError::UnknownItem(new)));
        assert_eq!(b.remove(t), Ok(()));
        assert_eq!(b.validate(), Ok(()));
    }

    #[test]
    fn round_rect_corners_must_fit() {
        let rr = |r: i64| PadShape::RoundRect { width: Nm(100), height: Nm(60), radius: Nm(r) };
        assert_eq!(check_pad_shape(&rr(30)), Ok(()));
        assert!(matches!(check_pad_shape(&rr(31)), Err(ModelError::BadSize(_))));
        assert!(matches!(check_pad_shape(&rr(1_000)), Err(ModelError::BadSize(_))));
    }

    fn rect(width: i64, height: i64) -> PadShape {
        PadShape::Rect { width: Nm(width), height: Nm(height) }
    }

    /// A board with one footprint that holds `pads`.
    fn board_with_pads(pads: Vec<Pad>) -> Board {
        let mut b = Board::new("x");
        let footprint = Footprint {
            id: ItemId::from_u128(1),
            reference: "J1".to_owned(),
            value: String::new(),
            link: None,
            assembly: Assembly { mounting: Mounting::ThroughHole, in_bom: true, in_placement: true, fitted: true },
            position: p(0, 0),
            rotation: Rotation(0),
            side: Side::Top,
            pads,
            graphics: Vec::new(),
            texts: Vec::new(),
        };
        b.footprints.insert(footprint).expect("room");
        b
    }

    #[test]
    fn pads_hold_slots_bare_holes_and_copper_per_layer() {
        let circle = PadShape::Circle { diameter: Nm(2 * MM) };
        // A connector shell pin: an oval pad around a plated slot that runs along the pad's y axis.
        let slot = Hole { slot: Some(Slot { length: Nm(1_700_000), rotation: Rotation(90_000) }), ..Hole::round(Nm(600_000), true) };
        let shell = Pad::through_hole("S1", p(0, 0), PadShape::Oval { width: Nm(MM), height: Nm(2_100_000) }, slot);
        // A mounting hole: no copper, no number, not plated.
        let mounting = Pad::bare_hole(p(5 * MM, 0), Hole::round(Nm(3 * MM), false));
        // Copper that differs per layer around a hole off centre, with its own mask and paste openings.
        let mut stacked = Pad::through_hole("1", p(10 * MM, 0), rect(3 * MM, 2 * MM), Hole::round(Nm(MM), true));
        stacked.copper_offset = p(MM / 2, 0);
        stacked.inner = Some(circle);
        stacked.back = None;
        if let Some(front) = &mut stacked.front {
            front.mask = Opening::Open { by: Some(Nm(50_000)) };
            front.paste = Opening::Open { by: Some(Nm(-50_000)) };
        }
        let smd = Pad::smd("2", p(15 * MM, 0), rect(MM, MM));
        assert_eq!(smd.front.as_ref().map(|f| (f.mask, f.paste)), Some((Opening::Rules, Opening::Rules)));
        assert!(smd.inner.is_none() && smd.back.is_none() && smd.hole.is_none());

        let b = board_with_pads(vec![shell, mounting, stacked, smd]);
        assert_eq!(b.validate(), Ok(()));
        let first = serde_json::to_string_pretty(&b).expect("json");
        let back: Board = serde_json::from_str(&first).expect("parse");
        assert_eq!(back, b);
        assert_eq!(serde_json::to_string_pretty(&back).expect("json"), first, "re-save must be byte-identical");
    }

    #[test]
    fn validate_refuses_pads_that_cannot_be_made() {
        let check = |pad: Pad| board_with_pads(vec![pad]).validate();
        let around = |hole: Hole| Pad::through_hole("1", p(0, 0), rect(2 * MM, 2 * MM), hole);
        let slot = |length: i64| Hole { slot: Some(Slot { length: Nm(length), rotation: Rotation(0) }), ..Hole::round(Nm(MM), true) };
        let with_front = |mask: Opening, paste: Opening| {
            let mut pad = Pad::smd("1", p(0, 0), rect(MM, MM));
            if let Some(front) = &mut pad.front {
                (front.mask, front.paste) = (mask, paste);
            }
            pad
        };
        let grown = |by: i64| Opening::Open { by: Some(Nm(by)) };
        let smd = || Pad::smd("7", p(0, 0), rect(MM, MM));
        let bad = |why| Err(ModelError::BadPad("7".to_owned(), why));

        assert_eq!(check(Pad { front: None, ..smd() }), bad("it needs copper or a hole"));
        assert_eq!(check(Pad { position: p(i64::MAX, 0), ..smd() }), Err(ModelError::OutOfRange("pad position")));
        assert_eq!(check(Pad { net: Some(NetId::from_u128(99)), ..smd() }), Err(ModelError::UnknownNet("pad")));
        assert!(matches!(check(Pad { number: " ".to_owned(), ..smd() }), Err(ModelError::BadName(..))), "no number is empty, not blank");
        assert!(matches!(check(Pad { number: "x".repeat(MAX_NAME_CHARS + 1), ..smd() }), Err(ModelError::BadName(..))));

        assert_eq!(check(around(Hole::round(Nm(0), true))), Err(ModelError::BadSize("hole diameter")));
        assert_eq!(check(around(slot(2 * MM))), Ok(()));
        let too_short = Err(ModelError::BadSize("slot length (longer than the slot is wide)"));
        assert_eq!(check(around(slot(MM))), too_short);
        assert_eq!(check(around(slot(MM / 2))), too_short);
        assert_eq!(check(around(slot(-1))), Err(ModelError::BadSize("slot length")));
        assert_eq!(check(around(slot(i64::MAX))), Err(ModelError::BadSize("slot length")));

        // Copper can be off centre only around a hole.
        let off_centre = |pad: Pad| Pad { copper_offset: p(MM, 0), ..pad };
        assert_eq!(check(off_centre(around(Hole::round(Nm(MM), true)))), Ok(()));
        assert_eq!(check(off_centre(smd())), bad("only copper around a hole can be off centre"));
        let mounting = || Pad { number: "7".to_owned(), ..Pad::bare_hole(p(0, 0), Hole::round(Nm(MM), false)) };
        assert_eq!(check(off_centre(mounting())), bad("only copper around a hole can be off centre"));
        let far = Pad { copper_offset: p(i64::MAX, 0), ..around(Hole::round(Nm(MM), true)) };
        assert_eq!(check(far), Err(ModelError::OutOfRange("pad copper offset")));

        assert_eq!(check(with_front(grown(-MM), Opening::Closed)), Ok(()));
        assert_eq!(check(with_front(Opening::Open { by: None }, Opening::Rules)), Ok(()));
        assert_eq!(check(with_front(grown(i64::MIN), Opening::Rules)), Err(ModelError::OutOfRange("pad mask expansion")));
        assert_eq!(check(with_front(Opening::Rules, grown(i64::MAX))), Err(ModelError::OutOfRange("pad paste expansion")));

        // Every layer's shape is checked, not only the front one.
        let mut inner = around(Hole::round(Nm(MM), true));
        inner.inner = Some(PadShape::Circle { diameter: Nm(0) });
        assert_eq!(check(inner), Err(ModelError::BadSize("pad diameter")));
        let mut back = around(Hole::round(Nm(MM), true));
        if let Some(face) = &mut back.back {
            face.shape = rect(MM, -1);
        }
        assert_eq!(check(back), Err(ModelError::BadSize("pad height")));
    }

    #[test]
    fn a_net_needs_copper_or_a_plated_hole() {
        let on_net = |pad: Pad| {
            let mut b = Board::new("x");
            let net = b.add_net("GND").expect("net");
            let mut with_pad = board_with_pads(vec![Pad { net: Some(net), ..pad }]);
            with_pad.nets = b.nets;
            with_pad.validate()
        };
        let bare = |plated: bool| Pad::bare_hole(p(0, 0), Hole::round(Nm(MM), plated));
        assert_eq!(on_net(Pad::smd("1", p(0, 0), rect(MM, MM))), Ok(()));
        assert_eq!(on_net(bare(true)), Ok(()));
        assert_eq!(on_net(bare(false)), Err(ModelError::BadPad(String::new(), "a net needs copper or a plated hole")));
    }

    #[test]
    fn rotations_stay_within_one_turn() {
        let slotted = |rotation: i32| {
            let hole = Hole { slot: Some(Slot { length: Nm(2 * MM), rotation: Rotation(rotation) }), ..Hole::round(Nm(MM), true) };
            board_with_pads(vec![Pad::through_hole("1", p(0, 0), rect(3 * MM, 3 * MM), hole)]).validate()
        };
        assert_eq!(slotted(179_999), Ok(()));
        for outside in [180_000, -1, i32::MAX, i32::MIN] {
            assert_eq!(slotted(outside), Err(ModelError::OutOfRange("slot rotation (below half a turn)")));
        }
        let turned = |rotation: i32| board_with_pads(vec![Pad { rotation: Rotation(rotation), ..Pad::smd("1", p(0, 0), rect(MM, MM)) }]).validate();
        assert_eq!(turned(359_999), Ok(()));
        for outside in [360_000, -1, i32::MAX, i32::MIN] {
            assert_eq!(turned(outside), Err(ModelError::OutOfRange("pad rotation")));
            let mut b = board_with_pads(Vec::new());
            if let Some(f) = b.footprints.get_mut(ItemId::from_u128(1)) {
                f.rotation = Rotation(outside);
            }
            assert_eq!(b.validate(), Err(ModelError::OutOfRange("footprint rotation")));
            let mut b = Board::new("x");
            let text = Text {
                id: ItemId::from_u128(2),
                layer: TOP_SILK,
                text: "R1".to_owned(),
                position: p(0, 0),
                height: Nm(MM),
                stroke: Nm(150_000),
                rotation: Rotation(outside),
                mirrored: false,
                justify: Justify::Center,
            };
            b.texts.insert(text).expect("room");
            assert_eq!(b.validate(), Err(ModelError::OutOfRange("text rotation")));
        }
    }

    /// A visible footprint text on the top silkscreen, one millimetre high.
    fn label(id: u128, content: TextContent) -> FootprintText {
        FootprintText {
            id: ItemId::from_u128(id),
            content,
            visible: true,
            layer: TOP_SILK,
            position: p(0, MM),
            height: Nm(MM),
            stroke: Nm(150_000),
            rotation: Rotation(0),
            mirrored: false,
            justify: Justify::Center,
        }
    }

    fn fixed(text: &str) -> TextContent {
        TextContent::Fixed(text.to_owned())
    }

    /// A board with one footprint (id 1, `J1`) that holds `texts`.
    fn board_with_texts(texts: Vec<FootprintText>) -> Board {
        let mut b = board_with_pads(Vec::new());
        if let Some(f) = b.footprints.get_mut(ItemId::from_u128(1)) {
            f.texts = texts;
        }
        b
    }

    #[test]
    fn footprint_text_shows_the_reference_the_value_or_its_own_words() {
        // The designator on the silkscreen and again on the fabrication drawing, the value hidden.
        let on_fab = FootprintText { layer: layer_ids::TOP_FABRICATION, ..label(3, TextContent::Reference) };
        let value = FootprintText { visible: false, rotation: Rotation(90_000), mirrored: true, ..label(4, TextContent::Value) };
        let mut b = board_with_texts(vec![label(2, TextContent::Reference), on_fab, value, label(5, fixed("PIN 1"))]);
        let id = ItemId::from_u128(1);
        if let Some(f) = b.footprints.get_mut(id) {
            f.value = "USB-C".to_owned();
        }
        let shown = |b: &Board| b.footprints.get(id).map(|f| f.texts.iter().map(|t| f.wording(t).to_owned()).collect::<Vec<_>>());
        assert_eq!(shown(&b), Some(vec!["J1".to_owned(), "J1".to_owned(), "USB-C".to_owned(), "PIN 1".to_owned()]));
        // Renaming the footprint renames it everywhere it is shown: no text holds a copy.
        if let Some(f) = b.footprints.get_mut(id) {
            f.reference = "J2".to_owned();
        }
        assert_eq!(shown(&b), Some(vec!["J2".to_owned(), "J2".to_owned(), "USB-C".to_owned(), "PIN 1".to_owned()]));

        assert_eq!(b.validate(), Ok(()));
        let first = serde_json::to_string_pretty(&b).expect("json");
        let back: Board = serde_json::from_str(&first).expect("parse");
        assert_eq!(back, b);
        assert_eq!(serde_json::to_string_pretty(&back).expect("json"), first, "re-save must be byte-identical");
    }

    #[test]
    fn validate_refuses_footprint_text_that_cannot_be_drawn() {
        let check = |text: FootprintText| board_with_texts(vec![text]).validate();
        let reference = || label(2, TextContent::Reference);
        assert_eq!(check(reference()), Ok(()));
        assert_eq!(check(FootprintText { layer: LayerId(999), ..reference() }), Err(ModelError::BadLayer("text", "existing")));
        assert_eq!(check(FootprintText { position: p(0, i64::MIN), ..reference() }), Err(ModelError::OutOfRange("text position")));
        assert_eq!(check(FootprintText { height: Nm(0), ..reference() }), Err(ModelError::BadSize("text height")));
        assert_eq!(check(FootprintText { stroke: Nm(-1), ..reference() }), Err(ModelError::BadSize("text stroke")));
        for outside in [360_000, -1, i32::MAX, i32::MIN] {
            assert_eq!(check(FootprintText { rotation: Rotation(outside), ..reference() }), Err(ModelError::OutOfRange("text rotation")));
        }
        assert_eq!(check(label(2, fixed(&"x".repeat(MAX_NAME_CHARS)))), Ok(()));
        for words in [&"x".repeat(MAX_NAME_CHARS + 1), "", " "] {
            assert!(matches!(check(label(2, fixed(words))), Err(ModelError::BadName(..))), "{words:?}");
        }
        // A hidden text is checked like a visible one: showing it must not break the board.
        assert_eq!(check(FootprintText { visible: false, height: Nm(0), ..reference() }), Err(ModelError::BadSize("text height")));
    }

    #[test]
    fn validate_refuses_board_text_that_cannot_be_drawn() {
        let check = |text: Text| {
            let mut b = Board::new("x");
            b.texts.insert(text).expect("room");
            b.validate()
        };
        let note = || Text {
            id: ItemId::from_u128(2),
            layer: TOP_SILK,
            text: "REV A".to_owned(),
            position: p(0, 0),
            height: Nm(MM),
            stroke: Nm(150_000),
            rotation: Rotation(0),
            mirrored: false,
            justify: Justify::Center,
        };
        assert_eq!(check(note()), Ok(()));
        assert_eq!(check(Text { layer: LayerId(999), ..note() }), Err(ModelError::BadLayer("text", "existing")));
        assert_eq!(check(Text { position: p(i64::MAX, 0), ..note() }), Err(ModelError::OutOfRange("text position")));
        assert_eq!(check(Text { height: Nm(0), ..note() }), Err(ModelError::BadSize("text height")));
        assert_eq!(check(Text { stroke: Nm(0), ..note() }), Err(ModelError::BadSize("text stroke")));
        // Height and stroke are each checked as themselves.
        assert_eq!(check(Text { height: Nm(1), stroke: Nm(-1), ..note() }), Err(ModelError::BadSize("text stroke")));
        assert_eq!(check(Text { text: "x".repeat(MAX_NAME_CHARS), ..note() }), Ok(()));
        assert!(matches!(check(Text { text: "x".repeat(MAX_NAME_CHARS + 1), ..note() }), Err(ModelError::BadName(..))));
    }

    #[test]
    fn footprint_texts_share_the_ids_of_the_board() {
        let id = |n: u128| Err(ModelError::BadId(ItemId::from_u128(n).to_string()));
        // The footprint's own id, another text's id and the nil id are all taken.
        assert_eq!(board_with_texts(vec![label(1, TextContent::Reference)]).validate(), id(1));
        assert_eq!(board_with_texts(vec![label(2, TextContent::Reference), label(2, TextContent::Value)]).validate(), id(2));
        assert_eq!(board_with_texts(vec![label(0, TextContent::Reference)]).validate(), id(0));
        // So is the id of an item elsewhere on the board.
        let mut b = board_with_texts(vec![label(2, TextContent::Reference)]);
        let track = b.add_track(TOP_COPPER, Nm(1), line(p(0, 0), p(1, 0)), None).expect("track");
        if let Some(t) = b.footprints.get_mut(ItemId::from_u128(1)).and_then(|f| f.texts.first_mut()) {
            t.id = track;
        }
        assert_eq!(b.validate(), Err(ModelError::BadId(track.to_string())));
    }

    #[test]
    fn a_footprint_holds_a_bounded_number_of_texts() {
        let texts = |n: usize| (0..n).map(|i| label(2 + i as u128, TextContent::Reference)).collect::<Vec<_>>();
        let full = board_with_texts(texts(MAX_FOOTPRINT_TEXTS));
        assert_eq!(full.validate(), Ok(()));
        let json = serde_json::to_string(&full).expect("json");
        assert_eq!(serde_json::from_str::<Board>(&json).ok().as_ref(), Some(&full));
        let over = board_with_texts(texts(MAX_FOOTPRINT_TEXTS + 1));
        assert_eq!(over.validate(), Err(ModelError::TooMany("footprint texts")));
        // A file is refused while it is read, before the extra text takes any memory.
        let err = serde_json::from_str::<Board>(&serde_json::to_string(&over).expect("json")).err().map(|e| e.to_string());
        assert!(err.is_some_and(|e| e.contains(&format!("more than {MAX_FOOTPRINT_TEXTS}"))));
    }

    #[test]
    fn a_library_link_names_a_footprint() {
        let linked = |library: &str, footprint: &str| {
            let mut b = board_with_texts(Vec::new());
            if let Some(f) = b.footprints.get_mut(ItemId::from_u128(1)) {
                f.link = Some(LibraryLink { library: library.to_owned(), footprint: footprint.to_owned() });
            }
            b.validate()
        };
        assert_eq!(linked("Connectors", "USB-C-16P"), Ok(()));
        assert_eq!(linked("", "USB-C-16P"), Ok(()), "a source that names the footprint alone");
        let long = "x".repeat(MAX_NAME_CHARS + 1);
        for (library, footprint) in [(" ", "USB-C-16P"), (long.as_str(), "USB-C-16P"), ("Connectors", ""), ("Connectors", " "), ("Connectors", &long)]
        {
            assert!(matches!(linked(library, footprint), Err(ModelError::BadName(..))), "{library:?} {footprint:?}");
        }
    }

    #[test]
    fn assembly_attributes_are_written_in_words() {
        let mut b = board_with_texts(Vec::new());
        let id = ItemId::from_u128(1);
        for (mounting, word) in [(Mounting::SurfaceMount, "surface_mount"), (Mounting::ThroughHole, "through_hole"), (Mounting::Other, "other")] {
            // As a fiducial has them: nothing to buy, yet the placement machine needs its position.
            let assembly = Assembly { mounting, in_bom: false, in_placement: true, fitted: true };
            if let Some(f) = b.footprints.get_mut(id) {
                f.assembly = assembly;
            }
            assert_eq!(b.validate(), Ok(()));
            let json = serde_json::to_value(&b).expect("json");
            let written = json.pointer("/footprints/0/assembly").cloned();
            assert_eq!(written, Some(serde_json::json!({ "mounting": word, "in_bom": false, "in_placement": true, "fitted": true })));
            let back: Board = serde_json::from_value(json).expect("parse");
            assert_eq!(back.footprints.get(id).map(|f| f.assembly), Some(assembly));
        }
        // A footprint must say how it is mounted, in one of the three words.
        let mut json = serde_json::to_value(&b).expect("json");
        if let Some(m) = json.pointer_mut("/footprints/0/assembly/mounting") {
            *m = serde_json::json!("glued");
        }
        assert!(serde_json::from_value::<Board>(json.clone()).is_err());
        if let Some(a) = json.pointer_mut("/footprints/0/assembly").and_then(|a| a.as_object_mut()) {
            a.remove("mounting");
        }
        assert!(serde_json::from_value::<Board>(json).is_err());
    }

    #[test]
    fn a_via_is_tented_per_side() {
        let mut b = Board::new("x");
        let v = b.add_via(p(0, 0), Nm(600_000), Nm(300_000), None).expect("via");
        assert_eq!(b.vias.get(v).map(|v| (v.top_mask, v.bottom_mask)), Some((Opening::Rules, Opening::Rules)));
        if let Some(via) = b.vias.get_mut(v) {
            via.top_mask = Opening::Closed;
            via.bottom_mask = Opening::Open { by: None };
        }
        assert_eq!(b.validate(), Ok(()));
        let too_far = Opening::Open { by: Some(Nm(i64::MIN)) };
        for (top, bottom) in [(too_far, Opening::Rules), (Opening::Rules, too_far)] {
            if let Some(via) = b.vias.get_mut(v) {
                (via.top_mask, via.bottom_mask) = (top, bottom);
            }
            assert_eq!(b.validate(), Err(ModelError::OutOfRange("via mask expansion")));
        }
    }

    fn square(x: i64, y: i64, side: i64) -> Contour {
        Contour::rect(circuitcraft_geom::Rect::from_corners(p(x, y), p(x + side, y + side)))
    }

    /// A solid ground zone on the top copper: 20 mm square, not poured.
    fn zone(b: &mut Board) -> Zone {
        let net = match b.net_by_name("GND") {
            Some(n) => n.id,
            None => b.add_net("GND").expect("net"),
        };
        Zone {
            id: ItemId::from_u128(7),
            layer: TOP_COPPER,
            net: Some(net),
            outline: square(0, 0, 20 * MM),
            holes: Vec::new(),
            clearance: Nm(200_000),
            min_width: Nm(150_000),
            priority: 0,
            fill: ZoneFill::Solid,
            pads: PadConnection::Relief { gap: Nm(250_000), spoke: Nm(300_000) },
            keep_islands: false,
            filled: Vec::new(),
        }
    }

    /// The board with `change` made to its one zone, validated.
    fn with_zone(change: impl FnOnce(&mut Zone)) -> Result<(), ModelError> {
        let mut b = Board::new("x");
        let mut z = zone(&mut b);
        change(&mut z);
        b.zones.insert(z).expect("room");
        b.validate()
    }

    #[test]
    fn a_zone_holds_its_holes_its_settings_and_its_poured_copper() {
        let mut b = Board::new("x");
        let mut z = zone(&mut b);
        z.holes = vec![square(5 * MM, 5 * MM, MM)];
        z.fill = ZoneFill::Hatched { width: Nm(200_000), gap: Nm(400_000) };
        // Poured: one area with a hole in it.
        let ring = |x: i64, side: i64| vec![p(x, x), p(x + side, x), p(x + side, x + side), p(x, x + side)];
        z.filled = vec![Polygon { outer: ring(0, 20 * MM), holes: vec![ring(5 * MM, MM)] }];
        b.zones.insert(z).expect("room");
        assert_eq!(b.validate(), Ok(()));
        let first = serde_json::to_string_pretty(&b).expect("json");
        let back: Board = serde_json::from_str(&first).expect("parse");
        assert_eq!(back, b);
        assert_eq!(serde_json::to_string_pretty(&back).expect("json"), first, "re-save must be byte-identical");
        // Each choice is written in a word, or under one.
        let json = serde_json::to_value(&b).expect("json");
        assert_eq!(json.pointer("/zones/0/fill"), Some(&serde_json::json!({ "hatched": { "width": 200_000, "gap": 400_000 } })));
        assert_eq!(json.pointer("/zones/0/pads"), Some(&serde_json::json!({ "relief": { "gap": 250_000, "spoke": 300_000 } })));
        for (pads, word) in [(PadConnection::Solid, "solid"), (PadConnection::None, "none")] {
            assert_eq!(serde_json::to_value(pads).expect("json"), serde_json::json!(word));
        }
        assert_eq!(serde_json::to_value(ZoneFill::Solid).expect("json"), serde_json::json!("solid"));
    }

    #[test]
    fn validate_refuses_zones_that_cannot_be_poured() {
        assert_eq!(with_zone(|_| {}), Ok(()));
        assert_eq!(with_zone(|z| z.layer = TOP_SILK), Err(ModelError::BadLayer("zone", "copper")));
        assert_eq!(with_zone(|z| z.net = Some(NetId::from_u128(99))), Err(ModelError::UnknownNet("zone")));
        assert_eq!(with_zone(|z| z.outline.start = p(i64::MAX, 0)), Err(ModelError::OutOfRange("zone outline")));
        assert_eq!(with_zone(|z| z.holes = vec![square(i64::MIN, 0, 1)]), Err(ModelError::OutOfRange("zone hole")));
        assert_eq!(with_zone(|z| z.holes = vec![square(0, 0, 1); MAX_ZONE_HOLES]), Ok(()));
        assert_eq!(with_zone(|z| z.holes = vec![square(0, 0, 1); MAX_ZONE_HOLES + 1]), Err(ModelError::TooMany("zone holes")));
        assert_eq!(with_zone(|z| z.clearance = Nm(-1)), Err(ModelError::BadSize("zone clearance")));
        assert_eq!(with_zone(|z| z.min_width = Nm(0)), Ok(()));
        assert_eq!(with_zone(|z| z.min_width = Nm(-1)), Err(ModelError::BadSize("zone minimum width")));
        let hatched = |width: i64, gap: i64| with_zone(|z| z.fill = ZoneFill::Hatched { width: Nm(width), gap: Nm(gap) });
        assert_eq!(hatched(1, 1), Ok(()));
        assert_eq!(hatched(0, 1), Err(ModelError::BadSize("zone hatch width")));
        assert_eq!(hatched(1, 0), Err(ModelError::BadSize("zone hatch gap")));
        let relief = |gap: i64, spoke: i64| with_zone(|z| z.pads = PadConnection::Relief { gap: Nm(gap), spoke: Nm(spoke) });
        assert_eq!(relief(1, 1), Ok(()));
        assert_eq!(relief(0, 1), Err(ModelError::BadSize("thermal relief gap")));
        assert_eq!(relief(1, i64::MAX), Err(ModelError::BadSize("thermal relief spoke")));
    }

    #[test]
    fn validate_refuses_poured_copper_that_is_no_area() {
        let triangle = || vec![p(0, 0), p(MM, 0), p(0, MM)];
        let poured = |filled: Vec<Polygon>| with_zone(|z| z.filled = filled);
        assert_eq!(poured(vec![Polygon::new(triangle())]), Ok(()));
        let corners = Err(ModelError::BadArea("zone fill", "an area or a hole in one needs at least three corners"));
        assert_eq!(poured(vec![Polygon::new(vec![p(0, 0), p(MM, 0)])]), corners);
        assert_eq!(poured(vec![Polygon::default()]), corners);
        assert_eq!(poured(vec![Polygon { outer: triangle(), holes: vec![Vec::new()] }]), corners);
        assert_eq!(poured(vec![Polygon::new(vec![p(0, 0), p(MM, 0), p(0, i64::MAX)])]), Err(ModelError::OutOfRange("zone fill")));
        assert_eq!(
            poured(vec![Polygon { outer: triangle(), holes: vec![vec![p(0, 0), p(1, 0), p(i64::MIN, 1)]] }]),
            Err(ModelError::OutOfRange("zone fill"))
        );
        // The points of every area and hole count against one limit.
        let big = || Polygon { outer: triangle(), holes: vec![vec![p(0, 0); MAX_POINTS / 2]] };
        assert_eq!(poured(vec![big(), big()]), Err(ModelError::TooMany("zone fill points")));
        assert_eq!(poured(vec![Polygon::new(triangle()); MAX_ZONE_POLYGONS + 1]), Err(ModelError::TooMany("zone fill areas")));
        // A ring without three corners is refused as it is read, so a file of empty rings can't
        // fill the memory before the board is checked: the second hole here is never reached.
        let mut b = Board::new("x");
        let z = zone(&mut b);
        b.zones.insert(z).expect("room");
        let json = serde_json::to_value(&b).expect("json");
        let read = |filled: &str| {
            let mut json = json.clone();
            if let Some(f) = json.pointer_mut("/zones/0/filled") {
                *f = serde_json::from_str(filled).expect("json");
            }
            serde_json::from_value::<Board>(json).map(|_| ()).map_err(|e| e.to_string())
        };
        let ring = r#"[{"x":0,"y":0},{"x":9,"y":0},{"x":0,"y":9}]"#;
        assert_eq!(read(&format!(r#"[{{"outer":{ring},"holes":[{ring}]}}]"#)), Ok(()));
        for few in [r#"[{"outer":[],"holes":[]}]"#.to_owned(), format!(r#"[{{"outer":{ring},"holes":[[],7]}}]"#)] {
            assert_eq!(read(&few), Err(items::FEW_CORNERS.to_owned()), "{few}");
        }
        assert!(read(&format!(r#"[{{"outer":{ring},"holes":[],"extra":1}}]"#)).is_err_and(|e| e.contains("unknown field")));
        // A file is refused while it is read, before the extra area takes any memory.
        let mut b = Board::new("x");
        let mut z = zone(&mut b);
        z.filled = vec![Polygon::new(triangle()); MAX_ZONE_POLYGONS + 1];
        b.zones.insert(z).expect("room");
        let err = serde_json::from_str::<Board>(&serde_json::to_string(&b).expect("json")).err().map(|e| e.to_string());
        assert!(err.is_some_and(|e| e.contains(&format!("more than {MAX_ZONE_POLYGONS}"))));
    }

    fn keepout(id: u128) -> Keepout {
        Keepout { id: ItemId::from_u128(id), layer: BOTTOM_COPPER, outline: square(0, 0, 5 * MM), tracks: false, vias: true, zones: false }
    }

    #[test]
    fn a_keepout_says_what_it_keeps_out_of_which_copper_layer() {
        let check = |k: Keepout| {
            let mut b = Board::new("x");
            b.keepouts.insert(k).expect("room");
            b.validate()
        };
        // Vias only, on the bottom layer: more than a drawing on a keepout layer can say.
        assert_eq!(check(keepout(2)), Ok(()));
        assert_eq!(check(Keepout { vias: false, zones: true, ..keepout(2) }), Ok(()));
        assert_eq!(check(Keepout { vias: false, ..keepout(2) }), Err(ModelError::BadArea("keepout", "it must keep tracks, vias or zones out")));
        assert_eq!(check(Keepout { layer: layer_ids::KEEPOUT, ..keepout(2) }), Err(ModelError::BadLayer("keepout", "copper")));
        assert_eq!(check(Keepout { outline: square(i64::MAX, 0, 0), ..keepout(2) }), Err(ModelError::OutOfRange("keepout outline")));
        assert_eq!(check(keepout(0)), Err(ModelError::BadId(ItemId::from_u128(0).to_string())));
    }

    #[test]
    fn a_hole_can_be_on_the_board_without_a_footprint() {
        let mut b = Board::new("x");
        let h = b.add_hole(p(3 * MM, 3 * MM), Hole::round(Nm(3_200_000), false)).expect("hole");
        let slot = Hole { slot: Some(Slot { length: Nm(4 * MM), rotation: Rotation(90_000) }), ..Hole::round(Nm(MM), true) };
        let s = b.add_hole(p(10 * MM, 3 * MM), slot).expect("slot");
        assert_eq!(b.holes.get(h).map(|h| (h.position, h.hole.diameter, h.hole.plated)), Some((p(3 * MM, 3 * MM), Nm(3_200_000), false)));
        assert_eq!(b.validate(), Ok(()));
        let back: Board = serde_json::from_str(&serde_json::to_string(&b).expect("json")).expect("parse");
        assert_eq!(back, b);
        // A hole is checked like the hole of a pad, and nothing is added when it is refused.
        assert_eq!(b.add_hole(p(0, 0), Hole::round(Nm(0), false)), Err(ModelError::BadSize("hole diameter")));
        assert_eq!(b.add_hole(p(i64::MAX, 0), Hole::round(Nm(MM), false)), Err(ModelError::OutOfRange("hole position")));
        let short = Hole { slot: Some(Slot { length: Nm(MM), rotation: Rotation(0) }), ..Hole::round(Nm(MM), false) };
        assert!(matches!(b.add_hole(p(0, 0), short), Err(ModelError::BadSize(_))));
        assert_eq!(b.holes.len(), 2);
        // A hole or a keepout filed under an id that is no longer its own is refused.
        let mut misfiled = b.clone();
        if let Some(hole) = misfiled.holes.get_mut(h) {
            hole.id = ItemId::from_u128(77);
        }
        assert_eq!(misfiled.validate(), Err(ModelError::BadId(h.to_string())));
        let mut misfiled = b.clone();
        misfiled.keepouts.insert(keepout(78)).expect("room");
        if let Some(k) = misfiled.keepouts.get_mut(ItemId::from_u128(78)) {
            k.id = ItemId::from_u128(77);
        }
        assert_eq!(misfiled.validate(), Err(ModelError::BadId(ItemId::from_u128(78).to_string())));
        // Holes and keepouts share the ids of the board, and are removed like any item.
        b.keepouts.insert(keepout(h.as_u128())).expect("room");
        assert_eq!(b.validate(), Err(ModelError::BadId(h.to_string())));
        b.keepouts.remove(h);
        let k = b.fresh_id().expect("id");
        b.keepouts.insert(Keepout { id: k, ..keepout(1) }).expect("room");
        assert_eq!(b.validate(), Ok(()));
        for id in [h, s, k] {
            assert_eq!(b.remove(id), Ok(()));
            assert_eq!(b.remove(id), Err(ModelError::UnknownItem(id)));
        }
        assert!(b.holes.is_empty() && b.keepouts.is_empty());
    }

    #[test]
    fn design_settings_decide_what_items_leave_to_them() {
        let d = DesignSettings { mask_expansion: Nm(50_000), paste_expansion: Nm(-25_000), tent_vias: true, ..DesignSettings::default() };
        let own = Opening::Open { by: Some(Nm(7)) };
        let shared = Opening::Open { by: None };
        assert_eq!([Opening::Rules, shared, own, Opening::Closed].map(|o| d.pad_mask(o)), [Some(Nm(50_000)), Some(Nm(50_000)), Some(Nm(7)), None]);
        assert_eq!([Opening::Rules, shared, own, Opening::Closed].map(|o| d.pad_paste(o)), [Some(Nm(-25_000)), Some(Nm(-25_000)), Some(Nm(7)), None]);
        // A via that leaves it to the settings is tented, or open by the mask expansion.
        assert_eq!([Opening::Rules, shared, own, Opening::Closed].map(|o| d.via_mask(o)), [None, Some(Nm(50_000)), Some(Nm(7)), None]);
        let open = DesignSettings { tent_vias: false, ..d };
        assert_eq!(open.via_mask(Opening::Rules), Some(Nm(50_000)));
    }

    #[test]
    fn validate_refuses_design_settings_nothing_could_be_made_to() {
        let check = |design: DesignSettings| Board { design, ..Board::new("x") }.validate();
        let d = DesignSettings::default;
        assert_eq!(check(d()), Ok(()));
        assert_eq!(check(DesignSettings { mask_expansion: Nm(-MM), paste_expansion: Nm(MM), clearance: Nm(0), ..d() }), Ok(()));
        assert_eq!(check(DesignSettings { mask_expansion: Nm(i64::MIN), ..d() }), Err(ModelError::OutOfRange("solder mask expansion")));
        assert_eq!(check(DesignSettings { paste_expansion: Nm(i64::MAX), ..d() }), Err(ModelError::OutOfRange("paste expansion")));
        assert_eq!(check(DesignSettings { clearance: Nm(-1), ..d() }), Err(ModelError::BadSize("clearance")));
        assert_eq!(check(DesignSettings { track_width: Nm(0), ..d() }), Err(ModelError::BadSize("track width")));
        assert_eq!(check(DesignSettings { via_diameter: Nm(0), ..d() }), Err(ModelError::BadSize("via diameter")));
        assert_eq!(check(DesignSettings { via_drill: Nm(0), ..d() }), Err(ModelError::BadSize("via drill")));
        let same = DesignSettings { via_drill: d().via_diameter, ..d() };
        assert_eq!(check(same), Err(ModelError::BadSize("via drill (smaller than its diameter)")));
        assert_eq!(check(DesignSettings { origin: p(0, i64::MIN), ..d() }), Err(ModelError::OutOfRange("origin")));
    }

    #[test]
    fn a_file_writes_each_kind_under_its_name_and_every_field() {
        let mut b = board_with_pads(vec![Pad::smd("1", p(0, 0), rect(MM, MM))]);
        b.add_track(TOP_COPPER, Nm(1), line(p(0, 0), p(1, 0)), None).expect("track");
        let json = serde_json::to_value(&b).expect("json");
        assert_eq!(json.pointer("/tracks/0/path"), Some(&serde_json::json!({ "line": { "from": { "x": 0, "y": 0 }, "to": { "x": 1, "y": 0 } } })));
        assert_eq!(json.pointer("/footprints/0/pads/0/front/shape"), Some(&serde_json::json!({ "rect": { "width": MM, "height": MM } })));
        assert_eq!(json.pointer("/footprints/0/pads/0/front/mask"), Some(&serde_json::json!("rules")));
        assert_eq!(serde_json::to_value(Opening::Open { by: None }).expect("json"), serde_json::json!({ "open": { "by": null } }));
        assert_eq!(serde_json::to_value(Opening::Closed).expect("json"), serde_json::json!("closed"));
        assert_eq!(serde_json::from_value::<Board>(json.clone()).ok().as_ref(), Some(&b));
        // The kind an object used to be tagged with is a field no object has.
        assert!(serde_json::from_str::<TrackPath>(r#"{"kind":"line","from":{"x":0,"y":0},"to":{"x":1,"y":0}}"#).is_err());
        assert!(serde_json::from_str::<Opening>(r#"{"open":{}}"#).is_err(), "`by` must be written");
    }

    /// A board that holds one of everything a file can say, in every kind it comes in.
    fn everything() -> Board {
        let mut b = Board::new("Everything");
        let gnd = b.add_net("GND").expect("net");
        let id = |b: &Board| b.fresh_id().expect("id");
        let arc = circuitcraft_geom::Arc::new(p(0, 0), p(MM, MM), p(2 * MM, 0));
        b.outline = Some(square(0, 0, 50 * MM));
        b.cutouts = vec![Contour::circle(p(25 * MM, 25 * MM), Nm(MM)).expect("circle")];
        b.add_track(TOP_COPPER, Nm(250_000), line(p(0, 0), p(MM, 0)), Some(gnd)).expect("track");
        b.add_track(BOTTOM_COPPER, Nm(250_000), TrackPath::Arc { arc }, None).expect("track");
        b.add_via(p(MM, 0), Nm(600_000), Nm(300_000), Some(gnd)).expect("via");
        b.add_hole(p(3 * MM, 3 * MM), Hole::round(Nm(3 * MM), false)).expect("hole");
        let mut z = zone(&mut b);
        z.holes = vec![square(5 * MM, 5 * MM, MM)];
        z.fill = ZoneFill::Hatched { width: Nm(200_000), gap: Nm(400_000) };
        z.filled = vec![Polygon { outer: vec![p(0, 0), p(MM, 0), p(0, MM)], holes: vec![vec![p(1, 1), p(9, 1), p(1, 9)]] }];
        b.zones.insert(Zone { id: id(&b), ..z }).expect("room");
        b.keepouts.insert(Keepout { id: id(&b), ..keepout(1) }).expect("room");
        let note = label(1, fixed("REV A"));
        let text = Text {
            id: id(&b),
            layer: TOP_SILK,
            text: "REV A".to_owned(),
            position: note.position,
            height: note.height,
            stroke: note.stroke,
            rotation: Rotation(0),
            mirrored: false,
            justify: Justify::Left,
        };
        b.texts.insert(text).expect("room");
        let shapes = || {
            [
                GraphicShape::Line { from: p(0, 0), to: p(MM, 0) },
                GraphicShape::Arc { arc },
                GraphicShape::Circle { center: p(0, 0), radius: Nm(MM) },
                GraphicShape::Polygon { outline: square(0, 0, MM), filled: true },
            ]
        };
        for shape in shapes() {
            b.graphics.insert(Graphic { id: id(&b), layer: TOP_SILK, width: Nm(100_000), shape }).expect("room");
        }
        // A footprint with a pad of every shape, a slot, openings of every kind, and every text.
        let slot = Hole { slot: Some(Slot { length: Nm(2 * MM), rotation: Rotation(0) }), ..Hole::round(Nm(MM), true) };
        let mut openings = Pad::smd("1", p(0, 0), rect(MM, MM));
        openings.net = Some(gnd);
        if let Some(front) = &mut openings.front {
            (front.mask, front.paste) = (Opening::Open { by: Some(Nm(50_000)) }, Opening::Open { by: None });
        }
        let mut closed = Pad::through_hole("2", p(5 * MM, 0), PadShape::Oval { width: Nm(2 * MM), height: Nm(3 * MM) }, slot);
        closed.inner = Some(PadShape::Circle { diameter: Nm(2 * MM) });
        let pads = vec![
            openings,
            closed,
            Pad::smd("3", p(10 * MM, 0), PadShape::RoundRect { width: Nm(MM), height: Nm(MM), radius: Nm(100_000) }),
            Pad::smd("4", p(15 * MM, 0), PadShape::Custom { outline: square(0, 0, MM) }),
        ];
        let footprint = Footprint {
            id: id(&b),
            reference: "J1".to_owned(),
            value: "USB-C".to_owned(),
            link: Some(LibraryLink { library: "Connectors".to_owned(), footprint: "USB-C-16P".to_owned() }),
            assembly: Assembly { mounting: Mounting::SurfaceMount, in_bom: true, in_placement: true, fitted: true },
            position: p(20 * MM, 20 * MM),
            rotation: Rotation(90_000),
            side: Side::Bottom,
            pads,
            graphics: shapes().into_iter().map(|shape| Graphic { id: id(&b), layer: TOP_SILK, width: Nm(100_000), shape }).collect(),
            texts: [TextContent::Reference, TextContent::Value, fixed("PIN 1")]
                .into_iter()
                .map(|c| FootprintText { id: id(&b), ..label(1, c) })
                .collect(),
        };
        b.footprints.insert(footprint).expect("room");
        b
    }

    /// Every object in `json`: where it is (as a JSON pointer) and the keys it has.
    fn objects(json: &serde_json::Value, at: String, found: &mut Vec<(String, Vec<String>)>) {
        match json {
            serde_json::Value::Object(o) => {
                found.push((at.clone(), o.keys().cloned().collect()));
                o.iter().for_each(|(key, v)| objects(v, format!("{at}/{key}"), found));
            }
            serde_json::Value::Array(a) => a.iter().enumerate().for_each(|(i, v)| objects(v, format!("{at}/{i}"), found)),
            _ => {}
        }
    }

    #[test]
    fn every_field_in_a_file_must_be_there_and_no_other_may_be() {
        let b = everything();
        assert_eq!(b.validate(), Ok(()));
        let json = serde_json::to_value(&b).expect("json");
        assert_eq!(serde_json::from_value::<Board>(json.clone()).ok().as_ref(), Some(&b));
        let mut found = Vec::new();
        objects(&json, String::new(), &mut found);
        // The board really holds every kind: each is written somewhere, under its name.
        let kinds = [
            "line",
            "arc",
            "circle",
            "polygon",
            "rect",
            "round_rect",
            "oval",
            "custom",
            "open",
            "hatched",
            "relief",
            "fixed",
            "slot",
            "link",
            "outline",
            "design",
            "hole",
            "front",
            "back",
            "assembly",
        ];
        for kind in kinds {
            assert!(found.iter().any(|(_, keys)| keys.iter().any(|k| k == kind)), "the board has no {kind}");
        }
        let reads = |change: &dyn Fn(&mut serde_json::Map<String, serde_json::Value>), at: &str| {
            let mut json = json.clone();
            change(json.pointer_mut(at).and_then(|v| v.as_object_mut()).expect("an object"));
            serde_json::from_value::<Board>(json).is_ok()
        };
        // A field that may be empty is written as null: left out, the file is refused. A field
        // this build doesn't know is refused too; reading on would drop it at the next save.
        for (at, keys) in &found {
            for key in keys {
                assert!(!reads(&|o| drop(o.remove(key)), at), "{at} reads without `{key}`");
            }
            assert!(!reads(&|o| drop(o.insert("extra".to_owned(), serde_json::json!(1))), at), "{at} reads with a field nobody knows");
        }
        assert!(found.len() > 150, "{} objects", found.len());
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
