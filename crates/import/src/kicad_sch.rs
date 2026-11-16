//! KiCad schematics (`.kicad_sch`), read-only.
//!
//! Written from KiCad's published description of the format and from real schematics, for the
//! files KiCad 6 to 9 write. KiCad 5 wrote another format (`.sch`), which is recognised and
//! refused. The reader keeps what the model holds and says in the notes what it left out. What
//! a file says in KiCad's terms becomes the model's:
//!
//! - A schematic is a tree of files: each sheet symbol names the file of the sheet below. The
//!   files are read through the caller, by the names the sheet symbols give them, each once.
//! - y grows downward on a sheet and upward in the model; symbols are drawn y up in both.
//! - A placed symbol is turned first and mirrored after, in the file; the model mirrors first,
//!   so the angle is worked out again.
//! - The symbols the placed symbols draw with are kept once for the whole project, what is
//!   common to all units of a symbol once beside the units; the alternate body style is left out.
//! - Placed symbols with the same designator are the units of one component, which belongs to
//!   the sheet of the first one read. A power symbol becomes a power port where its supply pin
//!   is; one without a supply pin (KiCad's power flag) stays a component.
//! - The designator of a placed symbol is the one recorded for the sheet's place in the tree:
//!   with the placed symbol from KiCad 7 on, in the top file's `symbol_instances` in KiCad 6. A
//!   sheet used more than once is read once, with the designators of its first use.

use crate::import::{self, Ids, ImportError, ImportedSchematic, Notes};
use crate::sexp::{Node, Reader};
use circuitcraft_geom::Arc;
use circuitcraft_model::schematic::{
    BusEntry, Component, ComponentRef, Drawing, DrawingShape, Field, Fill, Junction, LabelKind, MAX_SHEET_DEPTH, MAX_SHEET_ENTRIES, MAX_SHEETS,
    MAX_UNITS, NetLabel, NoConnect, Note, PageSize, Part, PartText, PartTextContent, Pin, PinKind, PortIo, PowerPort, PowerStyle, Schematic, Sheet,
    SheetDrawing, SheetEntry, SheetId, SheetSymbol, SheetText, Symbol, SymbolId, SymbolLink, SymbolUnit, Wire,
};
use circuitcraft_model::{ItemId, Justify, LibraryLink, MAX_NAME_CHARS, Rotation};
use circuitcraft_units::{Nm, Point};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// The newest file version this reader was written against (KiCad 9).
const NEWEST_KNOWN: f64 = 20_250_114.0;

/// The most text all of a schematic's files may add up to, which bounds the work a caller handing
/// over files without limit can make the reader do.
pub const MAX_SCHEMATIC_BYTES: usize = 1 << 30;

/// The most units all the symbols a schematic keeps may have between them. A symbol has every
/// unit up to its highest, so one unit numbered high asks for many.
const MAX_TOTAL_UNITS: usize = circuitcraft_model::MAX_ITEMS;

/// Reads a KiCad schematic from the text of its top file. `name` becomes the schematic's name
/// and the top sheet's. `open` is asked for the text of each file a sheet symbol names, by the
/// name the sheet symbol gives it, with `/` between folders and no leading `./` (what the name
/// is relative to is the caller's to decide); a file it can't give is noted and its sheet left
/// empty. Two names for one file are two sheets, and a sheet symbol naming the top file is read
/// as a sheet of its own once, so `open` should give nothing for the top file's own name. No
/// file is ever written.
pub fn read_schematic(text: &str, name: &str, open: &mut dyn FnMut(&str) -> Option<String>) -> Result<ImportedSchematic, ImportError> {
    read_within(text, name, open, MAX_SCHEMATIC_BYTES)
}

/// [`read_schematic`], refusing files that add up to more than `max_bytes`.
fn read_within(text: &str, name: &str, open: &mut dyn FnMut(&str) -> Option<String>, max_bytes: usize) -> Result<ImportedSchematic, ImportError> {
    let within_limit = |bytes: usize| {
        if bytes > max_bytes { Err(ImportError::unsupported("the schematic's files add up to more than 1 GiB")) } else { Ok(()) }
    };
    let name = if name.trim().is_empty() { "Imported" } else { name };
    let name: String = name.chars().take(MAX_NAME_CHARS).collect();
    within_limit(text.len())?;
    let mut reader = Project::new(text, &name);
    reader.legacy = legacy_designators(text)?;
    let top = reader.next_sheet_id();
    reader.files.insert(String::new(), top);
    reader.queue.push_back(Pending { file: String::new(), sheet: top, name: name.clone(), path: String::new(), legacy: String::new(), depth: 1 });
    let mut bytes = text.len();
    let mut first = true;
    while let Some(pending) = reader.queue.pop_front() {
        let owned;
        let source = if first {
            first = false;
            text
        } else {
            match open(&pending.file) {
                Some(t) => {
                    bytes = bytes.saturating_add(t.len());
                    within_limit(bytes)?;
                    owned = t;
                    owned.as_str()
                }
                None => {
                    reader.notes.add("sheet whose file can't be opened left empty");
                    let number = reader.next_number();
                    reader.add_sheet(Sheet::new(pending.sheet, &pending.name, number))?;
                    continue;
                }
            }
        };
        reader.read_file(source, &pending)?;
    }
    reader.finish()
}

/// A sheet file waiting to be read.
struct Pending {
    /// The file's name as the sheet symbol gives it; empty for the top file.
    file: String,
    sheet: SheetId,
    name: String,
    /// The sheet's place in the tree as the file's instance records write it: the top file's id
    /// and the ids of the sheet symbols down to it, each after a `/`. Empty until the top file's
    /// id is known.
    path: String,
    /// The same place as KiCad 6 writes it in the top file's `symbol_instances`: the ids of the
    /// sheet symbols alone, without the top file's.
    legacy: String,
    /// How deep the sheet is, the top sheet being 1.
    depth: usize,
}

/// A symbol of a file's `lib_symbols`, converted.
#[derive(Clone)]
struct LibSymbol {
    symbol: SymbolId,
    /// For a power symbol, where it connects and what that pin is called: its power input pin,
    /// in symbol coordinates.
    power: Option<(Point, String)>,
    units: usize,
}

/// The most distinct sheet-to-sheet references a schematic may make. Each new one is checked for
/// a loop and for depth by a walk over those known, so the work grows with the square of this:
/// at most about a hundred million steps. Real designs have tens.
const MAX_SHEET_REFERENCES: usize = 10_000;

/// The reader's state across all files.
struct Project {
    ids: Ids,
    notes: Notes,
    schematic: Schematic,
    /// Every id handed out so far, so ids taken from the files never repeat (a sheet file copied
    /// in KiCad keeps the ids of its original).
    taken: BTreeSet<u128>,
    /// The sheet for each file name read or waiting.
    files: BTreeMap<String, SheetId>,
    queue: VecDeque<Pending>,
    /// Symbol copies, each kept once.
    symbols: Vec<Symbol>,
    /// The units of those copies, together.
    units_kept: usize,
    /// The copies by name, library link and prefix, to find a symbol kept already.
    by_name: BTreeMap<(String, String, String, String), Vec<usize>>,
    /// Components by designator, for the units of one component on several sheets, with their
    /// field names.
    designators: BTreeMap<String, (ComponentRef, SymbolId, BTreeSet<String>)>,
    /// KiCad 6's designators and units from the top file's `symbol_instances`, by path.
    legacy: BTreeMap<String, (String, Option<u32>)>,
    /// Which files each file's sheet symbols name, to find a loop.
    references: BTreeMap<String, BTreeSet<String>>,
    reference_count: usize,
    sheets_read: u32,
}

impl Project {
    fn new(text: &str, name: &str) -> Project {
        Project {
            ids: Ids::new(text),
            notes: Notes::default(),
            schematic: Schematic::new(name),
            taken: BTreeSet::new(),
            files: BTreeMap::new(),
            queue: VecDeque::new(),
            symbols: Vec::new(),
            units_kept: 0,
            by_name: BTreeMap::new(),
            designators: BTreeMap::new(),
            legacy: BTreeMap::new(),
            references: BTreeMap::new(),
            reference_count: 0,
            sheets_read: 0,
        }
    }

    /// A fresh id that no item has.
    fn fresh(&mut self) -> u128 {
        loop {
            let id = self.ids.item().as_u128();
            if self.taken.insert(id) {
                return id;
            }
        }
    }

    /// The id the file gives `node` (its `uuid`), if it is a usable one not yet taken; a fresh
    /// one otherwise.
    fn id_of(&mut self, node: &Node) -> ItemId {
        let from_file = node.child("uuid").and_then(|u| u.text(0)).and_then(|t| t.parse::<ItemId>().ok()).map(ItemId::as_u128);
        match from_file {
            Some(id) if id != 0 && self.taken.insert(id) => ItemId::from_u128(id),
            _ => ItemId::from_u128(self.fresh()),
        }
    }

    fn next_sheet_id(&mut self) -> SheetId {
        SheetId::from_u128(self.fresh())
    }

    fn next_number(&mut self) -> u32 {
        self.sheets_read = self.sheets_read.saturating_add(1);
        self.sheets_read
    }

    fn add_sheet(&mut self, sheet: Sheet) -> Result<(), ImportError> {
        if self.schematic.sheets.len() >= MAX_SHEETS {
            return Err(ImportError::unsupported(format!("the schematic has more than {MAX_SHEETS} sheets")));
        }
        self.schematic.sheets.insert(sheet)?;
        Ok(())
    }

