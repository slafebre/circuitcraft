//! A schematic: the sheets of a project and what is drawn on them.
//!
//! A [`Schematic`] holds one copy of every symbol its sheets use ([`Symbol`]) and its sheets
//! ([`Sheet`]). On a sheet, a [`Component`] is one thing on the bill of materials (a resistor, a
//! quad NAND gate); it is drawn as one or more [`Part`]s, one for each unit of its symbol, and
//! its parts may be spread over several sheets. The component belongs to the sheet of its first
//! part; parts on other sheets name it by that sheet and its id ([`ComponentRef`]).
//!
//! Wires, buses, junctions, net labels, power ports and sheet symbols say how things connect;
//! working out the nets from them is for the compiler, not the model. Coordinates are sheet
//! coordinates in [`Nm`], y up. A symbol's drawing and pins are relative to its origin; a part
//! mirrors them left to right first if it is mirrored, then turns them, then moves them to its
//! position ([`Part::to_sheet`]).

use crate::items::{LibraryLink, Rotation, id_type, random_uuid};
use crate::store::{Items, Keyed, MAX_ITEMS, bounded};
use crate::{ItemId, Justify, ModelError, check_len, check_name, check_point, check_rotation, check_size};
use circuitcraft_geom::{Arc, MAX_POINTS};
use circuitcraft_units::{Nm, Point};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

id_type!(
    /// The identity of a sheet, unique within its schematic.
    SheetId
);

id_type!(
    /// The identity of a symbol copy, unique within its schematic.
    SymbolId
);

/// The most sheets one schematic may have.
pub const MAX_SHEETS: usize = 10_000;
/// The most units one symbol may have (four gates of a quad NAND are four units).
pub const MAX_UNITS: usize = 1_024;
/// The most pins one unit of a symbol may have.
pub const MAX_PINS: usize = 100_000;
/// The most drawings and texts one unit of a symbol may have, each.
pub const MAX_SYMBOL_DRAWINGS: usize = 100_000;
/// The most fields one component may have, besides its designator and value.
pub const MAX_FIELDS: usize = 1_000;
/// The most texts one part may show.
pub const MAX_PART_TEXTS: usize = 1_000;
/// The most entries one sheet symbol may have.
pub const MAX_SHEET_ENTRIES: usize = 10_000;
/// How deep sheet symbols may nest, the top sheet counting as one.
pub const MAX_SHEET_DEPTH: usize = 64;

/// Every sheet of a project, and one copy of every symbol they use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Schematic {
    /// The schematic's name, usually the project's.
    pub name: String,
    /// One copy of each symbol the sheets use, so a project draws and compiles without its
    /// libraries.
    pub symbols: Items<Symbol>,
    /// The sheets. A sheet that no sheet symbol stands for is a top sheet; a flat design has
    /// several.
    pub sheets: Items<Sheet>,
}

/// A symbol as the project keeps it: its drawing and pins, unit by unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Symbol {
    /// Identity.
    pub id: SymbolId,
    /// The symbol's name (`LM358`).
    pub name: String,
    /// Where it came from, if it came from a library.
    #[serde(deserialize_with = "crate::items::required")]
    pub link: Option<SymbolLink>,
    /// What a new component's designator starts with (`R`, `U`); may be empty.
    pub prefix: String,
    /// What every unit draws besides its own: a shared outline, or the supply pins of a quad
    /// gate. Its pins belong to the component once, however many of its parts show them; its
    /// `name` is empty.
    pub shared: SymbolUnit,
    /// Its units, at least one; a part draws one of them, and [`Symbol::shared`] with it.
    #[serde(deserialize_with = "bounded::<_, _, MAX_UNITS>")]
    pub units: Vec<SymbolUnit>,
}

impl Symbol {
    /// Unit `n`, counted from 1.
    pub fn unit(&self, n: u32) -> Option<&SymbolUnit> {
        self.units.get(usize::try_from(n).ok()?.checked_sub(1)?)
    }
}

/// Where a symbol came from: a symbol in a library. Nothing follows the link yet; it is kept so
/// the copy can later be compared with its library and updated from it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolLink {
    /// The library's name. Empty when the source names the symbol alone, never blank.
    pub library: String,
    /// The symbol's name in that library.
    pub symbol: String,
}

/// One unit of a symbol: one gate of a quad NAND, or the whole of a resistor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolUnit {
    /// The unit's own name, if it has one (`A`, `Power`); may be empty.
    pub name: String,
    /// Its drawing, relative to the symbol origin.
    #[serde(deserialize_with = "bounded::<_, _, MAX_SYMBOL_DRAWINGS>")]
    pub drawings: Vec<Drawing>,
    /// Its fixed text, relative to the symbol origin.
    #[serde(deserialize_with = "bounded::<_, _, MAX_SYMBOL_DRAWINGS>")]
    pub texts: Vec<SheetText>,
    /// Its pins.
    #[serde(deserialize_with = "bounded::<_, _, MAX_PINS>")]
    pub pins: Vec<Pin>,
}

impl SymbolUnit {
    /// A unit with no name, drawing or pins.
    pub fn empty() -> SymbolUnit {
        SymbolUnit { name: String::new(), drawings: Vec::new(), texts: Vec::new(), pins: Vec::new() }
    }
}

/// How a pin behaves electrically, for the electrical rules check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinKind {
    /// Takes a signal.
    Input,
    /// Drives a signal.
    Output,
    /// Takes or drives a signal.
    Bidirectional,
    /// Drives a signal or lets go of it (a three-state output).
    TriState,
    /// Neither takes nor drives: a resistor's or a connector's pin.
    Passive,
    /// Pulls low and lets go (open collector or open drain).
    OpenCollector,
    /// Pulls high and lets go (open emitter or open source).
    OpenEmitter,
    /// Neither drives nor loads the net (high impedance).
    HighImpedance,
    /// Takes power: a supply pin.
    PowerIn,
    /// Gives power: a regulator's output.
    PowerOut,
    /// Not said.
    Unspecified,
    /// Connected to nothing inside the part, so anything may join it.
    Free,
    /// Must not be connected.
    NoConnect,
}

