//! KiCad board files (`.kicad_pcb`), read-only.
//!
//! Written from KiCad's published description of the format and from real boards, for the files
//! KiCad 5 to 9 write. The reader keeps what the model holds and says in the notes what it left
//! out. What a file says in KiCad's terms becomes the model's:
//!
//! - y grows downward in the file and upward in the model; angles keep their sense on screen.
//! - A footprint on the bottom is written already flipped; the model holds every footprint as
//!   seen from the top and flips it itself, so its coordinates and layers are flipped back.
//! - A pad's and a footprint text's angle is written as it ends up on the board; the model holds
//!   it relative to the footprint.
//! - Drawings on `Edge.Cuts` become the board outline when they close into one.

use crate::import::{self, Ids, ImportError, Imported, Layers, Notes, Standard};
use crate::sexp::{Node, Reader};
use circuitcraft_geom::{Arc, Contour, MAX_POINTS, Polygon};
use circuitcraft_model::{
    Assembly, Board, Footprint, FootprintText, Graphic, GraphicShape, Hole, Justify, Keepout, LayerId, LayerKind, LibraryLink, MAX_NAME_CHARS,
    MAX_ZONE_POLYGONS, Mounting, Net, NetId, Opening, Pad, PadConnection, PadFace, PadShape, Rotation, Side, Slot, Text, TextContent, Track,
    TrackPath, Via, Zone, ZoneFill,
};
use circuitcraft_units::{Nm, Point};
use std::collections::BTreeMap;

/// What a board's zones and keepouts may add up to, all layers together: each copy of an outline
/// costs its corners plus [`ZONE_COST`].
const MAX_ZONE_COST: usize = 4 * MAX_POINTS;

/// What a zone or keepout costs besides its corners: about what the item itself takes, counted
/// in corners.
const ZONE_COST: usize = 64;

/// The newest file version this reader was written against (KiCad 9).
const NEWEST_KNOWN: f64 = 20_241_229.0;

/// Reads a KiCad board. `name` becomes the board's name (a file has none of its own).
pub fn read_board(text: &str, name: &str) -> Result<Imported, ImportError> {
    let (mut reader, head) = Reader::open(text)?;
    if head != "kicad_pcb" {
        return Err(ImportError::WrongFormat("a KiCad board"));
    }
    let mut kicad = Kicad::new(text, name);
    while let Some(item) = reader.next_item()? {
        kicad.item(&item)?;
    }
    kicad.layers()?;
    import::finish(kicad.board, kicad.notes)
}

/// How coordinates in the file become the model's: y always turns over, and x too inside a
/// footprint on the bottom side, which the file holds flipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Frame {
    mirror: bool,
}

const BOARD: Frame = Frame { mirror: false };

impl Frame {
    /// The point `(x, y)` millimetres of the file.
    fn point(self, x: f64, y: f64) -> Option<Point> {
        let x = Nm::from_mm(if self.mirror { -x } else { x }).ok()?;
        Some(Point::new(x, Nm::from_mm(-y).ok()?))
    }

    /// The two numbers of `node` from its `i`-th argument on, as a point.
    fn at(self, node: &Node, i: usize) -> Option<Point> {
        self.point(node.number(i)?, node.number(i + 1)?)
    }

    /// The point in the list of `parent` that starts with `name`.
    fn child(self, parent: &Node, name: &str) -> Option<Point> {
        self.at(parent.child(name)?, 0)
    }

    /// The model's counter-clockwise sweep for a sweep the file writes clockwise.
    fn sweep(self, clockwise: f64) -> f64 {
        if self.mirror { clockwise } else { -clockwise }
    }
}

/// A length in millimetres, if it is in the design range.
fn length(value: Option<f64>) -> Option<Nm> {
    Nm::from_mm(value?).ok()
}

/// The reader's state: the board so far and what the file's names and numbers stand for.
struct Kicad {
    ids: Ids,
    notes: Notes,
    board: Board,
    /// Layer names as the file writes them, once the layers are known.
    names: BTreeMap<String, LayerId>,
    standard: Option<Standard>,
    thickness: Nm,
    nets: BTreeMap<i64, NetId>,
    net_names: BTreeMap<String, NetId>,
    /// What the zones and keepouts read so far add up to ([`MAX_ZONE_COST`]).
    zone_cost: usize,
}

/// What a footprint's file entry says about where it is, needed to read what it holds.
struct Placement {
    frame: Frame,
    /// The footprint's angle on the board, in degrees.
    angle: f64,
    mask_margin: Option<Nm>,
    paste_margin: Option<Nm>,
}

impl Placement {
    /// The model's rotation, relative to the footprint, of something the file gives an angle on
    /// the board.
    fn relative(&self, on_board: f64) -> Rotation {
        let turn = if self.frame.mirror { self.angle - on_board } else { on_board - self.angle };
        import::degrees(turn).unwrap_or_default()
    }
}

impl Kicad {
    fn new(text: &str, name: &str) -> Kicad {
        let name = if name.trim().is_empty() { "Imported" } else { name };
        Kicad {
            ids: Ids::new(text),
            notes: Notes::default(),
            board: Board::new(&name.chars().take(MAX_NAME_CHARS).collect::<String>()),
            names: BTreeMap::new(),
            standard: None,
            thickness: Nm(1_600_000),
            nets: BTreeMap::new(),
            net_names: BTreeMap::new(),
            zone_cost: 0,
        }
    }

    /// One item of the file.
    fn item(&mut self, node: &Node) -> Result<(), ImportError> {
        let Some(kind) = node.head() else { return Ok(()) };
        match kind {
            "version" if node.number(0).is_some_and(|v| v > NEWEST_KNOWN) => {
                self.notes.add("written by a newer KiCad than this reader knows; read as far as it is understood");
            }
            "general" => self.thickness = length(node.child("thickness").and_then(|t| t.number(0))).filter(|t| t.0 > 0).unwrap_or(self.thickness),
            "layers" => self.read_layers(node)?,
            "setup" => self.setup(node),
            "net" => self.net(node),
            "footprint" | "module" => self.with_layers(node, Kicad::footprint)?,
            "segment" | "arc" => self.with_layers(node, Kicad::track)?,
            "via" => self.with_layers(node, Kicad::via)?,
            "zone" => self.with_layers(node, Kicad::zone)?,
            "gr_text" => self.with_layers(node, Kicad::text)?,
            "gr_line" | "gr_arc" | "gr_circle" | "gr_rect" | "gr_poly" | "gr_curve" => self.with_layers(node, Kicad::drawing)?,
            "dimension" | "target" | "image" | "gr_text_box" | "net_class" | "group" | "generated" | "table" => {
                self.notes.add(&format!("{kind} left out"));
            }
            _ => {}
        }
        Ok(())
    }

    /// Reads an item that sits on layers; a file that lists none before its first item gets the
    /// two-layer set. An item that can't be read is left out with a note.
    fn with_layers(&mut self, node: &Node, read: fn(&mut Kicad, &Node) -> Option<()>) -> Result<(), ImportError> {
        self.layers()?;
        if read(self, node).is_none() {
            self.notes.add(&format!("{} that could not be read left out", node.head().unwrap_or("item")));
        }
        Ok(())
    }

    /// The standard layers, made from the file's list or, without one, for two copper layers.
    fn layers(&mut self) -> Result<Standard, ImportError> {
        if let Some(standard) = &self.standard {
            return Ok(standard.clone());
        }
        self.build_layers(&["F.Cu", "B.Cu"])
    }

    fn read_layers(&mut self, node: &Node) -> Result<(), ImportError> {
        if self.standard.is_some() {
            return Err(ImportError::unsupported("the file lists its layers after the items on them"));
        }
        // `(0 "F.Cu" signal)`: the name is the second word.
        let listed: Vec<&str> = node.args().iter().filter_map(|layer| layer.text(0)).collect();
        self.build_layers(&listed).map(|_| ())
    }

    fn build_layers(&mut self, listed: &[&str]) -> Result<Standard, ImportError> {
        let inner = |name: &str| name.strip_prefix("In").and_then(|n| n.strip_suffix(".Cu")).and_then(|n| n.parse::<usize>().ok());
        let inner_count = listed.iter().filter_map(|n| inner(n)).max().unwrap_or(0);
        let single = !listed.contains(&"B.Cu") && inner_count == 0;
        let mut layers = Layers::default();
        let standard = layers.standard(if single { 1 } else { inner_count.saturating_add(2) }, self.thickness);
        let mut names = BTreeMap::new();
        let sides = [
            ("Paste", standard.paste),
            ("SilkS", standard.silk),
            ("Mask", standard.mask),
            ("CrtYd", standard.courtyard),
            ("Fab", standard.fabrication),
        ];
        for (name, [top, bottom]) in sides {
            names.insert(format!("F.{name}"), top);
            names.insert(format!("B.{name}"), bottom);
        }
        names.insert("Edge.Cuts".to_owned(), standard.outline);
        for (i, &id) in standard.copper.iter().enumerate() {
            let name = match i {
                0 => "F.Cu".to_owned(),
                _ if i + 1 == standard.copper.len() => "B.Cu".to_owned(),
                _ => format!("In{i}.Cu"),
            };
            names.insert(name, id);
        }
        // A board with one copper layer has it on the top; the file may still name the bottom.
        if let Some(&only) = standard.copper.last() {
            names.entry("B.Cu".to_owned()).or_insert(only);
        }
        let full = || ImportError::unsupported(format!("the file lists more than {} layers", circuitcraft_model::MAX_LAYERS));
        if listed.iter().any(|n| matches!(*n, "F.Adhes" | "B.Adhes")) {
            let [top, bottom] = layers.pair("Adhesive", LayerKind::Mechanical).ok_or_else(full)?;
            names.insert("F.Adhes".to_owned(), top);
            names.insert("B.Adhes".to_owned(), bottom);
        }
        // Every other layer the file lists holds drawings that are not manufactured.
        for name in listed {
            if !names.contains_key(*name) && inner(name).is_none() {
                let id = layers.single(name, LayerKind::Mechanical).ok_or_else(full)?;
                names.insert((*name).to_owned(), id);
            }
        }
        self.board.stack = layers.finish();
        self.names = names;
        self.standard = Some(standard.clone());
        Ok(standard)
    }

    /// The layer a file's name stands for.
    fn layer_named(&self, name: &str) -> Option<LayerId> {
        self.names.get(name).copied()
    }

    /// The layer in `(layer NAME)` of `node`.
    fn layer_of(&self, node: &Node) -> Option<LayerId> {
        self.layer_named(node.child("layer")?.text(0)?)
    }

