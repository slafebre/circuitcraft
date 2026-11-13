//! The command table. Add a command here with tests; every front end picks it up.
//!
//! Each command reads its parameters into a type of its own. The JSON Schema that front ends and
//! agents see is generated from that type, so what is published is what is accepted: a parameter
//! the type doesn't have is refused, and so is one of the wrong kind.

use crate::{CommandSpec, EngineError, RunFn, Session};
use circuitcraft_io::native;
use circuitcraft_io::storage::Kind;
use circuitcraft_model::{Board, DesignSettings, Hole, ItemId, MAX_ITEMS, MAX_OUTLINE_GAP, NetId, TrackPath};
use circuitcraft_units::{Nm, Point};
use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// A command that reads its parameters into `$params`: its schema, and the entry point that
/// parses them and runs `$run`. Both come from the one type, so they can't disagree.
macro_rules! typed {
    ($run:ident, $params:ty) => {
        (schema::<$params> as fn() -> Value, (|s, v| $run(s, parse::<$params>(v)?)) as RunFn)
    };
}

/// A command without a menu item or a shortcut that changes something; the builder methods of
/// [`CommandSpec`] say otherwise.
const fn spec(id: &'static str, label: &'static str, about: &'static str, (schema, run): (fn() -> Value, RunFn)) -> CommandSpec {
    CommandSpec { id, label, menu: None, shortcut: None, about, read_only: false, desktop_only: false, schema, run }
}

/// Every command. Menus appear in the order their first command does here.
pub static COMMANDS: &[CommandSpec] = &[
    spec(
        "board.new",
        "New Board",
        "Replaces the open board with a new, empty one, undoably; it has no project folder until it is saved. Returns {name}.",
        typed!(board_new, BoardNew),
    )
    .menu("File/New Board"),
    spec(
        "file.open",
        "Open Project",
        "Opens a project folder, a .ccproj project file or a .ccpcb board file, replacing the open board, undoably. Returns {name, folder}.",
        typed!(file_open, FileOpen),
    ),
    spec(
        "file.save",
        "Save Project",
        "Saves the board. Without path: back to the files it was opened from or last saved to. With path: Save As, where a folder without a project gets <folder name>.ccproj and .ccpcb (an existing <folder name>.ccpcb is refused unless overwrite), and a folder holding a project is refused unless overwrite, which saves into that project's files. Returns {folder, project, board}.",
        typed!(file_save, FileSave),
    ),
    spec(
        "file.autosave",
        "Keep Unsaved Work",
        "Writes the board to the recovery folder if it differs from its files and from the copy that is there; each board has a copy of its own, and saving the board removes it. Front ends run this every so often, so a crash loses little. Where there is no recovery folder (the browser, today) nothing is written. Returns {written, id}.",
        typed!(file_autosave, NoParams),
    ),
    spec(
        "recovery.list",
        "List Unsaved Work",
        "Lists the boards with unsaved work in the recovery folder that this session doesn't hold itself: left by sessions that ended without saving, or kept by other sessions that are still running. Returns {recoverable: [{id, name, from, problem}]}, where from is the board file a board belongs to (null if it was never saved) and problem says why a file can't be read (null if it can).",
        typed!(recovery_list, NoParams),
    )
    .menu("File/Unsaved Work")
    .read_only(),
    spec(
        "recovery.open",
        "Recover Unsaved Board",
        "Opens a board from the recovery folder, replacing the open board, undoably. It opens as an unsaved board without files, and keeps a copy in the recovery folder under a new id until it is saved: save it with a path. Returns {name, from, id}, where from is the board file it belonged to, for the person to decide where it goes, and id is the new id of its copy.",
        typed!(recovery_open, RecoveryOpen),
    ),
    spec(
        "recovery.discard",
        "Discard Unsaved Work",
        "Removes a board's unsaved work from the recovery folder. A board that is open here and still unsaved gets a new copy the next time unsaved work is kept. Returns {discarded}.",
        typed!(recovery_discard, RecoveryDiscard),
    ),
    spec("edit.undo", "Undo", "Undoes the last change. Returns {undone}: its label, or null when there was nothing to undo.", typed!(edit_undo, NoParams))
        .menu("Edit/Undo")
        .shortcut("Ctrl+Z"),
    spec("edit.redo", "Redo", "Redoes the last undone change. Returns {redone}: its label, or null when there was nothing to redo.", typed!(edit_redo, NoParams))
        .menu("Edit/Redo")
        .shortcut("Ctrl+Y"),
    spec(
        "item.remove",
        "Remove Item",
        "Removes a track, via, footprint, zone, keepout, hole, text or drawing, or a net (which disconnects everything on it). Returns {removed}.",
        typed!(item_remove, ItemRemove),
    ),
    spec("select.set", "Select", "Selects these items and no others; an empty list selects nothing. Returns {selected}: how many are.", typed!(select_set, Items)),
    spec("select.add", "Add to Selection", "Selects these items as well as the ones that are selected. Returns {selected}.", typed!(select_add, Items)),
    spec("select.remove", "Remove from Selection", "Deselects these items. Returns {selected}.", typed!(select_remove, Items)),
    spec("select.list", "List Selection", "Returns {ids}: the selected items, in id order.", typed!(select_list, NoParams)).read_only(),
    spec("net.add", "Add Net", "Adds a net. Returns {id}.", typed!(net_add, NetAdd)),
    spec("track.add", "Add Track", "Adds a straight track. Returns {id}.", typed!(track_add, TrackAdd)),
    spec("via.add", "Add Via", "Adds a via from the top to the bottom copper layer. Returns {id}.", typed!(via_add, ViaAdd)),
    spec("hole.add", "Add Hole", "Adds a round hole that belongs to no footprint. Returns {id}.", typed!(hole_add, HoleAdd)),
    spec(
        "outline.assemble",
        "Outline from Drawings",
        "Joins the lines and arcs on the outline layer whose ends are within gap; the largest closed shape becomes the board outline and the others, which must lie wholly inside it (curves compared to within 0.02 mm), its cutouts, replacing any the board had, and the drawings are removed. Refused when a drawing on the layer is part of no closed shape. Returns {used, cutouts}.",
        typed!(outline_assemble, OutlineAssemble),
    )
    .menu("Design/Outline from Drawings"),
    spec(
        "settings.set",
        "Set Design Settings",
        "Changes the design settings that are given and leaves the others. Returns the settings, as board.info lists them.",
        typed!(settings_set, SettingsSet),
    ),
    spec("view.active", "Set Active Layer", "Makes a layer the one being worked on. Returns the view, as project.info lists it.", typed!(view_active, ViewActive)),
    spec("view.layer", "Show or Hide Layer", "Shows or hides a layer. Returns the view.", typed!(view_layer, ViewLayer)),
    spec(
        "view.single",
        "Single Layer Mode",
        "Draws only the active layer (and the board outline), or every layer that isn't hidden again. Returns the view.",
        typed!(view_single, ViewSingle),
    )
    .menu("View/Single Layer Mode"),
    spec("view.highlight", "Highlight Net", "Highlights a net, or none. Returns the view.", typed!(view_highlight, ViewHighlight)),
    spec(
        "project.info",
        "Project Information",
        "Returns what the session has open: {name, folder, project, documents: [{kind, name, file, modified, revision, selected, view: {activeLayer, singleLayer, highlight, hiddenLayers}}]}.",
        typed!(project_info, NoParams),
    )
    .menu("Reports/Project Information")
    .read_only(),
    spec(
        "board.info",
        "Board Information",
        "Returns the board's name, layers, design settings, item counts and undo state.",
        typed!(board_info, NoParams),
    )
    .menu("Reports/Board Information")
    .read_only(),
    spec(
        "file.detect",
        "Detect File Format",
        "Tells what kind of design file a file is, from its first 4 KiB. Returns {path, format, importable, note}.",
        typed!(file_detect, FileDetect),
    )
    .read_only(),
    spec(
        "commands",
        "List Commands",
        "Returns every command: {id, label, menu, shortcut, about, readOnly, desktopOnly, params}, where params is the JSON Schema of its parameters.",
        typed!(list_commands, NoParams),
    )
    .read_only(),
    spec("about", "About CircuitCraft", "Returns the name, version and licence of this build.", typed!(about, NoParams)).menu("Help/About CircuitCraft").read_only(),
];