/// A pin of a symbol unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    /// Its number, which the footprint's pad of the same number joins (`1`, `A3`); never blank.
    pub number: String,
    /// Its name (`VCC`, `~RESET`); may be empty.
    pub name: String,
    /// How it behaves electrically.
    pub kind: PinKind,
    /// The end a wire connects to, relative to the symbol origin.
    pub position: Point,
    /// The direction from that end towards the symbol's body, a quarter turn: zero points right
    /// (+x), a quarter turn up (+y).
    pub direction: Rotation,
    /// How long it is drawn, from the connecting end towards the body; may be zero.
    pub length: Nm,
    /// Whether it is drawn. A hidden pin still connects: a hidden power pin joins the net its
    /// name says.
    pub visible: bool,
    /// Whether its name is shown.
    pub name_visible: bool,
    /// Whether its number is shown.
    pub number_visible: bool,
}

/// How a closed drawing is filled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fill {
    /// Not filled.
    #[default]
    None,
    /// Filled in the colour of its outline.
    Solid,
    /// Filled in the background colour of a component body.
    Background,
}

/// A line, rectangle, circle or arc on a sheet or in a symbol. A file writes it under its kind:
/// `{"polyline": {"points": […]}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum DrawingShape {
    /// Straight lines through two or more points.
    Polyline {
        /// The points, at least two.
        #[serde(deserialize_with = "bounded::<_, _, MAX_POINTS>")]
        points: Vec<Point>,
    },
    /// A rectangle with sides along the axes, given by two opposite corners.
    Rectangle {
        /// One corner.
        from: Point,
        /// The opposite corner.
        to: Point,
    },
    /// A circle.
    Circle {
        /// Centre.
        center: Point,
        /// Radius.
        radius: Nm,
    },
    /// An arc.
    Arc {
        /// The arc.
        arc: Arc,
    },
}

/// A drawing: a shape, its pen width and its fill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Drawing {
    /// What is drawn.
    pub shape: DrawingShape,
    /// Pen width; zero for the thinnest line the output can draw.
    pub width: Nm,
    /// How it is filled, when it is closed.
    pub fill: Fill,
}

/// Text on a sheet or in a symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SheetText {
    /// The text itself, never blank; may hold several lines.
    pub text: String,
    /// Anchor point.
    pub position: Point,
    /// Rotation about the anchor.
    pub rotation: Rotation,
    /// Character height.
    pub height: Nm,
    /// Horizontal anchoring.
    pub justify: Justify,
}

/// A sheet: one page of the schematic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sheet {
    /// Identity.
    pub id: SheetId,
    /// The sheet's name.
    pub name: String,
    /// Its place among the sheets of the project, from 1; used to put them in order.
    pub number: u32,
    /// The size of the page, if the source gives one.
    #[serde(deserialize_with = "crate::items::required")]
    pub page: Option<PageSize>,
    /// The components that belong to this sheet.
    pub components: Items<Component>,
    /// The parts drawn on it, which may belong to components of other sheets.
    pub parts: Items<Part>,
    /// Wires.
    pub wires: Items<Wire>,
    /// Buses: a bundle of numbered signals drawn as one line.
    pub buses: Items<Wire>,
    /// Bus entries: where a wire leaves a bus.
    pub bus_entries: Items<BusEntry>,
    /// Junctions: dots that join wires that cross or meet.
    pub junctions: Items<Junction>,
    /// Net labels and ports.
    pub labels: Items<NetLabel>,
    /// Power ports: they join every point with the same net name, across the whole project.
    pub power_ports: Items<PowerPort>,
    /// No-connect markers: a pin or wire end meant to be left open.
    pub no_connects: Items<NoConnect>,
    /// Sheet symbols: the sheets below this one.
    pub sheet_symbols: Items<SheetSymbol>,
    /// Free text.
    pub notes: Items<Note>,
    /// Free drawings.
    pub drawings: Items<SheetDrawing>,
}

/// The size of a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageSize {
    /// Width.
    pub width: Nm,
    /// Height.
    pub height: Nm,
}

/// One thing on the bill of materials, drawn as one or more [`Part`]s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Component {
    /// Identity, which a footprint on the board uses to find it.
    pub id: ItemId,
    /// The symbol it is drawn with.
    pub symbol: SymbolId,
    /// Its designator (`R12`), never blank; a component not yet annotated ends in `?`.
    pub designator: String,
    /// Its value (`10k`); may be empty.
    pub value: String,
    /// The footprint it is to have on the board, if one is chosen.
    #[serde(deserialize_with = "crate::items::required")]
    pub footprint: Option<LibraryLink>,
    /// Its other fields (datasheet, manufacturer, part number), in their source's order, each
    /// name used once.
    #[serde(deserialize_with = "bounded::<_, _, MAX_FIELDS>")]
    pub fields: Vec<Field>,
    /// Whether the bill of materials lists it.
    pub in_bom: bool,
    /// Whether it goes onto the board.
    pub on_board: bool,
    /// Whether it is fitted when the board is assembled.
    pub fitted: bool,
}

/// A named field of a component.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// Its name (`Datasheet`), never blank.
    pub name: String,
    /// Its value; may be empty.
    pub value: String,
}

/// Which component a part belongs to: the sheet the component belongs to, and its id there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentRef {
    /// The sheet the component belongs to.
    pub sheet: SheetId,
    /// The component.
    pub component: ItemId,
}

/// One unit of a component, drawn on a sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Part {
    /// Identity.
    pub id: ItemId,
    /// The component it belongs to.
    pub component: ComponentRef,
    /// Which unit of the component's symbol it draws, counted from 1.
    pub unit: u32,
    /// Where the symbol origin goes.
    pub position: Point,
    /// Rotation, applied after the symbol is mirrored.
    pub rotation: Rotation,
    /// Mirrored left to right about its origin, before it is turned.
    pub mirrored: bool,
    /// The component's designator, value and fields as this part shows them.
    #[serde(deserialize_with = "bounded::<_, _, MAX_PART_TEXTS>")]
    pub texts: Vec<PartText>,
}