    /// Reads one file into the sheet `pending` names.
    fn read_file(&mut self, text: &str, pending: &Pending) -> Result<(), ImportError> {
        let (mut reader, head) = Reader::open(text)?;
        if head != "kicad_sch" {
            return Err(ImportError::WrongFormat("a KiCad schematic"));
        }
        let number = self.next_number();
        let mut file = File {
            sheet: Sheet::new(pending.sheet, &pending.name, number),
            lib: BTreeMap::new(),
            nodes: BTreeMap::new(),
            entries: BTreeMap::new(),
            path: pending.path.clone(),
            legacy: pending.legacy.clone(),
        };
        while let Some(item) = reader.next_item()? {
            if item.head() == Some("lib_symbols") {
                file.index(item);
            } else {
                self.item(&mut file, &item, pending)?;
            }
        }
        self.add_sheet(file.sheet)
    }

    /// One top-level item of a file.
    fn item(&mut self, file: &mut File<'_>, node: &Node, pending: &Pending) -> Result<(), ImportError> {
        match node.head().unwrap_or_default() {
            "version" => {
                if node.number(0).is_some_and(|v| v > NEWEST_KNOWN) {
                    self.notes.add("file newer than KiCad 9: read as far as it goes");
                }
            }
            "uuid" => {
                // The top file's id starts every instance path.
                if pending.file.is_empty()
                    && let Some(id) = node.text(0)
                {
                    file.path = format!("/{}", id.to_ascii_lowercase());
                }
            }
            "paper" => file.sheet.page = self.page(node),
            "junction" => {
                if let Some(position) = sheet_point(node.child("at"), 0) {
                    let id = self.id_of(node);
                    file.sheet.junctions.insert(Junction { id, position })?;
                }
            }
            "no_connect" => {
                if let Some(position) = sheet_point(node.child("at"), 0) {
                    let id = self.id_of(node);
                    file.sheet.no_connects.insert(NoConnect { id, position })?;
                }
            }
            "bus_entry" => {
                let at = node.child("at");
                let size = node.child("size");
                let from = sheet_point(at, 0);
                let to = at.zip(size).and_then(|(a, s)| sheet_xy(a.number(0)? + s.number(0)?, a.number(1)? + s.number(1)?));
                match from.zip(to) {
                    Some((from, to)) => {
                        let id = self.id_of(node);
                        file.sheet.bus_entries.insert(BusEntry { id, from, to })?;
                    }
                    None => self.notes.add("bus entry out of range left out"),
                }
            }
            kind @ ("wire" | "bus") => match points(node, false) {
                Some(points) if points.len() >= 2 => {
                    let id = self.id_of(node);
                    let wire = Wire { id, points };
                    if kind == "wire" {
                        file.sheet.wires.insert(wire)?
                    } else {
                        file.sheet.buses.insert(wire)?
                    };
                }
                _ => self.notes.add("wire or bus without two points in range left out"),
            },
            kind @ ("polyline" | "rectangle" | "circle" | "arc" | "bezier") => match drawing(node, kind, false) {
                Some(drawing) => {
                    let id = self.id_of(node);
                    file.sheet.drawings.insert(SheetDrawing { id, drawing })?;
                }
                None => self.notes.add("drawing out of range left out"),
            },
            "text" => match text(node, false) {
                Some(text) => {
                    let id = self.id_of(node);
                    file.sheet.notes.insert(Note { id, text })?;
                }
                None => self.notes.add("text that is blank or out of range left out"),
            },
            "text_box" => self.notes.add("text box left out"),
            kind @ ("label" | "global_label" | "hierarchical_label") => self.label(file, node, kind)?,
            "symbol" => self.placed_symbol(file, node)?,
            "sheet" => self.sheet_symbol(file, node, pending)?,
            "image" => self.notes.add("image left out"),
            "bus_alias" => self.notes.add("bus alias left out"),
            "netclass_flag" | "directive_label" => self.notes.add("net class directive left out"),
            "rule_area" => self.notes.add("rule area left out"),
            "table" => self.notes.add("table left out"),
            "generator" | "generator_version" | "title_block" | "sheet_instances" | "symbol_instances" | "embedded_fonts" | "embedded_files" => {}
            other => self.notes.add(&format!("{} left out", other.chars().take(40).collect::<String>())),
        }
        Ok(())
    }

    /// The page size of a `paper` entry.
    fn page(&mut self, node: &Node) -> Option<PageSize> {
        // Sizes in millimetres, landscape.
        let size = match node.text(0)? {
            "A5" => Some((210.0, 148.0)),
            "A4" => Some((297.0, 210.0)),
            "A3" => Some((420.0, 297.0)),
            "A2" => Some((594.0, 420.0)),
            "A1" => Some((841.0, 594.0)),
            "A0" => Some((1189.0, 841.0)),
            "A" | "USLetter" => Some((279.4, 215.9)),
            "B" | "USLedger" => Some((431.8, 279.4)),
            "USLegal" => Some((355.6, 215.9)),
            "C" => Some((558.8, 431.8)),
            "D" => Some((863.6, 558.8)),
            "E" => Some((1117.6, 863.6)),
            "User" => node.number(1).zip(node.number(2)),
            _ => None,
        };
        let Some((w, h)) = size else {
            self.notes.add("page size not known: left out");
            return None;
        };
        let (w, h) = if node.flag("portrait") { (h, w) } else { (w, h) };
        Some(PageSize { width: positive(w)?, height: positive(h)? })
    }

    /// The symbol a placed symbol draws with, by its name in the file, converted the first time
    /// a placed symbol uses it; none for a name the file doesn't hold or can't be read.
    fn lib_entry(&mut self, file: &mut File<'_>, lib_id: &str) -> Option<LibSymbol> {
        if let Some(done) = file.lib.get(lib_id) {
            return done.clone();
        }
        let entry = self.convert_entry(file, lib_id);
        file.lib.insert(lib_id.to_owned(), entry.clone());
        entry
    }

    fn convert_entry(&mut self, file: &File<'_>, lib_id: &str) -> Option<LibSymbol> {
        let node = file.nodes.get(lib_id)?;
        // A derived symbol draws as its parent does; only its fields differ.
        let mut drawn = node;
        for _ in 0..16 {
            let Some(parent) = drawn.child("extends").and_then(|e| e.text(0)) else { break };
            match file.nodes.get(parent).or_else(|| file.entries.get(parent).and_then(|full| file.nodes.get(full))) {
                Some(p) => drawn = p,
                None => {
                    self.notes.add("symbol that extends a missing one left out");
                    return None;
                }
            }
        }
        let Some((symbol, power)) = self.convert_symbol(lib_id, node, drawn) else {
            self.notes.add("symbol that can't be read left out");
            return None;
        };
        let units = symbol.units.len();
        // A power symbol joins the net it names where its power input pin is. One without such
        // a pin (KiCad's power flag, which drives a net for the electrical rules check) is an
        // ordinary component.
        let power =
            power.then(|| std::iter::once(&symbol.shared).chain(&symbol.units).flat_map(|u| &u.pins).find(|p| p.kind == PinKind::PowerIn)).flatten();
        let power = power.map(|p| (p.position, p.name.clone()));
        Some(LibSymbol { symbol: self.keep_symbol(symbol), power, units })
    }

    /// A symbol in the model's terms: what all units share, and each unit. `node` gives
    /// its name and fields, `drawn` its drawing (the same unless it extends another).
    fn convert_symbol(&mut self, lib_id: &str, node: &Node, drawn: &Node) -> Option<(Symbol, bool)> {
        let (library, entry) = match lib_id.split_once(':') {
            Some((library, entry)) if !library.trim().is_empty() => (library, entry),
            _ => ("", entry_name(lib_id)),
        };
        let name = clip(entry);
        if name.trim().is_empty() {
            return None;
        }
        let power = node.child("power").is_some() || node.flag("power");
        let prefix = property(node, "Reference").map(|p| clip(p.trim_end_matches('?'))).unwrap_or_default();
        let names_hidden = drawn.child("pin_names").is_some_and(hidden);
        let numbers_hidden = drawn.child("pin_numbers").is_some_and(hidden);
        // Unit and body style from each unit's name, `NAME_UNIT_STYLE`; unit 0 is common to all.
        let mut common = SymbolUnit::empty();
        let mut units: BTreeMap<usize, SymbolUnit> = BTreeMap::new();
        let mut styles_left_out = false;
        for unit in drawn.all("symbol") {
            let Some((number, style)) = unit.text(0).and_then(unit_and_style) else {
                self.notes.add("symbol unit with a name that says no unit left out");
                continue;
            };
            if style > 1 {
                styles_left_out = true;
                continue;
            }
            if number > MAX_UNITS {
                self.notes.add("symbol unit numbered beyond the units a symbol may have left out");
                continue;
            }
            let target = if number == 0 { &mut common } else { units.entry(number).or_insert_with(SymbolUnit::empty) };
            if let Some(n) = unit.child("unit_name").and_then(|n| n.text(0)) {
                target.name = clip(n);
            }
            for part in unit.args() {
                match part.head().unwrap_or_default() {
                    kind @ ("polyline" | "rectangle" | "circle" | "arc" | "bezier") => match drawing(part, kind, true) {
                        Some(d) => target.drawings.push(d),
                        None => self.notes.add("symbol drawing out of range left out"),
                    },
                    "text" => match text(part, true) {
                        Some(t) => target.texts.push(t),
                        None => self.notes.add("symbol text that is blank or out of range left out"),
                    },
                    "pin" => match self.pin(part, names_hidden, numbers_hidden) {
                        Some(p) => target.pins.push(p),
                        None => self.notes.add("pin without a number or out of range left out"),
                    },
                    "text_box" => self.notes.add("text box left out"),
                    _ => {}
                }
            }
        }
        if styles_left_out {
            self.notes.add("alternate body style of a symbol left out");
        }
        // Units numbered 1 to the highest; a number the file skips draws the shared part alone.
        let highest = units.keys().next_back().copied().unwrap_or(1).max(1);
        if self.units_kept.saturating_add(highest) > MAX_TOTAL_UNITS {
            self.notes.add("symbol beyond the units a schematic may hold left out");
            return None;
        }
        let out: Vec<SymbolUnit> = (1..=highest).map(|n| units.remove(&n).unwrap_or_else(SymbolUnit::empty)).collect();
        common.name = String::new();
        let link = SymbolLink { library: clip(library), symbol: name.clone() };
        Some((Symbol { id: SymbolId::from_u128(0), name, link: Some(link), prefix, shared: common, units: out }, power))
    }