/// The JSON Schema of a command's parameters, without the name of the Rust type behind it, and
/// with each description on one line (a doc comment is wrapped; a description is not).
fn schema<P: JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(P)).unwrap_or_default();
    if let Some(o) = schema.as_object_mut() {
        o.remove("title");
        o.remove("$schema");
    }
    for property in schema.get_mut("properties").and_then(Value::as_object_mut).into_iter().flat_map(|p| p.values_mut()) {
        if let Some(Value::String(description)) = property.get_mut("description") {
            *description = description.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    schema
}

/// Reads parameters into a command's type; `null` counts as `{}`. The message names the
/// parameter that is wrong.
fn parse<P: DeserializeOwned>(params: &Value) -> Result<P, EngineError> {
    let none = Value::Object(Map::new());
    let params = match params {
        Value::Null => &none,
        Value::Object(_) => params,
        _ => return Err(bad("parameters are given as an object")),
    };
    serde_path_to_error::deserialize(params).map_err(|e| {
        let at = e.path().to_string();
        if at == "." { bad(e.inner().to_string()) } else { bad(format!("`{at}`: {}", e.inner())) }
    })
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams(msg.into())
}

fn failed(e: impl std::fmt::Display) -> EngineError {
    EngineError::Failed(e.to_string())
}

/// A length in millimetres (JSON lengths are millimetres), as whole nanometres.
fn mm(v: f64, key: &str) -> Result<Nm, EngineError> {
    Nm::from_mm(v).map_err(|e| bad(format!("`{key}`: {e}")))
}

/// A point, `[x, y]` in millimetres.
fn point([x, y]: [f64; 2], key: &str) -> Result<Point, EngineError> {
    Ok(Point::new(mm(x, key)?, mm(y, key)?))
}

/// An optional net, by name; an unknown name is an error, not a new net.
fn net(board: &Board, name: Option<&str>) -> Result<Option<NetId>, EngineError> {
    match name {
        None => Ok(None),
        Some(name) => board.net_by_name(name).map(|n| Some(n.id)).ok_or_else(|| bad(format!("no net named {name:?}"))),
    }
}

/// A layer, by name.
fn layer(board: &Board, name: &str) -> Result<circuitcraft_model::LayerId, EngineError> {
    board.stack.by_name(name).map(|l| l.id).ok_or_else(|| bad(format!("no layer named {name:?}")))
}

/// No parameters.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoParams {}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct BoardNew {
    /// The board's name, 1 to 256 characters; "Untitled" when left out.
    name: Option<String>,
}

/// The longest board name accepted.
const MAX_NAME_CHARS: usize = 256;