/// What a part's text shows. A file writes it as `"designator"`, `"value"` or
/// `{"field": "Datasheet"}`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartTextContent {
    /// The component's designator.
    Designator,
    /// The component's value.
    Value,
    /// The component's field of this name.
    Field(String),
}

/// A component's designator, value or field as one of its parts shows it, in sheet
/// coordinates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartText {
    /// What it shows.
    pub content: PartTextContent,
    /// Whether it is drawn.
    pub visible: bool,
    /// Anchor point.
    pub position: Point,
    /// Rotation about the anchor.
    pub rotation: Rotation,
    /// Character height.
    pub height: Nm,
    /// Horizontal anchoring.
    pub justify: Justify,
}

/// A wire or a bus: straight lines through two or more points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wire {
    /// Identity.
    pub id: ItemId,
    /// The points, at least two.
    #[serde(deserialize_with = "bounded::<_, _, MAX_POINTS>")]
    pub points: Vec<Point>,
}

/// Where a wire leaves a bus: a short line from the bus to the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusEntry {
    /// Identity.
    pub id: ItemId,
    /// One end.
    pub from: Point,
    /// The other end.
    pub to: Point,
}

/// A junction dot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Junction {
    /// Identity.
    pub id: ItemId,
    /// Where it is.
    pub position: Point,
}

/// Which way a port or a sheet entry passes signals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortIo {
    /// Not said.
    #[default]
    Unspecified,
    /// Into the sheet.
    Input,
    /// Out of the sheet.
    Output,
    /// Both ways.
    Bidirectional,
    /// Out of the sheet, or let go (three-state).
    TriState,
    /// Neither way in particular.
    Passive,
}

/// What a label joins. A file writes it as `"local"` or `{"port": "input"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelKind {
    /// A net label: names the wire it sits on, on its own sheet.
    Local,
    /// A label that joins every label of the same name on every sheet.
    Global(PortIo),
    /// A port: joins the sheet entry of the same name on the sheet symbol above.
    Port(PortIo),
}

/// A net label or a port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetLabel {
    /// Identity.
    pub id: ItemId,
    /// The net name, never blank.
    pub name: String,
    /// What it joins.
    pub kind: LabelKind,
    /// The point it connects at.
    pub position: Point,
    /// Rotation about that point.
    pub rotation: Rotation,
    /// Character height.
    pub height: Nm,
}

/// How a power port is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerStyle {
    /// A bar across the end of its line.
    #[default]
    Bar,
    /// An arrow.
    Arrow,
    /// A circle.
    Circle,
    /// A wave.
    Wave,
    /// Power ground: lines that get shorter.
    Ground,
    /// Signal ground: a triangle.
    SignalGround,
    /// Earth.
    Earth,
}

/// A power port: joins every point with the same net name, across the whole project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerPort {
    /// Identity.
    pub id: ItemId,
    /// The net it joins (`GND`, `+3V3`), never blank.
    pub net: String,
    /// The point it connects at.
    pub position: Point,
    /// Rotation about that point.
    pub rotation: Rotation,
    /// How it is drawn.
    pub style: PowerStyle,
    /// The symbol it is drawn with in place of its style, if its source drew it with one: turned
    /// by `rotation`, never mirrored, with its first power input pin (in its shared part, then
    /// its units in order) at `position`.
    #[serde(deserialize_with = "crate::items::required")]
    pub symbol: Option<SymbolId>,
}

/// A marker on a pin or a wire end that is meant to be left open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoConnect {
    /// Identity.
    pub id: ItemId,
    /// Where it is.
    pub position: Point,
}

/// A sheet symbol: a box that stands for a sheet below this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SheetSymbol {
    /// Identity, part of the path that tells two uses of one sheet apart.
    pub id: ItemId,
    /// The sheet it stands for.
    pub sheet: SheetId,
    /// The name it shows; may differ from the sheet's own.
    pub name: String,
    /// One corner of the box.
    pub from: Point,
    /// The opposite corner.
    pub to: Point,
    /// Its entries, each joining the port of the same name on the sheet below.
    #[serde(deserialize_with = "bounded::<_, _, MAX_SHEET_ENTRIES>")]
    pub entries: Vec<SheetEntry>,
}

/// An entry on a sheet symbol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SheetEntry {
    /// Identity.
    pub id: ItemId,
    /// The name of the port it joins, never blank.
    pub name: String,
    /// Which way it passes signals.
    pub io: PortIo,
    /// The point it connects at, on the edge of the box.
    pub position: Point,
}

/// Free text on a sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Note {
    /// Identity.
    pub id: ItemId,
    /// The text.
    pub text: SheetText,
}

/// A free drawing on a sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SheetDrawing {
    /// Identity.
    pub id: ItemId,
    /// What is drawn.
    pub drawing: Drawing,
}

macro_rules! keyed {
    ($($t:ty => $id:ty),* $(,)?) => {
        $(impl Keyed for $t {
            type Id = $id;
            fn id(&self) -> $id {
                self.id
            }
        })*
    };
}

keyed!(
    Symbol => SymbolId,
    Sheet => SheetId,
    Component => ItemId,
    Part => ItemId,
    Wire => ItemId,
    BusEntry => ItemId,
    Junction => ItemId,
    NetLabel => ItemId,
    PowerPort => ItemId,
    NoConnect => ItemId,
    SheetSymbol => ItemId,
    Note => ItemId,
    SheetDrawing => ItemId,
);

impl Part {
    /// Where a point of its symbol (in symbol coordinates) lands on the sheet ([`place`]).
    pub fn to_sheet(&self, p: Point) -> Option<Point> {
        place(p, self.position, self.rotation, self.mirrored)
    }
}

