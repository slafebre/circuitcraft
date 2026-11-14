//! EAGLE board files (`.brd`, the XML format of EAGLE 6 and later), read-only.
//!
//! Written by hand from the format's document type definition and from real boards. The reader
//! keeps what the model holds and says in the notes what it left out. What a file says in
//! EAGLE's terms becomes the model's:
//!
//! - A package is drawn once in a library and placed by elements; each element becomes a
//!   footprint with its own copy.
//! - Sizes a file leaves to its design rules (the copper around a drilled pad or via, mask and
//!   paste openings, which vias are covered) are worked out from those rules.
//! - `tKeepout` and `bKeepout` hold the room components need: the model's courtyards.
//!   `tRestrict` and `bRestrict` keep copper out: the model's keepout layers. `vRestrict`
//!   becomes via keepouts on every copper layer.
//! - Drawings on `Dimension` become the board outline when they close into one.

use crate::import::{self, Ids, ImportError, Imported, Layers, Notes, Standard};
use crate::xml::{self, Element};
use circuitcraft_geom::{Contour, Edge};
use circuitcraft_model::{
    Assembly, Board, BoardHole, Footprint, FootprintText, Graphic, GraphicShape, Hole, Justify, Keepout, LayerId, LayerKind, LibraryLink,
    MAX_NAME_CHARS, Mounting, Net, NetId, Opening, Pad, PadConnection, PadShape, Rotation, Side, Text, TextContent, Track, TrackPath, Via, Zone,
    ZoneFill,
};
use circuitcraft_units::{Nm, Point};
use std::collections::BTreeMap;

/// EAGLE's layer numbers, as its files and documentation give them.
mod number {
    pub(super) const TOP: i64 = 1;
    pub(super) const BOTTOM: i64 = 16;
    pub(super) const UNROUTED: i64 = 19;
    pub(super) const DIMENSION: i64 = 20;
    pub(super) const VIA_RESTRICT: i64 = 43;
    /// Layers that only show what is elsewhere in the file (pads, vias, drills, holes).
    pub(super) const DISPLAY_ONLY: [i64; 4] = [17, 18, 44, 45];
    /// Top and bottom layers that hold drawings that are not manufactured.
    pub(super) const MECHANICAL_PAIRS: [(i64, i64, &str); 4] = [(23, 24, "Origins"), (33, 34, "Finish"), (35, 36, "Glue"), (37, 38, "Test")];
}

/// The most the elements of one board may place. Every element gets a copy of its package, so a
/// small file can name a large package many times; each copy is weighed by what the board gets
/// from it (one for each pad, drawing and text and for each corner of a polygon, and more for long
/// names and text), and this bounds what the copies add up to. Via keepouts, copied to every
/// copper layer, count the same way. A board of ten thousand parts uses a fifth of it.
const MAX_PLACED: usize = 1_000_000;

/// Reads an EAGLE board. `name` becomes the board's name (a file has none of its own).
pub fn read_board(text: &str, name: &str) -> Result<Imported, ImportError> {
    let root = xml::parse(text)?;
    if root.name != "eagle" {
        return Err(ImportError::WrongFormat("an EAGLE file"));
    }
    let drawing = root.child("drawing").ok_or(ImportError::WrongFormat("an EAGLE board"))?;
    let board = drawing.child("board").ok_or(ImportError::WrongFormat("an EAGLE board (a schematic or a library has no board)"))?;
    let mut eagle = Eagle::new(text, name, drawing, board);
    let signals = board.child("signals").map(|s| s.children.as_slice()).unwrap_or_default();
    // The signals say which pads are on which net; the elements, read first, need to know.
    for signal in signals {
        eagle.contacts(signal);
    }
    let too_much = || ImportError::unsupported(format!("the elements place more than {MAX_PLACED} pads, drawings and corners"));
    for part in board.child("plain").map(|p| p.children.as_slice()).unwrap_or_default() {
        eagle.plain(part);
    }
    for element in board.child("elements").map(|e| e.children.as_slice()).unwrap_or_default() {
        if eagle.element(element).is_none() {
            eagle.notes.add("element that could not be read left out");
        }
        if eagle.placed > MAX_PLACED {
            return Err(too_much());
        }
    }
    for signal in signals {
        eagle.signal(signal);
    }
    eagle.board.stack = eagle.layers.finish();
    import::finish(eagle.board, eagle.notes)
}

/// A length as the design rules write it: `5mil`, `0.035mm`, `2.5`; millimetres without a unit.
fn measure(value: &str) -> Option<Nm> {
    let value = value.trim();
    let (digits, per_unit) = [("mil", 0.0254), ("mm", 1.0), ("mic", 0.001), ("inch", 25.4)]
        .into_iter()
        .find_map(|(unit, mm)| value.strip_suffix(unit).map(|digits| (digits, mm)))
        .unwrap_or((value, 1.0));
    let number = digits.trim().parse::<f64>().ok().filter(|n| n.is_finite())?;
    Nm::from_mm(number * per_unit).ok()
}

/// A share of a size, kept between a smallest and a largest value: how the design rules give
/// the copper around a hole and the mask and paste openings.
#[derive(Debug, Clone, Copy)]
struct Share {
    ratio: f64,
    min: Nm,
    max: Nm,
}

impl Share {
    fn of(params: &BTreeMap<&str, &str>, ratio: &str, min: &str, max: &str) -> Option<Share> {
        let ratio = params.get(ratio)?.trim().parse::<f64>().ok().filter(|r| r.is_finite() && *r >= 0.0)?;
        let (min, max) = (measure(params.get(min)?)?, measure(params.get(max)?)?);
        (min.0 >= 0 && max >= min).then_some(Share { ratio, min, max })
    }

    fn for_size(self, size: Nm) -> Nm {
        Nm(((size.0 as f64 * self.ratio) as i64).clamp(self.min.0, self.max.0))
    }
}

/// What the reader needs of the file's design rules.
#[derive(Debug, Clone, Copy, Default)]
struct Rules {
    /// The solder mask opening beyond a pad.
    stop: Option<Share>,
    /// How much smaller than a pad its paste aperture is.
    cream: Option<Share>,
    /// The copper ring around a drilled pad and around a via.
    pad_ring: Option<Share>,
    via_ring: Option<Share>,
    /// Vias drilled no larger than this are covered by solder mask.
    via_stop_limit: Option<Nm>,
    thermal_gap: Option<Nm>,
    /// How much longer than wide a `long` pad is, as a share of its width.
    elongation: f64,
}

impl Rules {
    fn of(board: &Element, design: &mut circuitcraft_model::DesignSettings) -> Rules {
        let params: BTreeMap<&str, &str> = board
            .child("designrules")
            .map(|d| d.all("param").filter_map(|p| Some((p.attr("name")?, p.attr("value")?))).collect())
            .unwrap_or_default();
        let length = |name: &str| params.get(name).and_then(|v| measure(v));
        let rules = Rules {
            stop: Share::of(&params, "mvStopFrame", "mlMinStopFrame", "mlMaxStopFrame"),
            cream: Share::of(&params, "mvCreamFrame", "mlMinCreamFrame", "mlMaxCreamFrame"),
            pad_ring: Share::of(&params, "rvPadTop", "rlMinPadTop", "rlMaxPadTop"),
            via_ring: Share::of(&params, "rvViaOuter", "rlMinViaOuter", "rlMaxViaOuter"),
            via_stop_limit: length("mlViaStopLimit"),
            thermal_gap: length("slThermalIsolate").filter(|g| g.0 > 0),
            elongation: params
                .get("psElongationLong")
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|e| (0.0..=1_000.0).contains(e))
                .unwrap_or(100.0)
                / 100.0,
        };
        if let Some(clearance) = length("mdWireWire").filter(|c| c.0 >= 0) {
            design.clearance = clearance;
        }
        if let Some(stop) = rules.stop {
            design.mask_expansion = stop.min;
        }
        if let Some(cream) = rules.cream {
            design.paste_expansion = Nm(-cream.min.0);
        }
        // Each via says for itself whether it is covered: the rule goes by its drill.
        design.tent_vias = false;
        rules
    }

    /// The opening over copper whose smaller side is `size`: left to the design settings when
    /// it comes out as they have it.
    fn opening(share: Option<Share>, size: Nm, shrink: bool, settings: Nm) -> Opening {
        match share.map(|s| s.for_size(size)).map(|by| if shrink { Nm(-by.0) } else { by }) {
            Some(by) if by != settings => Opening::Open { by: Some(by) },
            _ => Opening::Rules,
        }
    }
}

/// `R90`, `MR270`, `SMR0`: an angle in degrees, and whether the thing is mirrored.
fn rot(element: &Element) -> (f64, bool) {
    let rot = element.attr("rot").unwrap_or("R0");
    let angle = rot.trim_start_matches(|c: char| c.is_ascii_alphabetic()).parse::<f64>().ok().filter(|a| a.is_finite()).unwrap_or(0.0);
    (angle, rot.contains('M'))
}

/// The point `(x, y)` of an element, under attributes called `x` and `y`.
fn point(element: &Element, x: &str, y: &str) -> Option<Point> {
    Some(Point::new(Nm::from_mm(element.number(x)?).ok()?, Nm::from_mm(element.number(y)?).ok()?))
}

fn length(element: &Element, name: &str) -> Option<Nm> {
    Nm::from_mm(element.number(name)?).ok()
}

/// The outline of a `polygon`: its vertices in order, each joined to the next by a line or, when
/// the vertex says `curve`, by an arc that turns that many degrees.
fn contour(polygon: &Element) -> Option<Contour> {
    let mut corners: Vec<(Point, f64)> = Vec::new();
    for vertex in polygon.all("vertex") {
        let corner = (point(vertex, "x", "y")?, vertex.number("curve").unwrap_or(0.0));
        // A vertex on top of the one before adds nothing; its curve is the one that counts.
        match corners.last_mut() {
            Some(last) if last.0 == corner.0 => last.1 = corner.1,
            _ => corners.push(corner),
        }
    }
    if corners.len() > 1 && corners.first().map(|c| c.0) == corners.last().map(|c| c.0) {
        corners.pop();
    }
    let curved = corners.iter().any(|c| c.1 != 0.0);
    if corners.len() < 2 || (corners.len() == 2 && !curved) {
        return None;
    }
    let start = corners.first()?.0;
    let edges = (0..corners.len())
        .filter_map(|i| {
            let (from, curve) = *corners.get(i)?;
            let to = corners.get((i + 1) % corners.len())?.0;
            Some(match import::chord_arc(from, to, curve) {
                Some(arc) => Edge::Arc { mid: arc.mid, to },
                None => Edge::Line { to },
            })
        })
        .collect();
    Some(Contour { start, edges })
}