fn board_new(s: &mut Session, p: BoardNew) -> Result<Value, EngineError> {
    let name = p.name.as_deref().unwrap_or("Untitled").trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(bad(format!("`name` must be 1 to {MAX_NAME_CHARS} characters")));
    }
    // Undoable: the old board comes back with its own project folder (the session keys folders on
    // the board's generation), so unsaved work is never lost and never saved in the wrong place.
    s.replace_board("New Board", Board::new(name), None, true);
    Ok(json!({ "name": s.board().name }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileOpen {
    /// A project folder, a .ccproj project file or a .ccpcb board file.
    path: String,
}

fn file_open(s: &mut Session, p: FileOpen) -> Result<Value, EngineError> {
    let (board, files) = native::open(s.storage(), Path::new(&p.path)).map_err(failed)?;
    s.replace_board("Open", board, Some(files), true);
    Ok(json!({ "name": s.board().name, "folder": s.folder().map(|f| f.to_string_lossy()) }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileSave {
    /// The project folder to save into (Save As); left out, the board goes back to its own files.
    path: Option<String>,
    /// Whether to replace a project that is already in the folder; false when left out.
    overwrite: Option<bool>,
}

fn file_save(s: &mut Session, p: FileSave) -> Result<Value, EngineError> {
    let files = match &p.path {
        // Back to the files this board came from, whatever they are called.
        None => {
            let files = s.target().ok_or_else(|| bad("`path` is required: this board hasn't been saved yet"))?.clone();
            native::save(s.storage(), &files, s.board()).map_err(failed)?;
            files
        }
        // Save As.
        Some(folder) => native::save_project(s.storage(), Path::new(folder), s.board(), p.overwrite.unwrap_or(false)).map_err(failed)?,
    };
    let saved = json!({
        "folder": files.folder().to_string_lossy(),
        "project": files.project().map(|p| p.to_string_lossy()),
        "board": files.board().to_string_lossy(),
    });
    s.saved_to(files);
    Ok(saved)
}

/// The path of the recovery file with this id, which is 32 hexadecimal digits and nothing else,
/// so an id can't name a file outside the recovery folder.
fn recovery_file(s: &Session, id: &str) -> Result<PathBuf, EngineError> {
    let folder = s.storage().recovery_folder().ok_or_else(|| failed("there is nowhere to keep unsaved work here"))?;
    if id.len() != 32 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(bad("`id` is 32 hexadecimal digits, as recovery.list gives it"));
    }
    Ok(folder.join(format!("{id}.{}", native::RECOVERY_EXT)))
}

/// The id a recovery file is listed under: its name without the extension.
fn recovery_id(file: &Path) -> Option<&str> {
    file.file_stem().and_then(|stem| stem.to_str())
}

fn file_autosave(s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
    const NOTHING: fn() -> Value = || json!({ "written": false, "id": null });
    if !s.modified() {
        // The board is as its files are: a copy has nothing to add, and would be offered for
        // recovery although nothing was lost.
        s.drop_recovery_copy();
        return Ok(NOTHING());
    }
    // Nowhere to keep it: front ends run this unasked, so that is not an error here.
    if s.storage().recovery_folder().is_none() {
        return Ok(NOTHING());
    }
    let revision = s.doc.revision();
    let own = s.state().and_then(|state| state.recovery.clone());
    // The copy is looked at, not trusted to be there: another session may have discarded it.
    let kept = |file: &PathBuf| s.state().and_then(|state| state.autosaved) == Some(revision) && matches!(s.storage().kind(file), Ok(Kind::File(_)));
    if own.as_ref().is_some_and(kept) {
        return Ok(NOTHING());
    }
    let file = match own {
        Some(file) => file,
        // 128 random bits: a name no other board has, in this session or another.
        None => recovery_file(s, &format!("{:032x}", ItemId::random()?.as_u128()))?,
    };
    // Where the board belongs: its files, or for a recovered board what its recovery file said.
    let from = s.target().map(|files| files.board()).or_else(|| s.state().and_then(|state| state.came_from.as_deref()).map(Path::new));
    let text = native::write_recovery(s.board(), from).map_err(failed)?;
    if let Err(e) = write_copy(s, &file, &text) {
        // Whatever is in the way there, the next try is under a new name.
        s.state_mut().recovery = None;
        return Err(e);
    }
    let id = recovery_id(&file).map(str::to_owned);
    let state = s.state_mut();
    (state.recovery, state.autosaved) = (Some(file), Some(revision));
    Ok(json!({ "written": true, "id": id }))
}

/// Writes a recovery file, making the recovery folder if it isn't there.
fn write_copy(s: &Session, file: &Path, text: &str) -> Result<(), EngineError> {
    let write = || {
        file.parent().map_or(Ok(()), |folder| s.storage().create_folder(folder))?;
        s.storage().write(file, text.as_bytes())
    };
    write().map_err(|e| failed(format!("{}: {e}", file.display())))
}

/// The most recovery files listed.
const MAX_RECOVERABLE: usize = 100;

/// Whether the open board, or one that undo or redo leads back to, keeps its unsaved work in
/// `file`. A board that the history has let go of no longer holds its copy: that work can only
/// be recovered now.
fn holds(s: &Session, file: &Path) -> bool {
    s.doc.generations().any(|generation| s.boards.get(&generation).is_some_and(|state| state.recovery.as_deref() == Some(file)))
}

/// The recovery files this session doesn't hold itself, by id, in name order.
fn recoverable(s: &Session) -> Result<Vec<(String, PathBuf)>, EngineError> {
    let Some(folder) = s.storage().recovery_folder() else { return Ok(Vec::new()) };
    let files = s.storage().files(&folder).map_err(|e| failed(format!("{}: {e}", folder.display())))?;
    let left = files.into_iter().filter(|f| f.extension().is_some_and(|e| e == native::RECOVERY_EXT) && !holds(s, f));
    // Only files named as this program names them: the id must lead back to the same file.
    let named =
        left.filter_map(|f| recovery_id(&f).filter(|id| recovery_file(s, id).is_ok_and(|same| same == f)).map(str::to_owned).map(|id| (id, f)));
    Ok(named.take(MAX_RECOVERABLE).collect())
}

fn recovery_list(s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
    let entries =
        recoverable(s)?.into_iter().map(|(id, file)| match native::read_text(s.storage(), &file).and_then(|text| native::peek_recovery(&text)) {
            Ok(header) => json!({ "id": id, "name": header.name, "from": header.from, "problem": null }),
            Err(e) => json!({ "id": id, "name": null, "from": null, "problem": e.to_string() }),
        });
    Ok(json!({ "recoverable": entries.collect::<Vec<_>>() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RecoveryOpen {
    /// The board to recover, as recovery.list names it; left out when there is only one.
    id: Option<String>,
}

fn recovery_open(s: &mut Session, p: RecoveryOpen) -> Result<Value, EngineError> {
    let file = match &p.id {
        Some(id) => recovery_file(s, id)?,
        None => match recoverable(s)?.as_slice() {
            [] => return Err(failed("there is no unsaved work to recover")),
            [(_, only)] => only.clone(),
            many => return Err(bad(format!("{} boards can be recovered; give the `id` of one (recovery.list names them)", many.len()))),
        },
    };
    let text = native::read_text(s.storage(), &file).map_err(failed)?;
    let (board, from) = native::read_recovery(&text).map_err(failed)?;
    // The recovered board keeps its work in a file of its own, and the one it was read from is
    // removed. That one may be the copy of a board that is open in another session, or in this
    // one: two boards writing to one file would overwrite each other's work, and a board whose
    // copy is gone writes it again.
    let own = recovery_file(s, &format!("{:032x}", ItemId::random()?.as_u128()))?;
    write_copy(s, &own, &text)?;
    if let Err(e) = s.storage().remove(&file) {
        log::warn!("could not remove {}: {e}", file.display());
    }
    // No files of its own: where it is saved is for the person to say, not for a file to claim.
    s.replace_board("Recover", board, None, false);
    let revision = s.doc.revision();
    let id = recovery_id(&own).map(str::to_owned);
    let state = s.state_mut();
    (state.recovery, state.autosaved, state.came_from) = (Some(own), Some(revision), from.clone());
    Ok(json!({ "name": s.board().name, "from": from, "id": id }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RecoveryDiscard {
    /// The board to discard, as recovery.list names it.
    id: String,
}

fn recovery_discard(s: &mut Session, p: RecoveryDiscard) -> Result<Value, EngineError> {
    let file = recovery_file(s, &p.id)?;
    if !matches!(s.storage().kind(&file), Ok(Kind::File(_))) {
        return Err(failed(format!("no unsaved work with id {}", p.id)));
    }
    s.storage().remove(&file).map_err(|e| failed(format!("{}: {e}", file.display())))?;
    // A board of this session that kept its work there has no copy now; if it is still unsaved,
    // the next autosave writes a new one.
    for state in s.boards.values_mut().filter(|state| state.recovery.as_deref() == Some(file.as_path())) {
        state.recovery = None;
    }
    Ok(json!({ "discarded": p.id }))
}

fn edit_undo(s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
    Ok(json!({ "undone": s.doc.undo() }))
}

fn edit_redo(s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
    Ok(json!({ "redone": s.doc.redo() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ItemRemove {
    /// The id of the item or net, a UUID.
    id: String,
}

fn item_remove(s: &mut Session, p: ItemRemove) -> Result<Value, EngineError> {
    let id: ItemId = p.id.parse()?;
    s.doc.edit("Remove", |b| b.remove(id))?;
    Ok(json!({ "removed": id.to_string() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Items {
    /// The ids of items on the board (tracks, vias, footprints, zones, keepouts, holes, texts
    /// and drawings), each a UUID.
    ids: Vec<String>,
}

impl Items {
    /// The ids, each of an item that is on the board.
    fn on(&self, board: &Board) -> Result<Vec<ItemId>, EngineError> {
        if self.ids.len() > MAX_ITEMS {
            return Err(bad("`ids`: more ids than a board has items"));
        }
        self.ids
            .iter()
            .map(|id| id.parse::<ItemId>().ok().filter(|&id| board.has_item(id)).ok_or_else(|| bad(format!("`ids`: no item with id {id:?}"))))
            .collect()
    }
}

/// How many items are selected.
fn selected(s: &Session) -> Value {
    json!({ "selected": s.selection().count() })
}

fn select_set(s: &mut Session, p: Items) -> Result<Value, EngineError> {
    s.state_mut().selection = p.on(s.board())?.into_iter().collect();
    Ok(selected(s))
}

/// Changes the selection. What is kept is what is on the board now, so the selection never
/// grows past the board, and an item that was removed while selected is not selected by a later
/// undo once the selection has been changed.
fn reselect(s: &mut Session, change: impl FnOnce(&mut std::collections::BTreeSet<ItemId>)) -> Value {
    let mut selection = s.selection().collect();
    change(&mut selection);
    s.state_mut().selection = selection;
    selected(s)
}

fn select_add(s: &mut Session, p: Items) -> Result<Value, EngineError> {
    let ids = p.on(s.board())?;
    Ok(reselect(s, |selection| selection.extend(ids)))
}

fn select_remove(s: &mut Session, p: Items) -> Result<Value, EngineError> {
    let ids = p.on(s.board())?;
    Ok(reselect(s, |selection| {
        for id in &ids {
            selection.remove(id);
        }
    }))
}

fn select_list(s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
    Ok(json!({ "ids": s.selection().map(|id| id.to_string()).collect::<Vec<_>>() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NetAdd {
    /// The net's name, which no other net has.
    name: String,
}

fn net_add(s: &mut Session, p: NetAdd) -> Result<Value, EngineError> {
    let id = s.doc.edit("Add Net", |b| b.add_net(&p.name))?;
    Ok(json!({ "id": id.to_string() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrackAdd {
    /// The copper layer, by name.
    layer: String,
    /// The copper width in millimetres; the design settings' track width when left out.
    width: Option<f64>,
    /// Where it starts, [x, y] in millimetres.
    from: [f64; 2],
    /// Where it ends, [x, y] in millimetres.
    to: [f64; 2],
    /// The net it carries, by name; none when left out.
    net: Option<String>,
}

fn track_add(s: &mut Session, p: TrackAdd) -> Result<Value, EngineError> {
    let board = s.board();
    let layer = layer(board, &p.layer)?;
    let width = p.width.map_or(Ok(board.design.track_width), |w| mm(w, "width"))?;
    let path = TrackPath::Line { from: point(p.from, "from")?, to: point(p.to, "to")? };
    let net = net(board, p.net.as_deref())?;
    let id = s.doc.edit("Add Track", |b| b.add_track(layer, width, path, net))?;
    Ok(json!({ "id": id.to_string() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViaAdd {
    /// Its centre, [x, y] in millimetres.
    at: [f64; 2],
    /// The copper diameter in millimetres; the design settings' via diameter when left out.
    diameter: Option<f64>,
    /// The drill diameter in millimetres; the design settings' via drill when left out.
    drill: Option<f64>,
    /// The net it carries, by name; none when left out.
    net: Option<String>,
}

fn via_add(s: &mut Session, p: ViaAdd) -> Result<Value, EngineError> {
    let board = s.board();
    let at = point(p.at, "at")?;
    let diameter = p.diameter.map_or(Ok(board.design.via_diameter), |d| mm(d, "diameter"))?;
    let drill = p.drill.map_or(Ok(board.design.via_drill), |d| mm(d, "drill"))?;
    let net = net(board, p.net.as_deref())?;
    let id = s.doc.edit("Add Via", |b| b.add_via(at, diameter, drill, net))?;
    Ok(json!({ "id": id.to_string() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct HoleAdd {
    /// Its centre, [x, y] in millimetres.
    at: [f64; 2],
    /// Its diameter in millimetres.
    diameter: f64,
    /// Whether it is plated; false when left out.
    plated: Option<bool>,
}

fn hole_add(s: &mut Session, p: HoleAdd) -> Result<Value, EngineError> {
    let at = point(p.at, "at")?;
    let hole = Hole::round(mm(p.diameter, "diameter")?, p.plated.unwrap_or(false));
    let id = s.doc.edit("Add Hole", |b| b.add_hole(at, hole))?;
    Ok(json!({ "id": id.to_string() }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OutlineAssemble {
    /// How far apart two ends may be and still join, in millimetres: 0 to 1, and 0.001 when left
    /// out.
    gap: Option<f64>,
}

/// The gap `outline.assemble` closes when none is given: a micrometre, which covers the rounding
/// of drawings that were meant to meet.
const OUTLINE_GAP: Nm = Nm(1_000);

fn outline_assemble(s: &mut Session, p: OutlineAssemble) -> Result<Value, EngineError> {
    let gap = p.gap.map_or(Ok(OUTLINE_GAP), |g| mm(g, "gap"))?;
    if !(0..=MAX_OUTLINE_GAP.0).contains(&gap.0) {
        return Err(bad(format!("`gap` must be 0 to {} mm", MAX_OUTLINE_GAP.to_mm())));
    }
    // The gap is the only parameter: whatever else goes wrong is the drawings' doing.
    let done = s.doc.edit("Outline from Drawings", |b| b.assemble_outline(gap)).map_err(failed)?;
    Ok(json!({ "used": done.used, "cutouts": done.cutouts }))
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsSet {
    /// How much larger than the copper a solder mask opening is, in millimetres; may be negative.
    mask_expansion: Option<f64>,
    /// How much larger than the copper a paste aperture is, in millimetres; may be negative.
    paste_expansion: Option<f64>,
    /// Whether solder mask covers a via that leaves it to the settings.
    tent_vias: Option<bool>,
    /// The clearance between copper on different nets, in millimetres.
    clearance: Option<f64>,
    /// The width of a new track, in millimetres.
    track_width: Option<f64>,
    /// The copper diameter of a new via, in millimetres.
    via_diameter: Option<f64>,
    /// The drill diameter of a new via, in millimetres; smaller than its copper diameter.
    via_drill: Option<f64>,
    /// The point that drill and placement files measure from, [x, y] in millimetres.
    origin: Option<[f64; 2]>,
}

fn settings_set(s: &mut Session, p: SettingsSet) -> Result<Value, EngineError> {
    let old = s.board().design;
    let length = |given: Option<f64>, key: &str, old: Nm| given.map_or(Ok(old), |v| mm(v, key));
    let design = DesignSettings {
        mask_expansion: length(p.mask_expansion, "maskExpansion", old.mask_expansion)?,
        paste_expansion: length(p.paste_expansion, "pasteExpansion", old.paste_expansion)?,
        tent_vias: p.tent_vias.unwrap_or(old.tent_vias),
        clearance: length(p.clearance, "clearance", old.clearance)?,
        track_width: length(p.track_width, "trackWidth", old.track_width)?,
        via_diameter: length(p.via_diameter, "viaDiameter", old.via_diameter)?,
        via_drill: length(p.via_drill, "viaDrill", old.via_drill)?,
        origin: p.origin.map_or(Ok(old.origin), |o| point(o, "origin"))?,
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

/// How the board is viewed, with layers and the net by name.
fn view(s: &Session) -> Value {
    let stack = &s.board().stack;
    let name = |id| stack.get(id).map(|l| l.name.as_str());
    let hidden = s.state().map(|state| &state.view.hidden);
    json!({
        "activeLayer": s.active_layer().and_then(name),
        "singleLayer": s.single_layer(),
        "highlight": s.highlight().and_then(|net| s.board().nets.get(net)).map(|n| n.name.as_str()),
        "hiddenLayers": stack.layers.iter().filter(|l| hidden.is_some_and(|h| h.contains(&l.id))).map(|l| l.name.as_str()).collect::<Vec<_>>(),
    })
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewActive {
    /// The layer, by name.
    layer: String,
}

fn view_active(s: &mut Session, p: ViewActive) -> Result<Value, EngineError> {
    s.state_mut().view.active = Some(layer(s.board(), &p.layer)?);
    Ok(view(s))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewLayer {
    /// The layer, by name.
    layer: String,
    /// Whether it is drawn.
    visible: bool,
}

fn view_layer(s: &mut Session, p: ViewLayer) -> Result<Value, EngineError> {
    let id = layer(s.board(), &p.layer)?;
    // What is kept is what the board has, so the list never grows past the board's layers.
    let stack = &s.board().stack;
    let mut hidden: std::collections::BTreeSet<_> =
        s.state().into_iter().flat_map(|state| &state.view.hidden).copied().filter(|&l| stack.get(l).is_some()).collect();
    if p.visible {
        hidden.remove(&id);
    } else {
        hidden.insert(id);
    }
    s.state_mut().view.hidden = hidden;
    Ok(view(s))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewSingle {
    /// Whether only the active layer is drawn; left out, it is switched from what it was.
    on: Option<bool>,
}

fn view_single(s: &mut Session, p: ViewSingle) -> Result<Value, EngineError> {
    s.state_mut().view.single_layer = p.on.unwrap_or(!s.single_layer());
    Ok(view(s))
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewHighlight {
    /// The net to highlight, by name; left out, no net is highlighted.
    net: Option<String>,
}

fn view_highlight(s: &mut Session, p: ViewHighlight) -> Result<Value, EngineError> {
    s.state_mut().view.highlight = net(s.board(), p.net.as_deref())?;
    Ok(view(s))
}

fn project_info(s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
    let files = s.target();
    let text = |p: &Path| p.to_string_lossy().into_owned();
    Ok(json!({
        "name": s.board().name,
        "folder": s.folder().map(text),
        "project": files.and_then(|f| f.project()).map(text),
        "documents": [{
            "kind": "board",
            "name": s.board().name,
            "file": files.map(|f| text(f.board())),
            "modified": s.modified(),
            "revision": s.doc.revision(),
            "selected": s.selection().count(),
            "view": view(s),
        }],
    }))
}

fn board_info(s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileDetect {
    /// The file to look at.
    path: String,
}

fn file_detect(s: &mut Session, p: FileDetect) -> Result<Value, EngineError> {
    let path = Path::new(&p.path);
    // Only a regular file is read: reading a pipe or a device can block forever.
    match s.storage().kind(path) {
        Ok(Kind::File(_)) => {}
        Ok(_) => return Err(failed(format!("{}: not a regular file", p.path))),
        Err(e) => return Err(failed(format!("cannot open {}: {e}", p.path))),
    }
    let head = s.storage().read(path, circuitcraft_io::SNIFF_LEN as u64).map_err(|e| failed(format!("cannot read {}: {e}", p.path)))?;
    let format = circuitcraft_io::detect(&head);
    Ok(json!({
        "path": p.path,
        "format": format.map(|f| f.name()),
        "importable": matches!(format, Some(circuitcraft_io::Format::CircuitCraftBoard | circuitcraft_io::Format::CircuitCraftProject)),
        "note": match format {
            Some(circuitcraft_io::Format::CircuitCraftBoard | circuitcraft_io::Format::CircuitCraftProject) => "open it with file.open",
            Some(_) => "recognised; import arrives in 0.1",
            None => "not a format CircuitCraft recognises",
        },
    }))
}

fn list_commands(_s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
    let one = |c: &CommandSpec| {
        json!({
            "id": c.id,
            "label": c.label,
            "menu": c.menu,
            "shortcut": c.shortcut,
            "about": c.about,
            "readOnly": c.read_only,
            "desktopOnly": c.desktop_only,
            "params": (c.schema)(),
        })
    };
    Ok(Value::Array(COMMANDS.iter().map(one).collect()))
}

fn about(_s: &mut Session, _: NoParams) -> Result<Value, EngineError> {
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
    use circuitcraft_io::storage::{DiskStorage, MemoryStorage, Storage as _};
    use std::sync::Arc;

    /// A session on the real disk, with nowhere to keep unsaved work, so a test never touches the
    /// recovery folder of whoever runs it.
    fn on_disk() -> Session {
        Session::with_storage(Box::new(DiskStorage::with_recovery(None)))
    }

    /// A path that a command reported as text, to compare as a path: on Windows the parts of a
    /// path in memory are joined with `\`, so `/work\Blinky` there is `/work/Blinky` here.
    fn path(reported: &Value) -> Option<PathBuf> {
        reported.as_str().map(PathBuf::from)
    }

    /// The same for the path a test expects.
    fn at(path: &str) -> Option<PathBuf> {
        Some(PathBuf::from(path))
    }

    /// A session on files in `memory`, which other sessions can share.
    fn sharing(memory: &Arc<MemoryStorage>) -> Session {
        Session::with_storage(Box::new(memory.clone()))
    }

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
        let mut s = on_disk();
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
        let mut other = on_disk();
        other.execute("board.new", &json!({ "name": "Board B" })).expect("new B");
        other.execute("file.save", &json!({ "path": b.to_string_lossy() })).expect("save B");
        let mut s = on_disk();
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
        let mut s = on_disk();
        let r = s.execute("file.detect", &json!({ "path": path.to_string_lossy() })).expect("detect");
        assert_eq!(r["format"], "KiCad board");
    }

    #[test]
    fn build_undo_and_redo_through_commands() {
        let mut s = on_disk();
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
        let mut s = on_disk();
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
        let mut s = on_disk();
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
        native::save_project(&DiskStorage::with_recovery(None), &folder, &board, false).expect("save");
        folder
    }

    #[test]
    fn the_outline_is_assembled_from_drawings_and_survives_a_save() {
        let dir = TempDir::new("outline");
        let folder = save_board_with_drawn_outline(&dir);
        let mut s = on_disk();
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
        let mut other = on_disk();
        other.execute("file.open", &json!({ "path": folder.to_string_lossy() })).expect("open");
        assert_eq!(other.board(), s.board());
        assert_eq!(s.execute("edit.undo", &Value::Null).expect("undo")["undone"], "Outline from Drawings");
        assert!(s.board().outline.is_none() && s.board().cutouts.is_empty() && s.board().graphics.len() == 5);
    }

    /// A track from the origin to (`x`, 0) mm on the top copper; returns its id as JSON.
    fn track(s: &mut Session, x: i64) -> Value {
        s.execute("track.add", &json!({ "layer": "Top Copper", "from": [0, 0], "to": [x, 0] })).expect("track")["id"].clone()
    }

    fn ids(s: &mut Session) -> Value {
        s.execute("select.list", &Value::Null).expect("list")["ids"].clone()
    }

    #[test]
    fn items_are_selected_and_stay_selected_through_undo() {
        let mut s = Session::in_memory();
        let (a, b, c) = (track(&mut s, 1), track(&mut s, 2), track(&mut s, 3));
        let sorted = |mut v: Vec<&Value>| {
            v.sort_by_key(|id| id.as_str().map(str::to_owned));
            json!(v)
        };
        assert_eq!(s.execute("select.set", &json!({ "ids": [a, b] })).expect("set"), json!({ "selected": 2 }));
        assert_eq!(ids(&mut s), sorted(vec![&a, &b]), "in id order");
        assert_eq!(s.execute("select.add", &json!({ "ids": [b, c] })).expect("add"), json!({ "selected": 3 }));
        assert_eq!(s.execute("select.remove", &json!({ "ids": [a] })).expect("remove"), json!({ "selected": 2 }));
        assert_eq!(ids(&mut s), sorted(vec![&b, &c]));
        assert_eq!(s.execute("project.info", &Value::Null).expect("info")["documents"][0]["selected"], 2);
        // Selecting is not an edit: nothing to undo, nothing to save.
        assert_eq!(s.doc().undo_label(), Some("Add Track"));
        // An item that is removed is no longer selected; undo brings it back selected.
        s.execute("item.remove", &json!({ "id": c })).expect("remove");
        assert_eq!(ids(&mut s), json!([b]));
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!(ids(&mut s), sorted(vec![&b, &c]));
        // Unless the selection was changed while it was gone: then what was selected is what was
        // on the board.
        for change in ["select.add", "select.remove"] {
            s.execute("item.remove", &json!({ "id": c })).expect("remove");
            s.execute(change, &json!({ "ids": [] })).expect(change);
            s.execute("edit.undo", &Value::Null).expect("undo");
            assert_eq!(ids(&mut s), json!([b]), "{change}");
            s.execute("select.add", &json!({ "ids": [c] })).expect("add");
        }
        // Refused, changing nothing: an id that is no item's, a net's id, text that is no id.
        let net = s.execute("net.add", &json!({ "name": "GND" })).expect("net")["id"].clone();
        for bad in [json!("3f2c9a10-6b7e-4d21-9a0c-58e1f4b2d7a3"), net, json!("nope")] {
            for command in ["select.set", "select.add", "select.remove"] {
                assert!(matches!(s.execute(command, &json!({ "ids": [a, bad] })), Err(EngineError::BadParams(_))), "{command} {bad}");
            }
        }
        assert!(matches!(s.execute("select.set", &json!({ "ids": [7] })), Err(EngineError::BadParams(_))));
        assert!(matches!(s.execute("select.set", &json!({})), Err(EngineError::BadParams(_))), "the ids must be given");
        assert_eq!(ids(&mut s), sorted(vec![&b, &c]));
        // Each board has its own selection: a new board has none, and undo brings the old one back.
        s.execute("board.new", &Value::Null).expect("new");
        assert_eq!(ids(&mut s), json!([]));
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!(ids(&mut s), sorted(vec![&b, &c]));
        assert_eq!(s.execute("select.set", &json!({ "ids": [] })).expect("clear"), json!({ "selected": 0 }));
    }

    #[test]
    fn the_view_is_kept_per_board_and_follows_the_board() {
        use circuitcraft_model::layer_ids::{BOTTOM_COPPER, OUTLINE, TOP_COPPER, TOP_SILK};
        let mut s = Session::in_memory();
        let view = |s: &mut Session| s.execute("project.info", &Value::Null).expect("info")["documents"][0]["view"].clone();
        let fresh = json!({ "activeLayer": "Top Copper", "singleLayer": false, "highlight": null, "hiddenLayers": [] });
        assert_eq!(view(&mut s), fresh);
        assert!(s.layer_visible(TOP_SILK) && !s.layer_visible(circuitcraft_model::LayerId(999)));
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert_eq!(s.execute("view.active", &json!({ "layer": "Bottom Copper" })).expect("active")["activeLayer"], "Bottom Copper");
        s.execute("view.layer", &json!({ "layer": "Top Silkscreen", "visible": false })).expect("hide");
        s.execute("view.layer", &json!({ "layer": "Top Paste", "visible": false })).expect("hide");
        s.execute("view.layer", &json!({ "layer": "Top Paste", "visible": true })).expect("show");
        s.execute("view.highlight", &json!({ "net": "GND" })).expect("highlight");
        let set = json!({ "activeLayer": "Bottom Copper", "singleLayer": false, "highlight": "GND", "hiddenLayers": ["Top Silkscreen"] });
        assert_eq!(view(&mut s), set);
        assert_eq!((s.active_layer(), s.highlight().is_some()), (Some(BOTTOM_COPPER), true));
        assert!(!s.layer_visible(TOP_SILK) && s.layer_visible(TOP_COPPER));
        // Single-layer mode draws the active layer and the outline; without a parameter it switches.
        assert_eq!(s.execute("view.single", &Value::Null).expect("single")["singleLayer"], true);
        assert!(s.layer_visible(BOTTOM_COPPER) && s.layer_visible(OUTLINE) && !s.layer_visible(TOP_COPPER) && !s.layer_visible(TOP_SILK));
        assert_eq!(s.execute("view.single", &Value::Null).expect("single")["singleLayer"], false);
        assert_eq!(s.execute("view.single", &json!({ "on": true })).expect("single")["singleLayer"], true);
        s.execute("view.single", &json!({ "on": false })).expect("single");
        // Looking is not an edit.
        assert_eq!(s.doc().undo_label(), Some("Add Net"));
        // Refused, changing nothing: a layer or a net the board doesn't have.
        let bad = |s: &mut Session, id: &str, p: Value| matches!(s.execute(id, &p), Err(EngineError::BadParams(_)));
        assert!(bad(&mut s, "view.active", json!({ "layer": "Nope" })));
        assert!(bad(&mut s, "view.layer", json!({ "layer": "Nope", "visible": false })));
        assert!(bad(&mut s, "view.layer", json!({ "layer": "Top Paste" })));
        assert!(bad(&mut s, "view.highlight", json!({ "net": "VCC" })));
        assert_eq!(view(&mut s), set);
        // A net that is removed is no longer highlighted; without a net, none is.
        s.execute("edit.undo", &Value::Null).expect("undo the net");
        assert_eq!(view(&mut s)["highlight"], Value::Null);
        s.execute("edit.redo", &Value::Null).expect("redo");
        assert_eq!(view(&mut s)["highlight"], "GND");
        assert_eq!(s.execute("view.highlight", &Value::Null).expect("none")["highlight"], Value::Null);
        // A new board starts with a view of its own; undo brings back the old board's.
        s.execute("board.new", &Value::Null).expect("new");
        assert_eq!(view(&mut s), fresh);
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!(
            view(&mut s),
            json!({ "activeLayer": "Bottom Copper", "singleLayer": false, "highlight": null, "hiddenLayers": ["Top Silkscreen"] })
        );
    }

    #[test]
    fn the_session_says_what_it_has_open_and_whether_it_changed() {
        let files = Arc::new(MemoryStorage::default());
        let mut s = sharing(&files);
        let info = |s: &mut Session| s.execute("project.info", &Value::Null).expect("info");
        let fresh = info(&mut s);
        assert_eq!((&fresh["name"], &fresh["folder"], &fresh["project"]), (&json!("Untitled"), &Value::Null, &Value::Null));
        assert_eq!(fresh["documents"].as_array().map(Vec::len), Some(1));
        let doc = &fresh["documents"][0];
        assert_eq!(
            (&doc["kind"], &doc["file"], &doc["modified"], &doc["revision"], &doc["selected"]),
            (&json!("board"), &Value::Null, &json!(false), &json!(0), &json!(0))
        );
        assert!(!s.modified());
        // An edit makes it modified; saving makes it unmodified and gives it files.
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert!(s.modified() && info(&mut s)["documents"][0]["modified"] == true);
        s.execute("file.save", &json!({ "path": "/work/Blinky" })).expect("save");
        let saved = info(&mut s);
        assert_eq!((path(&saved["folder"]), path(&saved["project"])), (at("/work/Blinky"), at("/work/Blinky/Blinky.ccproj")));
        assert_eq!((path(&saved["documents"][0]["file"]), &saved["documents"][0]["modified"]), (at("/work/Blinky/Blinky.ccpcb"), &json!(false)));
        // Undo changes the board away from its files; redo brings it back to them.
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert!(s.modified());
        s.execute("edit.redo", &Value::Null).expect("redo");
        assert!(!s.modified());
        // The same net added again is another net: the board differs, until it is saved.
        s.execute("edit.undo", &Value::Null).expect("undo");
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert!(s.modified());
        s.execute("file.save", &Value::Null).expect("save");
        assert!(!s.modified());
        // A board that is opened is as its files are; a new board is as empty as it was made.
        let mut other = sharing(&files);
        other.execute("file.open", &json!({ "path": "/work/Blinky" })).expect("open");
        assert!(!other.modified());
        assert_eq!(other.board(), s.board());
        other.execute("board.new", &Value::Null).expect("new");
        assert!(!other.modified());
        other.execute("net.add", &json!({ "name": "VCC" })).expect("net");
        assert!(other.modified());
        // Each board is compared with its own files: back on the opened board, nothing differs.
        other.execute("edit.undo", &Value::Null).expect("undo the net");
        other.execute("edit.undo", &Value::Null).expect("undo the new board");
        assert!(!other.modified() && other.board() == s.board());
    }

    fn recoverable(s: &mut Session) -> Vec<Value> {
        s.execute("recovery.list", &Value::Null).expect("list")["recoverable"].as_array().cloned().unwrap_or_default()
    }

    #[test]
    fn unsaved_work_is_kept_and_recovered_by_a_later_session() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let autosave = |s: &mut Session| s.execute("file.autosave", &Value::Null).expect("autosave");
        let mut s = sharing(&files);
        // Nothing to keep for a board that hasn't changed.
        assert_eq!(autosave(&mut s), json!({ "written": false, "id": null }));
        assert!(files.snapshot().is_empty());
        s.execute("board.new", &json!({ "name": "Blinky" })).expect("new");
        assert_eq!(autosave(&mut s)["written"], false, "a new, empty board");
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        let first = autosave(&mut s);
        assert_eq!(first["written"], true);
        let id = first["id"].as_str().expect("id").to_owned();
        assert_eq!(files.snapshot().keys().cloned().collect::<Vec<_>>(), [PathBuf::from(format!("/recovery/{id}.ccrecovery"))]);
        // Unchanged since: nothing is written. Changed again: the same file is written again.
        assert_eq!(autosave(&mut s), json!({ "written": false, "id": null }));
        track(&mut s, 5);
        assert_eq!(autosave(&mut s), json!({ "written": true, "id": id }));
        assert_eq!(files.snapshot().len(), 1);
        // The session's own copy is not something for it to recover.
        assert!(recoverable(&mut s).is_empty());
        assert!(matches!(s.execute("recovery.open", &Value::Null), Err(EngineError::Failed(_))));
        let lost = s.board().clone();
        drop(s); // the crash

        let mut later = sharing(&files);
        assert_eq!(recoverable(&mut later), [json!({ "id": id, "name": "Blinky", "from": null, "problem": null })]);
        let recovered = later.execute("recovery.open", &Value::Null).expect("recover");
        assert_eq!((&recovered["name"], &recovered["from"]), (&json!("Blinky"), &Value::Null));
        assert!(
            recovered["id"]
                .as_str()
                .is_some_and(|new| new != id && files.snapshot().contains_key(&PathBuf::from(format!("/recovery/{new}.ccrecovery"))))
        );
        assert_eq!(later.board(), &lost);
        // It is unsaved work again: modified, without files, and still in the recovery folder,
        // now as this session's own copy, under a name of its own.
        assert!(later.modified() && later.target().is_none());
        assert_eq!((later.doc().undo_label(), files.snapshot().len()), (Some("Recover"), 1));
        assert!(!files.snapshot().contains_key(&PathBuf::from(format!("/recovery/{id}.ccrecovery"))), "the file it was read from is gone");
        assert!(recoverable(&mut later).is_empty());
        assert_eq!(autosave(&mut later)["written"], false, "the copy is as the board is");
        assert!(matches!(later.execute("file.save", &Value::Null), Err(EngineError::BadParams(_))), "it has no files to go back to");
        // Saving it removes the copy.
        later.execute("file.save", &json!({ "path": "/work/Blinky" })).expect("save");
        assert_eq!(
            files.snapshot().keys().cloned().collect::<Vec<_>>(),
            [PathBuf::from("/work/Blinky/Blinky.ccpcb"), "/work/Blinky/Blinky.ccproj".into()]
        );
        assert!(!later.modified());
        // Unsaved work on a saved board says where it belongs, and saving removes it too.
        later.execute("net.add", &json!({ "name": "VCC" })).expect("net");
        let id = autosave(&mut later)["id"].as_str().expect("id").to_owned();
        let mut third = sharing(&files);
        // Where it belongs is compared as a path, and everything else exactly.
        let mut listed = recoverable(&mut third);
        let from = listed.first_mut().map(|only| only["from"].take());
        assert_eq!(listed, [json!({ "id": id, "name": "Blinky", "from": null, "problem": null })]);
        assert_eq!(from.as_ref().and_then(path), at("/work/Blinky/Blinky.ccpcb"));
        later.execute("file.save", &Value::Null).expect("save");
        assert!(recoverable(&mut third).is_empty());
        assert_eq!(files.snapshot().len(), 2);
    }

    #[test]
    fn a_recovered_board_goes_on_saying_where_it_belongs() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let from = |s: &mut Session| recoverable(s).iter().map(|e| path(&e["from"])).collect::<Vec<_>>();
        // Unsaved work on a board that has files, left by a session that ended.
        let mut first = sharing(&files);
        first.execute("file.save", &json!({ "path": "/work/Blinky" })).expect("save");
        first.execute("net.add", &json!({ "name": "GND" })).expect("net");
        first.execute("file.autosave", &Value::Null).expect("autosave");
        drop(first);
        let mut watching = sharing(&files);
        assert_eq!(from(&mut watching), [at("/work/Blinky/Blinky.ccpcb")]);
        // A second session recovers it, works on and ends as well. The recovered board had no
        // files, yet the copy it leaves still says where the board belongs.
        let mut second = sharing(&files);
        let recovered = second.execute("recovery.open", &Value::Null).expect("recover");
        assert_eq!(path(&recovered["from"]), at("/work/Blinky/Blinky.ccpcb"));
        second.execute("net.add", &json!({ "name": "MORE" })).expect("net");
        assert_eq!(second.execute("file.autosave", &Value::Null).expect("autosave")["written"], true);
        drop(second);
        assert_eq!(from(&mut watching), [at("/work/Blinky/Blinky.ccpcb")]);
    }

    #[test]
    fn recovery_takes_an_id_and_refuses_what_is_not_one() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let mut ids = Vec::new();
        for name in ["One", "Two"] {
            let mut s = sharing(&files);
            s.execute("board.new", &json!({ "name": name })).expect("new");
            s.execute("net.add", &json!({ "name": "GND" })).expect("net");
            ids.push(s.execute("file.autosave", &Value::Null).expect("autosave")["id"].as_str().expect("id").to_owned());
        }
        // A file that is no recovery file, one with a name this program doesn't give, and one of
        // another kind.
        let damaged = "0123456789abcdef0123456789abcdef";
        files.write(Path::new(&format!("/recovery/{damaged}.ccrecovery")), b"{").expect("write");
        files.write(Path::new("/recovery/README.ccrecovery"), b"hello").expect("write");
        files.write(Path::new("/recovery/notes.txt"), b"hello").expect("write");
        let mut s = sharing(&files);
        s.execute("net.add", &json!({ "name": "MINE" })).expect("net");
        let listed = recoverable(&mut s);
        assert_eq!(listed.len(), 3, "{listed:?}");
        let problem = listed.iter().find(|e| e["id"] == damaged).expect("the damaged one");
        assert_eq!((&problem["name"], problem["problem"].as_str().is_some_and(|p| p.contains("malformed"))), (&Value::Null, true));
        // With several to choose from, one must be named; nothing is opened by guessing.
        assert!(matches!(s.execute("recovery.open", &Value::Null), Err(EngineError::BadParams(_))));
        for bad in ["", "../work/x", "0123456789ABCDEF0123456789ABCDEF", "0123456789abcdef0123456789abcde", "README"] {
            assert!(matches!(s.execute("recovery.open", &json!({ "id": bad })), Err(EngineError::BadParams(_))), "{bad:?}");
            assert!(matches!(s.execute("recovery.discard", &json!({ "id": bad })), Err(EngineError::BadParams(_))), "{bad:?}");
        }
        assert!(matches!(s.execute("recovery.open", &json!({ "id": damaged })), Err(EngineError::Failed(_))));
        assert!(matches!(s.execute("recovery.open", &json!({ "id": "f".repeat(32) })), Err(EngineError::Failed(_))), "no such file");
        assert!(matches!(s.execute("recovery.discard", &json!({ "id": "f".repeat(32) })), Err(EngineError::Failed(_))));
        assert!(s.board().net_by_name("MINE").is_some() && s.doc().undo_label() == Some("Add Net"), "nothing was opened");
        // Discarding removes one; the board that is open stays, and undo brings it back after a
        // recovery.
        assert_eq!(s.execute("recovery.discard", &json!({ "id": damaged })).expect("discard"), json!({ "discarded": damaged }));
        let opened = s.execute("recovery.open", &json!({ "id": ids[1] })).expect("open");
        assert_eq!(opened["name"], "Two");
        let own = opened["id"].as_str().expect("the id of its own copy").to_owned();
        assert_eq!(recoverable(&mut s).iter().map(|e| e["name"].clone()).collect::<Vec<_>>(), [json!("One")], "its own copy is not listed");
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert!(s.board().net_by_name("MINE").is_some());
        // The file it was read from is gone: the recovered board keeps a copy of its own.
        s.execute("edit.redo", &Value::Null).expect("redo");
        assert!(matches!(s.execute("recovery.open", &json!({ "id": ids[1] })), Err(EngineError::Failed(_))));
        // Discarding that leaves the board unsaved without a copy: the next autosave writes a new
        // one, without waiting for another edit.
        s.execute("recovery.discard", &json!({ "id": own })).expect("discard");
        let again = s.execute("file.autosave", &Value::Null).expect("autosave");
        assert!(again["written"] == true && again["id"] != json!(own), "{again}");
        // Something in the way of a board's copy: the try fails, and the next goes elsewhere.
        let blocked = again["id"].as_str().expect("id").to_owned();
        files.remove(Path::new(&format!("/recovery/{blocked}.ccrecovery"))).expect("remove");
        files.create_folder(Path::new(&format!("/recovery/{blocked}.ccrecovery"))).expect("folder");
        assert!(matches!(s.execute("file.autosave", &Value::Null), Err(EngineError::Failed(_))));
        let elsewhere = s.execute("file.autosave", &Value::Null).expect("autosave");
        assert!(elsewhere["written"] == true && elsewhere["id"] != json!(blocked), "{elsewhere}");
    }

    /// The names of the boards in the recovery folder, whoever holds them.
    fn kept(files: &MemoryStorage) -> Vec<String> {
        let texts =
            files.snapshot().into_iter().filter(|(path, _)| path.starts_with("/recovery")).map(|(_, bytes)| String::from_utf8(bytes).expect("text"));
        let mut names: Vec<String> = texts.map(|text| native::peek_recovery(&text).expect("a recovery file").name).collect();
        names.sort();
        names
    }

    #[test]
    fn each_board_keeps_its_own_copy_so_none_replaces_another() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let autosave = |s: &mut Session| s.execute("file.autosave", &Value::Null).expect("autosave")["written"].clone();
        // A session ends with unsaved work on a board called Precious.
        let mut crashed = sharing(&files);
        crashed.execute("board.new", &json!({ "name": "Precious" })).expect("new");
        crashed.execute("net.add", &json!({ "name": "LOST" })).expect("net");
        assert_eq!(autosave(&mut crashed), true);
        drop(crashed);
        // The next session recovers it and undoes that: the board it is back on is untouched, so
        // nothing is written, and the recovered work is still in its copy.
        let mut s = sharing(&files);
        s.execute("recovery.open", &Value::Null).expect("recover");
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!((autosave(&mut s), kept(&files)), (json!(false), vec!["Precious".to_owned()]));
        // Work on the board it is back on gets a copy of its own.
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert_eq!((autosave(&mut s), kept(&files)), (json!(true), vec!["Precious".to_owned(), "Untitled".to_owned()]));
        // That edit ended the redo, so the session no longer holds Precious: it can be recovered again.
        assert_eq!(recoverable(&mut s).iter().map(|e| e["name"].clone()).collect::<Vec<_>>(), [json!("Precious")]);

        // Two boards with unsaved work in one session: saving one leaves the other's copy.
        let files = Arc::new(MemoryStorage::with_recovery());
        let mut s = sharing(&files);
        s.execute("board.new", &json!({ "name": "A" })).expect("new");
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        autosave(&mut s);
        s.execute("board.new", &json!({ "name": "B" })).expect("new");
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert_eq!((autosave(&mut s), kept(&files)), (json!(true), vec!["A".to_owned(), "B".to_owned()]));
        assert!(recoverable(&mut s).is_empty(), "both are this session's own");
        s.execute("file.save", &json!({ "path": "/work/B" })).expect("save");
        assert_eq!(kept(&files), ["A"]);
        // Back on A, saving it removes A's copy: nothing is left to offer at the next start.
        s.execute("edit.undo", &Value::Null).expect("undo the net");
        s.execute("edit.undo", &Value::Null).expect("undo the new board");
        s.execute("file.save", &json!({ "path": "/work/A" })).expect("save");
        assert!(kept(&files).is_empty());
    }

    #[test]
    fn a_board_that_is_as_its_files_are_has_no_copy() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let autosave = |s: &mut Session| s.execute("file.autosave", &Value::Null).expect("autosave")["written"].clone();
        let mut s = sharing(&files);
        // A new board that is made and undone, and a saved board that is edited and undone: there
        // is no unsaved work, so there is nothing to offer for recovery.
        s.execute("board.new", &Value::Null).expect("new");
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!((autosave(&mut s), kept(&files).len()), (json!(false), 0));
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        s.execute("file.save", &json!({ "path": "/work/Blinky" })).expect("save");
        s.execute("net.add", &json!({ "name": "VCC" })).expect("net");
        assert_eq!((autosave(&mut s), kept(&files).len()), (json!(true), 1));
        // Undone back to how it was saved, the copy that was made is removed.
        s.execute("edit.undo", &Value::Null).expect("undo");
        assert_eq!((autosave(&mut s), kept(&files).len()), (json!(false), 0));
        // Redone, the work is unsaved again and is kept again.
        s.execute("edit.redo", &Value::Null).expect("redo");
        assert_eq!((autosave(&mut s), kept(&files).len()), (json!(true), 1));
    }

    #[test]
    fn a_copy_another_session_removed_is_written_again() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let autosave = |s: &mut Session| s.execute("file.autosave", &Value::Null).expect("autosave")["written"].clone();
        let mut first = sharing(&files);
        first.execute("board.new", &json!({ "name": "Live" })).expect("new");
        first.execute("net.add", &json!({ "name": "GND" })).expect("net");
        autosave(&mut first);
        // A second session, running at the same time, sees that copy, recovers it and saves it,
        // which removes it. The first session's work is unsaved without a copy.
        let mut second = sharing(&files);
        second.execute("recovery.open", &Value::Null).expect("recover");
        second.execute("file.save", &json!({ "path": "/work/Elsewhere" })).expect("save");
        assert!(kept(&files).is_empty() && first.modified());
        // Its next autosave notices, without an edit in between.
        assert_eq!((autosave(&mut first), kept(&files)), (json!(true), vec!["Live".to_owned()]));
        assert_eq!(autosave(&mut first), false);
        // A session that recovers a live copy and goes on working never writes to the same file:
        // each keeps its own work, whichever autosaves last.
        let mut third = sharing(&files);
        third.execute("recovery.open", &Value::Null).expect("recover");
        first.execute("net.add", &json!({ "name": "FIRST" })).expect("net");
        autosave(&mut first);
        third.execute("net.add", &json!({ "name": "THIRD" })).expect("net");
        autosave(&mut third);
        assert_eq!(autosave(&mut first), false, "its copy is there and is its own");
        let texts: Vec<String> = files
            .snapshot()
            .into_iter()
            .filter(|(path, _)| path.starts_with("/recovery"))
            .map(|(_, bytes)| String::from_utf8(bytes).expect("text"))
            .collect();
        assert_eq!(texts.len(), 2);
        assert_eq!(texts.iter().filter(|t| t.contains("\"FIRST\"")).count(), 1);
        assert_eq!(texts.iter().filter(|t| t.contains("\"THIRD\"")).count(), 1);
    }

    #[test]
    fn the_recovery_folder_is_listed_up_to_a_limit() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let text = native::write_recovery(&Board::new("Left"), None).expect("write");
        for i in 0..=MAX_RECOVERABLE {
            files.write(Path::new(&format!("/recovery/{i:032x}.ccrecovery")), text.as_bytes()).expect("write");
        }
        let mut s = sharing(&files);
        assert_eq!(recoverable(&mut s).len(), MAX_RECOVERABLE);
    }

    #[test]
    fn what_a_request_says_is_not_sent_back_at_length() {
        let mut s = Session::in_memory();
        let long = "x".repeat(100_000);
        for (id, params) in [
            ("select.set", json!({ "ids": [long] })),
            ("settings.set", json!({ long.as_str(): 1 })),
            ("view.highlight", json!({ "net": long })),
            ("view.active", json!({ "layer": long })),
            ("file.open", json!({ "path": long })),
            ("file.detect", json!({ "path": long })),
            ("net.add", json!({ "name": long })),
        ] {
            let message = s.execute(id, &params).err().map(|e| e.to_string()).unwrap_or_default();
            assert!(!message.is_empty() && message.chars().count() < 600, "{id}: {} characters", message.chars().count());
        }
        assert!(s.execute(&long, &Value::Null).is_err_and(|e| e.to_string().len() < 600));
        // More ids than a board can have items are refused before any is looked up.
        let many: Vec<&str> = vec!["x"; MAX_ITEMS + 1];
        assert_eq!(s.execute("select.set", &json!({ "ids": many })), Err(EngineError::BadParams("`ids`: more ids than a board has items".into())));
    }

    #[test]
    fn a_save_removes_only_the_copy_of_the_board_that_was_saved() {
        let files = Arc::new(MemoryStorage::with_recovery());
        let mut s = sharing(&files);
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        s.execute("file.autosave", &Value::Null).expect("autosave");
        // Another board is opened and saved while the first one's unsaved work is in the copy.
        s.execute("board.new", &json!({ "name": "Other" })).expect("new");
        s.execute("file.save", &json!({ "path": "/work/Other" })).expect("save");
        assert_eq!(files.snapshot().keys().filter(|p| p.starts_with("/recovery")).count(), 1, "the first board's copy stays");
        // Back on the first board, saving it removes the copy.
        s.execute("edit.undo", &Value::Null).expect("undo");
        s.execute("file.save", &json!({ "path": "/work/First" })).expect("save");
        assert_eq!(files.snapshot().keys().filter(|p| p.starts_with("/recovery")).count(), 0);
    }

    #[test]
    fn without_a_recovery_folder_nothing_is_kept_and_nothing_is_listed() {
        let mut s = on_disk();
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert_eq!(s.execute("file.autosave", &Value::Null).expect("autosave"), json!({ "written": false, "id": null }));
        assert!(recoverable(&mut s).is_empty());
        assert!(matches!(s.execute("recovery.discard", &json!({ "id": "f".repeat(32) })), Err(EngineError::Failed(_))));
        assert!(matches!(s.execute("recovery.open", &Value::Null), Err(EngineError::Failed(_))));
    }

    #[test]
    fn file_detect_reads_only_regular_files() {
        let files = Arc::new(MemoryStorage::default());
        files.write(Path::new("/work/board.kicad_pcb"), b"(kicad_pcb (version 20240108))").expect("write");
        let mut s = sharing(&files);
        assert_eq!(s.execute("file.detect", &json!({ "path": "/work/board.kicad_pcb" })).expect("detect")["format"], "KiCad board");
        assert!(matches!(s.execute("file.detect", &json!({ "path": "/work" })), Err(EngineError::Failed(_))), "a folder");
        assert!(matches!(s.execute("file.detect", &json!({})), Err(EngineError::BadParams(_))));
    }

    #[test]
    fn bad_edit_params_are_errors_and_change_nothing() {
        let mut s = on_disk();
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
        let mut s = on_disk();
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

        let mut other = on_disk();
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
        let mut s = on_disk();
        s.execute("board.new", &json!({ "name": "Board A" })).expect("new A");
        s.execute("file.save", &json!({ "path": a.to_string_lossy() })).expect("save A");
        s.execute("board.new", &json!({ "name": "Board B" })).expect("new B");
        s.execute("file.save", &json!({ "path": b.to_string_lossy() })).expect("save B");
        assert_eq!(s.execute("edit.undo", &Value::Null).expect("undo")["undone"], "New Board");
        assert_eq!((s.board().name.as_str(), s.folder()), ("Board A", Some(a.as_path())), "A comes back with A's folder");
        s.execute("net.add", &json!({ "name": "GND" })).expect("edit A");
        s.execute("file.save", &json!({})).expect("save");
        let open = |path: &std::path::Path| {
            let mut check = on_disk();
            check.execute("file.open", &json!({ "path": path.to_string_lossy() })).expect("open");
            check.board().clone()
        };
        assert_eq!(open(&b).name, "Board B", "folder B still holds board B");
        assert!(open(&a).net_by_name("GND").is_some(), "A's edit went to folder A");
    }

    #[test]
    fn a_new_board_does_not_lose_unsaved_work() {
        let mut s = on_disk();
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
        let mut s = on_disk();
        s.execute("board.new", &json!({ "name": "First" })).expect("new");
        s.execute("file.save", &json!({ "path": x.to_string_lossy() })).expect("save");
        let mut t = on_disk();
        t.execute("board.new", &json!({ "name": "Second" })).expect("new");
        assert!(matches!(t.execute("file.save", &json!({ "path": x.to_string_lossy() })), Err(EngineError::Failed(_))));
        assert!(t.folder().is_none());
        t.execute("file.save", &json!({ "path": x.to_string_lossy(), "overwrite": true })).expect("overwrite when asked");
    }

    #[test]
    fn file_open_failures_change_nothing() {
        let mut s = on_disk();
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        assert!(matches!(s.execute("file.open", &json!({ "path": "/definitely/not/here" })), Err(EngineError::Failed(_))));
        assert!(matches!(s.execute("file.open", &json!({})), Err(EngineError::BadParams(_))));
        assert_eq!(s.board().nets.len(), 1);
        assert!(s.folder().is_none());
    }

    #[test]
    fn file_detect_missing_file_fails_cleanly() {
        let mut s = on_disk();
        let r = s.execute("file.detect", &json!({ "path": "/definitely/not/here.brd" }));
        assert!(matches!(r, Err(EngineError::Failed(_))));
    }
}