/// Where a point of a symbol (in symbol coordinates) lands on a sheet when the symbol is
/// mirrored left to right (if `mirrored`), turned by `rotation`, then moved to `position`.
/// `None` when it would land outside the range a design may use.
pub fn place(p: Point, position: Point, rotation: Rotation, mirrored: bool) -> Option<Point> {
    let p = if mirrored { Point::new(Nm(p.x.0.checked_neg()?), p.y) } else { p };
    let p = rotation.turn(p)?;
    let p = Point::new(Nm(p.x.0.checked_add(position.x.0)?), Nm(p.y.0.checked_add(position.y.0)?));
    check_point(p, "point").is_ok().then_some(p)
}

impl Schematic {
    /// An empty schematic with this name.
    pub fn new(name: &str) -> Schematic {
        Schematic { name: name.to_owned(), symbols: Items::default(), sheets: Items::default() }
    }

    /// The component a part belongs to, if it exists.
    pub fn component(&self, r: ComponentRef) -> Option<&Component> {
        self.sheets.get(r.sheet)?.components.get(r.component)
    }

    /// The sheets no sheet symbol stands for, in order of their numbers.
    pub fn top_sheets(&self) -> Vec<&Sheet> {
        let below: BTreeSet<SheetId> = self.sheets.iter().flat_map(|s| s.sheet_symbols.iter().map(|b| b.sheet)).collect();
        let mut top: Vec<&Sheet> = self.sheets.iter().filter(|s| !below.contains(&s.id)).collect();
        top.sort_by_key(|s| (s.number, s.id));
        top
    }

    /// Checks the whole schematic: names, ids, references between sheets, components, parts and
    /// symbols, sizes, coordinates, collection limits, and that sheet symbols nest no deeper than
    /// [`MAX_SHEET_DEPTH`] and never in a loop. Run it on any schematic read from a file.
    pub fn validate(&self) -> Result<(), ModelError> {
        check_name(&self.name, "schematic name")?;
        if self.sheets.len() > MAX_SHEETS {
            return Err(ModelError::TooMany("sheets"));
        }
        if self.symbols.len() > MAX_ITEMS {
            return Err(ModelError::TooMany("symbols"));
        }
        misfiled(self.symbols.mismatched_key().map(|k| k.to_string()))?;
        misfiled(self.sheets.mismatched_key().map(|k| k.to_string()))?;
        // The field names of each component a part shows a field of, worked out once.
        let mut field_names: BTreeMap<ComponentRef, BTreeSet<&str>> = BTreeMap::new();
        // Sheets, symbols and every item share one id space, as a board's items do.
        let mut ids = BTreeSet::new();
        let mut claim = |id: u128| {
            if id == 0 || !ids.insert(id) { Err(ModelError::BadId(ItemId::from_u128(id).to_string())) } else { Ok(()) }
        };
        // The symbols a power port can be drawn with, those with a power input pin, found once
        // rather than for each port.
        let mut supplies = BTreeSet::new();
        for s in self.symbols.iter() {
            claim(s.id.as_u128())?;
            check_symbol(s)?;
            if std::iter::once(&s.shared).chain(&s.units).flat_map(|u| &u.pins).any(|pin| pin.kind == PinKind::PowerIn) {
                supplies.insert(s.id);
            }
        }
        for sheet in self.sheets.iter() {
            claim(sheet.id.as_u128())?;
            check_name(&sheet.name, "sheet name")?;
            if let Some(page) = sheet.page {
                check_size(page.width, "page width", false)?;
                check_size(page.height, "page height", false)?;
            }
            sheet.check_collections()?;
            let drawn_here: BTreeSet<ComponentRef> = sheet.parts.iter().map(|p| p.component).collect();
            for c in sheet.components.iter() {
                claim(c.id.as_u128())?;
                self.check_component(c)?;
                // The component belongs to the sheet of its first part, so a part of it is here.
                if !drawn_here.contains(&ComponentRef { sheet: sheet.id, component: c.id }) {
                    return Err(ModelError::BadArea("component", "a component needs a part on its own sheet"));
                }
            }
            for p in sheet.parts.iter() {
                claim(p.id.as_u128())?;
                self.check_part(p, &mut field_names)?;
            }
            for w in sheet.wires.iter().chain(sheet.buses.iter()) {
                claim(w.id.as_u128())?;
                check_points(&w.points, "wire")?;
            }
            for e in sheet.bus_entries.iter() {
                claim(e.id.as_u128())?;
                check_point(e.from, "bus entry")?;
                check_point(e.to, "bus entry")?;
            }
            for j in sheet.junctions.iter() {
                claim(j.id.as_u128())?;
                check_point(j.position, "junction")?;
            }
            for l in sheet.labels.iter() {
                claim(l.id.as_u128())?;
                check_name(&l.name, "net label")?;
                check_point(l.position, "net label")?;
                check_rotation(l.rotation, "net label rotation")?;
                check_size(l.height, "net label height", false)?;
            }
            for p in sheet.power_ports.iter() {
                claim(p.id.as_u128())?;
                check_name(&p.net, "power port net")?;
                check_point(p.position, "power port")?;
                check_quarter(p.rotation, "power port rotation")?;
                if let Some(symbol) = p.symbol {
                    let s = self.symbols.get(symbol).ok_or_else(|| ModelError::BadId(symbol.to_string()))?;
                    if !supplies.contains(&symbol) {
                        return Err(ModelError::BadName(s.name.clone(), "a power port's symbol needs a power input pin"));
                    }
                }
            }
            for n in sheet.no_connects.iter() {
                claim(n.id.as_u128())?;
                check_point(n.position, "no-connect marker")?;
            }
            for b in sheet.sheet_symbols.iter() {
                claim(b.id.as_u128())?;
                if !self.sheets.contains(b.sheet) {
                    return Err(ModelError::BadId(b.sheet.to_string()));
                }
                check_len(&b.name, "sheet symbol name")?;
                check_point(b.from, "sheet symbol")?;
                check_point(b.to, "sheet symbol")?;
                if b.entries.len() > MAX_SHEET_ENTRIES {
                    return Err(ModelError::TooMany("sheet entries"));
                }
                for e in &b.entries {
                    claim(e.id.as_u128())?;
                    check_name(&e.name, "sheet entry")?;
                    check_point(e.position, "sheet entry")?;
                }
            }
            for n in sheet.notes.iter() {
                claim(n.id.as_u128())?;
                check_text(&n.text, "note")?;
            }
            for d in sheet.drawings.iter() {
                claim(d.id.as_u128())?;
                check_drawing(&d.drawing)?;
            }
        }
        self.check_nesting()
    }