    /// One copy of each symbol: a symbol drawn the same as one kept already is that one.
    fn keep_symbol(&mut self, mut symbol: Symbol) -> SymbolId {
        let link = symbol.link.clone().unwrap_or(SymbolLink { library: String::new(), symbol: String::new() });
        let key = (symbol.name.clone(), link.library, link.symbol, symbol.prefix.clone());
        let same = self
            .by_name
            .get(&key)
            .into_iter()
            .flatten()
            .filter_map(|&i| self.symbols.get(i))
            .find(|s| s.shared == symbol.shared && s.units == symbol.units);
        if let Some(same) = same {
            return same.id;
        }
        symbol.id = SymbolId::from_u128(self.fresh());
        let id = symbol.id;
        self.units_kept = self.units_kept.saturating_add(symbol.units.len());
        self.by_name.entry(key).or_default().push(self.symbols.len());
        self.symbols.push(symbol);
        id
    }

    /// A pin of a symbol unit.
    fn pin(&mut self, node: &Node, names_hidden: bool, numbers_hidden: bool) -> Option<Pin> {
        let kind = match node.text(0)? {
            "input" => PinKind::Input,
            "output" => PinKind::Output,
            "bidirectional" => PinKind::Bidirectional,
            "tri_state" => PinKind::TriState,
            "passive" => PinKind::Passive,
            "free" => PinKind::Free,
            "unspecified" => PinKind::Unspecified,
            "power_in" => PinKind::PowerIn,
            "power_out" => PinKind::PowerOut,
            "open_collector" => PinKind::OpenCollector,
            "open_emitter" => PinKind::OpenEmitter,
            "no_connect" => PinKind::NoConnect,
            _ => {
                self.notes.add("pin of a kind not known: read as unspecified");
                PinKind::Unspecified
            }
        };
        if node.text(1).is_some_and(|style| style != "line") {
            self.notes.add("pin drawn with a mark (inverted, clock, ...): drawn as a plain line");
        }
        let at = node.child("at")?;
        let position = symbol_xy(at.number(0)?, at.number(1)?)?;
        let direction = quarter(at.number(2).unwrap_or(0.0))?;
        let length = Nm::from_mm(node.child("length").and_then(|l| l.number(0)).unwrap_or(0.0)).ok().filter(|l| l.0 >= 0)?;
        let name = node.child("name").and_then(|n| n.text(0)).map(|n| if n == "~" { String::new() } else { clip(n) }).unwrap_or_default();
        let number = clip(node.child("number").and_then(|n| n.text(0))?);
        if number.trim().is_empty() {
            return None;
        }
        Some(Pin {
            number,
            name,
            kind,
            position,
            direction,
            length,
            visible: !hidden(node),
            name_visible: !names_hidden,
            number_visible: !numbers_hidden,
        })
    }

    /// A net label, global label or hierarchical label.
    fn label(&mut self, file: &mut File<'_>, node: &Node, kind: &str) -> Result<(), ImportError> {
        let name = node.text(0).map(clip).unwrap_or_default();
        let at = node.child("at");
        let (Some(position), false) = (sheet_point(at, 0), name.trim().is_empty()) else {
            self.notes.add("label that is blank or out of range left out");
            return Ok(());
        };
        let io = || match node.child("shape").and_then(|s| s.text(0)) {
            Some("input") => PortIo::Input,
            Some("output") => PortIo::Output,
            Some("bidirectional") => PortIo::Bidirectional,
            Some("tri_state") => PortIo::TriState,
            Some("passive") => PortIo::Passive,
            _ => PortIo::Unspecified,
        };
        let kind = match kind {
            "global_label" => LabelKind::Global(io()),
            "hierarchical_label" => LabelKind::Port(io()),
            _ => LabelKind::Local,
        };
        let rotation = angle(at.and_then(|a| a.number(2)));
        let height = text_height(node);
        let id = self.id_of(node);
        file.sheet.labels.insert(NetLabel { id, name, kind, position, rotation, height })?;
        Ok(())
    }

    /// A placed symbol: a part of a component, or a power port.
    fn placed_symbol(&mut self, file: &mut File<'_>, node: &Node) -> Result<(), ImportError> {
        let lib_id = node.child("lib_name").or_else(|| node.child("lib_id")).and_then(|n| n.text(0)).unwrap_or_default();
        let Some(lib) = self.lib_entry(file, lib_id) else {
            self.notes.add("placed symbol whose drawing the file doesn't hold left out");
            return Ok(());
        };
        let at = node.child("at");
        let Some(position) = sheet_point(at, 0) else {
            self.notes.add("placed symbol out of range left out");
            return Ok(());
        };
        let turn = at.and_then(|a| a.number(2)).unwrap_or(0.0);
        let mirror = node.child("mirror").and_then(|m| m.text(0));
        // The file turns, then mirrors about an axis of the symbol; the model mirrors left to
        // right first, then turns. Mirroring left to right after turning by `a` is mirroring
        // first and turning by `-a`; mirroring top to bottom is that and half a turn more.
        let (rotation, mirrored) = match mirror {
            Some("y") => (angle(Some(-turn)), true),
            Some("x") => (angle(Some(180.0 - turn)), true),
            _ => (angle(Some(turn)), false),
        };
        // Symbols turn by quarters, so their pins stay where the wires drawn to them end.
        if rotation.0 % 90_000 != 0 {
            self.notes.add("placed symbol turned by other than a quarter turn left out");
            return Ok(());
        }
        let value = property(node, "Value").map(clip).unwrap_or_default();
        if let Some((pin, pin_name)) = lib.power {
            let at = circuitcraft_model::schematic::place(pin, position, rotation, mirrored);
            let (Some(at), false) = (at, value.trim().is_empty()) else {
                self.notes.add("power symbol without a net name or out of range left out");
                return Ok(());
            };
            // The port is drawn turned, never mirrored. Power symbols are symmetric about their
            // pin's axis, so mirroring one and turning it by `rotation` draws it as turning alone.
            // Older KiCad files may name the net after the hidden pin instead; the pin's name
            // stays in the symbol copy, for whoever works out the nets.
            if !pin_name.is_empty() && pin_name != value {
                self.notes.add("power symbol whose value and pin name differ: net named after its value");
            }
            let id = self.id_of(node);
            let port = PowerPort { id, net: value, position: at, rotation, style: PowerStyle::Bar, symbol: Some(lib.symbol) };
            file.sheet.power_ports.insert(port)?;
            return Ok(());
        }
        // The instance record that gives the designator gives the unit too, when it has one the
        // symbol has.
        let (designator, recorded_unit) = self.instance(node, &file.path, &file.legacy_key(node));
        let unit = recorded_unit.filter(|u| usize::try_from(*u).is_ok_and(|u| u <= lib.units)).or_else(|| unit_of(node)).unwrap_or(1);
        if usize::try_from(unit).map_or(true, |u| u > lib.units) {
            self.notes.add("placed symbol of a unit its symbol doesn't have left out");
            return Ok(());
        }
        let fitted = !node.child("dnp").is_some_and(|d| d.text(0) != Some("no"));
        let in_bom = node.child("in_bom").and_then(|b| b.text(0)) != Some("no");
        let on_board = node.child("on_board").and_then(|b| b.text(0)) != Some("no");
        let id = self.id_of(node);
        // Placed symbols with one designator are the units of one component; an unannotated
        // designator (`R?`) stands alone.
        let existing = (!designator.ends_with('?')).then(|| self.designators.get(&designator).cloned()).flatten();
        let (component, field_names) = match existing {
            Some((r, symbol, names)) if symbol == lib.symbol => (r, names),
            other => {
                if other.is_some() {
                    self.notes.add("two components share a designator: kept apart");
                }
                let (footprint, fields) = fields(node);
                let names: BTreeSet<String> = fields.iter().map(|f| f.name.clone()).collect();
                let component = Component {
                    id,
                    symbol: lib.symbol,
                    designator: designator.clone(),
                    value: value.clone(),
                    footprint,
                    fields,
                    in_bom,
                    on_board,
                    fitted,
                };
                file.sheet.components.insert(component)?;
                let r = ComponentRef { sheet: file.sheet.id, component: id };
                if other.is_none() && !designator.ends_with('?') {
                    self.designators.insert(designator, (r, lib.symbol, names.clone()));
                }
                (r, names)
            }
        };
        // The component took the file's id when this placed symbol made it; the part has one of
        // its own then.
        let part_id = if component.component == id { ItemId::from_u128(self.fresh()) } else { id };
        let texts = part_texts(node, &field_names, turn);
        file.sheet.parts.insert(Part { id: part_id, component, unit, position, rotation, mirrored, texts })?;
        Ok(())
    }

    /// The designator a placed symbol has at the sheet's place in the tree, and the unit the same
    /// record gives, if it gives one: its instance record for that path, else its first record
    /// (KiCad 7 and later), else the top file's record for it (KiCad 6), else its `Reference`
    /// field. `?` when it has none.
    fn instance(&self, node: &Node, path: &str, legacy_key: &str) -> (String, Option<u32>) {
        let records: Vec<&Node> = node.child("instances").into_iter().flat_map(|i| i.all("project")).flat_map(|p| p.all("path")).collect();
        let here = records.iter().find(|r| r.text(0).is_some_and(|p| p.eq_ignore_ascii_case(path)));
        let from_record = here.or(records.first()).and_then(|r| Some((r.child("reference")?.text(0)?, unit_of(r))));
        let (reference, unit) = from_record
            .or_else(|| self.legacy.get(legacy_key).map(|(r, u)| (r.as_str(), *u)))
            .or_else(|| property(node, "Reference").map(|r| (r, None)))
            .unzip();
        match reference.map(clip) {
            Some(r) if !r.trim().is_empty() => (r, unit.flatten()),
            _ => ("?".to_owned(), unit.flatten()),
        }
    }

