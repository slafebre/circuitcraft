//! The command table. Add a command here with tests; every front end picks it up.

use crate::{CommandSpec, EngineError, Session};
use circuitcraft_io::native;
use circuitcraft_model::{Board, DesignSettings, Hole, ItemId, MAX_OUTLINE_GAP, NetId, TrackPath};
use circuitcraft_units::{Nm, Point};
use serde_json::{Map, Value, json};
use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Every command. Menus appear in the order their first command does here.
pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "board.new",
        label: "New Board",
        menu: Some("File/New Board"),
        shortcut: None,
        params: r#"{"name"?: string (default "Untitled")} (undoable; the new board has no project folder until saved)"#,
        run: board_new,
    },
    CommandSpec {
        id: "file.open",
        label: "Open Project",
        menu: None,
        shortcut: None,
        params: r#"{"path": string (project folder, .ccproj or .ccpcb)} -> {"name", "folder"} (desktop only; replaces the open board, undoably)"#,
        run: file_open,
    },
    CommandSpec {
        id: "file.save",
        label: "Save Project",
        menu: None,
        shortcut: None,
        params: r#"{"path"?: string (project folder), "overwrite"?: bool (false)} -> {"folder", "project", "board"} (desktop only; without path: back to the files the board was opened from or last saved to; with path: Save As, where a folder without a project gets <folder name>.ccproj and .ccpcb (an existing <folder name>.ccpcb is refused unless overwrite), and a folder holding a project is refused unless overwrite, which saves into that project's files)"#,
        run: file_save,
    },
    CommandSpec { id: "edit.undo", label: "Undo", menu: Some("Edit/Undo"), shortcut: Some("Ctrl+Z"), params: "{} (none)", run: edit_undo },
    CommandSpec { id: "edit.redo", label: "Redo", menu: Some("Edit/Redo"), shortcut: Some("Ctrl+Y"), params: "{} (none)", run: edit_redo },
    CommandSpec {
        id: "item.remove",
        label: "Remove Item",
        menu: None,
        shortcut: None,
        params: r#"{"id": string (UUID)} (any track, via, footprint, zone, keepout, hole, text, drawing or net)"#,
        run: item_remove,
    },
    CommandSpec { id: "net.add", label: "Add Net", menu: None, shortcut: None, params: r#"{"name": string} -> {"id"}"#, run: net_add },
    CommandSpec {
        id: "track.add",
        label: "Add Track",
        menu: None,
        shortcut: None,
        params: r#"{"layer": string (copper layer name), "width"?: mm (the design settings' track width), "from": [x, y] mm, "to": [x, y] mm, "net"?: string} -> {"id"}"#,
        run: track_add,
    },
    CommandSpec {
        id: "via.add",
        label: "Add Via",
        menu: None,
        shortcut: None,
        params: r#"{"at": [x, y] mm, "diameter"?: mm, "drill"?: mm, "net"?: string} -> {"id"} (top to bottom copper; sizes default to the design settings' via)"#,
        run: via_add,
    },
    CommandSpec {
        id: "hole.add",
        label: "Add Hole",
        menu: None,
        shortcut: None,
        params: r#"{"at": [x, y] mm, "diameter": mm, "plated"?: bool (false)} -> {"id"} (a round hole that belongs to no footprint)"#,
        run: hole_add,
    },
    CommandSpec {
        id: "outline.assemble",
        label: "Outline from Drawings",
        menu: Some("Design/Outline from Drawings"),
        shortcut: None,
        params: r#"{"gap"?: mm (0.001, at most 1)} -> {"used", "cutouts"} (joins the lines and arcs on the outline layer whose ends are within gap; the largest closed shape becomes the board outline and the others, which must start inside it, its cutouts, replacing any the board had, and the drawings are removed; refused when a drawing on the layer is part of no closed shape)"#,
        run: outline_assemble,
    },
    CommandSpec {
        id: "settings.set",
        label: "Set Design Settings",
        menu: None,
        shortcut: None,
        params: r#"{"maskExpansion"?: mm, "pasteExpansion"?: mm, "tentVias"?: bool, "clearance"?: mm, "trackWidth"?: mm, "viaDiameter"?: mm, "viaDrill"?: mm, "origin"?: [x, y] mm} -> the settings, as board.info lists them (only what is given changes; any other key is refused)"#,
        run: settings_set,
    },
    CommandSpec {
        id: "board.info",
        label: "Board Information",
        menu: Some("Reports/Board Information"),
        shortcut: None,
        params: "{} (none)",
        run: board_info,
    },
    CommandSpec {
        id: "file.detect",
        label: "Detect File Format",
        menu: None,
        shortcut: None,
        params: r#"{"path": string} (desktop only; reads at most the first 4 KiB)"#,
        run: file_detect,
    },
    CommandSpec { id: "commands", label: "List Commands", menu: None, shortcut: None, params: "{} (none)", run: list_commands },
    CommandSpec { id: "about", label: "About CircuitCraft", menu: Some("Help/About CircuitCraft"), shortcut: None, params: "{} (none)", run: about },
];