    fn check_component(&self, c: &Component) -> Result<(), ModelError> {
        if !self.symbols.contains(c.symbol) {
            return Err(ModelError::BadId(c.symbol.to_string()));
        }
        check_name(&c.designator, "designator")?;
        check_len(&c.value, "value")?;
        if let Some(link) = &c.footprint {
            if !link.library.is_empty() {
                check_name(&link.library, "footprint library name")?;
            }
            check_name(&link.footprint, "footprint name")?;
        }
        if c.fields.len() > MAX_FIELDS {
            return Err(ModelError::TooMany("fields"));
        }
        let mut names = BTreeSet::new();
        for f in &c.fields {
            check_name(&f.name, "field name")?;
            check_len(&f.value, "field value")?;
            if !names.insert(f.name.as_str()) {
                return Err(ModelError::BadName(f.name.clone(), "a component's field names must differ"));
            }
        }
        Ok(())
    }

    fn check_part<'s>(&'s self, p: &Part, field_names: &mut BTreeMap<ComponentRef, BTreeSet<&'s str>>) -> Result<(), ModelError> {
        let component = self.component(p.component).ok_or_else(|| ModelError::BadId(p.component.component.to_string()))?;
        let units = self.symbols.get(component.symbol).map_or(0, |s| s.units.len());
        if p.unit == 0 || usize::try_from(p.unit).map_or(true, |u| u > units) {
            return Err(ModelError::OutOfRange("part unit"));
        }
        check_point(p.position, "part position")?;
        // Quarter turns keep every pin on the grid its wires end on: a pin turned by another
        // angle would land beside the wire drawn to it.
        check_quarter(p.rotation, "part rotation")?;
        if p.texts.len() > MAX_PART_TEXTS {
            return Err(ModelError::TooMany("part texts"));
        }
        for t in &p.texts {
            if let PartTextContent::Field(name) = &t.content {
                let names = field_names.entry(p.component).or_insert_with(|| component.fields.iter().map(|f| f.name.as_str()).collect());
                if !names.contains(name.as_str()) {
                    return Err(ModelError::BadName(name.clone(), "a part shows a field its component doesn't have"));
                }
            }
            check_point(t.position, "part text")?;
            check_rotation(t.rotation, "part text rotation")?;
            check_size(t.height, "part text height", false)?;
        }
        Ok(())
    }

    /// Sheet symbols nest no deeper than [`MAX_SHEET_DEPTH`], and no sheet stands, directly or
    /// further down, for itself. The walk goes from every sheet, so a loop no top sheet reaches
    /// is found too; each sheet's depth below it is worked out once.
    fn check_nesting(&self) -> Result<(), ModelError> {
        let children: BTreeMap<SheetId, Vec<SheetId>> =
            self.sheets.iter().map(|s| (s.id, s.sheet_symbols.iter().map(|b| b.sheet).collect())).collect();
        // The deepest chain of sheets below each sheet, itself included, once known.
        let mut depth: BTreeMap<SheetId, usize> = BTreeMap::new();
        for start in self.sheets.iter().map(|s| s.id) {
            // An explicit stack in place of recursion, so a deep file can't overflow the real one.
            let mut stack: Vec<(SheetId, usize)> = vec![(start, 0)];
            let mut on_path: BTreeSet<SheetId> = BTreeSet::from([start]);
            while let Some(&(sheet, next)) = stack.last() {
                let kids = children.get(&sheet).map_or(&[][..], Vec::as_slice);
                match kids.get(next) {
                    Some(&child) => {
                        if let Some(top) = stack.last_mut() {
                            top.1 += 1;
                        }
                        if on_path.contains(&child) {
                            return Err(ModelError::BadArea("sheet symbols", "a sheet stands for itself"));
                        }
                        if !depth.contains_key(&child) {
                            if stack.len() >= MAX_SHEET_DEPTH {
                                return Err(ModelError::TooMany("levels of sheets"));
                            }
                            on_path.insert(child);
                            stack.push((child, 0));
                        }
                    }
                    None => {
                        let below = kids.iter().filter_map(|k| depth.get(k)).max().copied().unwrap_or(0);
                        let mine = below.saturating_add(1);
                        if mine > MAX_SHEET_DEPTH {
                            return Err(ModelError::TooMany("levels of sheets"));
                        }
                        depth.insert(sheet, mine);
                        on_path.remove(&sheet);
                        stack.pop();
                    }
                }
            }
        }
        Ok(())
    }
}

impl Sheet {
    /// An empty sheet.
    pub fn new(id: SheetId, name: &str, number: u32) -> Sheet {
        Sheet {
            id,
            name: name.to_owned(),
            number,
            page: None,
            components: Items::default(),
            parts: Items::default(),
            wires: Items::default(),
            buses: Items::default(),
            bus_entries: Items::default(),
            junctions: Items::default(),
            labels: Items::default(),
            power_ports: Items::default(),
            no_connects: Items::default(),
            sheet_symbols: Items::default(),
            notes: Items::default(),
            drawings: Items::default(),
        }
    }

