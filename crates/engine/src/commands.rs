//! The command table. Add a command here with tests; every front end picks it up.

use crate::{CommandSpec, EngineError, Session};
use circuitcraft_io::native;
use circuitcraft_model::{Board, ItemId, NetId, TrackPath};
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
        params: r#"{"id": string (UUID)} (any track, via, footprint, zone, text, drawing or net)"#,
        run: item_remove,
    },
    CommandSpec { id: "net.add", label: "Add Net", menu: None, shortcut: None, params: r#"{"name": string} -> {"id"}"#, run: net_add },
    CommandSpec {
        id: "track.add",
        label: "Add Track",
        menu: None,
        shortcut: None,
        params: r#"{"layer": string (copper layer name), "width": mm, "from": [x, y] mm, "to": [x, y] mm, "net"?: string} -> {"id"}"#,
        run: track_add,
    },
    CommandSpec {
        id: "via.add",
        label: "Add Via",
        menu: None,
        shortcut: None,
        params: r#"{"at": [x, y] mm, "diameter"?: mm (0.6), "drill"?: mm (0.3), "net"?: string} -> {"id"} (top to bottom copper)"#,
        run: via_add,
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

/// An optional length in millimetres, with a default.
fn opt_mm(m: &Map<String, Value>, key: &str, default_mm: f64) -> Result<Nm, EngineError> {
    match m.get(key) {
        None | Some(Value::Null) => Nm::from_mm(default_mm).map_err(|e| bad(e.to_string())),
        Some(v) => mm(v, key),
    }
}

/// A required length in millimetres.
fn req_mm(m: &Map<String, Value>, key: &str) -> Result<Nm, EngineError> {
    mm(m.get(key).ok_or_else(|| bad(format!("`{key}` is required")))?, key)
}

/// A required point, `[x, y]` in millimetres.
fn point(m: &Map<String, Value>, key: &str) -> Result<Point, EngineError> {
    let v = m.get(key).ok_or_else(|| bad(format!("`{key}` is required")))?;
    match v.as_array().map(Vec::as_slice) {
        Some([x, y]) => Ok(Point::new(mm(x, key)?, mm(y, key)?)),
        _ => Err(bad(format!("`{key}` must be [x, y] in millimetres, got {v}"))),
    }
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
    let width = req_mm(&m, "width")?;
    let path = TrackPath::Line { from: point(&m, "from")?, to: point(&m, "to")? };
    let net = net(board, &m)?;
    let id = s.doc.edit("Add Track", |b| b.add_track(layer, width, path, net))?;
    Ok(json!({ "id": id.to_string() }))
}

fn via_add(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let at = point(&m, "at")?;
    let diameter = opt_mm(&m, "diameter", 0.6)?;
    let drill = opt_mm(&m, "drill", 0.3)?;
    let net = net(s.board(), &m)?;
    let id = s.doc.edit("Add Via", |b| b.add_via(at, diameter, drill, net))?;
    Ok(json!({ "id": id.to_string() }))
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
        "counts": {
            "nets": b.nets.len(),
            "footprints": b.footprints.len(),
            "tracks": b.tracks.len(),
            "vias": b.vias.len(),
            "zones": b.zones.len(),
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