/// The parameters as an object; `null` counts as `{}`.
fn object(params: &Value) -> Result<Map<String, Value>, EngineError> {
    match params {
        Value::Null => Ok(Map::new()),
        Value::Object(m) => Ok(m.clone()),
        other => Err(EngineError::BadParams(format!("expected an object, got {other}"))),
    }
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams(msg.into())
}

/// An optional string parameter.
fn opt_str<'a>(m: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, EngineError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(bad(format!("`{key}` must be a string, got {other}"))),
    }
}

/// An optional boolean parameter.
fn opt_bool(m: &Map<String, Value>, key: &str) -> Result<Option<bool>, EngineError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(bad(format!("`{key}` must be true or false, got {other}"))),
    }
}

/// A required string parameter.
fn req_str<'a>(m: &'a Map<String, Value>, key: &str) -> Result<&'a str, EngineError> {
    opt_str(m, key)?.ok_or_else(|| bad(format!("`{key}` is required")))
}

/// A length in millimetres (JSON lengths are millimetres), as whole nanometres.
fn mm(v: &Value, key: &str) -> Result<Nm, EngineError> {
    let f = v.as_f64().ok_or_else(|| bad(format!("`{key}` must be a number of millimetres, got {v}")))?;
    Nm::from_mm(f).map_err(|e| bad(format!("`{key}`: {e}")))
}

/// An optional length in millimetres.
fn opt_mm(m: &Map<String, Value>, key: &str) -> Result<Option<Nm>, EngineError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => mm(v, key).map(Some),
    }
}

/// A required length in millimetres.
fn req_mm(m: &Map<String, Value>, key: &str) -> Result<Nm, EngineError> {
    mm(m.get(key).ok_or_else(|| bad(format!("`{key}` is required")))?, key)
}

/// An optional point, `[x, y]` in millimetres.
fn opt_point(m: &Map<String, Value>, key: &str) -> Result<Option<Point>, EngineError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => match v.as_array().map(Vec::as_slice) {
            Some([x, y]) => Ok(Some(Point::new(mm(x, key)?, mm(y, key)?))),
            _ => Err(bad(format!("`{key}` must be [x, y] in millimetres, got {v}"))),
        },
    }
}

/// A required point, `[x, y]` in millimetres.
fn point(m: &Map<String, Value>, key: &str) -> Result<Point, EngineError> {
    opt_point(m, key)?.ok_or_else(|| bad(format!("`{key}` is required")))
}

/// An optional net, by name; an unknown name is an error, not a new net.
fn net(board: &Board, m: &Map<String, Value>) -> Result<Option<NetId>, EngineError> {
    match opt_str(m, "net")? {
        None => Ok(None),
        Some(name) => board.net_by_name(name).map(|n| Some(n.id)).ok_or_else(|| bad(format!("no net named {name:?}"))),
    }
}

/// The longest board name accepted.
const MAX_NAME_CHARS: usize = 256;

fn board_new(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let name = opt_str(&m, "name")?.unwrap_or("Untitled").trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(bad(format!("`name` must be 1 to {MAX_NAME_CHARS} characters")));
    }
    // Undoable: the old board comes back with its own project folder (the session keys folders on
    // the board's generation), so unsaved work is never lost and never saved in the wrong place.
    s.doc.replace("New Board", Board::new(name));
    Ok(json!({ "name": s.board().name }))
}

fn native_failed(e: native::NativeError) -> EngineError {
    EngineError::Failed(e.to_string())
}

fn file_open(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let path = PathBuf::from(req_str(&m, "path")?);
    let (board, target) = native::open(&path).map_err(native_failed)?;
    s.doc.replace("Open", board);
    s.set_target(target);
    Ok(json!({ "name": s.board().name, "folder": s.folder().map(|f| f.to_string_lossy()) }))
}

fn file_save(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let overwrite = opt_bool(&m, "overwrite")?.unwrap_or(false);
    match opt_str(&m, "path")? {
        // Back to the files this board came from, whatever they are called.
        None => {
            let target = s.target().ok_or_else(|| bad("`path` is required: this board hasn't been saved yet"))?;
            native::save(target, s.board()).map_err(native_failed)?;
            Ok(saved(target))
        }
        // Save As.
        Some(folder) => {
            let target = native::save_project(Path::new(folder), s.board(), overwrite).map_err(native_failed)?;
            let out = saved(&target);
            s.set_target(target);
            Ok(out)
        }
    }
}