    /// A sheet symbol, and its file queued to be read.
    fn sheet_symbol(&mut self, file: &mut File<'_>, node: &Node, pending: &Pending) -> Result<(), ImportError> {
        let at = node.child("at");
        let size = node.child("size");
        let from = sheet_point(at, 0);
        let to = at.zip(size).and_then(|(a, s)| sheet_xy(a.number(0)? + s.number(0)?, a.number(1)? + s.number(1)?));
        let name = property(node, "Sheetname").or_else(|| property(node, "Sheet name")).map(clip).unwrap_or_default();
        let target = property(node, "Sheetfile").or_else(|| property(node, "Sheet file")).map(file_name).unwrap_or_default();
        let (Some(from), Some(to), false) = (from, to, target.trim().is_empty()) else {
            self.notes.add("sheet symbol without a file or out of range left out");
            return Ok(());
        };
        // A reference made before was checked then; a new one is checked against those known.
        let known = self.references.get(&pending.file).is_some_and(|names| names.contains(&target));
        if !known {
            // A sheet that already leads, through the sheets it names, to this one would close a
            // loop.
            if target == pending.file || self.leads_to(&target, &pending.file) {
                self.notes.add("sheet symbol for a sheet that contains it left out");
                return Ok(());
            }
            // The sheets known below the target count too when it is used again here. (Sheets
            // below it that are found later can still make the schematic too deep, which its
            // check then refuses.)
            if pending.depth.saturating_add(self.levels_from(&target)) > MAX_SHEET_DEPTH {
                self.notes.add("sheet nested deeper than a schematic may go left out");
                return Ok(());
            }
            self.references.entry(pending.file.clone()).or_default().insert(target.clone());
            self.reference_count += 1;
            if self.reference_count > MAX_SHEET_REFERENCES {
                return Err(ImportError::unsupported(format!("the schematic's sheets name each other more than {MAX_SHEET_REFERENCES} times")));
            }
        }
        let id = self.id_of(node);
        let sheet = match self.files.get(&target) {
            Some(&sheet) => {
                self.notes.add("sheet used more than once: read once, with the designators of its first use");
                sheet
            }
            None => {
                if self.files.len() >= MAX_SHEETS {
                    return Err(ImportError::unsupported(format!("the schematic has more than {MAX_SHEETS} sheets")));
                }
                let sheet = self.next_sheet_id();
                self.files.insert(target.clone(), sheet);
                let display = if name.trim().is_empty() { target.clone() } else { name.clone() };
                // The path of the sheet below: this sheet's, then this sheet symbol's file id.
                let own = node.child("uuid").and_then(|u| u.text(0)).unwrap_or_default().to_ascii_lowercase();
                self.queue.push_back(Pending {
                    file: target,
                    sheet,
                    name: clip(&display),
                    path: format!("{}/{own}", file.path),
                    legacy: format!("{}/{own}", pending.legacy),
                    depth: pending.depth.saturating_add(1),
                });
                sheet
            }
        };
        let mut entries = Vec::new();
        for pin in node.all("pin") {
            if entries.len() >= MAX_SHEET_ENTRIES {
                self.notes.add("sheet entries beyond those a sheet symbol may have left out");
                break;
            }
            let name = pin.text(0).map(clip).unwrap_or_default();
            let io = match pin.text(1) {
                Some("input") => PortIo::Input,
                Some("output") => PortIo::Output,
                Some("bidirectional") => PortIo::Bidirectional,
                Some("tri_state") => PortIo::TriState,
                Some("passive") => PortIo::Passive,
                _ => PortIo::Unspecified,
            };
            match (sheet_point(pin.child("at"), 0), name.trim().is_empty()) {
                (Some(position), false) => {
                    let id = self.id_of(pin);
                    entries.push(SheetEntry { id, name, io, position });
                }
                _ => self.notes.add("sheet entry that is blank or out of range left out"),
            }
        }
        file.sheet.sheet_symbols.insert(SheetSymbol { id, sheet, name, from, to, entries })?;
        Ok(())
    }

    /// Whether the file `from` names `to`, directly or through the files it names.
    fn leads_to(&self, from: &str, to: &str) -> bool {
        let mut seen: BTreeSet<&str> = BTreeSet::from([from]);
        let mut todo: Vec<&str> = vec![from];
        while let Some(file) = todo.pop() {
            for next in self.references.get(file).into_iter().flatten() {
                if next == to {
                    return true;
                }
                if seen.insert(next.as_str()) {
                    todo.push(next);
                }
            }
        }
        false
    }

    /// How many levels of sheets `file` stands for as far as is known: itself and the deepest chain
    /// of files it names. The references hold no loop, which [`Project::leads_to`] keeps out.
    fn levels_from(&self, file: &str) -> usize {
        let mut levels: BTreeMap<&str, usize> = BTreeMap::new();
        // An explicit stack in place of recursion: each file is finished once all it names are.
        let mut stack: Vec<(&str, bool)> = vec![(file, false)];
        while let Some((f, expanded)) = stack.pop() {
            if levels.contains_key(f) {
                continue;
            }
            let names = self.references.get(f);
            if expanded {
                let below = names.into_iter().flatten().filter_map(|n| levels.get(n.as_str())).max().copied().unwrap_or(0);
                levels.insert(f, below.saturating_add(1));
            } else {
                stack.push((f, true));
                stack.extend(names.into_iter().flatten().filter(|n| !levels.contains_key(n.as_str())).map(|n| (n.as_str(), false)));
            }
        }
        levels.get(file).copied().unwrap_or(1)
    }

    /// The symbols kept, then the schematic, checked.
    fn finish(mut self) -> Result<ImportedSchematic, ImportError> {
        for symbol in std::mem::take(&mut self.symbols) {
            self.schematic.symbols.insert(symbol)?;
        }
        self.schematic.validate()?;
        Ok(ImportedSchematic { schematic: self.schematic, notes: self.notes })
    }
}

/// What one file is building: its sheet, the symbols it draws with by their names in the file,
/// and its place in the tree.
struct File<'a> {
    sheet: Sheet,
    /// The symbols converted so far, by name; none for one that can't be.
    lib: BTreeMap<String, Option<LibSymbol>>,
    /// The file's `lib_symbols`, by name, waiting to be converted when a placed symbol uses one.
    nodes: BTreeMap<String, Node<'a>>,
    /// Full names by entry name (`R` → `Device:R`), for a symbol that extends another by its
    /// entry name.
    entries: BTreeMap<String, String>,
    path: String,
    /// The sheet's place as KiCad 6 writes it ([`Pending::legacy`]).
    legacy: String,
}

impl<'a> File<'a> {
    /// Files the symbols of a `lib_symbols` list under their names; the first of a name counts.
    fn index(&mut self, lib: Node<'a>) {
        let Node::List(parts) = lib else { return };
        for symbol in parts.into_iter().skip(1) {
            if symbol.head() != Some("symbol") {
                continue;
            }
            let Some(name) = symbol.text(0).map(str::to_owned) else { continue };
            self.entries.entry(entry_name(&name).to_owned()).or_insert_with(|| name.clone());
            self.nodes.entry(name).or_insert(symbol);
        }
    }
}

impl File<'_> {
    /// Where KiCad 6 files a placed symbol's designator: the sheet's place in the tree, then the
    /// placed symbol's id.
    fn legacy_key(&self, node: &Node) -> String {
        format!("{}/{}", self.legacy, node.child("uuid").and_then(|u| u.text(0)).unwrap_or_default().to_ascii_lowercase())
    }
}

/// KiCad 6's designators: the top file's `symbol_instances`, each placed symbol's designator by
/// its path (the sheet symbols' ids down to its sheet, then its own id, each after a `/`). Later
/// versions record them with each placed symbol instead, and have no such list.
fn legacy_designators(text: &str) -> Result<BTreeMap<String, (String, Option<u32>)>, ImportError> {
    let mut out = BTreeMap::new();
    let (mut reader, _) = Reader::open(text)?;
    while let Some(item) = reader.next_item()? {
        if item.head() != Some("symbol_instances") {
            continue;
        }
        for record in item.all("path") {
            let (Some(path), Some(reference)) = (record.text(0), record.child("reference").and_then(|r| r.text(0))) else { continue };
            if out.len() >= circuitcraft_model::MAX_ITEMS {
                break;
            }
            out.entry(path.to_ascii_lowercase()).or_insert_with(|| (clip(reference), unit_of(record)));
        }
    }
    Ok(out)
}

/// The unit a node's `(unit N)` names, if it names one a symbol may have.
fn unit_of(node: &Node) -> Option<u32> {
    node.child("unit").and_then(|u| u.number(0)).filter(|u| *u >= 1.0 && *u <= MAX_UNITS as f64).map(|u| u as u32)
}

/// A sheet file's name as a sheet symbol gives it, with `/` between folders and no leading `./`,
/// so that two ways of writing one name are one file.
fn file_name(name: &str) -> String {
    let name = name.replace('\\', "/");
    let mut rest = name.as_str();
    while let Some(r) = rest.strip_prefix("./") {
        rest = r;
    }
    rest.to_owned()
}

/// The entry name of a library identifier (`Device:R` → `R`).
fn entry_name(lib_id: &str) -> &str {
    lib_id.rsplit_once(':').map_or(lib_id, |(_, entry)| entry)
}

/// The unit and body style of a unit's name, `NAME_UNIT_STYLE`.
fn unit_and_style(name: &str) -> Option<(usize, u32)> {
    let mut parts = name.rsplitn(3, '_');
    let style = parts.next()?.parse().ok()?;
    let unit = parts.next()?.parse().ok()?;
    parts.next()?;
    Some((unit, style))
}

/// At most [`MAX_NAME_CHARS`] characters of `s`.
fn clip(s: &str) -> String {
    s.chars().take(MAX_NAME_CHARS).collect()
}