/// How a `text` or a placed `attribute` is drawn.
struct Lettering {
    height: Nm,
    stroke: Nm,
    angle: f64,
    mirror: bool,
    justify: Justify,
    /// Whether the text hangs from its top or its middle; the model anchors text at its baseline.
    raised: bool,
}

impl Lettering {
    /// The model's rotation for text on the board or in a package. EAGLE turns text and then
    /// mirrors it, as it does elements; the model mirrors and then turns, which is the same when
    /// it turns the other way.
    fn rotation(&self) -> Option<Rotation> {
        import::degrees(if self.mirror { -self.angle } else { self.angle }).ok()
    }

    fn of(element: &Element) -> Option<Lettering> {
        let height = length(element, "size").filter(|h| h.0 > 0)?;
        // The stroke is a share of the height, in per cent; 8 when the file doesn't say.
        let ratio = element.number("ratio").filter(|r| (1.0..=100.0).contains(r)).unwrap_or(8.0);
        let (angle, mirror) = rot(element);
        let align = element.attr("align").unwrap_or("bottom-left");
        Some(Lettering {
            height,
            stroke: Nm(((height.0 as f64 * ratio / 100.0) as i64).max(1)),
            angle,
            mirror,
            justify: if align.ends_with("left") {
                Justify::Left
            } else if align.ends_with("right") {
                Justify::Right
            } else {
                Justify::Center
            },
            raised: !align.starts_with("bottom"),
        })
    }
}

/// The reader's state: the board so far and what the file's numbers and names stand for.
struct Eagle<'a> {
    ids: Ids,
    notes: Notes,
    board: Board,
    layers: Layers,
    standard: Standard,
    /// The model's layer for each of the file's layer numbers met so far.
    by_number: BTreeMap<i64, LayerId>,
    /// The names the file gives its layers.
    layer_names: BTreeMap<i64, &'a str>,
    rules: Rules,
    /// Packages by library and name, each with its weight ([`Element::weight`]).
    packages: BTreeMap<(&'a str, &'a str), (&'a Element<'a>, usize)>,
    /// The net of each pad that is on one, by element name and pad name.
    on_net: BTreeMap<(&'a str, &'a str), NetId>,
    net_names: BTreeMap<String, NetId>,
    /// What the elements read so far have placed ([`MAX_PLACED`]).
    placed: usize,
}

