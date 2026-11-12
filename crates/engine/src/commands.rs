//! The command table. Add a command here with tests; every front end picks it up.

use crate::{CommandSpec, EngineError, Session};
use circuitcraft_model::Board;
use serde_json::{Map, Value, json};
use std::io::Read as _;

/// Every command, in menu order within each menu.
pub static COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        id: "board.new",
        label: "New Board",
        menu: Some("File/New Board"),
        shortcut: None,
        params: r#"{"name"?: string (default "Untitled")}"#,
        run: board_new,
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

/// An optional string parameter.
fn opt_str<'a>(m: &'a Map<String, Value>, key: &str) -> Result<Option<&'a str>, EngineError> {
    match m.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(EngineError::BadParams(format!("`{key}` must be a string, got {other}"))),
    }
}

/// The longest board name accepted.
const MAX_NAME_CHARS: usize = 256;

fn board_new(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let m = object(params)?;
    let name = opt_str(&m, "name")?.unwrap_or("Untitled").trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(EngineError::BadParams(format!("`name` must be 1 to {MAX_NAME_CHARS} characters")));
    }
    s.board = Board::new(name);
    Ok(json!({ "name": s.board.name }))
}

fn board_info(s: &mut Session, params: &Value) -> Result<Value, EngineError> {
    object(params)?;
    let stack = &s.board.stack;
    let layers: Vec<Value> =
        stack.layers.iter().map(|l| json!({ "name": l.name, "kind": l.kind, "thicknessMm": l.thickness.map(|t| t.to_mm()) })).collect();
    Ok(json!({
        "name": s.board.name,
        "copperLayers": stack.copper_count(),
        "thicknessMm": stack.thickness().to_mm(),
        "layers": layers,
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
    fn file_detect_missing_file_fails_cleanly() {
        let mut s = Session::new();
        let r = s.execute("file.detect", &json!({ "path": "/definitely/not/here.brd" }));
        assert!(matches!(r, Err(EngineError::Failed(_))));
    }
}