/// A field's value, by its key.
fn property<'n>(node: &'n Node, key: &str) -> Option<&'n str> {
    node.all("property").find(|p| p.text(0) == Some(key)).and_then(|p| p.text(1))
}

/// Whether a node, or the effects inside it, says hide: the bare word, or `(hide yes)`.
fn hidden(node: &Node) -> bool {
    let says = |n: &Node| n.flag("hide") || n.child("hide").is_some_and(|h| h.text(0) != Some("no"));
    says(node) || node.child("effects").is_some_and(says)
}

/// A point of a sheet, from millimetres of the file: y turns over.
fn sheet_xy(x: f64, y: f64) -> Option<Point> {
    Some(Point::new(Nm::from_mm(x).ok()?, Nm::from_mm(-y).ok()?))
}

/// A point of a symbol, from millimetres of the file: y already grows upward.
fn symbol_xy(x: f64, y: f64) -> Option<Point> {
    Some(Point::new(Nm::from_mm(x).ok()?, Nm::from_mm(y).ok()?))
}

/// The two numbers of `node` from its `i`-th argument on, as a point of a sheet.
fn sheet_point(node: Option<&Node>, i: usize) -> Option<Point> {
    let node = node?;
    sheet_xy(node.number(i)?, node.number(i + 1)?)
}

/// A point of a sheet or of a symbol.
fn xy(node: &Node, i: usize, in_symbol: bool) -> Option<Point> {
    let (x, y) = (node.number(i)?, node.number(i + 1)?);
    if in_symbol { symbol_xy(x, y) } else { sheet_xy(x, y) }
}

/// A length in millimetres that must be positive.
fn positive(mm: f64) -> Option<Nm> {
    Nm::from_mm(mm).ok().filter(|n| n.0 > 0)
}

/// A rotation from degrees, counter-clockwise on screen; none for a value no angle has.
fn angle(degrees: Option<f64>) -> Rotation {
    degrees.and_then(|d| import::degrees(d).ok()).unwrap_or_default()
}

/// A quarter-turn direction from degrees.
fn quarter(degrees: f64) -> Option<Rotation> {
    let r = import::degrees(degrees).ok()?;
    (r.0 % 90_000 == 0).then_some(r)
}

/// The character height a text's effects give, or the usual 1.27 mm.
fn text_height(node: &Node) -> Nm {
    node.child("effects")
        .and_then(|e| e.child("font"))
        .and_then(|f| f.child("size"))
        .and_then(|s| s.number(0))
        .and_then(positive)
        .unwrap_or(Nm(1_270_000))
}

/// How a text's effects anchor it from side to side.
fn justify(node: &Node) -> Justify {
    match node.child("effects").and_then(|e| e.child("justify")) {
        Some(j) if j.flag("left") => Justify::Left,
        Some(j) if j.flag("right") => Justify::Right,
        _ => Justify::Center,
    }
}

/// The points of a `pts` list.
fn points(node: &Node, in_symbol: bool) -> Option<Vec<Point>> {
    node.child("pts")?.all("xy").map(|p| xy(p, 0, in_symbol)).collect()
}

/// A drawing of a sheet or of a symbol.
fn drawing(node: &Node, kind: &str, in_symbol: bool) -> Option<Drawing> {
    let at = |name: &str| node.child(name).and_then(|n| xy(n, 0, in_symbol));
    let shape = match kind {
        "polyline" => DrawingShape::Polyline { points: points(node, in_symbol).filter(|p| p.len() >= 2)? },
        "bezier" => {
            let p = points(node, in_symbol)?;
            let [a, b, c, d] = <[Point; 4]>::try_from(p).ok()?;
            DrawingShape::Polyline { points: import::bezier([a, b, c, d]) }
        }
        "rectangle" => DrawingShape::Rectangle { from: at("start")?, to: at("end")? },
        "circle" => DrawingShape::Circle { center: at("center")?, radius: positive(node.child("radius")?.number(0)?)? },
        "arc" => DrawingShape::Arc { arc: Arc { start: at("start")?, mid: at("mid")?, end: at("end")? } },
        _ => return None,
    };
    let width =
        Nm::from_mm(node.child("stroke").and_then(|s| s.child("width")).and_then(|w| w.number(0)).unwrap_or(0.0)).ok().filter(|w| w.0 >= 0)?;
    let fill = match node.child("fill").and_then(|f| f.child("type")).and_then(|t| t.text(0)) {
        Some("outline") => Fill::Solid,
        Some("background") => Fill::Background,
        _ => Fill::None,
    };
    Some(Drawing { shape, width, fill })
}

/// Text of a sheet or of a symbol.
fn text(node: &Node, in_symbol: bool) -> Option<SheetText> {
    let words = clip(node.text(0)?);
    if words.trim().is_empty() {
        return None;
    }
    let at = node.child("at")?;
    let position = xy(at, 0, in_symbol)?;
    let raw = at.number(2).unwrap_or(0.0);
    // The format says a symbol's text gives its angle in tenths of a degree; the files seen
    // write whole degrees. Tenths unless only degrees make a quarter turn of it.
    let degrees = if in_symbol && (raw / 10.0) % 90.0 != 0.0 && raw % 90.0 == 0.0 {
        raw
    } else if in_symbol {
        raw / 10.0
    } else {
        raw
    };
    Some(SheetText { text: words, position, rotation: angle(Some(degrees)), height: text_height(node), justify: justify(node) })
}

/// A component's footprint and other fields from its placed symbol's properties. The
/// designator, value and KiCad's own keys are not fields; an empty datasheet (`~`) is empty.
fn fields(node: &Node) -> (Option<LibraryLink>, Vec<Field>) {
    let mut footprint = None;
    let mut fields: Vec<Field> = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    for p in node.all("property") {
        let (Some(key), Some(value)) = (p.text(0), p.text(1)) else { continue };
        match key {
            "Reference" | "Value" => {}
            "Footprint" => {
                footprint = (!value.trim().is_empty()).then(|| match value.split_once(':') {
                    Some((library, entry)) => LibraryLink { library: clip(library), footprint: clip(entry) },
                    None => LibraryLink { library: String::new(), footprint: clip(value) },
                });
                if footprint.as_ref().is_some_and(|f| f.footprint.trim().is_empty() || (!f.library.is_empty() && f.library.trim().is_empty())) {
                    footprint = None;
                }
            }
            k if k.starts_with("ki_") || k.trim().is_empty() || fields.len() >= circuitcraft_model::schematic::MAX_FIELDS => {}
            k => {
                let name = clip(k);
                if names.insert(name.clone()) {
                    fields.push(Field { name, value: if value == "~" { String::new() } else { clip(value) } });
                }
            }
        }
    }
    (footprint, fields)
}