impl<'a> Eagle<'a> {
    fn new(text: &str, name: &str, drawing: &'a Element<'a>, board: &'a Element<'a>) -> Eagle<'a> {
        let name = if name.trim().is_empty() { "Imported" } else { name };
        let mut model = Board::new(&name.chars().take(MAX_NAME_CHARS).collect::<String>());
        let rules = Rules::of(board, &mut model.design);
        // `layerSetup` names the copper layers in use, as in `(1*2*15*16)`.
        let setup =
            board.child("designrules").and_then(|d| d.all("param").find(|p| p.attr("name") == Some("layerSetup"))).and_then(|p| p.attr("value"));
        let mut copper: Vec<i64> = setup
            .unwrap_or("(1*16)")
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|n| n.parse::<i64>().ok())
            .filter(|n| (number::TOP..=number::BOTTOM).contains(n))
            .collect();
        copper.sort_unstable();
        copper.dedup();
        if copper.len() < 2 {
            copper = vec![number::TOP, number::BOTTOM];
        }
        let mut layers = Layers::default();
        let standard = layers.standard(copper.len(), Nm(1_600_000));
        let mut by_number: BTreeMap<i64, LayerId> = copper.iter().copied().zip(standard.copper.iter().copied()).collect();
        // After the standard layers a stack has room for many more: these always fit.
        let names = layers.pair("Names", LayerKind::Silkscreen).unwrap_or(standard.silk);
        let values = layers.pair("Values", LayerKind::Silkscreen).unwrap_or(standard.silk);
        let restrict = layers.pair("Keepout", LayerKind::Keepout).unwrap_or(standard.courtyard);
        let sided = [
            (21, standard.silk),
            (25, names),
            (27, values),
            (29, standard.mask),
            (31, standard.paste),
            (39, standard.courtyard),
            (41, restrict),
            (51, standard.fabrication),
        ];
        for (top, [upper, lower]) in sided {
            by_number.insert(top, upper);
            by_number.insert(top + 1, lower);
        }
        by_number.insert(number::DIMENSION, standard.outline);
        let layer_names = drawing
            .child("layers")
            .map(|l| l.all("layer").filter_map(|layer| Some((layer.number("number")? as i64, layer.attr("name")?))).collect())
            .unwrap_or_default();
        let libraries = board.child("libraries").map(|l| l.children.as_slice()).unwrap_or_default();
        let packages = libraries
            .iter()
            .flat_map(|library| {
                let of = library.attr("name").unwrap_or_default();
                let packages = library.child("packages").map(|p| p.children.as_slice()).unwrap_or_default();
                packages.iter().filter_map(move |package| Some(((of, package.attr("name")?), (package, package.weight()))))
            })
            .collect();
        Eagle {
            ids: Ids::new(text),
            notes: Notes::default(),
            board: model,
            layers,
            standard,
            by_number,
            layer_names,
            rules,
            packages,
            on_net: BTreeMap::new(),
            net_names: BTreeMap::new(),
            placed: 0,
        }
    }

    /// The model's layer for the layer an element is on. A layer the model has no kind for
    /// becomes a layer for drawings, under the name the file gives it.
    fn layer(&mut self, element: &Element) -> Option<LayerId> {
        let number = element.number("layer")? as i64;
        if let Some(&id) = self.by_number.get(&number) {
            return Some(id);
        }
        if number::DISPLAY_ONLY.contains(&number) || number == number::UNROUTED || number == number::VIA_RESTRICT {
            return None;
        }
        if (number::TOP..=number::BOTTOM).contains(&number) {
            return None; // a copper layer the board doesn't use
        }
        if let Some(&(top, bottom, name)) = number::MECHANICAL_PAIRS.iter().find(|(top, bottom, _)| number == *top || number == *bottom) {
            let [upper, lower] = self.layers.pair(name, LayerKind::Mechanical)?;
            self.by_number.insert(top, upper);
            self.by_number.insert(bottom, lower);
            return self.by_number.get(&number).copied();
        }
        let name = self.layer_names.get(&number).map_or_else(|| format!("Layer {number}"), |n| (*n).to_owned());
        let id = self.layers.single(&name, LayerKind::Mechanical)?;
        self.by_number.insert(number, id);
        Some(id)
    }

    fn is_copper(&self, id: LayerId) -> bool {
        self.standard.copper.contains(&id)
    }

    /// What a `wire`, `circle`, `rectangle` or `polygon` draws, and how wide its line is.
    fn shape(&mut self, element: &Element) -> Option<(Nm, GraphicShape)> {
        let width = length(element, "width").filter(|w| w.0 > 0).unwrap_or(Nm(0));
        let shape = match element.name {
            "wire" => {
                let (from, to) = (point(element, "x1", "y1")?, point(element, "x2", "y2")?);
                match import::chord_arc(from, to, element.number("curve").unwrap_or(0.0)) {
                    Some(arc) => GraphicShape::Arc { arc },
                    None => GraphicShape::Line { from, to },
                }
            }
            "circle" => {
                let (center, radius) = (point(element, "x", "y")?, length(element, "radius")?);
                // A circle drawn with no line width is a filled disc, out to its radius.
                match width.0 {
                    0 => GraphicShape::Polygon { outline: import::disc(center, radius)?, filled: true },
                    _ if radius.0 > 0 => GraphicShape::Circle { center, radius },
                    _ => return None,
                }
            }
            "rectangle" => {
                let (a, b) = (point(element, "x1", "y1")?, point(element, "x2", "y2")?);
                let center = Point::new(Nm((a.x.0 + b.x.0) / 2), Nm((a.y.0 + b.y.0) / 2));
                let turn = import::degrees(rot(element).0).ok()?;
                let outline = import::rectangle(center, Nm((b.x.0 - a.x.0).abs()), Nm((b.y.0 - a.y.0).abs()), turn)?;
                GraphicShape::Polygon { outline, filled: true }
            }
            "polygon" => GraphicShape::Polygon { outline: contour(element)?, filled: true },
            _ => return None,
        };
        Some((width, shape))
    }

    /// A closed shape on `vRestrict`: vias stay out of it on every copper layer.
    fn via_keepout(&mut self, shape: GraphicShape) {
        let GraphicShape::Polygon { outline, .. } = shape else {
            self.notes.add("line on the via keepout layer left out: only closed shapes keep vias out");
            return;
        };
        // One copy for each copper layer, counted like the copies of a package.
        let with_this = self.placed.saturating_add(outline.edges.len().saturating_add(1).saturating_mul(self.standard.copper.len()));
        if with_this > MAX_PLACED {
            self.notes.add("via keepout beyond what one board may place left out");
            return;
        }
        self.placed = with_this;
        for layer in self.standard.copper.clone() {
            let area = Keepout { id: self.ids.item(), layer, outline: outline.clone(), tracks: false, vias: true, zones: false };
            if self.board.keepouts.insert(area).is_err() {
                self.notes.add("keepout beyond the most a board can hold left out");
            }
        }
    }

    /// One thing drawn straight on the board.
    fn plain(&mut self, part: &Element) {
        let read = match part.name {
            "wire" | "circle" | "rectangle" | "polygon" => self.plain_drawing(part),
            "text" => self.plain_text(part),
            "hole" => self.plain_hole(part),
            other => {
                self.notes.add(&format!("{other} left out"));
                Some(())
            }
        };
        if read.is_none() {
            self.notes.add(&format!("{} that could not be read left out", part.name));
        }
    }

    fn plain_drawing(&mut self, part: &Element) -> Option<()> {
        let (width, shape) = self.shape(part)?;
        if part.number("layer") == Some(number::VIA_RESTRICT as f64) {
            self.via_keepout(shape);
            return Some(());
        }
        let layer = self.layer(part)?;
        self.board.graphics.insert(Graphic { id: self.ids.item(), layer, width, shape }).ok().map(|_| ())
    }

    fn plain_text(&mut self, part: &Element) -> Option<()> {
        if part.text.trim().is_empty() {
            return Some(());
        }
        let lettering = Lettering::of(part)?;
        if lettering.raised {
            self.notes.add(import::RAISED_TEXT);
        }
        let text = Text {
            id: self.ids.item(),
            layer: self.layer(part)?,
            text: part.text.chars().take(MAX_NAME_CHARS).collect(),
            position: point(part, "x", "y")?,
            height: lettering.height,
            stroke: lettering.stroke,
            rotation: lettering.rotation()?,
            mirrored: lettering.mirror,
            justify: lettering.justify,
        };
        self.board.texts.insert(text).ok().map(|_| ())
    }

    fn plain_hole(&mut self, part: &Element) -> Option<()> {
        let hole = Hole::round(length(part, "drill").filter(|d| d.0 > 0)?, false);
        let hole = BoardHole { id: self.ids.item(), position: point(part, "x", "y")?, hole };
        self.board.holes.insert(hole).ok().map(|_| ())
    }

    /// An `element`: a package placed on the board.
    fn element(&mut self, element: &'a Element<'a>) -> Option<()> {
        let name = element.attr("name")?;
        let (library, package_name) = (element.attr("library").unwrap_or_default(), element.attr("package")?);
        let Some(&(package, weight)) = self.packages.get(&(library, package_name)) else {
            self.notes.add("element whose package the file doesn't hold left out");
            return Some(());
        };
        self.placed = self.placed.saturating_add(weight);
        if self.placed > MAX_PLACED {
            return Some(()); // the caller stops the import
        }
        let (angle, mirror) = rot(element);
        let short = |s: &str| s.trim().chars().take(MAX_NAME_CHARS).collect::<String>();
        let mut footprint = Footprint {
            id: self.ids.item(),
            reference: if name.trim().is_empty() { "?".to_owned() } else { short(name) },
            value: element.attr("value").unwrap_or_default().chars().take(MAX_NAME_CHARS).collect(),
            link: (!package_name.trim().is_empty()).then(|| LibraryLink { library: short(library), footprint: short(package_name) }),
            assembly: Assembly { mounting: Mounting::Other, in_bom: true, in_placement: true, fitted: true },
            position: point(element, "x", "y")?,
            // EAGLE turns an element and then mirrors it; the model mirrors a footprint and
            // then turns it, which is the same when it turns the other way.
            rotation: import::degrees(if mirror { -angle } else { angle }).ok()?,
            side: if mirror { Side::Bottom } else { Side::Top },
            pads: Vec::new(),
            graphics: Vec::new(),
            texts: Vec::new(),
        };
        // A smashed element places its name and value itself; the package's own are not shown.
        let smashed = element.attr("smashed") == Some("yes");
        for part in &package.children {
            // A part can place more than it weighs (an octagon pad holds its corners three
            // times), so one large package could pass the limit many times over before the
            // caller looks: stop at the part that passes it.
            if self.placed > MAX_PLACED {
                return Some(()); // the caller stops the import
            }
            if self.package_part(part, smashed, &mut footprint).is_none() {
                self.notes.add(&format!("package {} that could not be read left out", part.name));
            }
        }
        for attribute in element.all("attribute") {
            if self.placed_attribute(attribute, smashed, angle, &mut footprint).is_none() {
                self.notes.add("element attribute that could not be read left out");
            }
        }
        // A hole of the package is a pad without a name; a net is for the pads that have one.
        for pad in footprint.pads.iter_mut().filter(|pad| !pad.number.is_empty()) {
            pad.net = self.on_net.get(&(name, pad.number.as_str())).copied();
        }
        footprint.assembly.mounting = if footprint.pads.iter().any(|p| p.hole.is_none()) {
            Mounting::SurfaceMount
        } else if footprint.pads.iter().any(|p| p.hole.is_some_and(|h| h.plated)) {
            Mounting::ThroughHole
        } else {
            Mounting::Other
        };
        self.board.footprints.insert(footprint).ok().map(|_| ())
    }

    /// One thing a package holds.
    fn package_part(&mut self, part: &Element, smashed: bool, footprint: &mut Footprint) -> Option<()> {
        match part.name {
            "smd" => footprint.pads.push(self.smd(part)?),
            "pad" => footprint.pads.push(self.drilled_pad(part)?),
            "hole" => footprint.pads.push(Pad::bare_hole(point(part, "x", "y")?, Hole::round(length(part, "drill").filter(|d| d.0 > 0)?, false))),
            "wire" | "circle" | "rectangle" | "polygon" => self.package_drawing(part, footprint)?,
            "text" => self.package_text(part, smashed, footprint)?,
            "description" => {}
            other => self.notes.add(&format!("package {other} left out")),
        }
        Some(())
    }

    fn package_drawing(&mut self, part: &Element, footprint: &mut Footprint) -> Option<()> {
        let (width, shape) = self.shape(part)?;
        let number = part.number("layer")? as i64;
        // A package's drawings on the outline and via keepout layers are part of the board: they
        // go on the board, where the element puts them.
        if number == number::DIMENSION || number == number::VIA_RESTRICT {
            let Some(shape) = import::lift(footprint, shape) else {
                self.notes.add("package drawing that could not be placed on the board left out");
                return Some(());
            };
            if number == number::VIA_RESTRICT {
                self.via_keepout(shape);
                return Some(());
            }
            let graphic = Graphic { id: self.ids.item(), layer: self.standard.outline, width, shape };
            return self.board.graphics.insert(graphic).ok().map(|_| ());
        }
        let layer = self.layer(part)?;
        footprint.graphics.push(Graphic { id: self.ids.item(), layer, width, shape });
        Some(())
    }

    /// A package's `text`: `>NAME` and `>VALUE` stand for the element's name and value.
    fn package_text(&mut self, part: &Element, smashed: bool, footprint: &mut Footprint) -> Option<()> {
        let words = part.text.trim();
        let content = match words.to_ascii_uppercase().as_str() {
            "" => return Some(()),
            ">NAME" | ">VALUE" if smashed => return Some(()),
            ">NAME" => TextContent::Reference,
            ">VALUE" => TextContent::Value,
            placeholder if placeholder.starts_with('>') => {
                self.notes.add("package text that stands for an attribute left out");
                return Some(());
            }
            _ => TextContent::Fixed(words.chars().take(MAX_NAME_CHARS).collect()),
        };
        let lettering = Lettering::of(part)?;
        footprint.texts.push(FootprintText {
            id: self.ids.item(),
            content,
            visible: true,
            layer: self.layer(part)?,
            position: point(part, "x", "y")?,
            height: lettering.height,
            stroke: lettering.stroke,
            rotation: lettering.rotation()?,
            mirrored: lettering.mirror,
            justify: lettering.justify,
        });
        Some(())
    }

    /// An element's `attribute`. On a smashed element, `NAME` and `VALUE` with a place say
    /// where the name and value are drawn, in board coordinates. Any other attribute is a fact
    /// about the part, not something on the board.
    fn placed_attribute(&mut self, attribute: &Element, smashed: bool, angle: f64, footprint: &mut Footprint) -> Option<()> {
        let content = match attribute.attr("name").map(str::to_ascii_uppercase).as_deref() {
            Some("NAME") => TextContent::Reference,
            Some("VALUE") => TextContent::Value,
            _ => {
                self.notes.add("element attribute left out");
                return Some(());
            }
        };
        let (Some(on_board), true) = (point(attribute, "x", "y"), smashed) else { return Some(()) };
        let lettering = Lettering::of(attribute)?;
        let bottom = footprint.side == Side::Bottom;
        // The model holds a footprint as seen from the top and flips it itself: a layer the file
        // names on the bottom of a mirrored element is the top layer's counterpart.
        let layer = self.layer(attribute)?;
        let layer = if bottom { self.counterpart(layer) } else { layer };
        footprint.texts.push(FootprintText {
            id: self.ids.item(),
            content,
            visible: attribute.attr("display") != Some("off"),
            layer,
            position: footprint.to_footprint(on_board).and_then(import::in_range)?,
            height: lettering.height,
            stroke: lettering.stroke,
            // Both angles are as the file has them. Mirrored like its element (both or
            // neither), the text is turned against it by their difference; mirrored unlike it,
            // the text's own angle counts the other way round.
            rotation: import::degrees(if lettering.mirror == bottom { lettering.angle - angle } else { -lettering.angle - angle }).ok()?,
            mirrored: lettering.mirror != bottom,
            justify: lettering.justify,
        });
        Some(())
    }

    /// The layer on the other side of the board, for the layers that come in pairs.
    fn counterpart(&self, layer: LayerId) -> LayerId {
        let number = self.by_number.iter().find(|(_, id)| **id == layer).map(|(number, _)| *number);
        let other = number.and_then(|n| {
            let paired = [21, 25, 27, 29, 31, 39, 41, 51].into_iter().chain(number::MECHANICAL_PAIRS.iter().map(|p| p.0));
            let top = paired.clone().find(|top| *top == n || top + 1 == n)?;
            self.by_number.get(&if n == top { top + 1 } else { top }).copied()
        });
        // Copper: the layer as far from the bottom as this one is from the top.
        let copper = self.standard.copper.iter().position(|id| *id == layer).and_then(|i| self.standard.copper.iter().rev().nth(i).copied());
        other.or(copper).unwrap_or(layer)
    }

    /// A surface-mount pad: copper on the package's own side, or on the other when the package
    /// draws it on the bottom layer.
    fn smd(&mut self, part: &Element) -> Option<Pad> {
        let (width, height) = (length(part, "dx").filter(|w| w.0 > 0)?, length(part, "dy").filter(|h| h.0 > 0)?);
        let narrow = Nm(width.0.min(height.0));
        // Roundness is the share of the narrow side's half that the corners take, in per cent.
        let roundness = part.number("roundness").unwrap_or(0.0).clamp(0.0, 100.0);
        let shape = if roundness == 0.0 {
            PadShape::Rect { width, height }
        } else if roundness == 100.0 && width == height {
            PadShape::Circle { diameter: width }
        } else {
            PadShape::RoundRect { width, height, radius: Nm(((narrow.0 as f64) * roundness / 200.0) as i64) }
        };
        let design = self.board.design;
        let mut pad = Pad::smd(&pad_name(part)?, point(part, "x", "y")?, shape);
        pad.rotation = import::degrees(rot(part).0).ok()?;
        if let Some(face) = &mut pad.front {
            if part.attr("stop") == Some("no") {
                face.mask = Opening::Closed;
            } else {
                face.mask = Rules::opening(self.rules.stop, narrow, false, design.mask_expansion);
            }
            if part.attr("cream") == Some("no") {
                face.paste = Opening::Closed;
            } else {
                face.paste = Rules::opening(self.rules.cream, narrow, true, design.paste_expansion);
            }
        }
        // A package draws a pad for the other side of the board on the bottom layer.
        if part.number("layer").is_some_and(|layer| layer as i64 == number::BOTTOM) {
            pad.back = pad.front.take();
        }
        Some(pad)
    }

    /// A drilled pad: the same copper on every layer around a plated hole.
    fn drilled_pad(&mut self, part: &Element) -> Option<Pad> {
        let drill = length(part, "drill").filter(|d| d.0 > 0)?;
        // A pad without a diameter gets the copper ring the design rules give its drill.
        let ring = self.rules.pad_ring.map_or(Nm(drill.0 / 4), |share| share.for_size(drill));
        let diameter = match length(part, "diameter").filter(|d| *d > drill) {
            Some(diameter) => diameter,
            None => import::size(Nm(drill.0.saturating_add(ring.0.max(1).saturating_mul(2))))?,
        };
        let long = import::size(Nm((diameter.0 as f64 * (1.0 + self.rules.elongation)) as i64))?;
        let mut offset = Point::ORIGIN;
        let shape = match part.attr("shape").unwrap_or("round") {
            "square" => PadShape::Rect { width: diameter, height: diameter },
            "octagon" => PadShape::Custom { outline: octagon(diameter)? },
            "long" if long > diameter => PadShape::Oval { width: long, height: diameter },
            // An offset pad is a long one with its hole at one end.
            "offset" if long > diameter => {
                offset = Point::new(Nm((long.0 - diameter.0) / 2), Nm(0));
                PadShape::Oval { width: long, height: diameter }
            }
            _ => PadShape::Circle { diameter },
        };
        // A shape with corners of its own is held three times, on the front, inside and on the
        // back: its corners count against what the elements may place, as a polygon's do.
        if let PadShape::Custom { outline } = &shape {
            self.placed = self.placed.saturating_add(outline.edges.len().saturating_mul(3));
        }
        let mut pad = Pad::through_hole(&pad_name(part)?, point(part, "x", "y")?, shape, Hole::round(drill, true));
        pad.rotation = import::degrees(rot(part).0).ok()?;
        pad.copper_offset = offset;
        let mask = match part.attr("stop") {
            Some("no") => Opening::Closed,
            _ => Rules::opening(self.rules.stop, diameter, false, self.board.design.mask_expansion),
        };
        for face in [&mut pad.front, &mut pad.back].into_iter().flatten() {
            face.mask = mask;
        }
        Some(pad)
    }

    /// The net of a `signal`, made the first time the signal is met; none for a nameless one.
    fn net_of(&mut self, signal: &Element) -> Option<NetId> {
        let name: String = signal.attr("name").unwrap_or_default().chars().take(MAX_NAME_CHARS).collect();
        (!name.trim().is_empty()).then(|| self.net(name))
    }

    /// The `contactref`s of a signal: the pads, by element and pad name, that are on its net.
    fn contacts(&mut self, signal: &'a Element<'a>) {
        let Some(net) = self.net_of(signal) else { return };
        for contact in signal.all("contactref") {
            if let (Some(element), Some(pad)) = (contact.attr("element"), contact.attr("pad")) {
                self.on_net.insert((element, pad.trim()), net);
            }
        }
    }

    /// A `signal`: the tracks, vias and zones of its net.
    fn signal(&mut self, signal: &Element) {
        let net = self.net_of(signal);
        for part in &signal.children {
            let read = match part.name {
                "wire" => self.track(part, net),
                "via" => self.via(part, net),
                "polygon" => self.zone(part, net),
                _ => Some(()),
            };
            if read.is_none() {
                self.notes.add(&format!("{} of a signal that could not be read left out", part.name));
            }
        }
    }

    fn net(&mut self, name: String) -> NetId {
        if let Some(&id) = self.net_names.get(&name) {
            return id;
        }
        let id = self.ids.net();
        if self.board.nets.insert(Net { id, name: name.clone() }).is_err() {
            self.notes.add("net beyond the most a board can hold left out");
        }
        self.net_names.insert(name, id);
        id
    }

    fn track(&mut self, part: &Element, net: Option<NetId>) -> Option<()> {
        if part.number("layer") == Some(number::UNROUTED as f64) {
            return Some(()); // a connection still to be routed, not copper
        }
        let layer = self.layer(part).filter(|&id| self.is_copper(id))?;
        let (from, to) = (point(part, "x1", "y1")?, point(part, "x2", "y2")?);
        let path = match import::chord_arc(from, to, part.number("curve").unwrap_or(0.0)) {
            Some(arc) => TrackPath::Arc { arc },
            None => TrackPath::Line { from, to },
        };
        let width = length(part, "width").filter(|w| w.0 > 0)?;
        self.board.tracks.insert(Track { id: self.ids.item(), layer, width, path, net }).ok().map(|_| ())
    }

    fn via(&mut self, part: &Element, net: Option<NetId>) -> Option<()> {
        let drill = length(part, "drill").filter(|d| d.0 > 0)?;
        let ring = self.rules.via_ring.map_or(Nm(drill.0 / 4), |share| share.for_size(drill));
        let diameter = match length(part, "diameter").filter(|d| *d > drill) {
            Some(diameter) => diameter,
            None => import::size(Nm(drill.0.saturating_add(ring.0.max(1).saturating_mul(2))))?,
        };
        // `extent="1-16"`: the first and last copper layer the via joins.
        let ends: Vec<LayerId> = part
            .attr("extent")
            .unwrap_or("1-16")
            .split('-')
            .filter_map(|n| n.trim().parse::<i64>().ok())
            .filter_map(|n| self.by_number.get(&n).copied().filter(|&id| self.is_copper(id)))
            .collect();
        let (from, to) = match ends.as_slice() {
            [from, to] => (*from, *to),
            _ => (*self.standard.copper.first()?, *self.standard.copper.last()?),
        };
        // The design rules cover small vias with solder mask unless the via says otherwise.
        let covered = part.attr("alwaysstop") != Some("yes") && self.rules.via_stop_limit.is_some_and(|limit| drill <= limit);
        let mask = if covered { Opening::Closed } else { Opening::Rules };
        let via = Via { id: self.ids.item(), position: point(part, "x", "y")?, diameter, drill, from, to, top_mask: mask, bottom_mask: mask, net };
        self.board.vias.insert(via).ok().map(|_| ())
    }

    /// A signal's `polygon`: a zone to pour, or with `pour="cutout"` an area zones stay out of.
    fn zone(&mut self, part: &Element, net: Option<NetId>) -> Option<()> {
        let layer = self.layer(part).filter(|&id| self.is_copper(id))?;
        let outline = contour(part)?;
        let width = length(part, "width").filter(|w| w.0 >= 0).unwrap_or(Nm(0));
        let pour = part.attr("pour").unwrap_or("solid");
        if pour == "cutout" {
            let area = Keepout { id: self.ids.item(), layer, outline, tracks: false, vias: false, zones: true };
            return self.board.keepouts.insert(area).ok().map(|_| ());
        }
        let gap = length(part, "spacing").map(|spacing| Nm(spacing.0 - width.0)).filter(|gap| gap.0 > 0);
        let relief = match (self.rules.thermal_gap, width.0 > 0) {
            (Some(gap), true) => PadConnection::Relief { gap, spoke: width },
            _ => PadConnection::Solid,
        };
        let zone = Zone {
            id: self.ids.item(),
            layer,
            net,
            outline,
            holes: Vec::new(),
            clearance: length(part, "isolate").filter(|c| c.0 >= 0).unwrap_or(Nm(0)),
            min_width: width,
            // Rank 1 is poured first and 6 last; the model pours the highest priority first.
            priority: 6_u32.saturating_sub(part.number("rank").map_or(1, |r| r.clamp(1.0, 6.0) as u32)),
            fill: match (pour, gap) {
                ("hatch", Some(gap)) if width.0 > 0 => ZoneFill::Hatched { width, gap },
                _ => ZoneFill::Solid,
            },
            pads: if part.attr("thermals") == Some("no") { PadConnection::Solid } else { relief },
            keep_islands: part.attr("orphans") == Some("yes"),
            // An EAGLE file holds no poured copper: the program pours when it opens the board.
            filled: Vec::new(),
        };
        self.board.zones.insert(zone).ok().map(|_| ())
    }
}