fn saved(paths: &native::ProjectPaths) -> Value {
    json!({
        "folder": paths.folder().to_string_lossy(),
        "project": paths.project().map(|p| p.to_string_lossy()),
        "board": paths.board().to_string_lossy(),
    })
}

fn edit_undo(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    object(params)?;
    Ok(json!({ "undone": s.doc.undo() }))
}

fn edit_redo(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    object(params)?;
    Ok(json!({ "redone": s.doc.redo() }))
}

fn item_remove(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let id: ItemId = req_str(&m, "id")?.parse()?;
    s.doc.edit("Remove", |b| b.remove(id))?;
    Ok(json!({ "removed": id.to_string() }))
}

fn net_add(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let name = req_str(&m, "name")?;
    let id = s.doc.edit("Add Net", |b| b.add_net(name))?;
    Ok(json!({ "id": id.to_string() }))
}

fn track_add(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let board = s.board();
    let layer_name = req_str(&m, "layer")?;
    let layer = board.stack.by_name(layer_name).map(|l| l.id).ok_or_else(|| bad(format!("no layer named {layer_name:?}")))?;
    let width = opt_mm(&m, "width")?.unwrap_or(board.design.track_width);
    let path = TrackPath::Line { from: point(&m, "from")?, to: point(&m, "to")? };
    let net = net(board, &m)?;
    let id = s.doc.edit("Add Track", |b| b.add_track(layer, width, path, net))?;
    Ok(json!({ "id": id.to_string() }))
}

fn via_add(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let at = point(&m, "at")?;
    let diameter = opt_mm(&m, "diameter")?.unwrap_or(s.board().design.via_diameter);
    let drill = opt_mm(&m, "drill")?.unwrap_or(s.board().design.via_drill);
    let net = net(s.board(), &m)?;
    let id = s.doc.edit("Add Via", |b| b.add_via(at, diameter, drill, net))?;
    Ok(json!({ "id": id.to_string() }))
}

fn hole_add(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let at = point(&m, "at")?;
    let hole = Hole::round(req_mm(&m, "diameter")?, opt_bool(&m, "plated")?.unwrap_or(false));
    let id = s.doc.edit("Add Hole", |b| b.add_hole(at, hole))?;
    Ok(json!({ "id": id.to_string() }))
}

/// The gap `outline.assemble` closes when none is given: a micrometre, which covers the rounding
/// of drawings that were meant to meet.
const OUTLINE_GAP: Nm = Nm(1_000);

fn outline_assemble(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let gap = opt_mm(&m, "gap")?.unwrap_or(OUTLINE_GAP);
    if !(0..=MAX_OUTLINE_GAP.0).contains(&gap.0) {
        return Err(bad(format!("`gap` must be 0 to {} mm", MAX_OUTLINE_GAP.to_mm())));
    }
    // The gap is the only parameter: whatever else goes wrong is the drawings' doing.
    let done = s.doc.edit("Outline from Drawings", |b| b.assemble_outline(gap)).map_err(|e| EngineError::Failed(e.to_string()))?;
    Ok(json!({ "used": done.used, "cutouts": done.cutouts }))
}

/// The keys of `settings.set` and of the settings in `board.info`.
const SETTINGS: [&str; 8] = ["maskExpansion", "pasteExpansion", "tentVias", "clearance", "trackWidth", "viaDiameter", "viaDrill", "origin"];

fn settings_set(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    // Every key is optional, so a misspelled one would otherwise change nothing without a word.
    if let Some(unknown) = m.keys().find(|k| !SETTINGS.contains(&k.as_str())) {
        return Err(bad(format!("no setting `{unknown}`; the settings are {}", SETTINGS.join(", "))));
    }
    let old = s.board().design;
    let design = DesignSettings {
        mask_expansion: opt_mm(&m, "maskExpansion")?.unwrap_or(old.mask_expansion),
        paste_expansion: opt_mm(&m, "pasteExpansion")?.unwrap_or(old.paste_expansion),
        tent_vias: opt_bool(&m, "tentVias")?.unwrap_or(old.tent_vias),
        clearance: opt_mm(&m, "clearance")?.unwrap_or(old.clearance),
        track_width: opt_mm(&m, "trackWidth")?.unwrap_or(old.track_width),
        via_diameter: opt_mm(&m, "viaDiameter")?.unwrap_or(old.via_diameter),
        via_drill: opt_mm(&m, "viaDrill")?.unwrap_or(old.via_drill),
        origin: opt_point(&m, "origin")?.unwrap_or(old.origin),
    };
    s.doc.edit("Design Settings", |b| b.set_design(design))?;
    Ok(settings(&s.board().design))
}