    /// Every collection is within [`MAX_ITEMS`] and files each item under its own id.
    fn check_collections(&self) -> Result<(), ModelError> {
        let lens = [
            (self.components.len(), "components"),
            (self.parts.len(), "parts"),
            (self.wires.len(), "wires"),
            (self.buses.len(), "buses"),
            (self.bus_entries.len(), "bus entries"),
            (self.junctions.len(), "junctions"),
            (self.labels.len(), "net labels"),
            (self.power_ports.len(), "power ports"),
            (self.no_connects.len(), "no-connect markers"),
            (self.sheet_symbols.len(), "sheet symbols"),
            (self.notes.len(), "notes"),
            (self.drawings.len(), "drawings"),
        ];
        if let Some((_, what)) = lens.iter().find(|(n, _)| *n > MAX_ITEMS) {
            return Err(ModelError::TooMany(what));
        }
        let key = |k: Option<ItemId>| k.map(|k| k.to_string());
        misfiled(
            key(self.components.mismatched_key())
                .or_else(|| key(self.parts.mismatched_key()))
                .or_else(|| key(self.wires.mismatched_key()))
                .or_else(|| key(self.buses.mismatched_key()))
                .or_else(|| key(self.bus_entries.mismatched_key()))
                .or_else(|| key(self.junctions.mismatched_key()))
                .or_else(|| key(self.labels.mismatched_key()))
                .or_else(|| key(self.power_ports.mismatched_key()))
                .or_else(|| key(self.no_connects.mismatched_key()))
                .or_else(|| key(self.sheet_symbols.mismatched_key()))
                .or_else(|| key(self.notes.mismatched_key()))
                .or_else(|| key(self.drawings.mismatched_key())),
        )
    }
}

fn misfiled(key: Option<String>) -> Result<(), ModelError> {
    key.map_or(Ok(()), |k| Err(ModelError::BadId(k)))
}

/// A rotation that is a whole number of quarter turns.
fn check_quarter(r: Rotation, what: &'static str) -> Result<(), ModelError> {
    check_rotation(r, what)?;
    if r.0 % 90_000 == 0 { Ok(()) } else { Err(ModelError::OutOfRange(what)) }
}

fn check_symbol(s: &Symbol) -> Result<(), ModelError> {
    check_name(&s.name, "symbol name")?;
    check_len(&s.prefix, "designator prefix")?;
    if let Some(link) = &s.link {
        if !link.library.is_empty() {
            check_name(&link.library, "symbol library name")?;
        }
        check_name(&link.symbol, "library symbol name")?;
    }
    if s.units.is_empty() {
        return Err(ModelError::BadName(s.name.clone(), "a symbol needs a unit"));
    }
    if !s.shared.name.is_empty() {
        return Err(ModelError::BadName(s.shared.name.clone(), "what a symbol's units share has no name"));
    }
    if s.units.len() > MAX_UNITS {
        return Err(ModelError::TooMany("symbol units"));
    }
    for u in std::iter::once(&s.shared).chain(&s.units) {
        check_len(&u.name, "unit name")?;
        if u.pins.len() > MAX_PINS {
            return Err(ModelError::TooMany("pins"));
        }
        if u.drawings.len() > MAX_SYMBOL_DRAWINGS || u.texts.len() > MAX_SYMBOL_DRAWINGS {
            return Err(ModelError::TooMany("symbol drawings"));
        }
        for d in &u.drawings {
            check_drawing(d)?;
        }
        for t in &u.texts {
            check_text(t, "symbol text")?;
        }
        for p in &u.pins {
            check_name(&p.number, "pin number")?;
            check_len(&p.name, "pin name")?;
            check_point(p.position, "pin position")?;
            check_quarter(p.direction, "pin direction")?;
            check_size(p.length, "pin length", true)?;
        }
    }
    Ok(())
}

fn check_text(t: &SheetText, what: &'static str) -> Result<(), ModelError> {
    check_name(&t.text, what)?;
    check_point(t.position, what)?;
    check_rotation(t.rotation, what)?;
    check_size(t.height, what, false)
}

fn check_points(points: &[Point], what: &'static str) -> Result<(), ModelError> {
    if points.len() < 2 {
        return Err(ModelError::BadArea(what, "a line needs two points"));
    }
    if points.len() > MAX_POINTS {
        return Err(ModelError::TooMany("points"));
    }
    points.iter().try_for_each(|&p| check_point(p, what))
}