/// A pad's name, without the space around it and no longer than a name may be: cut before it is
/// copied, so a long name in a package placed many times is not copied whole each time.
fn pad_name(part: &Element) -> Option<String> {
    Some(part.attr("name")?.trim().chars().take(MAX_NAME_CHARS).collect())
}

/// A regular octagon `across` wide between opposite sides, around the origin.
fn octagon(across: Nm) -> Option<Contour> {
    let half = across.0 / 2;
    let corner = ((half as f64) * (std::f64::consts::PI / 8.0).tan()) as i64;
    let corners =
        [(half, -corner), (half, corner), (corner, half), (-corner, half), (-half, corner), (-half, -corner), (-corner, -half), (corner, -half)];
    import::polygon(corners.into_iter().map(|(x, y)| Point::new(Nm(x), Nm(y))).collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use circuitcraft_geom::Arc;
    use circuitcraft_model::{PadFace, Slot};

    const MM: i64 = 1_000_000;

    /// A point given in micrometres, to keep the tests in whole numbers.
    fn um(x: i64, y: i64) -> Point {
        Point::new(Nm(x * 1_000), Nm(y * 1_000))
    }

    /// A small board as EAGLE 9 writes one: an outline with a rounded corner (one side drawn
    /// twice), a resistor turned a quarter, a mirrored and smashed connector, a signal with
    /// tracks, vias, a zone and a cutout.
    pub(crate) const BOARD: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<!DOCTYPE eagle SYSTEM "eagle.dtd">
<eagle version="9.6.2">
<drawing>
<settings><setting alwaysvectorfont="no"/></settings>
<grid distance="0.1" unitdist="inch" unit="inch"/>
<layers>
<layer number="1" name="Top" color="4" fill="1" visible="yes" active="yes"/>
<layer number="16" name="Bottom" color="1" fill="1" visible="yes" active="yes"/>
<layer number="20" name="Dimension" color="15" fill="1" visible="yes" active="yes"/>
<layer number="21" name="tPlace" color="7" fill="1" visible="yes" active="yes"/>
<layer number="47" name="Measures" color="7" fill="1" visible="yes" active="yes"/>
</layers>
<board>
<plain>
<wire x1="0" y1="0" x2="30" y2="0" width="0" layer="20"/>
<wire x1="30" y1="0" x2="30" y2="18" width="0" layer="20"/>
<wire x1="30" y1="18" x2="28" y2="20" width="0" layer="20" curve="90"/>
<wire x1="28" y1="20" x2="0" y2="20" width="0" layer="20"/>
<wire x1="0" y1="20" x2="0" y2="0" width="0" layer="20"/>
<wire x1="0" y1="0" x2="0" y2="20" width="0" layer="20"/>
<text x="2" y="2" size="1.27" layer="21" ratio="10" rot="R90" align="center">Rev &amp; 1</text>
<hole x="3" y="3" drill="3.2"/>
<wire x1="1" y1="1" x2="2" y2="1" width="0.1" layer="47"/>
<rectangle x1="10" y1="10" x2="12" y2="11" layer="43"/>
<dimension x1="0" y1="0" x2="1" y2="1" x3="2" y3="2" layer="47"/>
</plain>
<libraries>
<library name="parts">
<packages>
<package name="R0603">
<description>Chip &lt;b&gt;resistor&lt;/b&gt;</description>
<smd name="1" x="-0.8" y="0" dx="0.9" dy="1" layer="1"/>
<smd name="2" x="0.8" y="0" dx="0.9" dy="1" layer="1" roundness="50" stop="no" cream="no"/>
<wire x1="-1" y1="0.6" x2="1" y2="0.6" width="0.127" layer="21"/>
<text x="0" y="1" size="1" layer="25" align="bottom-center">&gt;NAME</text>
<text x="0" y="-2" size="1" layer="27">&gt;VALUE</text>
</package>
<package name="HDR">
<pad name="A" x="0" y="0" drill="1"/>
<pad name="B" x="2.54" y="0" drill="1" diameter="1.9" shape="long" rot="R90"/>
<pad name="C" x="5.08" y="0" drill="1" diameter="1.9" shape="octagon" stop="no"/>
<hole x="8" y="0" drill="2"/>
<rectangle x1="-1" y1="-1" x2="1" y2="1" layer="51"/>
<circle x="0" y="0" radius="0.5" width="0" layer="21"/>
<text x="0" y="2" size="1" layer="25">&gt;NAME</text>
<text x="0" y="-3" size="1" layer="21">PIN 1</text>
</package>
</packages>
</library>
</libraries>
<designrules name="demo">
<param name="layerSetup" value="(1*16)"/>
<param name="mdWireWire" value="6mil"/>
<param name="rvPadTop" value="0.25"/>
<param name="rlMinPadTop" value="10mil"/>
<param name="rlMaxPadTop" value="20mil"/>
<param name="rvViaOuter" value="0.25"/>
<param name="rlMinViaOuter" value="8mil"/>
<param name="rlMaxViaOuter" value="20mil"/>
<param name="mvStopFrame" value="1"/>
<param name="mlMinStopFrame" value="4mil"/>
<param name="mlMaxStopFrame" value="4mil"/>
<param name="mvCreamFrame" value="0"/>
<param name="mlMinCreamFrame" value="0mil"/>
<param name="mlMaxCreamFrame" value="0mil"/>
<param name="mlViaStopLimit" value="0.5mm"/>
<param name="slThermalIsolate" value="10mil"/>
</designrules>
<elements>
<element name="R1" library="parts" package="R0603" value="10k" x="10" y="5" rot="R90"/>
<element name="J1" library="parts" package="HDR" value="" x="20" y="5" smashed="yes" rot="MR0">
<attribute name="NAME" x="20" y="8" size="1" layer="26" rot="MR0"/>
<attribute name="PROD_ID" value="X-1" x="20" y="5" size="1" layer="27" display="off"/>
</element>
<element name="X9" library="parts" package="MISSING" value="" x="0" y="0"/>
</elements>
<signals>
<signal name="GND">
<contactref element="R1" pad="1"/>
<contactref element="J1" pad="A"/>
<wire x1="10" y1="4.2" x2="20" y2="5" width="0.25" layer="1"/>
<wire x1="20" y1="5" x2="22" y2="7" width="0.25" layer="16" curve="-90"/>
<wire x1="10" y1="4.2" x2="20" y2="5" width="0" layer="19"/>
<via x="20" y="5" extent="1-16" drill="0.4"/>
<via x="25" y="5" extent="1-16" drill="0.8" diameter="1.6" alwaysstop="yes"/>
<polygon width="0.3" layer="16" isolate="0.4" rank="2" thermals="yes">
<vertex x="1" y="1"/>
<vertex x="29" y="1"/>
<vertex x="29" y="19" curve="90"/>
<vertex x="1" y="19"/>
</polygon>
<polygon width="0.2" layer="1" pour="cutout">
<vertex x="5" y="5"/>
<vertex x="8" y="5"/>
<vertex x="8" y="8"/>
</polygon>
</signal>
</signals>
</board>
</drawing>
</eagle>
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
        assert_eq!(
            notes.lines(),
            [
                "dimension left out",
                "drawing on the outline layer that repeats another left out",
                "element attribute left out",
                "element whose package the file doesn't hold left out",
                "text anchored at a height of its own: the anchoring from top to bottom is left out",
            ]
        );
        assert_eq!((board.name.as_str(), board.stack.copper_count()), ("Demo", 2));
        assert_eq!((board.nets.len(), board.footprints.len(), board.tracks.len(), board.vias.len()), (1, 2, 2, 2));
        assert_eq!((board.zones.len(), board.keepouts.len(), board.holes.len(), board.texts.len(), board.graphics.len()), (1, 3, 1, 1, 1));
        // The design rules the board's settings come from.
        let d = board.design;
        assert_eq!((d.clearance, d.mask_expansion, d.paste_expansion, d.tent_vias), (Nm(152_400), Nm(101_600), Nm(0), false));
        // The outline: five wires, one of them an arc, once the wire drawn twice is dropped.
        let outline = board.outline.as_ref().expect("outline");
        assert_eq!((outline.start, outline.edges.len(), board.cutouts.len()), (um(0, 0), 5, 0));
        assert_eq!(outline.edges.iter().filter(|e| matches!(e, Edge::Arc { .. })).count(), 1);
        // On the board itself: the text, the hole, a line on a layer the model has no kind for,
        // and vias kept out of a rectangle on both copper layers.
        let text = board.texts.iter().next().expect("text");
        assert_eq!(
            (text.text.as_str(), text.position, text.rotation, text.justify),
            ("Rev & 1", um(2_000, 2_000), Rotation(90_000), Justify::Center)
        );
        assert_eq!((text.height, text.stroke, layer(&board, text.layer)), (Nm(1_270_000), Nm(127_000), "Top Silkscreen"));
        assert_eq!(board.holes.iter().map(|h| (h.position, h.hole)).collect::<Vec<_>>(), [(um(3_000, 3_000), Hole::round(Nm(3_200_000), false))]);
        assert_eq!(board.graphics.iter().map(|g| layer(&board, g.layer)).collect::<Vec<_>>(), ["Measures"]);
        let mut keepouts: Vec<_> =
            board.keepouts.iter().map(|k| (layer(&board, k.layer), k.tracks, k.vias, k.zones, k.outline.edges.len())).collect();
        keepouts.sort_unstable();
        assert_eq!(
            keepouts,
            [("Bottom Copper", false, true, false, 3), ("Top Copper", false, false, true, 3), ("Top Copper", false, true, false, 3)]
        );
    }

    #[test]
    fn elements_become_footprints_with_their_packages() {
        let Imported { board, .. } = read_board(BOARD, "Demo").expect("import");
        let gnd = board.net_by_name("GND").map(|n| n.id);
        // The resistor: a quarter turn; pad 1 ends up where the track starts.
        let r1 = footprint(&board, "R1");
        assert_eq!((r1.position, r1.rotation, r1.side, r1.value.as_str()), (um(10_000, 5_000), Rotation(90_000), Side::Top, "10k"));
        assert_eq!(r1.link, Some(LibraryLink { library: "parts".to_owned(), footprint: "R0603".to_owned() }));
        assert_eq!(r1.assembly.mounting, Mounting::SurfaceMount);
        let [one, two] = r1.pads.as_slice() else { panic!("two pads") };
        assert_eq!((one.number.as_str(), one.position, one.net, two.net), ("1", um(-800, 0), gnd, None));
        assert_eq!(r1.to_board(one.position), Some(um(10_000, 4_200)));
        // Openings the rules give as the settings have them are left to the settings; a pad that
        // says it has none has none.
        let plain = PadShape::Rect { width: Nm(900_000), height: Nm(MM) };
        assert_eq!(one.front, Some(PadFace { shape: plain, mask: Opening::Rules, paste: Opening::Rules }));
        let rounded = PadShape::RoundRect { width: Nm(900_000), height: Nm(MM), radius: Nm(225_000) };
        assert_eq!(two.front, Some(PadFace { shape: rounded, mask: Opening::Closed, paste: Opening::Closed }));
        let texts: Vec<_> = r1.texts.iter().map(|t| (t.content.clone(), layer(&board, t.layer), t.position, t.justify)).collect();
        assert_eq!(
            texts,
            [(TextContent::Reference, "Top Names", um(0, 1_000), Justify::Center), (TextContent::Value, "Top Values", um(0, -2_000), Justify::Left),]
        );
        assert_eq!(r1.graphics.iter().map(|g| (layer(&board, g.layer), g.width)).collect::<Vec<_>>(), [("Top Silkscreen", Nm(127_000))]);

        // The connector: mirrored to the bottom. Its pads are drilled, with copper from the rules
        // where the package gives no diameter.
        let j1 = footprint(&board, "J1");
        assert_eq!((j1.position, j1.rotation, j1.side, j1.assembly.mounting), (um(20_000, 5_000), Rotation(0), Side::Bottom, Mounting::ThroughHole));
        let [a, b, c, hole] = j1.pads.as_slice() else { panic!("four pads") };
        // A quarter of the 1 mm drill is less than the smallest ring of 10 mil.
        let round = PadShape::Circle { diameter: Nm(1_508_000) };
        assert_eq!((a.front.as_ref().map(|f| &f.shape), a.inner.as_ref(), a.net), (Some(&round), Some(&round), gnd));
        assert_eq!(a.hole, Some(Hole { diameter: Nm(MM), slot: None::<Slot>, plated: true }));
        assert_eq!(j1.to_board(a.position), Some(um(20_000, 5_000)));
        // A long pad is twice as long as wide, turned with the pad.
        assert_eq!(b.front.as_ref().map(|f| &f.shape), Some(&PadShape::Oval { width: Nm(3_800_000), height: Nm(1_900_000) }));
        assert_eq!((b.rotation, j1.to_board(b.position)), (Rotation(90_000), Some(um(17_460, 5_000))), "mirrored: to the left");
        assert!(
            matches!(c.front.as_ref(), Some(PadFace { shape: PadShape::Custom { outline }, mask: Opening::Closed, .. }) if outline.edges.len() == 7)
        );
        assert_eq!((hole.number.as_str(), hole.hole, hole.front.is_some()), ("", Some(Hole::round(Nm(2 * MM), false)), false));
        // Smashed: the name is where the element's attribute puts it on the board, held on the
        // top layer's counterpart and relative to the footprint; the package's own is not shown.
        let texts: Vec<_> = j1.texts.iter().map(|t| (t.content.clone(), layer(&board, t.layer), t.position, t.mirrored)).collect();
        assert_eq!(
            texts,
            [
                (TextContent::Fixed("PIN 1".to_owned()), "Top Silkscreen", um(0, -3_000), false),
                (TextContent::Reference, "Top Names", um(0, 3_000), false),
            ]
        );
        assert_eq!(j1.graphics.iter().map(|g| layer(&board, g.layer)).collect::<Vec<_>>(), ["Top Fabrication", "Top Silkscreen"]);
        assert!(j1.graphics.iter().all(|g| matches!(g.shape, GraphicShape::Polygon { filled: true, .. })));
    }

    #[test]
    fn a_signal_brings_its_tracks_vias_and_zones() {
        let Imported { board, .. } = read_board(BOARD, "Demo").expect("import");
        let gnd = board.net_by_name("GND").map(|n| n.id);
        let mut tracks: Vec<_> = board.tracks.iter().map(|t| (layer(&board, t.layer), t.width, t.path, t.net)).collect();
        tracks.sort_by_key(|t| t.0);
        // A quarter turn clockwise from (20, 5) to (22, 7): about the centre (22, 5), up its left.
        let arc = Arc::new(um(20_000, 5_000), Point::new(Nm(20_585_786), Nm(6_414_214)), um(22_000, 7_000));
        assert_eq!(
            tracks,
            [
                ("Bottom Copper", Nm(250_000), TrackPath::Arc { arc }, gnd),
                ("Top Copper", Nm(250_000), TrackPath::Line { from: um(10_000, 4_200), to: um(20_000, 5_000) }, gnd),
            ]
        );
        let mut vias: Vec<_> = board.vias.iter().map(|v| (v.position, v.diameter, v.drill, v.top_mask, v.bottom_mask, v.net)).collect();
        vias.sort_by_key(|v| v.0.x);
        // The small via gets the smallest ring of 8 mil and, drilled under the limit, is covered.
        assert_eq!(
            vias,
            [
                (um(20_000, 5_000), Nm(806_400), Nm(400_000), Opening::Closed, Opening::Closed, gnd),
                (um(25_000, 5_000), Nm(1_600_000), Nm(800_000), Opening::Rules, Opening::Rules, gnd),
            ]
        );
        let zone = board.zones.iter().next().expect("zone");
        assert_eq!(
            (layer(&board, zone.layer), zone.net, zone.clearance, zone.min_width, zone.priority),
            ("Bottom Copper", gnd, Nm(400_000), Nm(300_000), 4)
        );
        assert_eq!(
            (zone.fill, zone.pads, zone.keep_islands),
            (ZoneFill::Solid, PadConnection::Relief { gap: Nm(254_000), spoke: Nm(300_000) }, false)
        );
        assert_eq!((zone.outline.start, zone.outline.edges.len()), (um(1_000, 1_000), 4));
        assert!(matches!(zone.outline.edges.get(2), Some(Edge::Arc { to, .. }) if *to == um(1_000, 19_000)), "the curved side");
        assert!(zone.filled.is_empty());
    }

    #[test]
    fn what_is_not_an_eagle_board_is_refused() {
        assert_eq!(read_board("<svg></svg>", "x"), Err(ImportError::WrongFormat("an EAGLE file")));
        assert!(matches!(read_board("<eagle version=\"9\"><drawing><schematic/></drawing></eagle>", "x"), Err(ImportError::WrongFormat(_))));
        assert!(matches!(read_board("<eagle version=\"9\"></eagle>", "x"), Err(ImportError::WrongFormat(_))));
        assert!(matches!(read_board("<eagle>", "x"), Err(ImportError::Syntax { .. })));
        // A board with nothing in it is still a board.
        let empty = read_board("<eagle version=\"9\"><drawing><board/></drawing></eagle>", "").expect("import");
        assert_eq!((empty.board.name.as_str(), empty.board.stack.copper_count(), empty.notes.is_empty()), ("Imported", 2, true));
    }

    #[test]
    fn sizes_left_to_the_design_rules_have_fallbacks_and_odd_items_are_left_out() {
        let board = |plain: &str, signal: &str| {
            let text = format!(
                "<eagle version=\"9\"><drawing><board><plain>{plain}</plain><designrules><param name=\"layerSetup\" value=\"(1*2*15*16)\"/></designrules>\
                 <signals><signal name=\"N\">{signal}</signal></signals></board></drawing></eagle>"
            );
            read_board(&text, "x").expect("import")
        };
        // Without ring rules a via gets a quarter of its drill as its ring, and is not covered.
        let read = board("", "<via x=\"0\" y=\"0\" extent=\"2-15\" drill=\"0.4\"/>");
        let via = read.board.vias.iter().next().expect("via");
        assert_eq!((via.diameter, via.top_mask), (Nm(600_000), Opening::Rules));
        assert_eq!((layer(&read.board, via.from), layer(&read.board, via.to)), ("Inner Copper 1", "Inner Copper 2"));
        assert_eq!(read.board.stack.copper_count(), 4);
        for (plain, signal, note) in [
            ("", "<wire x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" width=\"0\" layer=\"1\"/>", "wire of a signal that could not be read left out"),
            ("", "<wire x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" width=\"0.2\" layer=\"3\"/>", "wire of a signal that could not be read left out"),
            ("", "<wire x1=\"0\" y1=\"0\" x2=\"1e30\" y2=\"0\" width=\"0.2\" layer=\"1\"/>", "wire of a signal that could not be read left out"),
            ("", "<via x=\"0\" y=\"0\" extent=\"1-16\" drill=\"0\"/>", "via of a signal that could not be read left out"),
            (
                "",
                "<polygon width=\"0.2\" layer=\"1\"><vertex x=\"0\" y=\"0\"/><vertex x=\"1\" y=\"0\"/></polygon>",
                "polygon of a signal that could not be read left out",
            ),
            ("<hole x=\"0\" y=\"0\" drill=\"-1\"/>", "", "hole that could not be read left out"),
            ("<circle x=\"0\" y=\"0\" radius=\"0\" width=\"0.1\" layer=\"21\"/>", "", "circle that could not be read left out"),
            ("<wire x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" width=\"0.1\" layer=\"17\"/>", "", "wire that could not be read left out"),
            (
                "<wire x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" width=\"0.1\" layer=\"43\"/>",
                "",
                "line on the via keepout layer left out: only closed shapes keep vias out",
            ),
            ("<frame x1=\"0\" y1=\"0\" x2=\"1\" y2=\"1\" columns=\"1\" rows=\"1\" layer=\"47\"/>", "", "frame left out"),
        ] {
            let read = board(plain, signal);
            assert_eq!(read.notes.lines(), [note], "{plain}{signal}");
            assert_eq!(read.board.validate(), Ok(()));
        }
        // Text that is mirrored and turned: the model mirrors first, so it holds the turn the
        // other way round.
        let read = board(
            "<text x=\"0\" y=\"0\" size=\"1\" layer=\"22\" rot=\"MR90\">B</text><text x=\"0\" y=\"0\" size=\"1\" layer=\"21\" rot=\"R90\">T</text>",
            "",
        );
        let mut texts: Vec<_> = read.board.texts.iter().map(|t| (t.text.as_str(), t.rotation, t.mirrored)).collect();
        texts.sort_unstable();
        assert_eq!(texts, [("B", Rotation(270_000), true), ("T", Rotation(90_000), false)]);
        // A smashed name far from its element is beyond where a footprint's text can be.
        let far = "<eagle version=\"9\"><drawing><board><libraries><library name=\"l\"><packages><package name=\"p\"/></packages></library></libraries>\
            <elements><element name=\"E\" library=\"l\" package=\"p\" value=\"\" x=\"9999\" y=\"0\" smashed=\"yes\">\
            <attribute name=\"NAME\" x=\"-9999\" y=\"0\" size=\"1\" layer=\"25\"/></element></elements></board></drawing></eagle>";
        let read = read_board(far, "x").expect("import");
        assert_eq!(read.notes.lines(), ["element attribute that could not be read left out"]);
        // Layers that come in pairs are made as pairs the first time either is drawn on.
        let read = board(
            "<wire x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" width=\"0.1\" layer=\"36\"/><wire x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" width=\"0.1\" layer=\"200\"/>",
            "",
        );
        let drawn: Vec<_> = read.board.graphics.iter().map(|g| layer(&read.board, g.layer)).collect();
        assert!(drawn.contains(&"Bottom Glue") && drawn.contains(&"Layer 200") && read.board.stack.by_name("Top Glue").is_some(), "{drawn:?}");
        // Lengths as the rules write them.
        assert_eq!(
            [measure("5mil"), measure(" 0.035mm "), measure("2"), measure("1inch"), measure("10mic")],
            [Some(Nm(127_000)), Some(Nm(35_000)), Some(Nm(2 * MM)), Some(Nm(25_400_000)), Some(Nm(10_000))]
        );
        assert_eq!([measure("mil"), measure("nanmm"), measure("1e30mm"), measure("")], [None; 4]);
    }

    #[test]
    fn a_small_file_cannot_place_a_large_package_without_end() {
        let board = |package: &str, elements: usize| {
            format!(
                "<eagle version=\"9\"><drawing><board><libraries><library name=\"l\"><packages><package name=\"p\">{package}</package></packages></library></libraries>\
                 <elements>{}</elements></board></drawing></eagle>",
                "<element name=\"E\" library=\"l\" package=\"p\" value=\"\" x=\"0\" y=\"0\"/>".repeat(elements)
            )
        };
        let refused = |text: &str| matches!(read_board(text, "x"), Err(ImportError::Unsupported(why)) if why.contains("1000000"));
        // A package of 999 parts weighs a thousand with itself: a thousand copies fit, one more
        // does not.
        let parts = "<description/>".repeat(999);
        assert_eq!(read_board(&board(&parts, 1_000), "x").map(|read| read.board.footprints.len()), Ok(1_000));
        assert!(refused(&board(&parts, 1_001)));
        // One polygon is one part, and every corner of it counts: 4,998 corners make the
        // package weigh 5,000.
        let polygon = format!("<polygon width=\"0\" layer=\"21\">{}</polygon>", "<vertex x=\"0\" y=\"0\"/>".repeat(4_998));
        assert!(read_board(&board(&polygon, 200), "x").is_ok());
        assert!(refused(&board(&polygon, 201)));
        // Vias kept out of a shape are kept out on every copper layer: those copies count too.
        let corners: String = (0..70_000).map(|i| format!("<vertex x=\"{}\" y=\"{}\"/>", i % 300, i / 300)).collect();
        let keepout = format!("<polygon width=\"0\" layer=\"43\">{corners}</polygon>");
        let setup = |copper: &str| {
            format!(
                "<eagle version=\"9\"><drawing><board><plain>{keepout}</plain><designrules><param name=\"layerSetup\" value=\"{copper}\"/></designrules></board></drawing></eagle>"
            )
        };
        assert_eq!(read_board(&setup("(1*16)"), "x").map(|read| read.board.keepouts.len()), Ok(2));
        // On sixteen layers this one shape is more than a board may place: it is left out.
        let sixteen = read_board(&setup("(1*2*3*4*5*6*7*8*9*10*11*12*13*14*15*16)"), "x").expect("import");
        assert_eq!(
            (sixteen.board.keepouts.len(), sixteen.notes.lines()),
            (0, vec!["via keepout beyond what one board may place left out".to_owned()])
        );
        // Small shapes add up the same way: a triangle on two layers costs eight, so 125,000 of
        // them fit and the rest are left out.
        let triangles =
            "<polygon width=\"0\" layer=\"43\"><vertex x=\"0\" y=\"0\"/><vertex x=\"1\" y=\"0\"/><vertex x=\"1\" y=\"1\"/></polygon>".repeat(125_010);
        let many =
            read_board(&format!("<eagle version=\"9\"><drawing><board><plain>{triangles}</plain></board></drawing></eagle>"), "x").expect("import");
        assert_eq!(
            (many.board.keepouts.len(), many.notes.lines()),
            (250_000, vec!["10 × via keepout beyond what one board may place left out".to_owned()])
        );
        // A pad's name is cut to what a name may be before it is copied.
        let named = format!("<smd name=\"{}\" x=\"0\" y=\"0\" dx=\"1\" dy=\"1\" layer=\"1\"/>", "n".repeat(100_000));
        let read = read_board(&board(&named, 2), "x").expect("import");
        assert!(
            read.board.footprints.iter().all(|f| f.pads.iter().all(|p| p.number.len() == MAX_NAME_CHARS && p.number.capacity() < 2 * MAX_NAME_CHARS))
        );
        // And a long name weighs, like long text: 64,000 characters make a part a thousand heavier.
        assert!(refused(&board(&format!("<smd name=\"{}\" x=\"0\" y=\"0\" dx=\"1\" dy=\"1\" layer=\"1\"/>", "n".repeat(64_000)), 1_000)));
        let wordy = format!("<description>{}</description>", "x".repeat(64 * 998));
        assert!(read_board(&board(&wordy, 1_000), "x").is_ok() && refused(&board(&wordy, 1_001)));
        // An octagon pad holds its seven edges three times over: 999 of them make a copy of
        // the package weigh 1,000 + 999 × 21.
        let octagons: String =
            (0..999).map(|n| format!("<pad name=\"{n}\" x=\"0\" y=\"0\" drill=\"1\" diameter=\"2\" shape=\"octagon\"/>")).collect();
        assert_eq!(read_board(&board(&octagons, 45), "x").map(|read| read.board.footprints.len()), Ok(45));
        assert!(refused(&board(&octagons, 46)));
        // One package of 100,000 such pads would place 2,200,000. It is refused, and its
        // conversion stops at the pad that passes what a board may place, not after the last.
        let crowd = board(&"<pad name=\"1\" x=\"0\" y=\"0\" drill=\"1\" diameter=\"2\" shape=\"octagon\"/>".repeat(100_000), 1);
        assert!(refused(&crowd));
        let root = xml::parse(&crowd).expect("xml");
        let drawing = root.child("drawing").expect("drawing");
        let inner = drawing.child("board").expect("board");
        let mut eagle = Eagle::new(&crowd, "x", drawing, inner);
        let element = inner.child("elements").and_then(|elements| elements.children.first()).expect("element");
        assert_eq!(eagle.element(element), Some(()));
        assert!((MAX_PLACED + 1..=MAX_PLACED + 21).contains(&eagle.placed), "placed {}", eagle.placed);
        assert!(eagle.board.footprints.is_empty());
        // Short attributes weigh nothing, however many a part has: what counts is what the
        // board gets.
        let junk: String = (0..60).map(|i| format!(" a{i}=\"1\"")).collect();
        assert!(read_board(&board(&format!("<description{junk}/>"), 100_000), "x").is_ok());
    }

    #[test]
    fn a_large_board_of_ordinary_parts_is_within_what_elements_may_place() {
        // Four hundred 256-pad arrays and ten thousand two-pad parts with their outlines and
        // names, each pad and line written with every attribute a file gives one: far more
        // than any board in the corpus, and a quarter of what a board may place.
        let pad = |n: usize| {
            format!(
                "<smd name=\"{n}\" x=\"{}\" y=\"{}\" dx=\"0.4\" dy=\"0.4\" layer=\"1\" roundness=\"100\" rot=\"R0\" stop=\"yes\" thermals=\"yes\" cream=\"yes\"/>",
                n % 16,
                n / 16
            )
        };
        let array: String = (0..256).map(pad).collect();
        let line = "<wire x1=\"-1\" y1=\"0.5\" x2=\"1\" y2=\"0.5\" width=\"0.127\" layer=\"21\" style=\"continuous\" curve=\"0\" cap=\"round\"/>";
        let chip = format!(
            "{}{}{}<text x=\"0\" y=\"1\" size=\"1\" layer=\"25\">&gt;NAME</text><text x=\"0\" y=\"-1\" size=\"1\" layer=\"27\">&gt;VALUE</text>",
            pad(1),
            pad(2),
            line.repeat(6)
        );
        let place = |name: &str, package: &str, n: usize| {
            format!("<element name=\"{name}{n}\" library=\"l\" package=\"{package}\" value=\"v\" x=\"{}\" y=\"{}\" rot=\"R90\"/>", n % 100, n / 100)
        };
        let elements: String = (0..400).map(|n| place("U", "array", n)).chain((0..10_000).map(|n| place("R", "chip", n))).collect();
        let text = format!(
            "<eagle version=\"9\"><drawing><board><libraries><library name=\"l\"><packages><package name=\"array\">{array}</package><package name=\"chip\">{chip}</package>\
             </packages></library></libraries><elements>{elements}</elements></board></drawing></eagle>"
        );
        let read = read_board(&text, "x").expect("a large board imports");
        assert_eq!(read.board.footprints.len(), 10_400);
        assert_eq!(read.board.footprints.iter().map(|f| f.pads.len()).sum::<usize>(), 400 * 256 + 10_000 * 2);
        assert!(read.notes.is_empty(), "{:?}", read.notes.lines());
    }

    /// A board where nothing lines up with anything: an element mirrored and turned, pads turned
    /// in their package, a pad on the far side, an offset pad, design rules that give every
    /// opening a size of its own, smashed names at angles, and a hatched zone.
    const AWKWARD: &str = r#"<eagle version="9"><drawing><board>
<plain>
<rectangle x1="0" y1="0" x2="4" y2="2" layer="21" rot="R90"/>
</plain>
<libraries><library name="l"><packages>
<package name="P">
<smd name=" 1 " x="1" y="0.5" dx="2" dy="1" layer="1" rot="R90"/>
<smd name="2" x="3" y="0" dx="2" dy="1" layer="16"/>
<pad name="3" x="6" y="0" drill="1" diameter="2" shape="offset"/>
<wire x1="0" y1="0" x2="2" y2="0" width="0" layer="20"/>
<text x="0" y="0" size="1" layer="21" rot="MR90">M</text>
<hole x="9" y="0" drill="1"/>
</package>
</packages></library></libraries>
<designrules name="r">
<param name="mvStopFrame" value="0.1"/>
<param name="mlMinStopFrame" value="0.05mm"/>
<param name="mlMaxStopFrame" value="0.15mm"/>
<param name="mvCreamFrame" value="0.1"/>
<param name="mlMinCreamFrame" value="0.02mm"/>
<param name="mlMaxCreamFrame" value="0.08mm"/>
<param name="mlViaStopLimit" value="0.5mm"/>
<param name="slThermalIsolate" value="0.3mm"/>
</designrules>
<elements>
<element name="U1" library="l" package="P" value="" x="10" y="10" rot="MR90" smashed="yes">
<attribute name="NAME" x="10" y="12" size="1" layer="26" rot="MR180"/>
<attribute name="VALUE" x="10" y="8" size="1" layer="28" rot="MR90" display="off"/>
</element>
<element name="U2" library="l" package="P" value="" x="30" y="10" rot="R90" smashed="yes">
<attribute name="NAME" x="30" y="12" size="1" layer="25" rot="R180"/>
<attribute name="VALUE" x="30" y="8" size="1" layer="27" rot="MR90"/>
</element>
</elements>
<signals><signal name="N">
<contactref element="U1" pad="1"/>
<contactref element="U1" pad=""/>
<contactref element="U2" pad=" 1 "/>
<via x="0" y="0" extent="1-16" drill="0.4" alwaysstop="yes"/>
<polygon width="0.2" layer="1" spacing="0.5" pour="hatch" thermals="no" orphans="yes"><vertex x="0" y="0"/><vertex x="5" y="0"/><vertex x="5" y="5"/></polygon>
</signal></signals>
</board></drawing></eagle>
"#;

    #[test]
    fn a_mirrored_element_is_turned_and_then_mirrored() {
        let Imported { board, notes } = read_board(AWKWARD, "x").expect("import");
        assert_eq!(
            notes.lines(),
            ["board outline left as drawings: a drawing on the outline layer is part of no closed shape; close it, allow a wider gap or remove it"]
        );
        // EAGLE turns the package a quarter and then mirrors it: the pad at (1, 0.5) goes to
        // (-0.5, 1) and from there to (0.5, 1) of the element at (10, 10). The model mirrors and
        // then turns, so it holds the turn the other way round.
        let u1 = footprint(&board, "U1");
        assert_eq!((u1.side, u1.rotation), (Side::Bottom, Rotation(270_000)));
        assert_eq!(u1.to_board(u1.pads[0].position), Some(um(10_500, 11_000)));
        // The package writes the pad's name with spaces around it; a name has none.
        assert_eq!((u1.pads[0].number.as_str(), u1.pads[0].net), ("1", board.net_by_name("N").map(|n| n.id)));
        // The signal also names a pad without a name: the package's hole is one, and takes no net.
        assert!(u1.pads[1].net.is_none() && u1.pads[2].net.is_none() && u1.pads[3].net.is_none() && u1.pads[3].number.is_empty());
        // The same package unmirrored: the pad goes to (-0.5, 1) of the element at (30, 10).
        let u2 = footprint(&board, "U2");
        assert_eq!((u2.side, u2.rotation, u2.to_board(u2.pads[0].position)), (Side::Top, Rotation(90_000), Some(um(29_500, 11_000))));
        assert_eq!(u2.pads[0].net, board.net_by_name("N").map(|n| n.id), "a pad named with spaces around its name");
        // A package's line on the outline layer is on the board where each element puts it: two
        // lines here, which close nothing.
        let mut lifted: Vec<_> = board.graphics.iter().filter(|g| layer(&board, g.layer) == "Outline").map(|g| g.shape.clone()).collect();
        lifted.sort_by_key(|shape| match shape {
            GraphicShape::Line { from, .. } => from.x,
            _ => Nm(0),
        });
        assert!(
            matches!(lifted.as_slice(), [GraphicShape::Line { from: a, to: b }, GraphicShape::Line { from: c, to: d }]
            if (*a, *b, *c, *d) == (um(10_000, 10_000), um(10_000, 12_000), um(30_000, 10_000), um(30_000, 12_000))),
            "{lifted:?}"
        );
        // The rectangle on the board, turned a quarter about its middle.
        let silk: Vec<_> = board.graphics.iter().filter(|g| layer(&board, g.layer) == "Top Silkscreen").collect();
        assert!(matches!(&silk[0].shape, GraphicShape::Polygon { outline, filled: true }
            if outline.flatten(Nm(1)).ok() == Some(vec![um(3_000, -1_000), um(3_000, 3_000), um(1_000, 3_000), um(1_000, -1_000)])));
    }

    #[test]
    fn smashed_names_are_held_against_their_element_on_either_side() {
        let Imported { board, .. } = read_board(AWKWARD, "x").expect("import");
        let texts = |reference: &str| {
            let f = footprint(&board, reference);
            f.texts.iter().map(|t| (t.content.clone(), layer(&board, t.layer), t.position, t.rotation, t.mirrored, t.visible)).collect::<Vec<_>>()
        };
        // On the bottom: the name two above the element is at (2, 0) of the package; at 180 in an
        // element at 90 it is turned a quarter against it; the value is hidden.
        // The package's own text, mirrored and turned a quarter in the package: the model
        // mirrors first, so it holds the turn the other way round.
        let fixed = (TextContent::Fixed("M".to_owned()), "Top Silkscreen", um(0, 0), Rotation(270_000), true, true);
        assert_eq!(
            texts("U1"),
            [
                fixed.clone(),
                (TextContent::Reference, "Top Names", um(2_000, 0), Rotation(90_000), false, true),
                (TextContent::Value, "Top Values", um(-2_000, 0), Rotation(0), false, false),
            ]
        );
        // On the top the same place and the same angles give the same. A value mirrored though
        // its element is not: its own quarter turn counts the other way, half a turn in all
        // against an element at a quarter (and none, if it counted the same way).
        assert_eq!(
            texts("U2"),
            [
                fixed,
                (TextContent::Reference, "Top Names", um(2_000, 0), Rotation(90_000), false, true),
                (TextContent::Value, "Top Values", um(-2_000, 0), Rotation(180_000), true, true),
            ]
        );
    }

    #[test]
    fn openings_and_shapes_follow_the_design_rules_and_the_package() {
        let Imported { board, .. } = read_board(AWKWARD, "x").expect("import");
        // The settings take the smallest opening the rules allow.
        assert_eq!((board.design.mask_expansion, board.design.paste_expansion), (Nm(50_000), Nm(-20_000)));
        let u2 = footprint(&board, "U2");
        let [one, two, three, _hole] = u2.pads.as_slice() else { panic!("three pads and a hole") };
        // A tenth of the pad's narrow side of 1 mm, within the rules' limits: 0.1 mm of mask
        // beyond the pad, and paste 0.08 mm inside it (the limit), each unlike the settings.
        let face = PadFace {
            shape: PadShape::Rect { width: Nm(2 * MM), height: Nm(MM) },
            mask: Opening::Open { by: Some(Nm(100_000)) },
            paste: Opening::Open { by: Some(Nm(-80_000)) },
        };
        assert_eq!((one.front.as_ref(), one.back.as_ref(), one.rotation), (Some(&face), None, Rotation(90_000)));
        // A pad the package draws on the bottom layer is on the far side of the board.
        assert_eq!((two.front.as_ref(), two.back.as_ref(), two.rotation), (None, Some(&face), Rotation(0)));
        // An offset pad: twice as long as wide, the hole half a width from the middle; a tenth of
        // its 2 mm is more mask than the rules allow, so it gets their largest.
        let long = PadFace {
            shape: PadShape::Oval { width: Nm(4 * MM), height: Nm(2 * MM) },
            mask: Opening::Open { by: Some(Nm(150_000)) },
            paste: Opening::Closed,
        };
        assert_eq!((three.front.as_ref(), three.back.as_ref(), three.copper_offset), (Some(&long), Some(&long), um(1_000, 0)));
        // A via that says it always has an opening is not covered, however small its drill.
        let via = board.vias.iter().next().expect("via");
        assert_eq!((via.diameter, via.top_mask, via.bottom_mask), (Nm(600_000), Opening::Rules, Opening::Rules));
        // A hatched zone: lines 0.2 mm wide every 0.5 mm; no thermals; islands kept; first rank.
        let zone = board.zones.iter().next().expect("zone");
        assert_eq!(zone.fill, ZoneFill::Hatched { width: Nm(200_000), gap: Nm(300_000) });
        assert_eq!((zone.pads, zone.keep_islands, zone.priority), (PadConnection::Solid, true, 5));
    }

    #[test]
    fn what_the_reader_works_out_beyond_the_design_range_is_left_out() {
        let board = |rules: &str, plain: &str, package: &str, signal: &str| {
            let text = format!(
                "<eagle version=\"9\"><drawing><board><plain>{plain}</plain><libraries><library name=\"l\"><packages><package name=\"p\">{package}</package></packages></library></libraries>\
                 <designrules>{rules}</designrules><elements><element name=\"E\" library=\"l\" package=\"p\" value=\"\" x=\"9999\" y=\"0\" rot=\"R45\"/></elements>\
                 <signals><signal name=\"N\">{signal}</signal></signals></board></drawing></eagle>"
            );
            read_board(&text, "x").expect("import")
        };
        let ring = "<param name=\"rvViaOuter\" value=\"1\"/><param name=\"rlMinViaOuter\" value=\"9000mm\"/><param name=\"rlMaxViaOuter\" value=\"9000mm\"/>";
        for (rules, plain, package, signal, note) in [
            // A rectangle whose corners turn out of range.
            (
                "",
                "<rectangle x1=\"-9999\" y1=\"-9999\" x2=\"9999\" y2=\"9999\" layer=\"21\" rot=\"R45\"/>",
                "",
                "",
                "rectangle that could not be read left out",
            ),
            // A package's outline drawing that its element puts out of range.
            (
                "",
                "",
                "<wire x1=\"0\" y1=\"0\" x2=\"5\" y2=\"0\" width=\"0\" layer=\"20\"/>",
                "",
                "package drawing that could not be placed on the board left out",
            ),
            // A long pad the rules make eleven times as long as it is wide, and a via whose ring
            // the rules make nine metres.
            (
                "<param name=\"psElongationLong\" value=\"1000\"/>",
                "",
                "<pad name=\"1\" x=\"0\" y=\"0\" drill=\"500\" diameter=\"1000\" shape=\"long\"/>",
                "",
                "package pad that could not be read left out",
            ),
            (ring, "", "", "<via x=\"0\" y=\"0\" extent=\"1-16\" drill=\"5000\"/>", "via of a signal that could not be read left out"),
        ] {
            let read = board(rules, plain, package, signal);
            assert_eq!(read.notes.lines(), [note], "{plain}{package}{signal}");
        }
    }

    #[test]
    fn a_stack_of_layers_and_a_crowd_of_contacts_are_read_quickly() {
        // Thousands of layers of one name, each drawn on: the stack fills up and the rest are
        // left out, without the time growing with the square of their number.
        let layers: String = (100..6_000).map(|n| format!("<layer number=\"{n}\" name=\"X\" color=\"1\" fill=\"1\"/>")).collect();
        let wires: String = (100..6_000).map(|n| format!("<wire x1=\"0\" y1=\"0\" x2=\"1\" y2=\"0\" width=\"0.1\" layer=\"{n}\"/>")).collect();
        let text = format!("<eagle version=\"9\"><drawing><layers>{layers}</layers><board><plain>{wires}</plain></board></drawing></eagle>");
        let start = std::time::Instant::now();
        let read = read_board(&text, "x").expect("import");
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "{:?}", start.elapsed());
        assert_eq!(read.board.stack.layers.len(), circuitcraft_model::MAX_LAYERS);
        assert_eq!(read.notes.lines().len(), 1, "{:?}", read.notes.lines());
        // One element with tens of thousands of pads, each on the signal.
        let pads: String = (0..40_000).map(|n| format!("<smd name=\"{n}\" x=\"0\" y=\"0\" dx=\"1\" dy=\"1\" layer=\"1\"/>")).collect();
        let contacts: String = (0..40_000).map(|n| format!("<contactref element=\"E\" pad=\"{n}\"/>")).collect();
        let text = format!(
            "<eagle version=\"9\"><drawing><board><libraries><library name=\"l\"><packages><package name=\"p\">{pads}</package></packages></library></libraries>\
             <elements><element name=\"E\" library=\"l\" package=\"p\" value=\"\" x=\"0\" y=\"0\"/></elements><signals><signal name=\"N\">{contacts}</signal></signals></board></drawing></eagle>"
        );
        let start = std::time::Instant::now();
        let read = read_board(&text, "x").expect("import");
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "{:?}", start.elapsed());
        let net = read.board.net_by_name("N").map(|n| n.id);
        assert!(read.board.footprints.iter().all(|f| f.pads.len() == 40_000 && f.pads.iter().all(|p| p.net == net)));
    }

    #[test]
    fn the_same_file_gives_the_same_board_and_a_cut_file_never_crashes() {
        let first = read_board(BOARD, "Demo").expect("import");
        assert_eq!(read_board(BOARD, "Demo").expect("import"), first, "ids follow from the file");
        // Up to the closing tag: without the newline after it the document is still whole.
        for cut in (0..BOARD.trim_end().len()).step_by(3) {
            if let Some(prefix) = BOARD.get(..cut) {
                assert!(read_board(prefix, "x").is_err(), "a document cut at {cut} is not well formed");
            }
        }
    }
}