    /// The copper layers a zone's, keepout's or via's `layer` or `layers` list names, from the
    /// top down: copper layers are numbered in that order.
    fn copper_of(&self, node: &Node) -> Vec<LayerId> {
        let copper = self.standard.as_ref().map(|s| s.copper.clone()).unwrap_or_default();
        let named = node.child("layers").or_else(|| node.child("layer"));
        let mut found = Vec::new();
        for name in named.map(Node::args).unwrap_or_default().iter().filter_map(Node::word) {
            match name {
                "*.Cu" => found.extend(copper.iter().copied()),
                "F&B.Cu" => found.extend(copper.first().into_iter().chain(copper.last()).copied()),
                _ => found.extend(self.layer_named(name).filter(|id| copper.contains(id))),
            }
        }
        found.sort_unstable();
        found.dedup();
        found
    }

    /// The board's settings, from the ones the file's `setup` holds.
    fn setup(&mut self, node: &Node) {
        let value = |name: &str| length(node.child(name).and_then(|n| n.number(0)));
        let design = &mut self.board.design;
        if let Some(by) = value("pad_to_mask_clearance") {
            design.mask_expansion = by;
        }
        if let Some(by) = value("pad_to_paste_clearance") {
            design.paste_expansion = by;
        }
        if let Some(origin) = BOARD.child(node, "aux_axis_origin") {
            design.origin = origin;
        }
        if let Some(width) = value("last_trace_width").filter(|w| w.0 > 0) {
            design.track_width = width;
        }
        if let Some(clearance) = value("trace_clearance").filter(|c| c.0 >= 0) {
            design.clearance = clearance;
        }
        if let (Some(size), Some(drill)) = (value("via_size"), value("via_drill"))
            && drill.0 > 0
            && drill < size
        {
            (design.via_diameter, design.via_drill) = (size, drill);
        }
        // Up to KiCad 7 the plot settings say whether vias are left open in the solder mask;
        // from KiCad 8 the board says which sides are tented.
        let plotted = node.child("pcbplotparams").and_then(|p| p.child("viasonmask")).and_then(|v| v.text(0));
        design.tent_vias = match node.child("tenting") {
            Some(tenting) => tenting.flag("front") || tenting.flag("back"),
            None => plotted != Some("true"),
        };
    }

    /// `(net 3 "GND")`: a net and the number the rest of the file knows it by.
    fn net(&mut self, node: &Node) {
        let (Some(number), Some(name)) = (node.number(0), node.text(1)) else { return };
        if name.trim().is_empty() {
            return; // net 0, "no net"
        }
        let id = self.net_by_name(name);
        self.nets.insert(number as i64, id);
    }

    fn net_by_name(&mut self, name: &str) -> NetId {
        let name: String = name.chars().take(MAX_NAME_CHARS).collect();
        if let Some(&id) = self.net_names.get(&name) {
            return id;
        }
        let id = self.ids.net();
        // Ids never repeat and the collection has room for more nets than a file can name.
        if self.board.nets.insert(Net { id, name: name.clone() }).is_err() {
            self.notes.add("net beyond the most a board can hold left out");
        }
        self.net_names.insert(name, id);
        id
    }

    /// The net in `(net 3)`, `(net 3 "GND")` or `(net "GND")` of `node`; none for net 0.
    fn net_of(&mut self, node: &Node) -> Option<NetId> {
        let net = node.child("net")?;
        match net.number(0) {
            Some(number) => {
                let found = self.nets.get(&(number as i64)).copied();
                if found.is_none() && number != 0.0 {
                    self.notes.add("connection to a net the file doesn't list dropped");
                }
                found
            }
            None => net.text(0).filter(|name| !name.trim().is_empty()).map(|name| self.net_by_name(name)),
        }
    }

    /// `(segment …)` or `(arc …)`: a track.
    fn track(&mut self, node: &Node) -> Option<()> {
        let (from, to) = (BOARD.child(node, "start")?, BOARD.child(node, "end")?);
        let path = match BOARD.child(node, "mid") {
            Some(mid) => TrackPath::Arc { arc: Arc::new(from, mid, to) },
            None => TrackPath::Line { from, to },
        };
        let width = length(node.child("width")?.number(0)).filter(|w| w.0 > 0)?;
        let layer = self.layer_of(node).filter(|&id| self.board.stack.is_copper(id))?;
        let net = self.net_of(node);
        self.board.tracks.insert(Track { id: self.ids.item(), layer, width, path, net }).ok().map(|_| ())
    }

    fn via(&mut self, node: &Node) -> Option<()> {
        let position = BOARD.child(node, "at")?;
        let diameter = length(node.child("size")?.number(0))?;
        let drill = length(node.child("drill")?.number(0)).filter(|d| d.0 > 0 && *d < diameter)?;
        // The layers come back from the top down, whichever the file names first.
        let ends = self.copper_of(node);
        let (from, to) = (*ends.first()?, *ends.last()?);
        let tented = |side: &str| match node.child("tenting") {
            Some(tenting) if tenting.flag(side) => Opening::Closed,
            _ => Opening::Rules,
        };
        let net = self.net_of(node);
        let via = Via { id: self.ids.item(), position, diameter, drill, from, to, top_mask: tented("front"), bottom_mask: tented("back"), net };
        self.board.vias.insert(via).ok().map(|_| ())
    }

    /// `(gr_text "…" …)`: text on the board.
    fn text(&mut self, node: &Node) -> Option<()> {
        let at = node.child("at")?;
        let lettering = Lettering::of(node)?;
        if lettering.raised {
            self.notes.add(import::RAISED_TEXT);
        }
        let text = Text {
            id: self.ids.item(),
            layer: self.layer_of(node)?,
            text: node.text(0)?.chars().take(MAX_NAME_CHARS).collect(),
            position: BOARD.at(at, 0)?,
            height: lettering.height,
            stroke: lettering.stroke,
            rotation: import::degrees(at.number(2).unwrap_or(0.0)).ok()?,
            mirrored: lettering.mirror,
            justify: lettering.justify,
        };
        self.board.texts.insert(text).ok().map(|_| ())
    }

    /// `(gr_line …)` and the other drawings on the board.
    fn drawing(&mut self, node: &Node) -> Option<()> {
        let layer = self.layer_of(node)?;
        for (width, shape) in self.shapes(node, BOARD)? {
            self.board.graphics.insert(Graphic { id: self.ids.item(), layer, width, shape }).ok()?;
        }
        Some(())
    }

    /// What a `gr_…` or `fp_…` drawing draws: one shape, or the short lines of a curve.
    fn shapes(&mut self, node: &Node, frame: Frame) -> Option<Vec<(Nm, GraphicShape)>> {
        let kind = node.head()?;
        let stroke = node.child("stroke").and_then(|s| s.child("width")).or_else(|| node.child("width"));
        let width = length(stroke.and_then(|w| w.number(0))).filter(|w| w.0 > 0).unwrap_or(Nm(0));
        // KiCad 5 polygons are always filled; later files say.
        let filled = match node.child("fill").and_then(|f| f.text(0)) {
            Some(fill) => matches!(fill, "solid" | "yes"),
            None => kind.ends_with("_poly"),
        };
        let shape = match kind.get(3..)? {
            "line" => GraphicShape::Line { from: frame.child(node, "start")?, to: frame.child(node, "end")? },
            "arc" => GraphicShape::Arc { arc: arc(node, frame)? },
            "circle" => {
                let (center, on) = (frame.child(node, "center")?, frame.child(node, "end")?);
                let radius = Nm::from_f64_nm((on.x.0 as f64 - center.x.0 as f64).hypot(on.y.0 as f64 - center.y.0 as f64)).ok()?;
                match filled {
                    true => GraphicShape::Polygon { outline: import::disc(center, radius)?, filled },
                    false if radius.0 > 0 => GraphicShape::Circle { center, radius },
                    false => return None,
                }
            }
            "rect" => {
                let (a, b) = (frame.child(node, "start")?, frame.child(node, "end")?);
                let corners = vec![a, Point::new(b.x, a.y), b, Point::new(a.x, b.y)];
                GraphicShape::Polygon { outline: import::polygon(corners)?, filled }
            }
            "poly" => GraphicShape::Polygon { outline: import::polygon(self.points(node.child("pts")?, frame)?)?, filled },
            "curve" => {
                let points = self.points(node.child("pts")?, frame)?;
                let &[a, b, c, d] = points.as_slice() else { return None };
                let run = import::bezier([a, b, c, d]);
                return Some(run.windows(2).filter_map(|w| Some((width, GraphicShape::Line { from: *w.first()?, to: *w.get(1)? }))).collect());
            }
            _ => return None,
        };
        Some(vec![(width, shape)])
    }

    /// The corners in `(pts (xy 1 2) …)`. An arc among them is read as its start, middle and end.
    fn points(&mut self, pts: &Node, frame: Frame) -> Option<Vec<Point>> {
        let mut points = Vec::with_capacity(pts.args().len().min(MAX_POINTS));
        for part in pts.args() {
            match part.head() {
                Some("xy") => points.push(frame.at(part, 0)?),
                Some("arc") => {
                    self.notes.add("arc in a polygon read as two straight edges");
                    points.extend([frame.child(part, "start")?, frame.child(part, "mid")?, frame.child(part, "end")?]);
                }
                _ => {}
            }
        }
        Some(points)
    }

    /// `(zone …)`: a copper zone or a keepout, on each of its layers.
    fn zone(&mut self, node: &Node) -> Option<()> {
        let layers = self.copper_of(node);
        if layers.is_empty() {
            self.notes.add("zone on a layer that is not copper left out");
            return Some(());
        }
        let mut outlines = Vec::new();
        for polygon in node.all("polygon") {
            let points = self.points(polygon.child("pts")?, BOARD)?;
            outlines.push(import::polygon(points)?);
        }
        // A zone on several layers becomes a zone on each, with a copy of its outline. Each copy
        // costs its corners and something for itself, so a file can't make the board many times
        // its own size by naming many layers.
        let cost: usize = outlines.iter().map(|o| o.edges.len().saturating_add(ZONE_COST)).sum();
        let with_this = self.zone_cost.saturating_add(cost.saturating_mul(layers.len()));
        if with_this > MAX_ZONE_COST {
            self.notes.add("zone beyond what the zones of one board may add up to left out");
            return Some(());
        }
        self.zone_cost = with_this;
        if let Some(keepout) = node.child("keepout") {
            return self.keepouts(keepout, &layers, outlines);
        }
        let settings = ZoneSettings::of(node, &mut self.notes)?;
        let net = self.net_of(node);
        let only = (layers.len() == 1).then(|| layers.first().copied()).flatten();
        for &layer in &layers {
            let mut filled = self.poured(node, layer, only);
            for outline in &outlines {
                let zone = Zone {
                    id: self.ids.item(),
                    layer,
                    net,
                    outline: outline.clone(),
                    holes: Vec::new(),
                    clearance: settings.clearance,
                    min_width: settings.min_width,
                    priority: settings.priority,
                    fill: settings.fill,
                    pads: settings.pads,
                    keep_islands: settings.keep_islands,
                    // The poured copper belongs to the zone as a whole: the first outline holds it.
                    filled: std::mem::take(&mut filled),
                };
                self.board.zones.insert(zone).ok()?;
            }
        }
        Some(())
    }