/// The design settings under the keys `settings.set` takes, lengths in millimetres.
fn settings(d: &DesignSettings) -> Value {
    json!({
        "maskExpansion": d.mask_expansion.to_mm(),
        "pasteExpansion": d.paste_expansion.to_mm(),
        "tentVias": d.tent_vias,
        "clearance": d.clearance.to_mm(),
        "trackWidth": d.track_width.to_mm(),
        "viaDiameter": d.via_diameter.to_mm(),
        "viaDrill": d.via_drill.to_mm(),
        "origin": [d.origin.x.to_mm(), d.origin.y.to_mm()],
    })
}

fn board_info(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    object(params)?;
    let b = s.board();
    let stack = &b.stack;
    let layers: Vec<Value> = stack.layers.iter().map(|l| layer_info(stack, l)).collect();
    Ok(json!({
        "name": b.name,
        "copperLayers": stack.copper_count(),
        "thicknessMm": stack.thickness().to_mm(),
        "layers": layers,
        "settings": settings(&b.design),
        "outline": b.outline.is_some(),
        "counts": {
            "nets": b.nets.len(),
            "footprints": b.footprints.len(),
            "tracks": b.tracks.len(),
            "vias": b.vias.len(),
            "zones": b.zones.len(),
            "keepouts": b.keepouts.len(),
            "holes": b.holes.len(),
            "texts": b.texts.len(),
            "drawings": b.graphics.len(),
            "cutouts": b.cutouts.len(),
        },
        "folder": s.folder().map(|f| f.to_string_lossy()),
        "revision": s.doc().revision(),
        "undo": s.doc().undo_label(),
        "redo": s.doc().redo_label(),
    }))
}

/// One layer for `board.info`: the side it is on, and the layer its drawings flip to (`pair`),
/// by name, as commands name layers.
fn layer_info(stack: &circuitcraft_model::LayerStack, l: &circuitcraft_model::Layer) -> Value {
    json!({
        "name": l.name,
        "kind": l.kind,
        "thicknessMm": l.thickness.map(|t| t.to_mm()),
        "side": stack.side(l.id),
        "pair": stack.flipped(l.id).filter(|&p| p != l.id).and_then(|p| stack.get(p)).map(|p| p.name.as_str()),
    })
}

fn file_detect(_s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let path = opt_str(&m, "path")?.ok_or_else(|| EngineError::BadParams("`path` is required".into()))?;
    let file = std::fs::File::open(path).map_err(|e| EngineError::Failed(format!("cannot open {path}: {e}")))?;
    let mut head = Vec::with_capacity(circuitcraft_io::SNIFF_LEN);
    file.take(circuitcraft_io::SNIFF_LEN as u64).read_to_end(&mut head).map_err(|e| EngineError::Failed(format!("cannot read {path}: {e}")))?;
    let format = circuitcraft_io::detect(&head);
    Ok(json!({
        "path": path,
        "format": format.map(|f| f.name()),
        "importable": matches!(format, Some(circuitcraft_io::Format::CircuitCraftBoard | circuitcraft_io::Format::CircuitCraftProject)),
        "note": match format {
            Some(circuitcraft_io::Format::CircuitCraftBoard | circuitcraft_io::Format::CircuitCraftProject) => "open it with file.open",
            Some(_) => "recognised; import arrives in 0.1",
            None => "not a format CircuitCraft recognises",
        },
    }))
}

fn list_commands(_s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    object(params)?;
    Ok(Value::Array(
        COMMANDS.iter().map(|c| json!({ "id": c.id, "label": c.label, "menu": c.menu, "shortcut": c.shortcut, "params": c.params })).collect(),
    ))
}