fn check_drawing(d: &Drawing) -> Result<(), ModelError> {
    check_size(d.width, "drawing width", true)?;
    match &d.shape {
        DrawingShape::Polyline { points } => check_points(points, "drawing"),
        DrawingShape::Rectangle { from, to } => {
            check_point(*from, "drawing")?;
            check_point(*to, "drawing")
        }
        DrawingShape::Circle { center, radius } => {
            check_point(*center, "drawing")?;
            check_size(*radius, "circle radius", false)
        }
        DrawingShape::Arc { arc } => {
            check_point(arc.start, "drawing")?;
            check_point(arc.mid, "drawing")?;
            check_point(arc.end, "drawing")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u128) -> ItemId {
        ItemId::from_u128(n)
    }

    fn resistor() -> Symbol {
        let pin = |number: &str, x: i64, direction: i32| Pin {
            number: number.to_owned(),
            name: String::new(),
            kind: PinKind::Passive,
            position: Point::new(Nm(x), Nm(0)),
            direction: Rotation(direction),
            length: Nm(1_270_000),
            visible: true,
            name_visible: false,
            number_visible: true,
        };
        Symbol {
            id: SymbolId::from_u128(1),
            name: "R".to_owned(),
            link: Some(SymbolLink { library: "Device".to_owned(), symbol: "R".to_owned() }),
            prefix: "R".to_owned(),
            shared: SymbolUnit::empty(),
            units: vec![SymbolUnit {
                name: String::new(),
                drawings: vec![Drawing {
                    shape: DrawingShape::Rectangle { from: Point::new(Nm(-2_540_000), Nm(-1_000_000)), to: Point::new(Nm(2_540_000), Nm(1_000_000)) },
                    width: Nm(254_000),
                    fill: Fill::None,
                }],
                texts: Vec::new(),
                pins: vec![pin("1", -3_810_000, 0), pin("2", 3_810_000, 180_000)],
            }],
        }
    }

    /// One sheet with a resistor, a wire to its first pin, a label and a power port.
    fn sample() -> Schematic {
        let mut s = Schematic::new("Sample");
        let symbol = resistor();
        let symbol_id = symbol.id;
        s.symbols.insert(symbol).expect("symbol");
        let sheet_id = SheetId::from_u128(2);
        let mut sheet = Sheet::new(sheet_id, "Main", 1);
        sheet
            .components
            .insert(Component {
                id: id(10),
                symbol: symbol_id,
                designator: "R1".to_owned(),
                value: "10k".to_owned(),
                footprint: Some(LibraryLink { library: "Resistor_SMD".to_owned(), footprint: "R_0603".to_owned() }),
                fields: vec![Field { name: "Datasheet".to_owned(), value: String::new() }],
                in_bom: true,
                on_board: true,
                fitted: true,
            })
            .expect("component");
        sheet
            .parts
            .insert(Part {
                id: id(11),
                component: ComponentRef { sheet: sheet_id, component: id(10) },
                unit: 1,
                position: Point::new(Nm(50_000_000), Nm(-30_000_000)),
                rotation: Rotation(90_000),
                mirrored: false,
                texts: vec![PartText {
                    content: PartTextContent::Designator,
                    visible: true,
                    position: Point::new(Nm(52_000_000), Nm(-30_000_000)),
                    rotation: Rotation(0),
                    height: Nm(1_270_000),
                    justify: Justify::Left,
                }],
            })
            .expect("part");
        sheet
            .wires
            .insert(Wire { id: id(12), points: vec![Point::new(Nm(50_000_000), Nm(-33_810_000)), Point::new(Nm(50_000_000), Nm(-40_000_000))] })
            .expect("wire");
        sheet
            .labels
            .insert(NetLabel {
                id: id(13),
                name: "OUT".to_owned(),
                kind: LabelKind::Global(PortIo::Output),
                position: Point::new(Nm(50_000_000), Nm(-40_000_000)),
                rotation: Rotation(0),
                height: Nm(1_270_000),
            })
            .expect("label");
        sheet
            .power_ports
            .insert(PowerPort {
                id: id(14),
                net: "GND".to_owned(),
                position: Point::new(Nm(50_000_000), Nm(-26_190_000)),
                rotation: Rotation(0),
                style: PowerStyle::Ground,
                symbol: None,
            })
            .expect("power port");
        s.sheets.insert(sheet).expect("sheet");
        s
    }

    #[test]
    fn a_sample_is_valid_and_survives_json() {
        let s = sample();
        assert_eq!(s.validate(), Ok(()));
        let json = serde_json::to_string(&s).expect("write");
        let back: Schematic = serde_json::from_str(&json).expect("read");
        assert_eq!(back, s);
        assert_eq!(s.top_sheets().len(), 1);
    }

    #[test]
    fn a_part_places_its_pins_mirrored_then_turned() {
        let s = sample();
        let sheet = s.sheets.iter().next().expect("sheet");
        let part = sheet.parts.iter().next().expect("part");
        let symbol = s.component(part.component).and_then(|c| s.symbols.get(c.symbol)).expect("symbol");
        let pin = symbol.units.first().and_then(|u| u.pins.first()).expect("pin");
        // Pin 1 at (-3.81, 0), turned a quarter: (0, -3.81), at (50, -30): where the wire starts.
        assert_eq!(part.to_sheet(pin.position), Some(Point::new(Nm(50_000_000), Nm(-33_810_000))));
        let mirrored = Part { mirrored: true, ..part.clone() };
        assert_eq!(mirrored.to_sheet(pin.position), Some(Point::new(Nm(50_000_000), Nm(-26_190_000))));
        let far = Part { position: Point::new(Nm(i64::MAX), Nm(0)), rotation: Rotation(0), ..part.clone() };
        assert_eq!(far.to_sheet(Point::new(Nm(1), Nm(0))), None);
        // Past the range a design may use, though no number overflows.
        let edge = Part { position: Point::new(Nm(circuitcraft_units::MAX_ABS), Nm(0)), rotation: Rotation(0), ..part.clone() };
        assert_eq!(edge.to_sheet(Point::new(Nm(0), Nm(0))), Some(Point::new(Nm(circuitcraft_units::MAX_ABS), Nm(0))));
        assert_eq!(edge.to_sheet(Point::new(Nm(1), Nm(0))), None);
    }

    #[test]
    fn what_json_could_not_hold_is_refused() {
        // Lists longer than reading the schematic back allows.
        let mut s = sample();
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2))
            && let Some(c) = sheet.components.get_mut(id(10))
        {
            c.fields = (0..=MAX_FIELDS).map(|i| Field { name: format!("F{i}"), value: String::new() }).collect();
        }
        assert_eq!(s.validate(), Err(ModelError::TooMany("fields")));
        let mut s = sample();
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2))
            && let Some(part) = sheet.parts.get_mut(id(11))
        {
            let text = part.texts.first().cloned().expect("text");
            part.texts = vec![text; MAX_PART_TEXTS + 1];
        }
        assert_eq!(s.validate(), Err(ModelError::TooMany("part texts")));

        // A power port's symbol connects at its power input pin, so it needs one.
        let mut s = sample();
        let resistor = s.symbols.iter().next().map(|sym| sym.id).expect("symbol");
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2))
            && let Some(port) = sheet.power_ports.get_mut(id(14))
        {
            port.symbol = Some(resistor);
        }
        assert_eq!(s.validate(), Err(ModelError::BadName("R".to_owned(), "a power port's symbol needs a power input pin")));
        // A symbol whose power input is its last pin is a fine one for a port.
        let mut s = sample();
        let mut supply = s.symbols.get(resistor).cloned().expect("symbol");
        supply.id = SymbolId::from_u128(77);
        supply.name = "VCC".to_owned();
        if let Some(unit) = supply.units.first_mut()
            && let Some(last) = unit.pins.last_mut()
        {
            last.kind = PinKind::PowerIn;
        }
        s.symbols.insert(supply).expect("symbol");
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2))
            && let Some(port) = sheet.power_ports.get_mut(id(14))
        {
            port.symbol = Some(SymbolId::from_u128(77));
        }
        assert_eq!(s.validate(), Ok(()));

        let mut s = sample();
        if let Some(symbol) = s.symbols.get_mut(resistor) {
            symbol.shared.name = "Common".to_owned();
        }
        assert!(matches!(s.validate(), Err(ModelError::BadName(_, "what a symbol's units share has no name"))));
    }

    #[test]
    fn broken_references_are_refused() {
        let mut s = sample();
        let sheet = s.sheets.iter().next().map(|s| s.id).expect("sheet");
        if let Some(sheet) = s.sheets.get_mut(sheet)
            && let Some(part) = sheet.parts.get_mut(id(11))
        {
            part.unit = 2;
        }
        assert_eq!(s.validate(), Err(ModelError::OutOfRange("part unit")));

        let mut s = sample();
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2))
            && let Some(part) = sheet.parts.get_mut(id(11))
        {
            part.component.component = id(99);
        }
        // R1 has lost its only part, which now names a component that doesn't exist.
        assert_eq!(s.validate(), Err(ModelError::BadArea("component", "a component needs a part on its own sheet")));
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2)) {
            sheet.components.remove(id(10));
        }
        assert!(matches!(s.validate(), Err(ModelError::BadId(_))));

        let mut s = sample();
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2))
            && let Some(c) = sheet.components.get_mut(id(10))
        {
            c.fields.push(Field { name: "Datasheet".to_owned(), value: "x".to_owned() });
        }
        assert!(matches!(s.validate(), Err(ModelError::BadName(_, _))));
    }

    #[test]
    fn parts_turn_by_quarters_show_real_fields_and_sit_with_their_component() {
        let edit = |change: &dyn Fn(&mut Sheet)| {
            let mut s = sample();
            if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2)) {
                change(sheet);
            }
            s.validate()
        };
        assert_eq!(
            edit(&|sheet| {
                if let Some(p) = sheet.parts.get_mut(id(11)) {
                    p.rotation = Rotation(45_000);
                }
            }),
            Err(ModelError::OutOfRange("part rotation"))
        );
        assert!(matches!(
            edit(&|sheet| {
                if let Some(p) = sheet.parts.get_mut(id(11)) {
                    p.texts.push(PartText { content: PartTextContent::Field("Nope".to_owned()), ..p.texts[0].clone() });
                }
            }),
            Err(ModelError::BadName(_, _))
        ));
        assert_eq!(
            edit(&|sheet| {
                sheet.parts.remove(id(11));
            }),
            Err(ModelError::BadArea("component", "a component needs a part on its own sheet"))
        );
    }

    #[test]
    fn ids_are_shared_across_the_schematic() {
        let mut s = sample();
        if let Some(sheet) = s.sheets.get_mut(SheetId::from_u128(2)) {
            sheet.junctions.insert(Junction { id: id(1), position: Point::ORIGIN }).expect("junction");
        }
        // Id 1 is the symbol's.
        assert!(matches!(s.validate(), Err(ModelError::BadId(_))));
    }

    fn sheet_symbol(n: u128, sheet: u128) -> SheetSymbol {
        SheetSymbol {
            id: id(n),
            sheet: SheetId::from_u128(sheet),
            name: String::new(),
            from: Point::ORIGIN,
            to: Point::new(Nm(10), Nm(10)),
            entries: Vec::new(),
        }
    }

    #[test]
    fn sheets_may_not_stand_for_themselves() {
        let mut s = Schematic::new("Loop");
        let mut a = Sheet::new(SheetId::from_u128(1), "A", 1);
        let mut b = Sheet::new(SheetId::from_u128(2), "B", 2);
        a.sheet_symbols.insert(sheet_symbol(10, 2)).expect("a");
        b.sheet_symbols.insert(sheet_symbol(11, 1)).expect("b");
        s.sheets.insert(a).expect("a");
        s.sheets.insert(b).expect("b");
        assert_eq!(s.validate(), Err(ModelError::BadArea("sheet symbols", "a sheet stands for itself")));
        assert!(s.top_sheets().is_empty());
    }

    #[test]
    fn a_sheet_used_twice_is_fine_and_depth_is_bounded() {
        let mut s = Schematic::new("Twice");
        let mut top = Sheet::new(SheetId::from_u128(1), "Top", 1);
        top.sheet_symbols.insert(sheet_symbol(10, 2)).expect("first use");
        top.sheet_symbols.insert(sheet_symbol(11, 2)).expect("second use");
        s.sheets.insert(top).expect("top");
        s.sheets.insert(Sheet::new(SheetId::from_u128(2), "Channel", 2)).expect("channel");
        assert_eq!(s.validate(), Ok(()));

        let mut deep = Schematic::new("Deep");
        let levels = u128::try_from(MAX_SHEET_DEPTH).expect("small") + 1;
        for n in 1..=levels {
            let mut sheet = Sheet::new(SheetId::from_u128(n), "Level", u32::try_from(n).expect("small"));
            if n < levels {
                sheet.sheet_symbols.insert(sheet_symbol(1_000 + n, n + 1)).expect("below");
            }
            deep.sheets.insert(sheet).expect("sheet");
        }
        assert_eq!(deep.validate(), Err(ModelError::TooMany("levels of sheets")));
    }

    #[test]
    fn hostile_json_is_refused_without_growing_without_limit() {
        let unit = "{\"name\":\"\",\"drawings\":[],\"texts\":[],\"pins\":[]}";
        let units = format!("[{}]", vec![unit; MAX_UNITS + 1].join(","));
        let json = format!(
            "{{\"id\":\"00000000-0000-4000-8000-000000000001\",\"name\":\"X\",\"link\":null,\"prefix\":\"\",\"shared\":{unit},\"units\":{units}}}"
        );
        let read: Result<Symbol, _> = serde_json::from_str(&json);
        assert!(read.is_err());
        let missing = "{\"id\":\"00000000-0000-4000-8000-000000000001\",\"name\":\"X\",\"prefix\":\"\",\"units\":[]}";
        assert!(serde_json::from_str::<Symbol>(missing).is_err(), "a missing link is refused, not read as none");
    }
}