    /// `(keepout (tracks not_allowed) …)`: what the area keeps out, on each layer.
    fn keepouts(&mut self, keepout: &Node, layers: &[LayerId], outlines: Vec<Contour>) -> Option<()> {
        let kept_out = |what: &str| keepout.child(what).and_then(|w| w.text(0)) == Some("not_allowed");
        let (tracks, vias, zones) = (kept_out("tracks"), kept_out("vias"), kept_out("copperpour"));
        if kept_out("pads") || kept_out("footprints") {
            self.notes.add("keepout for pads or footprints: only what it says of tracks, vias and zones kept");
        }
        if !(tracks || vias || zones) {
            return Some(());
        }
        for &layer in layers {
            for outline in &outlines {
                let area = Keepout { id: self.ids.item(), layer, outline: outline.clone(), tracks, vias, zones };
                self.board.keepouts.insert(area).ok()?;
            }
        }
        Some(())
    }

    /// The copper a zone was poured with on `layer`. A `filled_polygon` without a layer of its
    /// own (KiCad 5) belongs to the zone's only layer.
    fn poured(&mut self, node: &Node, layer: LayerId, only: Option<LayerId>) -> Vec<Polygon> {
        let mut areas = Vec::new();
        let mut points = 0_usize;
        for filled in node.all("filled_polygon") {
            let on = filled.child("layer").and_then(|l| l.text(0)).and_then(|name| self.layer_named(name)).or(only);
            if on != Some(layer) {
                continue;
            }
            let Some(mut ring) = filled.child("pts").and_then(|pts| self.points(pts, BOARD)) else { continue };
            ring.dedup();
            points = points.saturating_add(ring.len());
            if ring.len() >= 3 {
                areas.push(Polygon::new(ring));
            }
        }
        if points > MAX_POINTS || areas.len() > MAX_ZONE_POLYGONS {
            self.notes.add("poured copper of a zone with more corners than a zone can hold left out");
            areas.clear();
        }
        areas
    }

    /// `(footprint …)`, or `(module …)` as KiCad 5 writes it.
    fn footprint(&mut self, node: &Node) -> Option<()> {
        let standard = self.standard.clone()?;
        let at = node.child("at")?;
        let side = if node.child("layer").and_then(|l| l.text(0)) == Some("B.Cu") { Side::Bottom } else { Side::Top };
        let angle = at.number(2).unwrap_or(0.0);
        let margin = |name: &str| length(node.child(name).and_then(|n| n.number(0)));
        let placement = Placement {
            frame: Frame { mirror: side == Side::Bottom },
            angle,
            mask_margin: margin("solder_mask_margin"),
            paste_margin: margin("solder_paste_margin"),
        };
        let mut footprint = Footprint {
            id: self.ids.item(),
            reference: String::new(),
            value: String::new(),
            link: library_link(node.text(0).unwrap_or_default()),
            assembly: Assembly { mounting: Mounting::Other, in_bom: true, in_placement: true, fitted: true },
            position: BOARD.at(at, 0)?,
            rotation: import::degrees(angle).ok()?,
            side,
            pads: Vec::new(),
            graphics: Vec::new(),
            texts: Vec::new(),
        };
        let mut lifted = Vec::new();
        for part in node.args() {
            self.footprint_part(part, &placement, &standard, &mut footprint, &mut lifted);
        }
        footprint.assembly = assembly(node, &footprint);
        if footprint.reference.trim().is_empty() {
            footprint.reference = "?".to_owned();
        }
        // What a footprint draws on the outline layer is part of the board's edge: it goes on
        // the board, where the footprint puts it.
        for (layer, width, shape) in lifted {
            match import::lift(&footprint, shape) {
                Some(shape) => drop(self.board.graphics.insert(Graphic { id: self.ids.item(), layer, width, shape })),
                None => self.notes.add("footprint drawing on the outline layer that could not be placed left out"),
            }
        }
        self.board.footprints.insert(footprint).ok().map(|_| ())
    }

    /// One thing a footprint holds.
    fn footprint_part(
        &mut self,
        part: &Node,
        placement: &Placement,
        standard: &Standard,
        footprint: &mut Footprint,
        lifted: &mut Vec<(LayerId, Nm, GraphicShape)>,
    ) {
        let Some(kind) = part.head() else { return };
        let read = match kind {
            "pad" => self.pad(part, placement, standard, footprint),
            "fp_text" | "property" => self.footprint_text(part, placement, footprint),
            "fp_line" | "fp_arc" | "fp_circle" | "fp_rect" | "fp_poly" | "fp_curve" => self.footprint_drawing(part, placement, footprint, lifted),
            // A board file writes a footprint's zone where it is on the board, not relative to
            // the footprint (as a KiCad 9 board shows): it is read like any zone of the board.
            "zone" => self.zone(part),
            "model" | "fp_text_box" | "dimension" | "group" | "image" => {
                self.notes.add(&format!("footprint {kind} left out"));
                Some(())
            }
            _ => Some(()),
        };
        if read.is_none() {
            self.notes.add(&format!("footprint {kind} that could not be read left out"));
        }
    }

    /// The layer a footprint's part is on, named as for a footprint on the top side.
    fn footprint_layer(&self, part: &Node, placement: &Placement) -> Option<LayerId> {
        let layer = self.layer_of(part)?;
        if placement.frame.mirror { self.board.stack.flipped(layer) } else { Some(layer) }
    }

    fn footprint_drawing(
        &mut self,
        part: &Node,
        placement: &Placement,
        footprint: &mut Footprint,
        lifted: &mut Vec<(LayerId, Nm, GraphicShape)>,
    ) -> Option<()> {
        let layer = self.footprint_layer(part, placement)?;
        let on_outline = self.board.stack.get(layer).is_some_and(|l| l.kind == LayerKind::Outline);
        for (width, shape) in self.shapes(part, placement.frame)? {
            match on_outline {
                true => lifted.push((layer, width, shape)),
                false => footprint.graphics.push(Graphic { id: self.ids.item(), layer, width, shape }),
            }
        }
        Some(())
    }

    /// `(fp_text reference "R1" …)`, or `(property "Reference" "R1" …)` as KiCad 8 writes it.
    fn footprint_text(&mut self, part: &Node, placement: &Placement, footprint: &mut Footprint) -> Option<()> {
        let words: String = part.text(1)?.chars().take(MAX_NAME_CHARS).collect();
        let content = match (part.head()?, part.text(0)?) {
            ("fp_text", "reference") | ("property", "Reference") => {
                footprint.reference = words;
                TextContent::Reference
            }
            ("fp_text", "value") | ("property", "Value") => {
                footprint.value = words;
                TextContent::Value
            }
            ("fp_text", _) => match words.as_str() {
                "${REFERENCE}" | "%R" => TextContent::Reference,
                "${VALUE}" | "%V" => TextContent::Value,
                _ if words.trim().is_empty() => return Some(()),
                _ => TextContent::Fixed(words),
            },
            // Any other property is a fact about the part (a datasheet, a sheet name), not text
            // on the board.
            _ => {
                self.notes.add("footprint property left out");
                return Some(());
            }
        };
        // A reference or value that is only named, without a place, is not drawn anywhere.
        let Some(at) = part.child("at") else { return Some(()) };
        let lettering = Lettering::of(part)?;
        if lettering.raised {
            self.notes.add(import::RAISED_TEXT);
        }
        footprint.texts.push(FootprintText {
            id: self.ids.item(),
            content,
            visible: !lettering.hidden,
            layer: self.footprint_layer(part, placement)?,
            position: placement.frame.at(at, 0)?,
            height: lettering.height,
            stroke: lettering.stroke,
            rotation: placement.relative(at.number(2).unwrap_or(0.0)),
            // A bottom-side footprint already mirrors what it holds.
            mirrored: lettering.mirror != placement.frame.mirror,
            justify: lettering.justify,
        });
        Some(())
    }

    /// `(pad "1" smd rect …)`: a pad, or, for a pad that is only an opening in the solder mask
    /// or the paste stencil, the drawings the model holds such openings as.
    fn pad(&mut self, part: &Node, placement: &Placement, standard: &Standard, footprint: &mut Footprint) -> Option<()> {
        let kind = part.text(1)?;
        let at = part.child("at")?;
        let position = placement.frame.at(at, 0)?;
        let rotation = placement.relative(at.number(2).unwrap_or(0.0));
        let size = part.child("size")?;
        let (width, height) = (length(size.number(0)).filter(|w| w.0 > 0)?, length(size.number(1)).filter(|h| h.0 > 0)?);
        let shape = self.pad_shape(part, width, height, placement.frame)?;
        let drill = part.child("drill");
        let hole = drill.and_then(|d| hole(d, kind == "thru_hole")).filter(|_| kind != "smd" && kind != "connect");
        // A hole that is not plated, in a pad no larger than the hole, has no copper around it.
        let bare = kind == "np_thru_hole" && hole.is_some_and(|h| width <= h.diameter && height <= h.slot.map_or(h.diameter, |s| s.length));
        let on = PadLayers::of(part, placement.frame.mirror);
        let face = |side: usize| PadFace {
            shape: shape.clone(),
            mask: opening(on.mask[side], length(part.child("solder_mask_margin").and_then(|m| m.number(0))).or(placement.mask_margin)),
            paste: opening(on.paste[side], length(part.child("solder_paste_margin").and_then(|m| m.number(0))).or(placement.paste_margin)),
        };
        let front = (on.copper[0] && !bare).then(|| face(0));
        let back = (on.copper[1] && !bare).then(|| face(1));
        let inner = (on.inner && !bare && self.board.stack.copper_count() > 2).then(|| shape.clone());
        let copper = front.is_some() || back.is_some() || inner.is_some();
        if part.child("solder_paste_margin_ratio").is_some() && copper {
            self.notes.add("paste opening given as a share of the pad's size left to the design settings");
        }
        if !copper && hole.is_none() {
            return self.apertures(&on, &shape, position, rotation, standard, footprint);
        }
        let offset = drill.and_then(|d| d.child("offset")).and_then(|o| placement.frame.at(o, 0));
        let net = self.net_of(part).filter(|_| copper || hole.is_some_and(|h| h.plated));
        footprint.pads.push(Pad {
            number: part.text(0)?.trim().chars().take(MAX_NAME_CHARS).collect(),
            position,
            rotation,
            copper_offset: offset.filter(|_| copper && hole.is_some()).unwrap_or(Point::ORIGIN),
            front,
            inner,
            back,
            hole,
            net,
        });
        Some(())
    }