/// The texts a placed symbol shows: its designator, value and those of its component's fields
/// (`fields`) the file shows, where the file puts them. The file gives a text's angle relative to
/// the symbol, turned by `turn` degrees; the text is drawn level or reading upward.
fn part_texts(node: &Node, fields: &BTreeSet<String>, turn: f64) -> Vec<PartText> {
    let mut texts = Vec::new();
    for p in node.all("property") {
        let Some(key) = p.text(0) else { continue };
        let content = match key {
            "Reference" => PartTextContent::Designator,
            "Value" => PartTextContent::Value,
            // The footprint is no field, so its text has nothing to show.
            "Footprint" => continue,
            k if !fields.contains(&clip(k)) => continue,
            k => PartTextContent::Field(clip(k)),
        };
        let at = p.child("at");
        let Some(position) = sheet_point(at, 0) else { continue };
        if texts.len() >= circuitcraft_model::schematic::MAX_PART_TEXTS {
            break;
        }
        texts.push(PartText {
            content,
            visible: !hidden(p),
            position,
            rotation: angle(Some((at.and_then(|a| a.number(2)).unwrap_or(0.0) + turn).rem_euclid(180.0))),
            height: text_height(p),
            justify: justify(p),
        });
    }
    texts
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitcraft_model::schematic::PinKind;

    const TOP: &str = include_str!("../samples/schematic.kicad_sch");
    const AMPLIFIER: &str = include_str!("../samples/amplifier.kicad_sch");

    fn mm(x: f64, y: f64) -> Point {
        Point::new(Nm::from_mm(x).expect("x"), Nm::from_mm(y).expect("y"))
    }

    /// The sample, its child sheet given by name, and how often each file was asked for.
    fn sample() -> (ImportedSchematic, BTreeMap<String, usize>) {
        let mut asked = BTreeMap::new();
        let read = read_schematic(TOP, "Sample", &mut |name| {
            *asked.entry(name.to_owned()).or_default() += 1;
            (name == "amplifier.kicad_sch").then(|| AMPLIFIER.to_owned())
        })
        .expect("import");
        (read, asked)
    }

    fn sheet<'s>(s: &'s Schematic, name: &str) -> &'s Sheet {
        s.sheets.iter().find(|sheet| sheet.name == name).expect("sheet")
    }

    fn component<'s>(s: &'s Schematic, designator: &str) -> &'s Component {
        s.sheets.iter().flat_map(|sheet| sheet.components.iter()).find(|c| c.designator == designator).expect("component")
    }

    /// Where each pin of each part lands on the sheet.
    fn pin_ends(s: &Schematic, sheet: &Sheet) -> Vec<(String, String, Point)> {
        let mut out = Vec::new();
        for part in sheet.parts.iter() {
            let c = s.component(part.component).expect("component");
            let symbol = s.symbols.get(c.symbol).expect("symbol");
            let unit = symbol.unit(part.unit).expect("unit");
            for pin in &unit.pins {
                out.push((c.designator.clone(), pin.number.clone(), part.to_sheet(pin.position).expect("in range")));
            }
        }
        out
    }

    #[test]
    fn reads_the_sample_and_its_sheet_below() {
        let (read, asked) = sample();
        let s = &read.schematic;
        assert_eq!(asked, BTreeMap::from([("amplifier.kicad_sch".to_owned(), 1)]));
        assert_eq!(s.sheets.len(), 2);
        let top = sheet(s, "Sample");
        let amp = sheet(s, "Amplifier");
        assert_eq!((top.number, amp.number), (1, 2));
        assert_eq!(s.top_sheets().iter().map(|t| t.id).collect::<Vec<_>>(), vec![top.id]);
        assert_eq!(top.page, Some(PageSize { width: Nm(297_000_000), height: Nm(210_000_000) }));
        assert_eq!(amp.page, Some(PageSize { width: Nm(120_000_000), height: Nm(200_000_000) }), "portrait turns the user size");

        let block = top.sheet_symbols.iter().next().expect("sheet symbol");
        assert_eq!((block.sheet, block.name.as_str()), (amp.id, "Amplifier"));
        assert_eq!((block.from, block.to), (mm(180.0, -40.0), mm(205.4, -60.0)));
        assert_eq!(block.entries.iter().map(|e| (e.name.as_str(), e.io)).collect::<Vec<_>>(), [("IN", PortIo::Input), ("OUT", PortIo::Output)]);

        let notes: Vec<String> = read.notes.lines();
        for expected in [
            "image left out",
            "frobnicate left out",
            "alternate body style of a symbol left out",
            "pin drawn with a mark (inverted, clock, ...): drawn as a plain line",
        ] {
            assert!(notes.iter().any(|n| n.ends_with(expected)), "{expected:?} not in {notes:?}");
        }
    }

    #[test]
    fn keeps_one_copy_of_each_symbol() {
        let (read, _) = sample();
        let s = &read.schematic;
        // One copy per symbol, though both files hold the amplifier.
        let mut names: Vec<&str> = s.symbols.iter().map(|sym| sym.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["+3V3", "Dual", "GND", "R"]);
        let dual = s.symbols.iter().find(|sym| sym.name == "Dual").expect("dual");
        assert_eq!(dual.units.len(), 3);
        assert_eq!(dual.units.get(2).map(|u| u.name.as_str()), Some("Power"));
        assert_eq!(dual.link, Some(SymbolLink { library: "Amplifier".to_owned(), symbol: "Dual".to_owned() }));
        assert_eq!(dual.prefix, "U");
        let power = dual.units.get(2).expect("power unit");
        assert_eq!(power.pins.iter().map(|p| (p.number.as_str(), p.kind)).collect::<Vec<_>>(), [("8", PinKind::PowerIn), ("4", PinKind::PowerIn)]);
        assert_eq!(power.texts.first().map(|t| t.rotation), Some(Rotation(90_000)), "900 tenths of a degree");
        let r = s.symbols.iter().find(|sym| sym.name == "R").expect("r");
        let pin = r.units.first().and_then(|u| u.pins.first()).expect("pin");
        assert!(pin.name.is_empty() && !pin.number_visible && pin.name_visible, "{pin:?}");
        assert_eq!((pin.position, pin.direction, pin.length), (mm(0.0, 3.81), Rotation(270_000), Nm(1_270_000)));
        let gnd = s.symbols.iter().find(|sym| sym.name == "GND").expect("gnd");
        assert!(gnd.units.first().and_then(|u| u.pins.first()).is_some_and(|p| !p.visible));
    }

    #[test]
    fn units_with_one_designator_make_one_component() {
        let (read, _) = sample();
        let s = &read.schematic;
        let top = sheet(s, "Sample");
        let amp = sheet(s, "Amplifier");
        // U1 is one component on the top sheet, with a unit on each sheet.
        let u1 = component(s, "U1");
        assert!(top.components.contains(u1.id));
        assert_eq!(u1.value, "LM358");
        assert_eq!(u1.footprint, Some(LibraryLink { library: "Package_SO".to_owned(), footprint: "SOIC-8".to_owned() }));
        let units = |sheet: &Sheet| {
            let mut u: Vec<u32> = sheet.parts.iter().filter(|p| p.component.component == u1.id).map(|p| p.unit).collect();
            u.sort_unstable();
            u
        };
        assert_eq!((units(top), units(amp)), (vec![1, 3], vec![2]));
        assert_eq!(u1.id, "3c4b5a69-7887-4955-ab6c-5d4e3f2a1b0c".parse().expect("id"), "the first unit's id, for the board to find");

        let r1 = component(s, "R1");
        assert_eq!(r1.footprint, Some(LibraryLink { library: "Resistor_SMD".to_owned(), footprint: "R_0603_1608Metric".to_owned() }));
        assert_eq!(
            r1.fields,
            [Field { name: "Datasheet".to_owned(), value: String::new() }, Field { name: "Manufacturer".to_owned(), value: "Acme".to_owned() }]
        );
        assert!(r1.fitted && r1.in_bom && r1.on_board);
        let r2 = component(s, "R2");
        assert!(!r2.fitted, "dnp");
        assert_eq!(r2.footprint, Some(LibraryLink { library: String::new(), footprint: "R_0402".to_owned() }));
        assert!(!component(s, "R?").in_bom);
    }

    #[test]
    fn reads_wires_labels_and_ports() {
        let (read, _) = sample();
        let s = &read.schematic;
        let top = sheet(s, "Sample");
        let amp = sheet(s, "Amplifier");
        assert_eq!(top.power_ports.iter().map(|p| p.net.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from(["+3V3", "GND"]));
        let labels = |sheet: &Sheet| {
            let mut l: Vec<(String, LabelKind)> = sheet.labels.iter().map(|l| (l.name.clone(), l.kind)).collect();
            l.sort_by(|a, b| a.0.cmp(&b.0));
            l
        };
        assert_eq!(
            labels(top),
            [("D0".to_owned(), LabelKind::Local), ("MID".to_owned(), LabelKind::Local), ("VOUT".to_owned(), LabelKind::Global(PortIo::Output))]
        );
        assert_eq!(labels(amp), [("IN".to_owned(), LabelKind::Port(PortIo::Input)), ("OUT".to_owned(), LabelKind::Port(PortIo::Output))]);
        assert_eq!((top.wires.len(), top.buses.len(), top.junctions.len(), top.no_connects.len()), (4, 1, 1, 1));
        let entry = top.bus_entries.iter().next().expect("bus entry");
        assert_eq!((entry.from, entry.to), (mm(60.0, -85.0), mm(62.54, -87.54)));
        assert_eq!(top.notes.iter().next().map(|n| n.text.text.as_str()), Some("Synthetic test schematic"));
        assert_eq!(top.drawings.len(), 1);
    }

    #[test]
    fn pins_land_on_the_wires_drawn_to_them() {
        let (read, _) = sample();
        let s = &read.schematic;
        let ends = pin_ends(s, sheet(s, "Sample"));
        let at = |designator: &str, number: &str| ends.iter().find(|(d, n, _)| d == designator && n == number).map(|e| e.2);
        assert_eq!(at("R1", "1"), Some(mm(100.0, -46.19)));
        assert_eq!(at("R1", "2"), Some(mm(100.0, -53.81)));
        // Turned a quarter in the file, then mirrored top to bottom.
        assert_eq!(at("R2", "1"), Some(mm(116.19, -50.0)));
        assert_eq!(at("U1", "1"), Some(mm(157.62, -60.0)));
        assert_eq!(at("U1", "2"), Some(mm(142.38, -62.54)), "where the no-connect marker is");
        let amp = pin_ends(s, sheet(s, "Amplifier"));
        let wire_ends: Vec<Point> = sheet(s, "Amplifier").wires.iter().flat_map(|w| w.points.clone()).collect();
        for number in ["6", "7"] {
            let end = amp.iter().find(|(_, n, _)| n == number).map(|e| e.2).expect("pin");
            assert!(wire_ends.contains(&end), "pin {number} at {end:?}");
        }
    }

    #[test]
    fn a_missing_sheet_file_leaves_its_sheet_empty() {
        let read = read_schematic(TOP, "Sample", &mut |_| None).expect("import");
        let empty = sheet(&read.schematic, "Amplifier");
        assert!(empty.parts.is_empty() && empty.wires.is_empty());
        assert!(read.notes.lines().iter().any(|n| n == "sheet whose file can't be opened left empty"));
        // The unit of U1 that sheet held is simply not there.
        assert_eq!(read.schematic.validate(), Ok(()));
    }

    /// A top file with sheet symbols for these files.
    fn top_with(files: &[&str]) -> String {
        let sheets: String = files
            .iter()
            .enumerate()
            .map(|(i, f)| format!("(sheet (at {} 10) (size 10 10) (uuid 00000000-0000-4000-8000-00000000000{i}) (property \"Sheetname\" \"S{i}\") (property \"Sheetfile\" \"{f}\"))", 20 * i))
            .collect();
        format!("(kicad_sch (version 20231120) (uuid 11111111-1111-4111-8111-111111111111) {sheets})")
    }

    #[test]
    fn files_beyond_the_size_limit_are_refused() {
        let top = top_with(&["a.kicad_sch"]);
        let child = "(kicad_sch (version 20231120))";
        let refused = |r: Result<ImportedSchematic, ImportError>| matches!(r, Err(ImportError::Unsupported(m)) if m.contains("add up to more than"));
        // The top file alone, before anything else is read.
        let mut asked = false;
        assert!(refused(read_within(
            &top,
            "Top",
            &mut |_| {
                asked = true;
                None
            },
            top.len() - 1
        )));
        assert!(!asked, "an oversized top file is refused before a sheet file is asked for");
        // The top file and a sheet file together.
        assert!(refused(read_within(&top, "Top", &mut |_| Some(child.to_owned()), top.len() + child.len() - 1)));
        assert!(read_within(&top, "Top", &mut |_| Some(child.to_owned()), top.len() + child.len()).is_ok());
    }

    #[test]
    fn a_sheet_used_twice_is_read_once() {
        let mut asked = 0;
        let read = read_schematic(&top_with(&["a.kicad_sch", "a.kicad_sch"]), "Top", &mut |_| {
            asked += 1;
            Some("(kicad_sch (version 20231120))".to_owned())
        })
        .expect("import");
        assert_eq!(asked, 1);
        assert_eq!(read.schematic.sheets.len(), 2);
        let top = sheet(&read.schematic, "Top");
        let below: BTreeSet<SheetId> = top.sheet_symbols.iter().map(|b| b.sheet).collect();
        assert_eq!((top.sheet_symbols.len(), below.len()), (2, 1));
        assert!(read.notes.lines().iter().any(|n| n.starts_with("sheet used more than once")));
    }

    #[test]
    fn a_sheet_that_contains_itself_is_left_out() {
        let looped = top_with(&["loop.kicad_sch"]);
        let read = read_schematic(&looped, "Top", &mut |name| (name == "loop.kicad_sch").then(|| top_with(&["loop.kicad_sch"]))).expect("import");
        assert_eq!(read.schematic.sheets.len(), 2);
        assert!(read.notes.lines().iter().any(|n| n == "sheet symbol for a sheet that contains it left out"));
    }

    #[test]
    fn other_files_and_versions() {
        let none = &mut |_: &str| None;
        assert_eq!(read_schematic("(kicad_pcb (version 1))", "x", none), Err(ImportError::WrongFormat("a KiCad schematic")));
        assert!(matches!(read_schematic("(kicad_sch (version 1)", "x", none), Err(ImportError::Syntax { .. })));
        let newer = read_schematic("(kicad_sch (version 20991231))", " ", none).expect("import");
        assert_eq!(newer.schematic.name, "Imported");
        assert!(newer.notes.lines().iter().any(|n| n.starts_with("file newer than KiCad 9")));
        assert!(matches!(crate::import_schematic("EESchema Schematic File Version 4\n", "x", none), Err(ImportError::Unsupported(_))));
    }

    #[test]
    fn placed_symbols_without_a_drawing_or_unit_are_left_out() {
        let text = r#"(kicad_sch (version 20231120)
            (lib_symbols (symbol "L:One" (property "Reference" "U") (symbol "One_1_1" (pin passive line (at 0 0 0) (length 1) (name "A") (number "1")))))
            (symbol (lib_id "L:Missing") (at 0 0 0) (unit 1) (property "Reference" "U1"))
            (symbol (lib_id "L:One") (at 0 0 0) (unit 2) (property "Reference" "U2"))
            (symbol (lib_id "L:One") (at 10 0 0) (unit 1) (property "Reference" "U3"))
            (symbol (lib_id "L:One") (at 20 0 0) (unit 1) (property "Reference" "U3")))"#;
        let read = read_schematic(text, "x", &mut |_| None).expect("import");
        let top = read.schematic.sheets.iter().next().expect("sheet");
        assert_eq!(top.components.iter().map(|c| c.designator.as_str()).collect::<Vec<_>>(), ["U3"]);
        assert_eq!(top.parts.len(), 2, "two units of U3 that are both unit 1 still both draw");
        let notes = read.notes.lines();
        assert!(notes.iter().any(|n| n == "placed symbol whose drawing the file doesn't hold left out"), "{notes:?}");
        assert!(notes.iter().any(|n| n == "placed symbol of a unit its symbol doesn't have left out"), "{notes:?}");
    }

    /// A one-sheet schematic: these `lib_symbols`, then these items.
    fn one_sheet(lib: &str, items: &str) -> ImportedSchematic {
        let text = format!("(kicad_sch (version 20231120) (uuid 22222222-2222-4222-8222-222222222222) (lib_symbols {lib}) {items})");
        read_schematic(&text, "One", &mut |_| None).expect("import")
    }

    const FLAG: &str = r##"(symbol "power:PWR_FLAG" (power) (property "Reference" "#FLG") (property "Value" "PWR_FLAG")
        (symbol "PWR_FLAG_1_1" (pin power_out line (at 0 0 90) (length 0) (name "pwr") (number "1"))))"##;
    const OFFSET_SUPPLY: &str = r##"(symbol "my:VDD" (power) (property "Reference" "#PWR") (property "Value" "VDD")
        (symbol "VDD_1_1" (pin power_in line (at 0 -2.54 90) (length 2.54) (name "VDD") (number "1"))))"##;

    #[test]
    fn a_power_flag_is_a_component_and_a_supply_connects_at_its_pin() {
        let read = one_sheet(
            &format!("{FLAG} {OFFSET_SUPPLY}"),
            r##"(symbol (lib_id "power:PWR_FLAG") (at 10 10 0) (unit 1) (property "Reference" "#FLG01") (property "Value" "PWR_FLAG"))
               (symbol (lib_id "power:PWR_FLAG") (at 30 10 0) (unit 1) (property "Reference" "#FLG02") (property "Value" "PWR_FLAG"))
               (symbol (lib_id "my:VDD") (at 50 10 90) (unit 1) (property "Reference" "#PWR01") (property "Value" "VDD"))"##,
        );
        let sheet = read.schematic.sheets.iter().next().expect("sheet");
        // Two flags on two nets must not become two ports of one net named PWR_FLAG.
        assert_eq!(sheet.components.iter().map(|c| c.designator.as_str()).collect::<BTreeSet<_>>(), BTreeSet::from(["#FLG01", "#FLG02"]));
        let port = sheet.power_ports.iter().next().expect("supply");
        assert_eq!(sheet.power_ports.len(), 1);
        // Its pin is 2.54 below its origin, turned a quarter: 2.54 to the right of (50, -10).
        assert_eq!((port.net.as_str(), port.position), ("VDD", mm(52.54, -10.0)));
    }

    #[test]
    fn kicad_6_designators_come_from_the_top_files_list() {
        let text = r#"(kicad_sch (version 20211123) (uuid 33333333-3333-4333-8333-333333333333)
            (lib_symbols (symbol "Device:R" (property "Reference" "R") (symbol "R_1_1" (pin passive line (at 0 3.81 270) (length 1.27) (name "~") (number "1")))))
            (symbol (lib_id "Device:R") (at 10 10 0) (unit 1) (uuid 44444444-4444-4444-8444-444444444444) (property "Reference" "R?") (property "Value" "1k"))
            (symbol_instances (path "/44444444-4444-4444-8444-444444444444" (reference "R7") (unit 1) (value "1k") (footprint ""))))"#;
        let read = read_schematic(text, "Six", &mut |_| None).expect("import");
        let designators: Vec<&str> = read.schematic.sheets.iter().flat_map(|s| s.components.iter()).map(|c| c.designator.as_str()).collect();
        assert_eq!(designators, ["R7"]);
    }

    const RESISTOR: &str =
        r#"(symbol "Device:R" (property "Reference" "R") (symbol "R_1_1" (pin passive line (at 2.54 0 180) (length 1.27) (name "~") (number "1"))))"#;
    const GROUND: &str = r##"(symbol "power:GND" (power) (property "Reference" "#PWR") (property "Value" "GND")
        (symbol "GND_1_1" (pin power_in line (at 0 0 270) (length 0) (name "GND") (number "1"))))"##;

    #[test]
    fn the_designator_is_the_one_recorded_for_this_place_in_the_tree() {
        // The top sheet's path is the top file's id; another project's record comes first.
        let read = one_sheet(
            RESISTOR,
            r#"(symbol (lib_id "Device:R") (at 10 10 0) (unit 1) (property "Reference" "R1")
                 (instances (project "other" (path "/99999999-9999-4999-8999-999999999999" (reference "R9") (unit 1)))
                            (project "this" (path "/22222222-2222-4222-8222-222222222222" (reference "R3") (unit 1)))))"#,
        );
        let designators: Vec<&str> = read.schematic.sheets.iter().flat_map(|s| s.components.iter()).map(|c| c.designator.as_str()).collect();
        assert_eq!(designators, ["R3"]);
    }

    const DUAL: &str = r#"(symbol "L:Dual" (property "Reference" "U")
        (symbol "Dual_1_1" (pin input line (at 0 0 0) (length 1) (name "A") (number "1")))
        (symbol "Dual_2_1" (pin input line (at 0 0 0) (length 1) (name "B") (number "7"))))"#;

    #[test]
    fn the_unit_comes_from_the_record_that_gives_the_designator() {
        // The symbol says unit 1; this place in the tree says U3 is unit 2 here.
        let read = one_sheet(
            DUAL,
            r#"(symbol (lib_id "L:Dual") (at 10 10 0) (unit 1) (property "Reference" "U3")
                 (instances (project "this" (path "/22222222-2222-4222-8222-222222222222" (reference "U3") (unit 2)))))
               (symbol (lib_id "L:Dual") (at 30 10 0) (unit 2) (property "Reference" "U4")
                 (instances (project "this" (path "/22222222-2222-4222-8222-222222222222" (reference "U4")))))
               (symbol (lib_id "L:Dual") (at 50 10 0) (unit 2) (property "Reference" "U6")
                 (instances (project "this" (path "/22222222-2222-4222-8222-222222222222" (reference "U6") (unit 3)))))"#,
        );
        let s = &read.schematic;
        let units: BTreeMap<&str, u32> = s
            .sheets
            .iter()
            .flat_map(|sheet| sheet.parts.iter())
            .map(|p| (s.component(p.component).map_or("", |c| c.designator.as_str()), p.unit))
            .collect();
        // A record without a unit, or with one the symbol doesn't have, keeps the symbol's own.
        assert_eq!(units, BTreeMap::from([("U3", 2), ("U4", 2), ("U6", 2)]));
    }

    #[test]
    fn kicad_6_units_come_from_the_top_files_list() {
        let text = format!(
            r#"(kicad_sch (version 20211123) (uuid 33333333-3333-4333-8333-333333333333) (lib_symbols {DUAL})
            (symbol (lib_id "L:Dual") (at 10 10 0) (unit 1) (uuid 44444444-4444-4444-8444-444444444444) (property "Reference" "U?"))
            (symbol_instances (path "/44444444-4444-4444-8444-444444444444" (reference "U5") (unit 2) (value "x") (footprint ""))))"#
        );
        let read = read_schematic(&text, "Six", &mut |_| None).expect("import");
        let part = read.schematic.sheets.iter().flat_map(|s| s.parts.iter()).next().expect("part");
        assert_eq!(part.unit, 2);
    }

    #[test]
    fn mirroring_left_to_right_flips_the_pins() {
        let read = one_sheet(RESISTOR, r#"(symbol (lib_id "Device:R") (at 10 10 0) (mirror y) (unit 1) (property "Reference" "R1"))"#);
        let s = &read.schematic;
        let part = s.sheets.iter().flat_map(|sheet| sheet.parts.iter()).next().expect("part");
        assert_eq!((part.rotation, part.mirrored), (Rotation(0), true));
        assert_eq!(pin_ends(s, s.sheets.iter().next().expect("sheet")).first().map(|e| e.2), Some(mm(7.46, -10.0)));
    }

    #[test]
    fn mirrored_power_ports_point_the_way_they_are_drawn() {
        let read = one_sheet(
            GROUND,
            r##"(symbol (lib_id "power:GND") (at 10 10 0) (mirror x) (unit 1) (property "Reference" "#PWR01") (property "Value" "GND"))
               (symbol (lib_id "power:GND") (at 20 10 90) (mirror y) (unit 1) (property "Reference" "#PWR02") (property "Value" "GND"))
               (symbol (lib_id "power:GND") (at 9999 10 0) (unit 1) (property "Reference" "#PWR03") (property "Value" "GND"))"##,
        );
        let mut ports: Vec<(Point, Rotation)> =
            read.schematic.sheets.iter().flat_map(|s| s.power_ports.iter()).map(|p| (p.position, p.rotation)).collect();
        ports.sort_by_key(|p| p.0.x);
        // Upside down, its bars above its pin; turned a quarter and mirrored, pointing right.
        assert_eq!(ports, [(mm(10.0, -10.0), Rotation(180_000)), (mm(20.0, -10.0), Rotation(270_000)), (mm(9999.0, -10.0), Rotation(0))]);
        assert!(read.notes.lines().iter().all(|n| !n.contains("mirror") && !n.contains("pin name differ")));
    }

    #[test]
    fn a_supply_named_apart_from_its_pin_is_noted() {
        let read =
            one_sheet(GROUND, r##"(symbol (lib_id "power:GND") (at 10 10 0) (unit 1) (property "Reference" "#PWR01") (property "Value" "GNDA"))"##);
        let port = read.schematic.sheets.iter().flat_map(|s| s.power_ports.iter()).next().expect("port");
        assert_eq!(port.net, "GNDA");
        assert!(read.notes.lines().iter().any(|n| n == "power symbol whose value and pin name differ: net named after its value"));
    }

    #[test]
    fn a_power_pin_beyond_the_sheet_leaves_out_its_port_alone() {
        let supply = OFFSET_SUPPLY;
        let read =
            one_sheet(supply, r##"(symbol (lib_id "my:VDD") (at 9999 10 90) (unit 1) (property "Reference" "#PWR01") (property "Value" "VDD"))"##);
        assert!(read.schematic.sheets.iter().all(|s| s.power_ports.is_empty()));
        assert!(read.notes.lines().iter().any(|n| n == "power symbol without a net name or out of range left out"));
    }

    #[test]
    fn field_angles_turn_with_their_symbol() {
        let read = one_sheet(
            RESISTOR,
            r#"(symbol (lib_id "Device:R") (at 10 10 90) (unit 1) (property "Reference" "R1" (at 10 6 90)) (property "Value" "1k" (at 10 14 0)))"#,
        );
        let part = read.schematic.sheets.iter().flat_map(|s| s.parts.iter()).next().expect("part");
        let angles: Vec<(PartTextContent, Rotation)> = part.texts.iter().map(|t| (t.content.clone(), t.rotation)).collect();
        // A horizontal resistor's designator, written at 90 relative to it, reads level.
        assert_eq!(angles, [(PartTextContent::Designator, Rotation(0)), (PartTextContent::Value, Rotation(90_000))]);
    }

    #[test]
    fn sheet_entries_beyond_the_limit_are_left_out() {
        let pins: String = (0..=MAX_SHEET_ENTRIES).map(|i| format!("(pin \"P{i}\" input (at 1 1 0))")).collect();
        let text = format!(
            "(kicad_sch (version 20231120) (sheet (at 0 0) (size 10 10) (property \"Sheetname\" \"S\") (property \"Sheetfile\" \"s.kicad_sch\") {pins}))"
        );
        let read = read_schematic(&text, "Top", &mut |_| None).expect("import");
        let block = read.schematic.sheets.iter().flat_map(|s| s.sheet_symbols.iter()).next().expect("sheet symbol");
        assert_eq!(block.entries.len(), MAX_SHEET_ENTRIES);
        assert!(read.notes.lines().iter().any(|n| n == "sheet entries beyond those a sheet symbol may have left out"));
        assert_eq!(read.schematic.validate(), Ok(()));
    }

    #[test]
    fn two_ways_of_writing_a_file_name_are_one_file() {
        let mut asked = Vec::new();
        read_schematic(&top_with(&["./sub/a.kicad_sch", "sub/a.kicad_sch"]), "Top", &mut |name| {
            asked.push(name.to_owned());
            None
        })
        .expect("import");
        assert_eq!(asked, ["sub/a.kicad_sch"]);
    }

    #[test]
    fn symbols_turned_by_odd_angles_are_left_out() {
        let read = one_sheet(
            r#"(symbol "Device:R" (property "Reference" "R") (symbol "R_1_1" (pin passive line (at 0 3.81 270) (length 1.27) (name "~") (number "1"))))"#,
            r#"(symbol (lib_id "Device:R") (at 10 10 45) (unit 1) (property "Reference" "R1"))"#,
        );
        assert!(read.schematic.sheets.iter().all(|s| s.parts.is_empty()));
        assert!(read.notes.lines().iter().any(|n| n == "placed symbol turned by other than a quarter turn left out"));
    }

    #[test]
    fn a_loop_through_a_sheet_used_twice_is_left_out() {
        // The top names a and b; a names b; b names a.
        let files = |name: &str| match name {
            "a.kicad_sch" => Some(top_with(&["b.kicad_sch"])),
            "b.kicad_sch" => Some(top_with(&["a.kicad_sch"])),
            _ => None,
        };
        let read = read_schematic(&top_with(&["a.kicad_sch", "b.kicad_sch"]), "Top", &mut |n| files(n)).expect("import");
        assert_eq!(read.schematic.sheets.len(), 3);
        assert!(read.notes.lines().iter().any(|n| n == "sheet symbol for a sheet that contains it left out"));
    }

    #[test]
    fn shared_pins_are_kept_once_and_unused_symbols_not_at_all() {
        let read = one_sheet(
            r#"(symbol "L:Many" (property "Reference" "U") (symbol "Many_0_1" (pin power_in line (at 0 0 0) (length 1) (name "V") (number "14")))
                   (symbol "Many_1024_1" (pin input line (at 5 0 0) (length 1) (name "A") (number "1"))))
               (symbol "L:Unused" (property "Reference" "X") (symbol "Unused_1_1"))"#,
            r#"(symbol (lib_id "L:Many") (at 10 10 0) (unit 1024) (property "Reference" "U1"))"#,
        );
        let symbols: Vec<&Symbol> = read.schematic.symbols.iter().collect();
        assert_eq!(symbols.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Many"]);
        let many = symbols.first().expect("many");
        assert_eq!((many.units.len(), many.shared.pins.len()), (1024, 1));
        assert_eq!(many.units.iter().map(|u| u.pins.len()).sum::<usize>(), 1, "the shared pin isn't copied into every unit");
    }

    #[test]
    fn symbols_beyond_the_units_a_schematic_may_hold_are_left_out() {
        // Each symbol has only unit 1024, so it is kept with 1024 units; together they pass the
        // budget a little before the last one.
        let count = MAX_TOTAL_UNITS / MAX_UNITS + 2;
        let lib: String = (0..count).map(|i| format!("(symbol \"L:S{i}\" (symbol \"S{i}_1024_1\"))")).collect();
        let items: String =
            (0..count).map(|i| format!("(symbol (lib_id \"L:S{i}\") (at 0 0 0) (unit 1) (property \"Reference\" \"U{i}\"))")).collect();
        let read = one_sheet(&lib, &items);
        let kept: usize = read.schematic.symbols.iter().map(|s| s.units.len()).sum();
        assert!(kept <= MAX_TOTAL_UNITS && kept + MAX_UNITS > MAX_TOTAL_UNITS, "{kept}");
        assert!(read.notes.lines().iter().any(|n| n.ends_with("symbol beyond the units a schematic may hold left out")));
    }

    #[test]
    fn parts_show_only_fields_their_component_has() {
        let read = one_sheet(
            r#"(symbol "Device:R" (property "Reference" "R") (symbol "R_1_1" (pin passive line (at 0 3.81 270) (length 1.27) (name "~") (number "1"))))"#,
            r#"(symbol (lib_id "Device:R") (at 10 10 0) (unit 1) (property "Reference" "R1") (property "MPN" "A" (at 1 1 0)) (property "MPN" "B" (at 2 2 0)))"#,
        );
        let sheet = read.schematic.sheets.iter().next().expect("sheet");
        let part = sheet.parts.iter().next().expect("part");
        let shown = part.texts.iter().filter(|t| t.content == PartTextContent::Field("MPN".to_owned())).count();
        assert_eq!(shown, 2, "both texts show the one MPN field the component keeps");
        assert_eq!(sheet.components.iter().next().map(|c| c.fields.clone()), Some(vec![Field { name: "MPN".to_owned(), value: "A".to_owned() }]));
    }
}