fn about(_s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    object(params)?;
    Ok(json!({
        "name": "CircuitCraft",
        "version": env!("CARGO_PKG_VERSION"),
        "license": "MIT OR Apache-2.0",
        "description": "Schematic capture and PCB layout, built in pure Rust.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder under the system temp folder, removed when dropped, even if the test fails.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            TempDir(std::env::temp_dir().join(format!("circuitcraft-{tag}-{}-{n}", std::process::id())))
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The cost budget for edits (ROADMAP.md, "Performance on large boards"): adding 100,000
    /// tracks one command at a time took 134 s and 2.3 GB while undo kept whole-board snapshots,
    /// and takes well under a second with change sets in a release build. The limit is loose so a
    /// slow CI machine passes, and tight enough that a cost growing with the board size fails.
    #[test]
    fn a_hundred_thousand_single_track_commands_stay_within_budget() {
        let mut s = Session::new();
        let start = std::time::Instant::now();
        for i in 0..100_000_u32 {
            let (x, y) = (f64::from(i / 1_000), f64::from(i % 1_000) * 0.01);
            s.execute("track.add", &json!({ "layer": "Top Copper", "width": 0.2, "from": [x, y], "to": [x + 0.5, y] })).expect("track");
        }
        let elapsed = start.elapsed();
        assert_eq!(s.board().tracks.len(), 100_000);
        assert!(elapsed < std::time::Duration::from_secs(30), "100,000 track.add commands took {elapsed:?}");
        assert_eq!(s.execute("edit.undo", &Value::Null).expect("undo")["undone"], "Add Track");
        assert_eq!(s.board().tracks.len(), 99_999);
    }

    #[test]
    fn undo_across_file_open_brings_back_the_previous_project() {
        let dir = TempDir::new("reopen");
        let (a, b) = (dir.0.join("A"), dir.0.join("B"));
        let mut other = Session::new();
        other.execute("board.new", &json!({ "name": "Board B" })).expect("new B");
        other.execute("file.save", &json!({ "path": b.to_string_lossy() })).expect("save B");
        let mut s = Session::new();
        s.execute("board.new", &json!({ "name": "Board A" })).expect("new A");
        s.execute("file.save", &json!({ "path": a.to_string_lossy() })).expect("save A");
        s.execute("file.open", &json!({ "path": b.to_string_lossy() })).expect("open B");
        assert_eq!(s.folder(), Some(b.as_path()));
        s.execute("edit.undo", &Value::Null).expect("undo the open");
        assert_eq!((s.board().name.as_str(), s.folder()), ("Board A", Some(a.as_path())));
        s.execute("edit.redo", &Value::Null).expect("redo the open");
        assert_eq!((s.board().name.as_str(), s.folder()), ("Board B", Some(b.as_path())));
    }

    #[test]
    fn file_detect_reads_a_kicad_board() {
        let dir = TempDir::new("detect");
        std::fs::create_dir_all(&dir.0).expect("temp dir");
        let path = dir.0.join("board.kicad_pcb");
        std::fs::write(&path, "(kicad_pcb (version 20240108))").expect("write");
        let mut s = Session::new();
        let r = s.execute("file.detect", &json!({ "path": path.to_string_lossy() })).expect("detect");
        assert_eq!(r["format"], "KiCad board");
    }

    #[test]
    fn build_undo_and_redo_through_commands() {
        let mut s = Session::new();
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        let t = s.execute("track.add", &json!({ "layer": "Top Copper", "width": 0.25, "from": [0, 0], "to": [10, 0], "net": "GND" })).expect("track");
        s.execute("via.add", &json!({ "at": [10, 0], "net": "GND" })).expect("via");
        let info = s.execute("board.info", &Value::Null).expect("info");
        let layer = |name: &str| info["layers"].as_array().and_then(|l| l.iter().find(|l| l["name"] == name)).cloned().unwrap_or_default();
        assert_eq!(
            layer("Top Courtyard"),
            json!({ "name": "Top Courtyard", "kind": "courtyard", "thicknessMm": null, "side": "top", "pair": "Bottom Courtyard" })
        );
        assert_eq!(layer("Keepout"), json!({ "name": "Keepout", "kind": "keepout", "thicknessMm": null, "side": null, "pair": null }));
        assert_eq!((&layer("Bottom Copper")["side"], &layer("Bottom Copper")["pair"]), (&json!("bottom"), &json!("Top Copper")));
        assert_eq!(info["counts"]["tracks"], 1);
        assert_eq!(info["counts"]["vias"], 1);
        assert_eq!(info["undo"], "Add Via");
        let track = s.board().tracks.iter().next().expect("a track");
        assert_eq!(track.width, Nm(250_000));
        assert_eq!(track.path, TrackPath::Line { from: Point::new(Nm(0), Nm(0)), to: Point::new(Nm(10_000_000), Nm(0)) });

        assert_eq!(s.execute("edit.undo", &Value::Null).expect("undo")["undone"], "Add Via");
        assert_eq!(s.board().vias.len(), 0);
        assert_eq!(s.execute("edit.redo", &Value::Null).expect("redo")["redone"], "Add Via");
        assert_eq!(s.board().vias.len(), 1);

        s.execute("item.remove", &json!({ "id": t["id"] })).expect("remove");
        assert_eq!(s.board().tracks.len(), 0);
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!(s.board().tracks.len(), 1);
    }

    #[test]
    fn design_settings_are_read_set_and_used_by_new_items() {
        let mut s = Session::new();
        let info = s.execute("board.info", &Value::Null).expect("info");
        let defaults = json!({
            "maskExpansion": 0.05, "pasteExpansion": 0.0, "tentVias": true, "clearance": 0.2,
            "trackWidth": 0.25, "viaDiameter": 0.6, "viaDrill": 0.3, "origin": [0.0, 0.0],
        });
        assert_eq!(info["settings"], defaults);
        // What board.info lists can be sent back as it is, and changes nothing.
        assert_eq!(s.execute("settings.set", &info["settings"]).expect("unchanged"), defaults);
        assert!(s.doc().undo_label().is_none());
        let set = s
            .execute("settings.set", &json!({ "trackWidth": 0.4, "viaDiameter": 0.8, "viaDrill": 0.4, "tentVias": false, "origin": [1, -2.5] }))
            .expect("set");
        assert_eq!(
            (&set["trackWidth"], &set["tentVias"], &set["origin"], &set["clearance"]),
            (&json!(0.4), &json!(false), &json!([1.0, -2.5]), &json!(0.2))
        );
        // A track without a width and a via without sizes take the settings'; given sizes win.
        s.execute("track.add", &json!({ "layer": "Top Copper", "from": [0, 0], "to": [1, 0] })).expect("track");
        s.execute("via.add", &json!({ "at": [1, 0] })).expect("via");
        s.execute("via.add", &json!({ "at": [2, 0], "diameter": 0.5, "drill": 0.2 })).expect("via");
        assert_eq!(s.board().tracks.iter().map(|t| t.width).collect::<Vec<_>>(), [Nm(400_000)]);
        let mut vias: Vec<_> = s.board().vias.iter().map(|v| (v.diameter, v.drill)).collect();
        vias.sort_unstable();
        assert_eq!(vias, [(Nm(500_000), Nm(200_000)), (Nm(800_000), Nm(400_000))]);
        // Undo steps back through the vias and the track to the settings.
        for _ in 0..3 {
            s.execute("edit.undo", &Value::Null).expect("undo");
        }
        assert_eq!(s.execute("edit.undo", &Value::Null).expect("undo")["undone"], "Design Settings");
        assert_eq!(s.execute("board.info", &Value::Null).expect("info")["settings"], defaults);
        // Refused: a setting that doesn't exist, a value of the wrong type, sizes no via can have.
        let bad = |s: &mut Session, p: Value| matches!(s.execute("settings.set", &p), Err(EngineError::BadParams(_)));
        assert!(bad(&mut s, json!({ "trackwidth": 0.4 })));
        assert!(bad(&mut s, json!({ "tentVias": "yes" })));
        assert!(bad(&mut s, json!({ "trackWidth": 0 })));
        assert!(bad(&mut s, json!({ "viaDrill": 0.6 })));
        assert!(bad(&mut s, json!({ "origin": [1] })));
        assert!(bad(&mut s, json!({ "clearance": 1e30 })));
        assert_eq!(s.execute("board.info", &Value::Null).expect("info")["settings"], defaults);
    }

    #[test]
    fn a_hole_is_added_counted_and_removed() {
        let mut s = Session::new();
        let h = s.execute("hole.add", &json!({ "at": [3, 3], "diameter": 3.2 })).expect("hole");
        s.execute("hole.add", &json!({ "at": [27, 3], "diameter": 1, "plated": true })).expect("hole");
        assert_eq!(s.execute("board.info", &Value::Null).expect("info")["counts"]["holes"], 2);
        let id: ItemId = h["id"].as_str().expect("id").parse().expect("uuid");
        assert_eq!(s.board().holes.get(id).map(|h| (h.hole.diameter, h.hole.plated)), Some((Nm(3_200_000), false)));
        s.execute("item.remove", &json!({ "id": h["id"] })).expect("remove");
        assert_eq!(s.board().holes.len(), 1);
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!(s.board().holes.len(), 2);
        let bad = |s: &mut Session, p: Value| matches!(s.execute("hole.add", &p), Err(EngineError::BadParams(_)));
        assert!(bad(&mut s, json!({ "at": [0, 0] })));
        assert!(bad(&mut s, json!({ "at": [0, 0], "diameter": 0 })));
        assert!(bad(&mut s, json!({ "diameter": 1 })));
        assert!(bad(&mut s, json!({ "at": [0, 0], "diameter": 1, "plated": 1 })));
        assert_eq!(s.board().holes.len(), 2);
    }

    /// Saves a board with a rectangle and a circle drawn on the outline layer, the last corner
    /// of the rectangle 500 nm short, and returns its folder.
    fn save_board_with_drawn_outline(dir: &TempDir) -> PathBuf {
        use circuitcraft_model::{Graphic, GraphicShape, layer_ids::OUTLINE};
        let mm = |x: i64, y: i64| Point::new(Nm(x * 1_000_000), Nm(y * 1_000_000));
        let mut board = Board::new("Drawn");
        let corners = [mm(0, 0), mm(40, 0), mm(40, 20), Point::new(Nm(500), Nm(20_000_000))];
        let mut shapes: Vec<GraphicShape> = (0..3).map(|i| GraphicShape::Line { from: corners[i], to: corners[i + 1] }).collect();
        shapes.push(GraphicShape::Line { from: mm(0, 20), to: mm(0, 0) });
        shapes.push(GraphicShape::Circle { center: mm(5, 5), radius: Nm(1_600_000) });
        for shape in shapes {
            board.graphics.insert(Graphic { id: board.fresh_id().expect("id"), layer: OUTLINE, width: Nm(100_000), shape }).expect("room");
        }
        let folder = dir.0.join("Drawn");
        native::save_project(&folder, &board, false).expect("save");
        folder
    }

    #[test]
    fn the_outline_is_assembled_from_drawings_and_survives_a_save() {
        let dir = TempDir::new("outline");
        let folder = save_board_with_drawn_outline(&dir);
        let mut s = Session::new();
        s.execute("file.open", &json!({ "path": folder.to_string_lossy() })).expect("open");
        let info = s.execute("board.info", &Value::Null).expect("info");
        assert_eq!((&info["outline"], &info["counts"]["drawings"]), (&json!(false), &json!(5)));
        // The corner that is 500 nm short stays open at a gap of 100 nm, and nothing changes.
        assert!(matches!(s.execute("outline.assemble", &json!({ "gap": 0.0001 })), Err(EngineError::Failed(_))));
        assert!(matches!(s.execute("outline.assemble", &json!({ "gap": 2 })), Err(EngineError::BadParams(_))));
        assert_eq!(s.doc().undo_label(), Some("Open"));
        // The default gap of a micrometre closes it.
        assert_eq!(s.execute("outline.assemble", &Value::Null).expect("assemble"), json!({ "used": 5, "cutouts": 1 }));
        let info = s.execute("board.info", &Value::Null).expect("info");
        assert_eq!((&info["outline"], &info["counts"]["drawings"], &info["counts"]["cutouts"]), (&json!(true), &json!(0), &json!(1)));
        s.execute("file.save", &json!({})).expect("save");
        let mut other = Session::new();
        other.execute("file.open", &json!({ "path": folder.to_string_lossy() })).expect("open");
        assert_eq!(other.board(), s.board());
        assert_eq!(s.execute("edit.undo", &Value::Null).expect("undo")["undone"], "Outline from Drawings");
        assert!(s.board().outline.is_none() && s.board().cutouts.is_empty() && s.board().graphics.len() == 5);
    }

    #[test]
    fn bad_edit_params_are_errors_and_change_nothing() {
        let mut s = Session::new();
        let bad = |s: &mut Session, id: &str, p: Value| matches!(s.execute(id, &p), Err(EngineError::BadParams(_)));
        assert!(bad(&mut s, "track.add", json!({ "layer": "Top Silkscreen", "width": 0.2, "from": [0, 0], "to": [1, 0] })));
        assert!(bad(&mut s, "track.add", json!({ "layer": "Nope", "width": 0.2, "from": [0, 0], "to": [1, 0] })));
        assert!(bad(&mut s, "track.add", json!({ "layer": "Top Copper", "width": -1, "from": [0, 0], "to": [1, 0] })));
        assert!(bad(&mut s, "track.add", json!({ "layer": "Top Copper", "width": 0.2, "from": [0], "to": [1, 0] })));
        assert!(bad(&mut s, "track.add", json!({ "layer": "Top Copper", "width": 0.2, "from": [1e30, 0], "to": [1, 0] })));
        assert!(bad(&mut s, "track.add", json!({ "layer": "Top Copper", "width": 0.2, "from": [0, 0], "to": [1, 0], "net": "X" })));
        assert!(bad(&mut s, "via.add", json!({ "at": [0, 0], "diameter": 0.3, "drill": 0.3 })));
        assert!(bad(&mut s, "net.add", json!({ "name": "" })));
        assert!(bad(&mut s, "item.remove", json!({ "id": 12345 })));
        assert!(bad(&mut s, "item.remove", json!({ "id": "not-a-uuid" })));
        assert!(bad(&mut s, "item.remove", json!({ "id": "3f2c9a10-6b7e-4d21-9a0c-58e1f4b2d7a3" })));
        assert_eq!(s.board().tracks.len() + s.board().vias.len() + s.board().nets.len(), 0);
        assert!(s.doc().undo_label().is_none(), "refused edits must not enter the undo history");
    }

    #[test]
    fn save_then_open_through_commands() {
        let dir = TempDir::new("save");
        let folder = dir.0.join("Blinky");
        let mut s = Session::new();
        s.execute("board.new", &json!({ "name": "Blinky" })).expect("new");
        assert!(matches!(s.execute("file.save", &json!({})), Err(EngineError::BadParams(_))), "nowhere to save yet");
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        let t = s.execute("track.add", &json!({ "layer": "Top Copper", "width": 0.25, "from": [0, 0], "to": [10, 0], "net": "GND" })).expect("track");
        let saved = s.execute("file.save", &json!({ "path": folder.to_string_lossy() })).expect("save");
        assert!(saved["board"].as_str().is_some_and(|b| b.ends_with("Blinky.ccpcb")));
        s.execute("via.add", &json!({ "at": [10, 0] })).expect("via");
        s.execute("file.save", &json!({})).expect("save again to the same folder");
        let detected = s.execute("file.detect", &json!({ "path": saved["board"] })).expect("detect");
        assert_eq!(detected["format"], "CircuitCraft board");
        assert_eq!(detected["importable"], true);

        let mut other = Session::new();
        let opened = other.execute("file.open", &json!({ "path": folder.to_string_lossy() })).expect("open");
        assert_eq!(opened["name"], "Blinky");
        assert_eq!(other.board(), s.board());
        assert_eq!(other.doc().undo_label(), Some("Open"), "opening is undoable");
        other.execute("item.remove", &json!({ "id": t["id"] })).expect("ids survive the round trip");
    }

    #[test]
    fn undo_brings_a_board_back_with_its_own_project_folder() {
        let dir = TempDir::new("new");
        let (a, b) = (dir.0.join("A"), dir.0.join("B"));
        let mut s = Session::new();
        s.execute("board.new", &json!({ "name": "Board A" })).expect("new A");
        s.execute("file.save", &json!({ "path": a.to_string_lossy() })).expect("save A");
        s.execute("board.new", &json!({ "name": "Board B" })).expect("new B");
        s.execute("file.save", &json!({ "path": b.to_string_lossy() })).expect("save B");
        assert_eq!(s.execute("edit.undo", &Value::Null).expect("undo")["undone"], "New Board");
        assert_eq!((s.board().name.as_str(), s.folder()), ("Board A", Some(a.as_path())), "A comes back with A's folder");
        s.execute("net.add", &json!({ "name": "GND" })).expect("edit A");
        s.execute("file.save", &json!({})).expect("save");
        let open = |path: &std::path::Path| {
            let mut check = Session::new();
            check.execute("file.open", &json!({ "path": path.to_string_lossy() })).expect("open");
            check.board().clone()
        };
        assert_eq!(open(&b).name, "Board B", "folder B still holds board B");
        assert!(open(&a).net_by_name("GND").is_some(), "A's edit went to folder A");
    }

    #[test]
    fn a_new_board_does_not_lose_unsaved_work() {
        let mut s = Session::new();
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        s.execute("board.new", &json!({ "name": "Other" })).expect("new");
        assert!(s.folder().is_none());
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert!(s.board().net_by_name("GND").is_some(), "the unsaved net is back");
    }

    #[test]
    fn save_to_another_folder_refuses_to_replace_a_project() {
        let dir = TempDir::new("guard");
        let x = dir.0.join("X");
        let mut s = Session::new();
        s.execute("board.new", &json!({ "name": "First" })).expect("new");
        s.execute("file.save", &json!({ "path": x.to_string_lossy() })).expect("save");
        let mut t = Session::new();
        t.execute("board.new", &json!({ "name": "Second" })).expect("new");
        assert!(matches!(t.execute("file.save", &json!({ "path": x.to_string_lossy() })), Err(EngineError::Failed(_))));
        assert!(t.folder().is_none());
        t.execute("file.save", &json!({ "path": x.to_string_lossy(), "overwrite": true })).expect("overwrite when asked");
    }

    #[test]
    fn file_open_failures_change_nothing() {
        let mut s = Session::new();
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert!(matches!(s.execute("file.open", &json!({ "path": "/definitely/not/here" })), Err(EngineError::Failed(_))));
        assert!(matches!(s.execute("file.open", &json!({})), Err(EngineError::BadParams(_))));
        assert_eq!(s.board().nets.len(), 1);
        assert!(s.folder().is_none());
    }

    #[test]
    fn file_detect_missing_file_fails_cleanly() {
        let mut s = Session::new();
        let r = s.execute("file.detect", &json!({ "path": "/definitely/not/here.brd" }));
        assert!(matches!(r, Err(EngineError::Failed(_))));
    }
}