    /// The copper shape of a pad `width` by `height`.
    fn pad_shape(&mut self, part: &Node, width: Nm, height: Nm, frame: Frame) -> Option<PadShape> {
        let narrow = width.0.min(height.0);
        Some(match part.text(2)? {
            "circle" => PadShape::Circle { diameter: width },
            "oval" if width == height => PadShape::Circle { diameter: width },
            "oval" => PadShape::Oval { width, height },
            "rect" => PadShape::Rect { width, height },
            "roundrect" => {
                let ratio = part.child("roundrect_rratio").and_then(|r| r.number(0)).unwrap_or(0.25).clamp(0.0, 0.5);
                PadShape::RoundRect { width, height, radius: Nm(((narrow as f64) * ratio) as i64).min(Nm(narrow / 2)) }
            }
            "trapezoid" => {
                self.notes.add("trapezoid pad read as a rectangle");
                PadShape::Rect { width, height }
            }
            "custom" => {
                // A custom pad is its anchor and the shapes added to it. One polygon is taken as
                // the whole pad; anything else is read as the anchor alone.
                let primitives = part.child("primitives").map(Node::args).unwrap_or_default();
                let outline = match primitives {
                    [only] if only.head() == Some("gr_poly") => only.child("pts").and_then(|pts| self.points(pts, frame)).and_then(import::polygon),
                    _ => None,
                };
                let round = part.child("options").and_then(|o| o.child("anchor")).and_then(|a| a.text(0)) == Some("circle");
                match outline {
                    Some(outline) => PadShape::Custom { outline },
                    None => {
                        self.notes.add("custom pad shape read as its anchor");
                        if round { PadShape::Circle { diameter: width } } else { PadShape::Rect { width, height } }
                    }
                }
            }
            _ => return None,
        })
    }

    /// A pad without copper or a hole is an opening in the solder mask or the paste stencil: a
    /// filled drawing on that layer.
    fn apertures(
        &mut self,
        on: &PadLayers,
        shape: &PadShape,
        position: Point,
        rotation: Rotation,
        standard: &Standard,
        footprint: &mut Footprint,
    ) -> Option<()> {
        let layers =
            [(on.mask[0], standard.mask[0]), (on.mask[1], standard.mask[1]), (on.paste[0], standard.paste[0]), (on.paste[1], standard.paste[1])];
        let place = |p: Point| {
            let p = rotation.turn(p)?;
            import::in_range(Point::new(Nm(p.x.0.checked_add(position.x.0)?), Nm(p.y.0.checked_add(position.y.0)?)))
        };
        let outline = match shape {
            PadShape::Circle { diameter } => import::disc(position, Nm(diameter.0 / 2))?,
            PadShape::Rect { width, height } | PadShape::RoundRect { width, height, .. } | PadShape::Oval { width, height } => {
                import::rectangle(position, *width, *height, rotation)?
            }
            PadShape::Custom { outline } => import::placed(outline, place)?,
        };
        let mut any = false;
        for (_, layer) in layers.into_iter().filter(|(listed, _)| *listed) {
            any = true;
            let shape = GraphicShape::Polygon { outline: outline.clone(), filled: true };
            footprint.graphics.push(Graphic { id: self.ids.item(), layer, width: Nm(0), shape });
        }
        any.then_some(())
    }
}

/// `(arc …)` as KiCad 6 on writes it (start, middle, end) or as KiCad 5 does (the centre under
/// `start`, the arc's start under `end`, and a clockwise angle).
fn arc(node: &Node, frame: Frame) -> Option<Arc> {
    let (start, end) = (frame.child(node, "start")?, frame.child(node, "end")?);
    match frame.child(node, "mid") {
        Some(mid) => Some(Arc::new(start, mid, end)),
        None => import::center_arc(start, end, frame.sweep(node.child("angle")?.number(0)?)),
    }
}

/// `Library:Name` as a link; none for a footprint without a name.
fn library_link(name: &str) -> Option<LibraryLink> {
    let (library, footprint) = name.split_once(':').unwrap_or(("", name));
    let short = |s: &str| s.trim().chars().take(MAX_NAME_CHARS).collect::<String>();
    (!footprint.trim().is_empty()).then(|| LibraryLink { library: short(library), footprint: short(footprint) })
}

/// What assembly needs to know, from `(attr …)` and, where a file leaves the mounting open, from
/// the pads.
fn assembly(node: &Node, footprint: &Footprint) -> Assembly {
    let attr = node.child("attr");
    let has = |word: &str| attr.is_some_and(|a| a.flag(word));
    let mounting = if has("smd") {
        Mounting::SurfaceMount
    } else if has("through_hole") {
        Mounting::ThroughHole
    } else if has("virtual") || footprint.pads.is_empty() {
        Mounting::Other
    } else if footprint.pads.iter().any(|p| p.hole.is_some_and(|h| h.plated)) {
        Mounting::ThroughHole
    } else {
        Mounting::SurfaceMount
    };
    // KiCad 5's `virtual` is a footprint without a part to buy or place.
    let part = !has("virtual");
    Assembly { mounting, in_bom: part && !has("exclude_from_bom"), in_placement: part && !has("exclude_from_pos_files"), fitted: !has("dnp") }
}

/// `(drill 0.8)`, `(drill oval 0.6 2)`: a round hole or a slot along one of the pad's axes.
fn hole(drill: &Node, plated: bool) -> Option<Hole> {
    let sizes: Vec<Nm> = drill.args().iter().filter_map(|a| length(a.word()?.parse().ok())).filter(|d| d.0 > 0).collect();
    let (&across, along) = (sizes.first()?, sizes.get(1).copied().filter(|_| drill.flag("oval")));
    Some(match along {
        Some(along) if along != across => {
            let rotation = if across > along { Rotation(0) } else { Rotation(90_000) };
            Hole { diameter: across.min(along), slot: Some(Slot { length: across.max(along), rotation }), plated }
        }
        _ => Hole::round(across, plated),
    })
}

/// The opening over copper on a side: none when the pad doesn't list that side's layer, its own
/// margin when it has one, and otherwise what the design settings say.
fn opening(listed: bool, margin: Option<Nm>) -> Opening {
    match (listed, margin) {
        (false, _) => Opening::Closed,
        (true, Some(by)) => Opening::Open { by: Some(by) },
        (true, None) => Opening::Rules,
    }
}

/// Which of a pad's layers the file lists, for the footprint's own side (index 0) and the other
/// side (index 1).
struct PadLayers {
    copper: [bool; 2],
    inner: bool,
    mask: [bool; 2],
    paste: [bool; 2],
}

impl PadLayers {
    fn of(part: &Node, bottom: bool) -> PadLayers {
        let names: Vec<&str> = part.child("layers").map(Node::args).unwrap_or_default().iter().filter_map(Node::word).collect();
        let (own, other) = if bottom { ("B", "F") } else { ("F", "B") };
        let listed = |side: &str, kind: &str| names.iter().any(|n| *n == format!("{side}.{kind}") || *n == format!("*.{kind}"));
        let sides = |kind: &str| [listed(own, kind), listed(other, kind)];
        let both = names.contains(&"F&B.Cu");
        PadLayers {
            copper: sides("Cu").map(|on| on || both),
            inner: names.iter().any(|n| *n == "*.Cu" || (n.starts_with("In") && n.ends_with(".Cu"))),
            mask: sides("Mask"),
            paste: sides("Paste"),
        }
    }
}

/// How text is drawn: `(effects (font (size 1 1) (thickness 0.15)) (justify left mirror))`.
struct Lettering {
    height: Nm,
    stroke: Nm,
    justify: Justify,
    mirror: bool,
    hidden: bool,
    /// Whether the text hangs from its top or stands on its bottom; the model holds only how
    /// text is anchored from side to side.
    raised: bool,
}

impl Lettering {
    fn of(node: &Node) -> Option<Lettering> {
        let effects = node.child("effects");
        let font = effects.and_then(|e| e.child("font"));
        let height = length(font.and_then(|f| f.child("size")).and_then(|s| s.number(0))).filter(|h| h.0 > 0).unwrap_or(Nm(1_000_000));
        // A file may leave the stroke out: an eighth of the height then.
        let stroke =
            length(font.and_then(|f| f.child("thickness")).and_then(|t| t.number(0))).filter(|t| t.0 > 0).unwrap_or(Nm((height.0 / 8).max(1)));
        let justify = effects.and_then(|e| e.child("justify"));
        let side = |word: &str| justify.is_some_and(|j| j.flag(word));
        let hides = |n: &Node| n.flag("hide") || n.child("hide").is_some_and(|h| h.text(0) != Some("no"));
        Some(Lettering {
            height,
            stroke,
            justify: if side("left") {
                Justify::Left
            } else if side("right") {
                Justify::Right
            } else {
                Justify::Center
            },
            mirror: side("mirror"),
            hidden: hides(node) || effects.is_some_and(hides),
            raised: side("top") || side("bottom"),
        })
    }
}

/// What a zone's entry says of how it is poured.
struct ZoneSettings {
    clearance: Nm,
    min_width: Nm,
    priority: u32,
    fill: ZoneFill,
    pads: PadConnection,
    keep_islands: bool,
}

