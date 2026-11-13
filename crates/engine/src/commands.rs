//! The command table. Add a command here with tests; every front end picks it up.

use crate::{CommandSpec, EngineError, Session};
use circuitcraft_model::{Board, NetId, TrackPath};
use circuitcraft_units::{Nm, Point};
use serde_json::{Map, Value, json};
use std::io::Read as _;

/// Every command. Menus appear in the order their first command does here.
pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "board.new",
        label: "New Board",
        menu: Some("File/New Board"),
        shortcut: None,
        params: r#"{"name"?: string (default "Untitled")}"#,
        run: board_new,
    },
    CommandSpec { id: "edit.undo", label: "Undo", menu: Some("Edit/Undo"), shortcut: Some("Ctrl+Z"), params: "{} (none)", run: edit_undo },
    CommandSpec { id: "edit.redo", label: "Redo", menu: Some("Edit/Redo"), shortcut: Some("Ctrl+Y"), params: "{} (none)", run: edit_redo },
    CommandSpec {
        id: "item.remove",
        label: "Remove Item",
        menu: None,
        shortcut: None,
        params: r#"{"id": integer} (any track, via, footprint, zone, text, drawing or net)"#,
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
    s.doc.replace("New Board", Board::new(name));
    Ok(json!({ "name": s.board().name }))
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
    let id = m.get("id").and_then(Value::as_u64).ok_or_else(|| bad("`id` must be a non-negative integer"))?;
    s.doc.edit("Remove", |b| b.remove(id))?;
    Ok(json!({ "removed": id }))
}

fn net_add(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let name = req_str(&m, "name")?;
    let id = s.doc.edit("Add Net", |b| b.add_net(name))?;
    Ok(json!({ "id": id.0 }))
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
    Ok(json!({ "id": id.0 }))
}

fn via_add(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let at = point(&m, "at")?;
    let diameter = opt_mm(&m, "diameter", 0.6)?;
    let drill = opt_mm(&m, "drill", 0.3)?;
    let net = net(s.board(), &m)?;
    let id = s.doc.edit("Add Via", |b| b.add_via(at, diameter, drill, net))?;
    Ok(json!({ "id": id.0 }))
}

fn board_info(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    object(params)?;
    let b = s.board();
    let stack = &b.stack;
    let layers: Vec<Value> =
        stack.layers.iter().map(|l| json!({ "name": l.name, "kind": l.kind, "thicknessMm": l.thickness.map(|t| t.to_mm()) })).collect();
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
        "undo": s.doc().undo_label(),
        "redo": s.doc().redo_label(),
    }))
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
        "importable": false,
        "note": match format {
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

    #[test]
    fn file_detect_reads_a_kicad_board() {
        let dir = std::env::temp_dir().join(format!("circuitcraft-detect-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("board.kicad_pcb");
        std::fs::write(&path, "(kicad_pcb (version 20240108))").expect("write");
        let mut s = Session::new();
        let r = s.execute("file.detect", &json!({ "path": path.to_string_lossy() })).expect("detect");
        assert_eq!(r["format"], "KiCad board");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_undo_and_redo_through_commands() {
        let mut s = Session::new();
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        let t = s.execute("track.add", &json!({ "layer": "Top Copper", "width": 0.25, "from": [0, 0], "to": [10, 0], "net": "GND" })).expect("track");
        s.execute("via.add", &json!({ "at": [10, 0], "net": "GND" })).expect("via");
        let info = s.execute("board.info", &Value::Null).expect("info");
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
        assert!(bad(&mut s, "item.remove", json!({ "id": -1 })));
        assert_eq!(s.board().tracks.len() + s.board().vias.len() + s.board().nets.len(), 0);
        assert!(s.doc().undo_label().is_none(), "refused edits must not enter the undo history");
    }

    #[test]
    fn file_detect_missing_file_fails_cleanly() {
        let mut s = Session::new();
        let r = s.execute("file.detect", &json!({ "path": "/definitely/not/here.brd" }));
        assert!(matches!(r, Err(EngineError::Failed(_))));
    }
}