impl ZoneSettings {
    fn of(node: &Node, notes: &mut Notes) -> Option<ZoneSettings> {
        let fill = node.child("fill");
        let of_fill = |name: &str| length(fill.and_then(|f| f.child(name)).and_then(|n| n.number(0))).filter(|v| v.0 > 0);
        let connect = node.child("connect_pads");
        let relief = match (of_fill("thermal_gap"), of_fill("thermal_bridge_width")) {
            (Some(gap), Some(spoke)) => PadConnection::Relief { gap, spoke },
            _ => PadConnection::Solid,
        };
        let pads = match connect.and_then(|c| c.text(0)) {
            Some("yes") => PadConnection::Solid,
            Some("no") => PadConnection::None,
            Some("thru_hole_only") => {
                notes.add("zone that gives thermal reliefs to through-hole pads only: read as reliefs for all pads");
                relief
            }
            _ => relief,
        };
        let hatched = fill.and_then(|f| f.child("mode")).and_then(|m| m.text(0)) == Some("hatch");
        Some(ZoneSettings {
            clearance: length(connect.and_then(|c| c.child("clearance")).and_then(|c| c.number(0))).filter(|c| c.0 >= 0).unwrap_or(Nm(0)),
            min_width: length(node.child("min_thickness").and_then(|m| m.number(0))).filter(|m| m.0 >= 0).unwrap_or(Nm(0)),
            priority: node.child("priority").and_then(|p| p.number(0)).map_or(0, |p| p.clamp(0.0, f64::from(u32::MAX)) as u32),
            fill: match (hatched, of_fill("hatch_thickness"), of_fill("hatch_gap")) {
                (true, Some(width), Some(gap)) => ZoneFill::Hatched { width, gap },
                _ => ZoneFill::Solid,
            },
            pads,
            // Mode 1 keeps every island; modes 0 and 2 remove all of them or the small ones.
            keep_islands: fill.and_then(|f| f.child("island_removal_mode")).and_then(|m| m.number(0)) == Some(1.0),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use circuitcraft_geom::Edge;

    const MM: i64 = 1_000_000;

    /// A point given in micrometres, to keep the tests in whole numbers.
    fn um(x: i64, y: i64) -> Point {
        Point::new(Nm(x * 1_000), Nm(y * 1_000))
    }

    /// A small board as KiCad 7 writes one: a resistor on the top, a connector on the bottom
    /// turned a quarter, an outline with a round cutout, tracks, a via, a zone on both sides and a
    /// keepout.
    pub(crate) const BOARD: &str = r#"(kicad_pcb (version 20221018) (generator pcbnew)
  (general (thickness 1.6))
  (paper "A4")
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal) (36 "B.SilkS" user "B.Silkscreen") (37 "F.SilkS" user "F.Silkscreen")
    (38 "B.Mask" user) (39 "F.Mask" user) (40 "Dwgs.User" user "User.Drawings") (44 "Edge.Cuts" user))
  (setup (pad_to_mask_clearance 0.05) (aux_axis_origin 10 20) (pcbplotparams (viasonmask false)))
  (net 0 "") (net 1 "GND") (net 2 "VCC")
  (footprint "Resistor_SMD:R_0603" (layer "F.Cu") (at 20 10 90) (attr smd)
    (fp_text reference "R1" (at 0 -1.5 90) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
    (fp_text value "10k" (at 0 1.5 90) (layer "F.Fab") hide (effects (font (size 1 1) (thickness 0.15))))
    (fp_text user "${REFERENCE}" (at 0 0 90) (layer "F.Fab") (effects (font (size 0.5 0.5) (thickness 0.08))))
    (fp_line (start -1 -0.5) (end 1 -0.5) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
    (pad "1" smd roundrect (at -0.8 0 90) (size 0.9 1) (layers "F.Cu" "F.Paste" "F.Mask") (roundrect_rratio 0.25) (net 1 "GND"))
    (pad "2" smd rect (at 0.8 0 90) (size 0.9 1) (layers "F.Cu" "F.Mask") (net 2 "VCC") (solder_mask_margin 0.1))
  )
  (footprint "Conn:Pin" (layer "B.Cu") (at 30 10 90) (attr through_hole exclude_from_bom)
    (property "Reference" "J1" (at 0 -2 90) (layer "B.SilkS") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))
    (pad "1" thru_hole oval (at 1 2 90) (size 1.6 2.4) (drill oval 0.8 1.6) (layers "*.Cu" "*.Mask") (net 1 "GND"))
    (pad "" np_thru_hole circle (at -3 0) (size 2 2) (drill 2) (layers "*.Cu" "*.Mask"))
    (pad "" smd rect (at 0 4 90) (size 1 2) (layers "B.Paste"))
    (fp_line (start -2 -2) (end 2 -2) (stroke (width 0.1) (type solid)) (layer "B.SilkS"))
  )
  (gr_line (start 0 0) (end 40 0) (stroke (width 0.1) (type default)) (layer "Edge.Cuts"))
  (gr_line (start 40 0) (end 40 20) (stroke (width 0.1) (type default)) (layer "Edge.Cuts"))
  (gr_line (start 40 20) (end 0 20) (stroke (width 0.1) (type default)) (layer "Edge.Cuts"))
  (gr_line (start 0 20) (end 0 0) (stroke (width 0.1) (type default)) (layer "Edge.Cuts"))
  (gr_circle (center 5 15) (end 6 15) (stroke (width 0.1) (type default)) (fill none) (layer "Edge.Cuts"))
  (gr_rect (start 1 1) (end 3 2) (stroke (width 0.1) (type default)) (fill solid) (layer "Dwgs.User"))
  (gr_text "REV A" (at 5 5 90) (layer "F.SilkS") (effects (font (size 1.5 1.5) (thickness 0.2)) (justify left)))
  (segment (start 20 10.8) (end 32 9) (width 0.25) (layer "F.Cu") (net 1))
  (arc (start 32 9) (mid 33 9.4) (end 33 10.2) (width 0.25) (layer "B.Cu") (net 1))
  (via (at 32 9) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (net 1))
  (zone (net 1) (net_name "GND") (layers "F&B.Cu") (hatch edge 0.5) (priority 2) (connect_pads (clearance 0.3)) (min_thickness 0.2)
    (fill yes (thermal_gap 0.4) (thermal_bridge_width 0.5) (island_removal_mode 1))
    (polygon (pts (xy 1 1) (xy 39 1) (xy 39 19) (xy 1 19)))
    (filled_polygon (layer "F.Cu") (pts (xy 2 2) (xy 38 2) (xy 38 18) (xy 2 18))))
  (zone (net 0) (net_name "") (layer "B.Cu") (hatch edge 0.5) (connect_pads (clearance 0)) (min_thickness 0.2)
    (keepout (tracks allowed) (vias not_allowed) (pads allowed) (copperpour not_allowed) (footprints allowed))
    (fill (thermal_gap 0.5) (thermal_bridge_width 0.5))
    (polygon (pts (xy 5 5) (xy 8 5) (xy 8 8))))
)
"#;

    fn layer(board: &Board, id: LayerId) -> &str {
        board.stack.get(id).map_or("?", |l| l.name.as_str())
    }

    fn footprint<'b>(board: &'b Board, reference: &str) -> &'b Footprint {
        board.footprints.iter().find(|f| f.reference == reference).expect("the footprint")
    }

    #[test]
    fn a_board_comes_in_with_everything_where_the_file_has_it() {
        let Imported { board, notes } = read_board(BOARD, "Demo").expect("import");
        assert_eq!(notes.lines(), Vec::<String>::new(), "nothing left out");
        assert_eq!(board.name, "Demo");
        assert_eq!(board.stack.copper_count(), 2);
        assert_eq!((board.nets.len(), board.footprints.len(), board.tracks.len(), board.vias.len()), (2, 2, 2, 1));
        assert_eq!((board.zones.len(), board.keepouts.len(), board.texts.len(), board.graphics.len()), (2, 1, 1, 1));
        let gnd = board.net_by_name("GND").map(|n| n.id);
        assert!(gnd.is_some());
        // Settings: the mask clearance, the origin (y turned over) and vias covered by mask.
        let d = board.design;
        assert_eq!((d.mask_expansion, d.origin, d.tent_vias), (Nm(50_000), um(10_000, -20_000), true));

        // The outline: the four lines, counter-clockwise from the lower left, and the circle cut out.
        let outline = board.outline.as_ref().expect("outline");
        let corners: Vec<Point> = outline.edges.iter().map(Edge::end).collect();
        assert_eq!((outline.start, corners), (um(0, -20_000), vec![um(40_000, -20_000), um(40_000, 0), um(0, 0), um(0, -20_000)]));
        assert_eq!(board.cutouts.len(), 1);
        // What is left as a drawing: the filled rectangle on the user layer.
        let drawing = board.graphics.iter().next().expect("drawing");
        assert_eq!(layer(&board, drawing.layer), "Dwgs.User");
        assert!(
            matches!(&drawing.shape, GraphicShape::Polygon { outline, filled: true } if outline.start == um(1_000, -1_000) && outline.edges.len() == 3)
        );

        // The resistor: a quarter turn on the top; pad 1 ends up where the track starts.
        let r1 = footprint(&board, "R1");
        assert_eq!((r1.position, r1.rotation, r1.side, r1.value.as_str()), (um(20_000, -10_000), Rotation(90_000), Side::Top, "10k"));
        assert_eq!(r1.link, Some(LibraryLink { library: "Resistor_SMD".to_owned(), footprint: "R_0603".to_owned() }));
        assert_eq!(r1.assembly, Assembly { mounting: Mounting::SurfaceMount, in_bom: true, in_placement: true, fitted: true });
        let [one, two] = r1.pads.as_slice() else { panic!("two pads") };
        assert_eq!((one.number.as_str(), one.position, one.rotation, one.net), ("1", um(-800, 0), Rotation(0), gnd));
        assert_eq!(r1.to_board(one.position), Some(um(20_000, -10_800)));
        let rounded = PadShape::RoundRect { width: Nm(900_000), height: Nm(MM), radius: Nm(225_000) };
        assert_eq!(one.front, Some(PadFace { shape: rounded, mask: Opening::Rules, paste: Opening::Rules }));
        assert!(one.back.is_none() && one.inner.is_none() && one.hole.is_none());
        // Pad 2 lists no paste layer and has a mask margin of its own.
        let plain = PadShape::Rect { width: Nm(900_000), height: Nm(MM) };
        assert_eq!(two.front, Some(PadFace { shape: plain, mask: Opening::Open { by: Some(Nm(100_000)) }, paste: Opening::Closed }));
        assert_eq!(two.net, board.net_by_name("VCC").map(|n| n.id));
        // Its texts: angles are written as on the board and held relative to the footprint.
        let texts: Vec<_> = r1.texts.iter().map(|t| (t.content.clone(), layer(&board, t.layer), t.position, t.rotation, t.visible)).collect();
        assert_eq!(
            texts,
            [
                (TextContent::Reference, "Top Silkscreen", um(0, 1_500), Rotation(0), true),
                (TextContent::Value, "Top Fabrication", um(0, -1_500), Rotation(0), false),
                (TextContent::Reference, "Top Fabrication", um(0, 0), Rotation(0), true),
            ]
        );
        assert_eq!(r1.graphics.len(), 1);
    }

    #[test]
    fn a_footprint_on_the_bottom_is_held_as_seen_from_the_top() {
        let Imported { board, .. } = read_board(BOARD, "Demo").expect("import");
        let gnd = board.net_by_name("GND").map(|n| n.id);
        let j1 = footprint(&board, "J1");
        assert_eq!((j1.position, j1.rotation, j1.side), (um(30_000, -10_000), Rotation(90_000), Side::Bottom));
        assert_eq!(j1.assembly, Assembly { mounting: Mounting::ThroughHole, in_bom: false, in_placement: true, fitted: true });
        let [pin, mounting] = j1.pads.as_slice() else { panic!("two pads") };
        // The file has the pad at (1, 2) of the flipped footprint, which the board puts at
        // (32, 9): where the track ends and the via is.
        assert_eq!((pin.position, pin.rotation, pin.net), (um(-1_000, -2_000), Rotation(0), gnd));
        assert_eq!(j1.to_board(pin.position), Some(um(32_000, -9_000)));
        // A slot along the pad's long side, copper on both outer layers and none inside a
        // two-layer board.
        let oval = PadShape::Oval { width: Nm(1_600_000), height: Nm(2_400_000) };
        let face = PadFace { shape: oval, mask: Opening::Rules, paste: Opening::Closed };
        assert_eq!((pin.front.as_ref(), pin.back.as_ref(), pin.inner.as_ref()), (Some(&face), Some(&face), None));
        assert_eq!(
            pin.hole,
            Some(Hole { diameter: Nm(800_000), slot: Some(Slot { length: Nm(1_600_000), rotation: Rotation(90_000) }), plated: true })
        );
        // A hole that is not plated in a pad no larger than it: a hole alone.
        assert_eq!((mounting.number.as_str(), mounting.hole, mounting.net), ("", Some(Hole::round(Nm(2 * MM), false)), None));
        assert!(mounting.front.is_none() && mounting.back.is_none());
        // Layers are named as for the top side, and the mirrored text is not mirrored again.
        let text = j1.texts.first().expect("the reference");
        assert_eq!((layer(&board, text.layer), text.position, text.mirrored, text.rotation), ("Top Silkscreen", um(0, 2_000), false, Rotation(0)));
        // The line on the bottom silkscreen and the paste opening without copper, as drawings.
        let drawn: Vec<_> = j1.graphics.iter().map(|g| (layer(&board, g.layer), g.width)).collect();
        assert_eq!(drawn, [("Top Paste", Nm(0)), ("Top Silkscreen", Nm(100_000))]);
        assert!(matches!(&j1.graphics[1].shape, GraphicShape::Line { from, to } if (*from, *to) == (um(2_000, 2_000), um(-2_000, 2_000))));
        assert!(matches!(&j1.graphics[0].shape, GraphicShape::Polygon { filled: true, .. }));
    }

    #[test]
    fn tracks_vias_zones_and_keepouts_come_in_on_their_layers() {
        let Imported { board, .. } = read_board(BOARD, "Demo").expect("import");
        let gnd = board.net_by_name("GND").map(|n| n.id);
        let mut tracks: Vec<_> = board.tracks.iter().map(|t| (layer(&board, t.layer), t.width, t.path, t.net)).collect();
        tracks.sort_by_key(|t| t.0);
        let arc = Arc::new(um(32_000, -9_000), um(33_000, -9_400), um(33_000, -10_200));
        assert_eq!(
            tracks,
            [
                ("Bottom Copper", Nm(250_000), TrackPath::Arc { arc }, gnd),
                ("Top Copper", Nm(250_000), TrackPath::Line { from: um(20_000, -10_800), to: um(32_000, -9_000) }, gnd),
            ]
        );
        let via = board.vias.iter().next().expect("via");
        assert_eq!((via.position, via.diameter, via.drill, via.net), (um(32_000, -9_000), Nm(800_000), Nm(400_000), gnd));
        assert_eq!((layer(&board, via.from), layer(&board, via.to), via.top_mask), ("Top Copper", "Bottom Copper", Opening::Rules));
        // One zone for each layer the file's zone is on; the poured copper goes to its layer.
        let mut zones: Vec<_> = board.zones.iter().map(|z| (layer(&board, z.layer), z.filled.len(), z)).collect();
        zones.sort_by_key(|z| z.0);
        assert_eq!(zones.iter().map(|z| (z.0, z.1)).collect::<Vec<_>>(), [("Bottom Copper", 0), ("Top Copper", 1)]);
        let top = zones[1].2;
        assert_eq!((top.net, top.clearance, top.min_width, top.priority, top.keep_islands), (gnd, Nm(300_000), Nm(200_000), 2, true));
        assert_eq!((top.fill, top.pads), (ZoneFill::Solid, PadConnection::Relief { gap: Nm(400_000), spoke: Nm(500_000) }));
        assert_eq!(top.outline.start, um(1_000, -1_000));
        assert_eq!(
            top.filled.first().map(|p| p.outer.clone()),
            Some(vec![um(2_000, -2_000), um(38_000, -2_000), um(38_000, -18_000), um(2_000, -18_000)])
        );
        let keepout = board.keepouts.iter().next().expect("keepout");
        assert_eq!((layer(&board, keepout.layer), keepout.tracks, keepout.vias, keepout.zones), ("Bottom Copper", false, true, true));
        let text = board.texts.iter().next().expect("text");
        assert_eq!((text.text.as_str(), text.position, text.rotation, text.justify), ("REV A", um(5_000, -5_000), Rotation(90_000), Justify::Left));
        assert_eq!((text.height, text.stroke, layer(&board, text.layer)), (Nm(1_500_000), Nm(200_000), "Top Silkscreen"));
    }

    /// The same kind of board as KiCad 5 writes it: bare words, `module`, arcs given by their
    /// centre and a clockwise angle, four copper layers.
    pub(crate) const OLD: &str = r#"(kicad_pcb (version 20171130) (host pcbnew "(5.1.5)")
  (general (thickness 0.8) (drawings 9) (tracks 2))
  (layers (0 F.Cu signal) (1 In1.Cu signal) (2 In2.Cu signal) (31 B.Cu signal) (37 F.SilkS user) (40 Dwgs.User user) (44 Edge.Cuts user))
  (setup (last_trace_width 0.2) (trace_clearance 0.15) (via_size 0.6) (via_drill 0.3) (pad_to_mask_clearance 0.051)
    (pcbplotparams (viasonmask true)))
  (net 0 "") (net 1 +3V3)
  (net_class Default "the default" (clearance 0.127) (add_net +3V3))
  (module Lib:Part (layer F.Cu) (tedit 5B301BBE) (tstamp 5DF910C6) (at 10 10 270)
    (fp_text reference U1 (at 0 0) (layer F.SilkS) (effects (font (size 1 1) (thickness 0.15))))
    (fp_arc (start 0 0) (end 1 0) (angle 90) (layer F.SilkS) (width 0.1))
    (fp_circle (center 0 0) (end 0.5 0) (layer F.SilkS) (width 0.1))
    (fp_poly (pts (xy 0 0) (xy 1 0) (xy 1 1)) (layer F.SilkS) (width 0.1))
    (pad 1 thru_hole circle (at 0 2 270) (size 1.5 1.5) (drill 0.8) (layers *.Cu *.Mask) (net 1 +3V3))
    (pad 2 smd custom (at 0 -2 270) (size 1 1) (layers F.Cu F.Mask) (options (clearance outline) (anchor rect))
      (primitives (gr_poly (pts (xy -1 -1) (xy 1 -1) (xy 0 1)) (width 0))))
    (pad 3 smd trapezoid (at 3 0 270) (size 1 1) (rect_delta 0 0.2) (layers F.Cu))
    (model x.wrl (at (xyz 0 0 0)))
  )
  (gr_line (start 1 0) (end 9 0) (layer Edge.Cuts) (width 0.1))
  (gr_arc (start 9 1) (end 9 0) (angle 90) (layer Edge.Cuts) (width 0.1))
  (gr_line (start 10 1) (end 10 9) (layer Edge.Cuts) (width 0.1))
  (gr_arc (start 9 9) (end 10 9) (angle 90) (layer Edge.Cuts) (width 0.1))
  (gr_line (start 9 10) (end 1 10) (layer Edge.Cuts) (width 0.1))
  (gr_arc (start 1 9) (end 1 10) (angle 90) (layer Edge.Cuts) (width 0.1))
  (gr_line (start 0 9) (end 0 1) (layer Edge.Cuts) (width 0.1))
  (gr_arc (start 1 1) (end 0 1) (angle 90) (layer Edge.Cuts) (width 0.1))
  (segment (start 8 10) (end 20 12) (width 0.2) (layer In1.Cu) (net 1))
  (via (at 20 12) (size 0.6) (drill 0.3) (layers F.Cu B.Cu) (net 1))
  (dimension 5 (width 0.1) (layer Dwgs.User))
  (zone (net 0) (net_name "") (layer F.Mask) (hatch edge 0.5) (connect_pads (clearance 0)) (min_thickness 0.2)
    (fill yes) (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1))))
)
"#;

    #[test]
    fn a_kicad_5_board_reads_the_same_way() {
        let Imported { board, notes } = read_board(OLD, "Old").expect("import");
        assert_eq!(
            notes.lines(),
            [
                "dimension left out",
                "footprint model left out",
                "net_class left out",
                "trapezoid pad read as a rectangle",
                "zone on a layer that is not copper left out"
            ]
        );
        assert_eq!(
            board.stack.copper().map(|l| l.name.as_str()).collect::<Vec<_>>(),
            ["Top Copper", "Inner Copper 1", "Inner Copper 2", "Bottom Copper"]
        );
        let d = board.design;
        assert_eq!(
            (d.track_width, d.clearance, d.via_diameter, d.via_drill, d.mask_expansion, d.tent_vias),
            (Nm(200_000), Nm(150_000), Nm(600_000), Nm(300_000), Nm(51_000), false)
        );
        // The outline: four lines joined by four quarter circles, each arc going the way the
        // file's clockwise angle says.
        let outline = board.outline.as_ref().expect("outline");
        assert_eq!((outline.start, outline.edges.len(), board.graphics.len()), (um(0, -9_000), 8, 0));
        assert_eq!(outline.edges.iter().filter(|e| matches!(e, Edge::Arc { .. })).count(), 4);
        let ring = outline.flatten(Nm(1_000)).expect("flatten");
        let area = circuitcraft_geom::area2(&ring).expect("area") as f64 / 2.0 / 1e12;
        // A 10 mm square less the four corners outside its 1 mm rounding.
        assert!((area - (100.0 - (4.0 - std::f64::consts::PI))).abs() < 0.01, "{area}");

        let u1 = footprint(&board, "U1");
        assert_eq!((u1.rotation, u1.assembly.mounting, u1.pads.len()), (Rotation(270_000), Mounting::ThroughHole, 3));
        let net = board.net_by_name("+3V3").map(|n| n.id);
        // Pad 1 is drilled through four layers: copper inside too. It lands where the track starts.
        let pin = &u1.pads[0];
        assert_eq!((pin.position, pin.rotation, pin.net, pin.inner.is_some()), (um(0, -2_000), Rotation(0), net, true));
        assert_eq!(u1.to_board(pin.position), Some(um(8_000, -10_000)));
        // A custom pad that is one polygon keeps its shape; the trapezoid becomes a rectangle.
        let custom = Contour::polyline(&[um(-1_000, 1_000), um(1_000, 1_000), um(0, -1_000)]);
        assert_eq!(u1.pads[1].front.as_ref().map(|f| &f.shape), custom.map(|outline| PadShape::Custom { outline }).as_ref());
        assert_eq!(u1.pads[2].front.as_ref().map(|f| &f.shape), Some(&PadShape::Rect { width: Nm(MM), height: Nm(MM) }));
        // The arc starts to the right of its centre and turns clockwise: a quarter turn down.
        let shapes: Vec<&GraphicShape> = u1.graphics.iter().map(|g| &g.shape).collect();
        assert!(
            matches!(shapes[0], GraphicShape::Arc { arc } if (arc.start, arc.mid, arc.end) == (um(1_000, 0), Point::new(Nm(707_107), Nm(-707_107)), um(0, -1_000)))
        );
        assert!(matches!(shapes[1], GraphicShape::Circle { radius, .. } if *radius == Nm(500_000)));
        assert!(matches!(shapes[2], GraphicShape::Polygon { filled: true, .. }), "a KiCad 5 polygon is filled");
        let track = board.tracks.iter().next().expect("track");
        assert_eq!((layer(&board, track.layer), track.net), ("Inner Copper 1", net));
    }

    #[test]
    fn what_is_not_a_kicad_board_is_refused() {
        assert_eq!(read_board("(kicad_sch (version 1))", "x"), Err(ImportError::WrongFormat("a KiCad board")));
        assert!(matches!(read_board("(kicad_pcb (version 1)", "x"), Err(ImportError::Syntax { .. })));
        assert!(matches!(read_board("", "x"), Err(ImportError::Syntax { .. })));
        let late = "(kicad_pcb (segment (start 0 0) (end 1 0) (width 0.2) (layer F.Cu) (net 0)) (layers (0 F.Cu signal)))";
        assert!(matches!(read_board(late, "x"), Err(ImportError::Unsupported(_))));
        // A board with nothing in it is still a board, named for its file or, without one, "Imported".
        let empty = read_board("(kicad_pcb (version 20221018))", "  ").expect("import");
        assert_eq!((empty.board.name.as_str(), empty.board.stack.copper_count(), empty.board.outline.is_some()), ("Imported", 2, false));
        // A file from a KiCad newer than this reader knows is read, and says so.
        let newer = read_board("(kicad_pcb (version 20991231))", "x").expect("import");
        assert_eq!(newer.notes.iter().count(), 1);
    }

    #[test]
    fn items_that_make_no_sense_are_left_out_with_a_note() {
        let board = |items: &str| read_board(&format!("(kicad_pcb (version 20221018) (net 1 \"A\") {items})"), "x").expect("import");
        for (item, note) in [
            ("(segment (start 0 0) (end 1 0) (width 0) (layer \"F.Cu\") (net 1))", "segment that could not be read left out"),
            ("(segment (start 0 0) (end 1 0) (width 0.2) (layer \"F.SilkS\"))", "segment that could not be read left out"),
            ("(segment (start 0 0) (end 1e30 0) (width 0.2) (layer \"F.Cu\"))", "segment that could not be read left out"),
            ("(segment (start 0 0) (end nan 0) (width 0.2) (layer \"F.Cu\"))", "segment that could not be read left out"),
            ("(via (at 0 0) (size 0.4) (drill 0.4) (layers \"F.Cu\" \"B.Cu\"))", "via that could not be read left out"),
            ("(via (at 0 0) (size 0.8) (drill 0.4) (layers \"F.SilkS\"))", "via that could not be read left out"),
            ("(gr_line (start 0 0) (end 1 0) (layer \"Nope\"))", "gr_line that could not be read left out"),
            ("(gr_circle (center 0 0) (end 0 0) (layer \"F.SilkS\"))", "gr_circle that could not be read left out"),
            ("(gr_poly (pts (xy 0 0) (xy 1 0)) (layer \"F.SilkS\"))", "gr_poly that could not be read left out"),
            ("(gr_text \"x\" (layer \"F.SilkS\"))", "gr_text that could not be read left out"),
            ("(footprint \"a:b\" (layer \"F.Cu\"))", "footprint that could not be read left out"),
            (
                "(footprint \"a:b\" (layer \"F.Cu\") (at 0 0) (pad \"1\" smd rect (at 0 0) (size 0 1) (layers \"F.Cu\")))",
                "footprint pad that could not be read left out",
            ),
            (
                "(footprint \"a:b\" (layer \"F.Cu\") (at 0 0) (pad \"1\" smd star (at 0 0) (size 1 1) (layers \"F.Cu\")))",
                "footprint pad that could not be read left out",
            ),
            (
                "(footprint \"a:b\" (layer \"F.Cu\") (at 0 0) (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"Dwgs.User\")))",
                "footprint pad that could not be read left out",
            ),
            ("(segment (start 0 0) (end 1 0) (width 0.2) (layer \"F.Cu\") (net 7))", "connection to a net the file doesn't list dropped"),
            ("(zone (net 1) (layer \"F.Cu\") (polygon (pts (xy 0 0) (xy 1 0))))", "zone that could not be read left out"),
        ] {
            let read = board(item);
            assert_eq!(read.notes.lines(), [note], "{item}");
            assert_eq!(read.board.validate(), Ok(()));
        }
        // A footprint without a reference gets a placeholder, and a net named twice is one net.
        let read = board(
            "(net 2 \"A\") (footprint \"\" (layer \"F.Cu\") (at 0 0) (pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\") (net 2 \"A\")))",
        );
        let f = read.board.footprints.iter().next().expect("footprint");
        assert_eq!((f.reference.as_str(), f.link.as_ref(), read.board.nets.len()), ("?", None, 1));
        assert_eq!(f.pads[0].net, read.board.net_by_name("A").map(|n| n.id));
    }

    #[test]
    fn a_zone_held_by_a_footprint_is_where_the_file_says_on_the_board() {
        // An antenna keepout under a part on the bottom, turned a quarter: the corners are board
        // coordinates, as the file writes them, whatever the footprint's place and side.
        let text = r#"(kicad_pcb (version 20241229)
  (footprint "a:b" (layer "B.Cu") (at 89.1 99.69 -90)
    (zone (net 0) (net_name "") (layers "F.Cu" "F.CrtYd") (hatch edge 0.5) (connect_pads (clearance 0)) (min_thickness 0.25)
      (keepout (tracks allowed) (vias not_allowed) (pads not_allowed) (copperpour not_allowed) (footprints allowed))
      (fill (thermal_gap 0.5) (thermal_bridge_width 0.5))
      (polygon (pts (xy 86.6 98.53) (xy 92.24 98.53) (xy 92.24 100.85) (xy 86.6 100.85))))))"#;
        let read = read_board(text, "x").expect("import");
        assert_eq!(read.notes.lines(), ["keepout for pads or footprints: only what it says of tracks, vias and zones kept"]);
        let keepout = read.board.keepouts.iter().next().expect("keepout");
        assert_eq!((layer(&read.board, keepout.layer), keepout.tracks, keepout.vias, keepout.zones), ("Top Copper", false, true, true));
        assert_eq!(keepout.outline.start, um(86_600, -98_530));
        assert_eq!(
            keepout.outline.edges.iter().map(Edge::end).collect::<Vec<_>>(),
            [um(92_240, -98_530), um(92_240, -100_850), um(86_600, -100_850)]
        );
        assert_eq!(read.board.keepouts.len(), 1, "the courtyard layer the zone also names holds no keepout");
    }

    #[test]
    fn a_zone_on_many_layers_cannot_make_the_board_many_times_the_file() {
        // 600,000 corners on 32 copper layers would be 19 million; a board's zones may have 16.
        let layers: String = (1..=30).map(|i| format!("({i} \"In{i}.Cu\" signal) ")).collect();
        let corners: String = (0..600_000).map(|i| format!("(xy {} {}) ", i % 1_000, i / 1_000)).collect();
        let zone = |layers: &str, pts: &str| format!("(zone (net 0) (layers {layers}) (polygon (pts {pts})))");
        let text = format!(
            "(kicad_pcb (version 20221018) (layers (0 \"F.Cu\" signal) {layers}(31 \"B.Cu\" signal)) {} {} {})",
            zone("\"F.Cu\"", "(xy 0 0) (xy 1 0) (xy 1 1)"),
            zone("\"*.Cu\"", &corners),
            zone("\"B.Cu\"", "(xy 0 0) (xy 1 0) (xy 1 1)"),
        );
        let read = read_board(&text, "x").expect("import");
        assert_eq!(read.notes.lines(), ["zone beyond what the zones of one board may add up to left out"]);
        // The zones before and after it are read: only the one that is too much is left out.
        assert_eq!((read.board.stack.copper_count(), read.board.zones.len()), (32, 2));
    }

    /// A board where nothing lines up with anything, so every angle, offset and option has to
    /// be read for itself: pads and texts turned against their footprints on both sides, a hole
    /// off the centre of its copper, margins given once for a footprint, tented vias, a hatched
    /// zone and zones that join pads fully or not at all.
    const AWKWARD: &str = r#"(kicad_pcb (version 20240108)
  (layers (0 "F.Cu" signal) (31 "B.Cu" signal))
  (setup (pad_to_mask_clearance 0.05) (tenting front) (pcbplotparams (viasonmask true)))
  (net 1 "A")
  (footprint "l:top" (layer "F.Cu") (at 10 10 90) (solder_mask_margin 0.07) (solder_paste_margin -0.03)
    (fp_text reference "T1" (at 0 0 120) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.1)) (justify left top)))
    (pad "1" smd rect (at 1 0 135) (size 2 1) (layers "F.Cu" "F.Mask" "F.Paste"))
    (pad "2" thru_hole circle (at 3 0 90) (size 2 2) (drill 1 (offset 0.25 -0.5)) (layers "*.Cu" "*.Mask") (net 1 "A"))
    (pad "3" np_thru_hole circle (at 6 0 90) (size 3 3) (drill 1) (layers "*.Cu" "*.Mask"))
    (pad "4" smd custom (at 9 0 90) (size 1.2 1.2) (layers "F.Cu") (options (clearance outline) (anchor circle))
      (primitives (gr_circle (center 0 0) (end 1 0) (width 0))))
    (fp_line (start 0 0) (end 2 0) (stroke (width 0.1) (type solid)) (layer "Edge.Cuts"))
  )
  (footprint "l:bottom" (layer "B.Cu") (at 30 10 90)
    (fp_text reference "B1" (at 0 0 60) (layer "B.SilkS") (effects (font (size 1 1) (thickness 0.1)) (justify mirror)))
    (pad "1" smd rect (at 1 0 135) (size 2 1) (layers "B.Cu" "B.Mask"))
    (fp_arc (start 0 0) (end 1 0) (angle 90) (layer "B.SilkS") (width 0.1))
  )
  (via (at 1 1) (size 0.8) (drill 0.4) (layers "B.Cu" "F.Cu") (tenting front))
  (via (at 2 1) (size 0.8) (drill 0.4) (layers "F.Cu" "B.Cu") (free))
  (zone (net 1) (net_name "A") (layer "F.Cu") (connect_pads yes (clearance 0.2)) (min_thickness 0.2)
    (fill yes (mode hatch) (thermal_gap 0.5) (thermal_bridge_width 0.5) (hatch_thickness 0.3) (hatch_gap 0.7))
    (polygon (pts (xy 0 0) (xy 5 0) (xy 5 5))))
  (zone (net 1) (net_name "A") (layer "B.Cu") (connect_pads no (clearance 0.2)) (min_thickness 0.2)
    (fill yes (thermal_gap 0.5) (thermal_bridge_width 0.5))
    (polygon (pts (xy 0 0) (xy 5 0) (xy 5 5))))
)
"#;

    #[test]
    fn angles_are_held_against_the_footprint_on_either_side() {
        let Imported { board, notes } = read_board(AWKWARD, "x").expect("import");
        assert_eq!(
            notes.lines(),
            [
                "board outline left as drawings: a drawing on the outline layer is part of no closed shape; close it, allow a wider gap or remove it",
                "custom pad shape read as its anchor",
                "text anchored at a height of its own: the anchoring from top to bottom is left out",
            ]
        );
        // On the top a pad or text at 135 or 120 on the board, in a footprint at 90, is turned 45
        // or 30 against it.
        let top = footprint(&board, "T1");
        assert_eq!(top.pads[0].rotation, Rotation(45_000));
        assert_eq!(top.texts.iter().map(|t| (t.rotation, t.justify)).collect::<Vec<_>>(), [(Rotation(30_000), Justify::Left)]);
        // On the bottom the turn is seen from the other side: 135 on the board against 90 is 45
        // the other way, and 60 against 90 is 30.
        let bottom = footprint(&board, "B1");
        assert_eq!((bottom.pads[0].position, bottom.pads[0].rotation), (um(-1_000, 0), Rotation(315_000)));
        assert_eq!(bottom.texts.iter().map(|t| (t.rotation, t.mirrored)).collect::<Vec<_>>(), [(Rotation(30_000), false)]);
        // An arc the file turns clockwise from (1, 0): in the flipped-back footprint it starts at
        // (-1, 0) and ends below the centre.
        let arc = Arc::new(um(-1_000, 0), Point::new(Nm(-707_107), Nm(-707_107)), um(0, -1_000));
        assert!(matches!(&bottom.graphics[0].shape, GraphicShape::Arc { arc: found } if *found == arc), "{:?}", bottom.graphics[0].shape);
        assert_eq!(layer(&board, bottom.graphics[0].layer), "Top Silkscreen");
    }

    #[test]
    fn pads_take_their_margins_offsets_and_copper_from_what_the_file_says() {
        let Imported { board, .. } = read_board(AWKWARD, "x").expect("import");
        let top = footprint(&board, "T1");
        let [one, two, three, four] = top.pads.as_slice() else { panic!("four pads") };
        // The footprint's mask and paste margins stand in for a pad that has none of its own.
        let (mask, paste) = (Opening::Open { by: Some(Nm(70_000)) }, Opening::Open { by: Some(Nm(-30_000)) });
        assert_eq!(one.front.as_ref().map(|f| (f.mask, f.paste)), Some((mask, paste)));
        // The copper sits off the hole by the drill's offset, y turned over.
        assert_eq!((two.copper_offset, two.hole, two.inner.is_some()), (um(250, 500), Some(Hole::round(Nm(MM), true)), false));
        // A hole that is not plated in a pad larger than it keeps its copper, and no net.
        assert_eq!((three.hole, three.front.is_some(), three.back.is_some(), three.net), (Some(Hole::round(Nm(MM), false)), true, true, None));
        // A custom pad of anything but one polygon is its anchor: here a circle.
        assert_eq!(four.front.as_ref().map(|f| &f.shape), Some(&PadShape::Circle { diameter: Nm(1_200_000) }));
        // The footprint's line on the outline layer is on the board, where the footprint puts it.
        let lifted: Vec<_> = board.graphics.iter().map(|g| (layer(&board, g.layer), &g.shape)).collect();
        assert!(
            matches!(lifted.as_slice(), [("Outline", GraphicShape::Line { from, to })] if (*from, *to) == (um(10_000, -10_000), um(10_000, -8_000))),
            "{lifted:?}"
        );
        assert!(top.graphics.is_empty() && board.outline.is_none());
    }

    #[test]
    fn vias_and_zones_take_their_options_from_the_file() {
        let Imported { board, .. } = read_board(AWKWARD, "x").expect("import");
        // The board's own tenting (KiCad 8 on) wins over the plot setting of older files.
        assert!(board.design.tent_vias);
        let mut vias: Vec<_> =
            board.vias.iter().map(|v| (v.position.x, layer(&board, v.from), layer(&board, v.to), v.top_mask, v.bottom_mask)).collect();
        vias.sort_by_key(|v| v.0);
        // Whichever layer the file names first, a via runs from the upper to the lower; one that
        // says it is tented on the front is closed there.
        assert_eq!(
            vias,
            [
                (Nm(MM), "Top Copper", "Bottom Copper", Opening::Closed, Opening::Rules),
                (Nm(2 * MM), "Top Copper", "Bottom Copper", Opening::Rules, Opening::Rules),
            ]
        );
        let mut zones: Vec<_> = board.zones.iter().map(|z| (layer(&board, z.layer), z.fill, z.pads)).collect();
        zones.sort_by_key(|z| z.0);
        assert_eq!(
            zones,
            [
                ("Bottom Copper", ZoneFill::Solid, PadConnection::None),
                ("Top Copper", ZoneFill::Hatched { width: Nm(300_000), gap: Nm(700_000) }, PadConnection::Solid),
            ]
        );
        // Without a tenting of the board's own, the plot setting decides.
        let plotted = |on: &str| {
            read_board(&format!("(kicad_pcb (version 20221018) (setup (pcbplotparams (viasonmask {on}))))"), "x")
                .expect("import")
                .board
                .design
                .tent_vias
        };
        assert_eq!((plotted("true"), plotted("false")), (false, true));
    }

    #[test]
    fn a_board_with_one_copper_layer_has_it_on_top() {
        let text = r#"(kicad_pcb (version 20221018) (layers (0 "F.Cu" signal) (37 "F.SilkS" user))
  (segment (start 0 0) (end 1 0) (width 0.2) (layer "F.Cu") (net 0))
  (segment (start 0 0) (end 1 0) (width 0.2) (layer "B.Cu") (net 0)))"#;
        let read = read_board(text, "x").expect("import");
        assert_eq!(read.board.stack.copper().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["Top Copper"]);
        assert_eq!(read.board.tracks.iter().map(|t| layer(&read.board, t.layer)).collect::<Vec<_>>(), ["Top Copper", "Top Copper"]);
    }

    #[test]
    fn what_the_reader_works_out_beyond_the_design_range_is_left_out() {
        // What the file says is within ten metres; where the footprint puts it is not.
        let far = |part: &str| {
            read_board(&format!("(kicad_pcb (version 20221018) (footprint \"a:b\" (layer \"F.Cu\") (at 9999 0 45) {part}))"), "x").expect("import")
        };
        let read = far("(fp_line (start 0 0) (end 5 0) (stroke (width 0.1)) (layer \"Edge.Cuts\"))");
        assert_eq!(read.notes.lines(), ["footprint drawing on the outline layer that could not be placed left out"]);
        assert!(read.board.graphics.is_empty());
        let read = read_board("(kicad_pcb (version 20221018) (footprint \"a:b\" (layer \"F.Cu\") (at 0 0) (pad \"\" smd rect (at 9999.9 0 45) (size 1 1) (layers \"F.Paste\"))))", "x").expect("import");
        assert_eq!(read.notes.lines(), ["footprint pad that could not be read left out"]);
        // The same for a paste opening of a shape of its own.
        let custom = "(pad \"\" smd custom (at 9999.9 0 45) (size 1 1) (layers \"F.Paste\") (primitives (gr_poly (pts (xy -1 -1) (xy 1 -1) (xy 0 1)) (width 0))))";
        let read =
            read_board(&format!("(kicad_pcb (version 20221018) (footprint \"a:b\" (layer \"F.Cu\") (at 0 0) {custom}))"), "x").expect("import");
        assert_eq!(read.notes.lines(), ["footprint pad that could not be read left out"]);
        // A board the model refuses as a whole is an error that says why: here a footprint with
        // more pads than one may have.
        let pads = "(pad \"1\" smd rect (at 0 0) (size 1 1) (layers \"F.Cu\")) ".repeat(circuitcraft_model::MAX_PADS + 1);
        let crowded = format!("(kicad_pcb (version 20221018) (footprint \"a:b\" (layer \"F.Cu\") (at 0 0) {pads}))");
        assert_eq!(read_board(&crowded, "x"), Err(ImportError::Invalid(circuitcraft_model::ModelError::TooMany("pads"))));
        // More layers than a board may have: refused when they are listed, quickly.
        let layers: String = (0..2_000).map(|i| format!("({i} \"{}{i}\" user) ", "x".repeat(300))).collect();
        let start = std::time::Instant::now();
        assert!(matches!(read_board(&format!("(kicad_pcb (version 20221018) (layers {layers}))"), "x"), Err(ImportError::Unsupported(_))));
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "{:?}", start.elapsed());
    }

    #[test]
    fn many_small_zones_on_many_layers_add_up_too() {
        let layers: String = (1..=30).map(|i| format!("({i} \"In{i}.Cu\" signal) ")).collect();
        let zones = "(zone (net 0) (layers \"*.Cu\") (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1)))) ".repeat(15_000);
        let text = format!("(kicad_pcb (version 20221018) (layers (0 \"F.Cu\" signal) {layers}(31 \"B.Cu\" signal)) {zones})");
        let read = read_board(&text, "x").expect("import");
        // Each of a zone's 32 copies costs its two edges and 64 more.
        let fit = MAX_ZONE_COST / ((2 + ZONE_COST) * 32);
        assert_eq!(read.board.zones.len(), fit * 32);
        assert_eq!(read.notes.iter().map(|(_, count)| count).sum::<usize>(), 15_000 - fit);
        assert!(fit < 10_000, "{fit}");
    }

    #[test]
    fn the_same_file_gives_the_same_board_and_a_cut_file_never_crashes() {
        let first = read_board(BOARD, "Demo").expect("import");
        assert_eq!(read_board(BOARD, "Demo").expect("import"), first, "ids follow from the file");
        for sample in [BOARD, OLD] {
            for cut in (0..sample.len()).step_by(3) {
                if let Some(prefix) = sample.get(..cut) {
                    // An error or a board: never a panic, and a board always validates.
                    if let Ok(read) = read_board(prefix, "x") {
                        assert_eq!(read.board.validate(), Ok(()), "cut at {cut}");
                    }
                }
            }
        }
    }
}
