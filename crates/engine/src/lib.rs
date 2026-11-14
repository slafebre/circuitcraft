//! Sessions and the command registry.
//!
//! **Everything is a command.** Every user-visible action is a [`CommandSpec`] in [`COMMANDS`] with
//! a JSON form. Menus, shortcuts, the CLI, the control channel and MCP all reach the same commands
//! through [`Session::execute`]; front ends never change the design any other way.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod commands;

pub use commands::COMMANDS;

use circuitcraft_io::native::ProjectPaths;
use circuitcraft_io::storage::{DiskStorage, MemoryStorage, Storage};
use circuitcraft_model::{Board, Document, ItemId, LayerId, LayerKind, ModelError, NetId};
use serde_json::Value;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Why a command did not run or did not finish.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EngineError {
    /// No command has this id.
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    /// The JSON parameters were missing, of the wrong type or out of range.
    #[error("bad parameters: {0}")]
    BadParams(String),
    /// The command ran and failed.
    #[error("{0}")]
    Failed(String),
    /// The command panicked; the design was restored to how it was before the command.
    #[error("internal error in `{0}`; the design was restored")]
    Panicked(String),
}

impl From<ModelError> for EngineError {
    /// A refused model edit means the request was wrong (bad size, unknown net, …).
    fn from(e: ModelError) -> Self {
        EngineError::BadParams(e.to_string())
    }
}

/// A command's JSON entry point.
pub type RunFn = fn(&mut Session, &Value) -> Result<Value, EngineError>;

/// One command: what it is called, where it appears, what it takes and how to run it.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    /// Stable id, lower case, dot-separated (`board.new`). Ours, not another product's.
    pub id: &'static str,
    /// Label for menus and the command palette.
    pub label: &'static str,
    /// Menu path (`"File/New Board"`), or `None` when the command has no menu item.
    pub menu: Option<&'static str>,
    /// Default shortcut, as space-separated keys (`"P T"` for a two-key sequence), or `None`.
    pub shortcut: Option<&'static str>,
    /// What the command does and what it returns, for people and agents.
    pub about: &'static str,
    /// Whether the command only reports: it changes neither the design, nor what the session
    /// holds besides it, nor any file. A front end can run one without asking and show what it
    /// returns.
    pub read_only: bool,
    /// Whether the command needs the desktop; it is refused in the browser, and says so.
    pub desktop_only: bool,
    /// The JSON Schema of the parameters, generated from the type the command reads them into.
    pub schema: fn() -> Value,
    /// The JSON entry point.
    pub run: RunFn,
}

impl CommandSpec {
    /// Puts the command in a menu.
    const fn menu(mut self, path: &'static str) -> CommandSpec {
        self.menu = Some(path);
        self
    }

    /// Gives the command a default shortcut.
    const fn shortcut(mut self, keys: &'static str) -> CommandSpec {
        self.shortcut = Some(keys);
        self
    }

    /// Marks the command as one that only reports.
    const fn read_only(mut self) -> CommandSpec {
        self.read_only = true;
        self
    }

    /// Why the command can't run here, if it can't: a desktop-only command in the browser.
    fn refused(&self, in_browser: bool) -> Option<EngineError> {
        (self.desktop_only && in_browser)
            .then(|| EngineError::Failed(format!("`{}` needs the desktop app; it is not available in the browser", self.id)))
    }
}

/// The longest error message a command gives, in characters. Messages quote what they refuse
/// (a name, a path, a parameter), and what is refused can be as long as the request.
const MAX_ERROR_CHARS: usize = 500;

/// The same error with its message cut to [`MAX_ERROR_CHARS`].
fn clipped(e: EngineError) -> EngineError {
    let clip = |m: String| if m.chars().count() > MAX_ERROR_CHARS { m.chars().take(MAX_ERROR_CHARS).chain(['…']).collect() } else { m };
    match e {
        EngineError::BadParams(m) => EngineError::BadParams(clip(m)),
        EngineError::Failed(m) => EngineError::Failed(clip(m)),
        other => other,
    }
}

/// Looks a command up by id.
pub fn command(id: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// How a board is looked at: what the renderer draws and what new items go on. It refers to
/// layers and nets by id; one that the board no longer has counts as not set
/// ([`Session::active_layer`], [`Session::highlight`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct View {
    /// The layer being worked on.
    active: Option<LayerId>,
    /// Layers that are not drawn.
    hidden: BTreeSet<LayerId>,
    /// Whether only the active layer is drawn.
    single_layer: bool,
    /// The net that is highlighted.
    highlight: Option<NetId>,
}

/// What the session keeps for a board besides the board itself.
#[derive(Debug, Default)]
struct BoardState {
    /// The files the board was opened from or last saved to.
    files: Option<ProjectPaths>,
    /// The board as its files hold it, or as empty as it was made; `None` when it was never
    /// either (a recovered board). The copy shares its items with the board, so it costs a
    /// pointer for each.
    saved: Option<Board>,
    /// Whether the board differed from `saved` at a revision, so that asking again costs nothing
    /// until the board changes.
    differs: Cell<Option<(u64, bool)>>,
    /// The copy of this board's unsaved work in the recovery folder, once there is one. Each
    /// board has its own, so keeping one board's work never replaces another's.
    recovery: Option<PathBuf>,
    /// The revision that copy holds.
    autosaved: Option<u64>,
    /// For a recovered board, the board file its recovery file said it belonged to, so that its
    /// own copy goes on saying so.
    came_from: Option<String>,
    /// The selected items. One that the board no longer has counts as not selected.
    selection: BTreeSet<ItemId>,
    view: View,
}

/// One open project and everything needed to run commands on it: its documents (today one
/// board), where they are stored, what is selected and how each is viewed.
#[derive(Debug)]
pub struct Session {
    /// The open board and its undo history.
    doc: Document,
    /// Where files are read and written.
    storage: Box<dyn Storage>,
    /// What is kept for each board, by [`Document::generation`], so undoing back into an earlier
    /// board brings back its files, its selection and its view too, and a save without a path
    /// goes to the current board's own files.
    boards: BTreeMap<u64, BoardState>,
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

impl Session {
    /// A session with a new, empty board, on the storage of the place it runs in: the disk, or
    /// memory in the browser.
    pub fn new() -> Session {
        if cfg!(target_arch = "wasm32") { Session::in_memory() } else { Session::with_storage(Box::new(DiskStorage::new())) }
    }

    /// A session with a new, empty board whose files are kept in memory.
    pub fn in_memory() -> Session {
        Session::with_storage(Box::new(MemoryStorage::default()))
    }

    /// A session with a new, empty board, reading and writing files in `storage`.
    pub fn with_storage(storage: Box<dyn Storage>) -> Session {
        let doc = Document::new(Board::new("Untitled"));
        let fresh = BoardState { saved: Some(doc.board().clone()), ..BoardState::default() };
        Session { boards: BTreeMap::from([(doc.generation(), fresh)]), doc, storage }
    }

    /// Where the session reads and writes files.
    pub fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    /// The board being edited.
    pub fn board(&self) -> &Board {
        self.doc.board()
    }

    /// The open document, with its undo history.
    pub fn doc(&self) -> &Document {
        &self.doc
    }

    fn state(&self) -> Option<&BoardState> {
        self.boards.get(&self.doc.generation())
    }

    fn state_mut(&mut self) -> &mut BoardState {
        self.boards.entry(self.doc.generation()).or_default()
    }

    /// The files the current board was opened from or last saved to, if any.
    pub fn target(&self) -> Option<&ProjectPaths> {
        self.state()?.files.as_ref()
    }

    /// The project folder the current board was opened from or last saved to, if any.
    pub fn folder(&self) -> Option<&Path> {
        self.target().map(ProjectPaths::folder)
    }

    /// Whether the board differs from its files, or from the empty board it was made as. Undoing
    /// back to how it was saved makes it unmodified again.
    pub fn modified(&self) -> bool {
        let Some(state) = self.state() else { return true };
        let revision = self.doc.revision();
        match state.differs.get() {
            Some((at, differs)) if at == revision => differs,
            _ => {
                // Items the two share are compared by pointer, so this costs little more than
                // walking the board, and only once for each change.
                let differs = state.saved.as_ref() != Some(self.board());
                state.differs.set(Some((revision, differs)));
                differs
            }
        }
    }

    /// The selected items that are on the board, in id order.
    pub fn selection(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.state().into_iter().flat_map(|s| &s.selection).copied().filter(|&id| self.board().has_item(id))
    }

    /// The layer being worked on: the one that was chosen, or the top copper layer (or else the
    /// first layer) when none was or the board no longer has it. `None` for a board without
    /// layers.
    pub fn active_layer(&self) -> Option<LayerId> {
        let stack = &self.board().stack;
        let chosen = self.state().and_then(|s| s.view.active).filter(|&id| stack.get(id).is_some());
        chosen.or_else(|| stack.copper().next().or(stack.layers.first()).map(|l| l.id))
    }

    /// Whether only the active layer is drawn.
    pub fn single_layer(&self) -> bool {
        self.state().is_some_and(|s| s.view.single_layer)
    }

    /// Whether a layer is drawn: it is on the board and not hidden, and in single-layer mode it
    /// is the active layer or the board outline, which is drawn with every layer.
    pub fn layer_visible(&self, id: LayerId) -> bool {
        let Some(layer) = self.board().stack.get(id) else { return false };
        let hidden = self.state().is_some_and(|s| s.view.hidden.contains(&id));
        let alone = self.single_layer() && self.active_layer() != Some(id) && layer.kind != LayerKind::Outline;
        !hidden && !alone
    }

    /// The highlighted net, if there is one and the board still has it.
    pub fn highlight(&self) -> Option<NetId> {
        self.state().and_then(|s| s.view.highlight).filter(|&net| self.board().nets.contains(net))
    }

    /// Replaces the board with a new or opened one, undoably, and starts keeping its files,
    /// selection and view. `saved` says whether the board is as its files are (or as empty as a
    /// new board is); a recovered board is not.
    fn replace_board(&mut self, label: &str, board: Board, files: Option<ProjectPaths>, saved: bool) {
        let state = BoardState { files, saved: saved.then(|| board.clone()), ..BoardState::default() };
        self.doc.replace(label, board);
        self.boards.insert(self.doc.generation(), state);
        self.forget_unreachable();
    }

    /// Records that the current board is now in `files` exactly as it is here.
    fn saved_to(&mut self, files: ProjectPaths) {
        let saved = self.board().clone();
        let state = self.state_mut();
        (state.files, state.saved) = (Some(files), Some(saved));
        state.differs.set(None);
        self.forget_unreachable();
        // The unsaved work is saved now: its copy in the recovery folder has done its job.
        self.drop_recovery_copy();
    }

    /// Removes the current board's copy in the recovery folder, if it has one. A copy that can't
    /// be removed is left for the person to discard: it is offered again, which loses nothing.
    fn drop_recovery_copy(&mut self) {
        if let Some(file) = self.state_mut().recovery.take()
            && let Err(e) = self.storage.remove(&file)
        {
            log::warn!("could not remove {}: {e}", file.display());
        }
    }

    /// Forgets what was kept for boards that undo and redo can no longer reach, so the map stays
    /// as small as the history.
    fn forget_unreachable(&mut self) {
        let live: BTreeSet<u64> = self.doc.generations().collect();
        self.boards.retain(|g, _| live.contains(g));
    }

    /// Runs a command with JSON parameters (`{}` or `null` for the defaults). Programmatic calls
    /// never open dialogs.
    ///
    /// Last-resort guard: a command that panics is caught, the board and its history are restored
    /// to how they were before the command, and [`EngineError::Panicked`] is returned. The
    /// document's guard reverses what the command changed instead of copying the board. What the
    /// session keeps besides the board is left alone: files, selections and views are keyed by
    /// generations, which are never reused, so what a failed command recorded belongs to no board
    /// (it is forgotten later) or says what really happened (files that were written). On wasm a
    /// panic aborts, so commands must not rely on this guard.
    pub fn execute(&mut self, id: &str, params: &Value) -> Result<Value, EngineError> {
        let spec = command(id).ok_or_else(|| EngineError::UnknownCommand(id.chars().take(MAX_ERROR_CHARS).collect()))?;
        self.run(spec, params, cfg!(target_arch = "wasm32"))
    }

    /// Runs `spec`, in the browser or not.
    fn run(&mut self, spec: &CommandSpec, params: &Value, in_browser: bool) -> Result<Value, EngineError> {
        if let Some(refused) = spec.refused(in_browser) {
            return Err(refused);
        }
        let id = spec.id;
        self.doc.open_guard();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (spec.run)(self, params)));
        match outcome {
            Ok(result) => {
                self.doc.close_guard();
                if let Err(e) = &result {
                    log::debug!("command `{id}` failed: {e}");
                }
                result.map_err(clipped)
            }
            Err(_) => {
                self.doc.rollback_guard();
                log::error!("command `{id}` panicked; the design was restored");
                Err(EngineError::Panicked(id.to_owned()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_are_unique_lowercase_and_documented() {
        let mut ids: Vec<_> = COMMANDS.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "duplicate command id");
        for c in COMMANDS {
            assert!(c.id.chars().all(|ch| ch.is_ascii_lowercase() || ch == '.' || ch == '_'), "{}", c.id);
            assert!(!c.label.is_empty() && c.about.ends_with('.'), "{} needs a label and a sentence about it", c.id);
        }
    }

    #[test]
    fn unknown_command_is_an_error() {
        let mut s = Session::in_memory();
        assert_eq!(s.execute("nope", &Value::Null), Err(EngineError::UnknownCommand("nope".into())));
    }

    #[test]
    fn every_command_runs_with_default_params_without_panicking() {
        for c in COMMANDS {
            let mut s = Session::in_memory();
            let r = s.execute(c.id, &json!({}));
            assert!(!matches!(r, Err(EngineError::Panicked(_))), "{} panicked", c.id);
        }
    }

    #[test]
    fn every_command_publishes_the_schema_of_what_it_accepts() {
        for c in COMMANDS {
            let schema = (c.schema)();
            assert_eq!((&schema["type"], &schema["additionalProperties"]), (&json!("object"), &json!(false)), "{}", c.id);
            assert!(schema.get("title").is_none() && schema.get("$schema").is_none(), "{} names its Rust type", c.id);
            let mut s = Session::in_memory();
            // A parameter the schema doesn't have is refused by name, and so is anything but an object.
            let unknown = s.execute(c.id, &json!({ "no_such_parameter": 1 }));
            assert!(matches!(&unknown, Err(EngineError::BadParams(m)) if m.contains("`no_such_parameter`")), "{}: {unknown:?}", c.id);
            for not_an_object in [json!([1]), json!("x"), json!(1)] {
                assert_eq!(s.execute(c.id, &not_an_object), Err(EngineError::BadParams("parameters are given as an object".into())), "{}", c.id);
            }
            // What the schema requires is required, and every parameter says what it is for.
            let required: Vec<&str> = schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let missing = s.execute(c.id, &json!({}));
            match required.first() {
                Some(first) => assert_eq!(missing, Err(EngineError::BadParams(format!("missing field `{first}`"))), "{}", c.id),
                None => assert!(!matches!(&missing, Err(EngineError::BadParams(m)) if m.contains("missing field")), "{}: {missing:?}", c.id),
            }
            for (name, property) in schema["properties"].as_object().into_iter().flatten() {
                let description = property["description"].as_str().unwrap_or_default();
                assert!(!description.is_empty() && !description.contains('\n'), "{}: `{name}` needs a description on one line", c.id);
            }
        }
        // The list of commands carries each schema and flag.
        let mut s = Session::in_memory();
        let listed = s.execute("commands", &Value::Null).expect("commands");
        let track = listed.as_array().and_then(|l| l.iter().find(|c| c["id"] == "track.add")).expect("track.add");
        assert_eq!(track["params"]["required"], json!(["layer", "from", "to"]));
        assert_eq!(track["params"]["properties"]["width"]["type"], json!(["number", "null"]));
        assert_eq!((&track["readOnly"], &track["desktopOnly"]), (&json!(false), &json!(false)));
        assert!(track["about"].as_str().is_some_and(|a| a.contains("Returns {id}")));
    }

    #[test]
    fn a_bad_parameter_is_named() {
        let mut s = Session::in_memory();
        let message = |s: &mut Session, id: &str, params: Value| match s.execute(id, &params) {
            Err(EngineError::BadParams(m)) => m,
            other => format!("{other:?}"),
        };
        assert_eq!(message(&mut s, "settings.set", json!({ "tentVias": "yes" })), "`tentVias`: invalid type: string \"yes\", expected a boolean");
        assert_eq!(message(&mut s, "via.add", json!({ "at": [1] })), "`at`: invalid length 1, expected an array of length 2");
        assert_eq!(message(&mut s, "via.add", json!({ "at": [1, "2"] })), "`at[1]`: invalid type: string \"2\", expected f64");
        assert!(
            message(&mut s, "settings.set", json!({ "trackwidth": 1 }))
                .starts_with("`trackwidth`: unknown field `trackwidth`, expected one of `maskExpansion`")
        );
        let far = message(&mut s, "via.add", json!({ "at": [1e30, 0] }));
        assert!(far.starts_with("`at`: ") && far.ends_with("nm is outside the ±10 m design range"), "{far}");
        // A parameter that may be left out may also be null.
        assert!(s.execute("via.add", &json!({ "at": [1, 2], "net": null, "drill": null })).is_ok());
    }

    #[test]
    fn a_desktop_only_command_is_refused_in_the_browser() {
        let about = *command("about").expect("about");
        assert_eq!((about.refused(true), about.refused(false)), (None, None));
        let needs_desktop = CommandSpec { desktop_only: true, ..about };
        assert_eq!(needs_desktop.refused(false), None);
        let refusal = EngineError::Failed("`about` needs the desktop app; it is not available in the browser".into());
        assert_eq!(needs_desktop.refused(true), Some(refusal.clone()));
        // And it is not run there.
        let mut s = Session::in_memory();
        assert_eq!(s.run(&needs_desktop, &Value::Null, true), Err(refusal));
        assert!(s.run(&needs_desktop, &Value::Null, false).is_ok() && s.run(&about, &Value::Null, true).is_ok());
    }

    #[test]
    fn a_session_can_be_handed_to_another_thread() {
        fn sendable<T: Send>() {}
        sendable::<Session>();
    }

    #[test]
    fn what_is_kept_for_boards_is_bounded_by_the_history() {
        let mut s = Session::in_memory();
        for _ in 0..(circuitcraft_model::UNDO_LIMIT + 100) {
            s.execute("board.new", &Value::Null).expect("new");
        }
        // The boards undo can reach, the open one, and the one the last command would have gone
        // back to had it failed: it was pruned while that command ran.
        assert_eq!(s.boards.len(), circuitcraft_model::UNDO_LIMIT + 2);
    }

    /// Everything a command could change: the board, its history, what the session keeps for it
    /// and every file.
    fn everything(s: &mut Session, files: &circuitcraft_io::storage::MemoryStorage) -> impl PartialEq + std::fmt::Debug + use<> {
        let project = s.execute("project.info", &Value::Null).expect("project.info");
        let selection: Vec<_> = s.selection().collect();
        // What the session keeps for each board, as it prints: its files, its recovery copy, its view.
        let kept = format!("{:?}", s.boards);
        (
            s.board().clone(),
            s.doc().undo_label().map(str::to_owned),
            s.doc().redo_label().map(str::to_owned),
            project,
            selection,
            files.snapshot(),
            kept,
        )
    }

    #[test]
    fn a_read_only_command_changes_nothing() {
        let read_only: Vec<&str> = COMMANDS.iter().filter(|c| c.read_only).map(|c| c.id).collect();
        assert_eq!(
            read_only,
            [
                "recovery.list",
                "select.list",
                "view.render",
                "project.info",
                "board.info",
                "board.query",
                "item.info",
                "file.detect",
                "commands",
                "about"
            ]
        );
        // A session with something of everything: a saved project, unsaved work that has been
        // autosaved, an undone edit, a selection, a view, and unsaved work left by another session.
        let files = std::sync::Arc::new(circuitcraft_io::storage::MemoryStorage::with_recovery());
        let mut other = Session::with_storage(Box::new(files.clone()));
        other.execute("net.add", &json!({ "name": "LOST" })).expect("net");
        other.execute("file.autosave", &Value::Null).expect("autosave");
        let mut s = Session::with_storage(Box::new(files.clone()));
        s.execute("net.add", &json!({ "name": "GND" })).expect("net");
        let track = s.execute("track.add", &json!({ "layer": "Top Copper", "from": [0, 0], "to": [1, 0], "net": "GND" })).expect("track");
        s.execute("file.save", &json!({ "path": "/work/Blinky" })).expect("save");
        s.execute("via.add", &json!({ "at": [1, 0] })).expect("via");
        s.execute("via.add", &json!({ "at": [2, 0] })).expect("via");
        s.execute("edit.undo", &Value::Null).expect("undo");
        s.execute("file.autosave", &Value::Null).expect("autosave");
        s.execute("select.set", &json!({ "ids": [track["id"]] })).expect("select");
        s.execute("view.highlight", &json!({ "net": "GND" })).expect("highlight");
        s.execute("view.layer", &json!({ "layer": "Top Paste", "visible": false })).expect("hide");
        let before = everything(&mut s, &files);
        for id in read_only {
            let params = match id {
                "file.detect" => json!({ "path": "/work/Blinky/Blinky.ccpcb" }),
                "item.info" => json!({ "id": track["id"] }),
                _ => json!({}),
            };
            let result = s.execute(id, &params);
            assert!(result.is_ok(), "{id}: {result:?}");
            assert_eq!(everything(&mut s, &files), before, "{id} changed something");
        }
        // The check has teeth: each of these changes one of the things compared.
        for (id, params) in [
            ("net.add", json!({ "name": "VCC" })),
            ("edit.undo", json!({})),
            ("select.set", json!({ "ids": [] })),
            ("view.single", json!({})),
            ("file.save", json!({})),
            ("recovery.open", json!({})),
        ] {
            let before = everything(&mut s, &files);
            s.execute(id, &params).expect(id);
            assert_ne!(everything(&mut s, &files), before, "{id} changed nothing");
        }
    }

    #[test]
    fn board_new_then_info() {
        let mut s = Session::in_memory();
        s.execute("board.new", &json!({"name": "Blinky"})).expect("board.new");
        let info = s.execute("board.info", &Value::Null).expect("board.info");
        assert_eq!(info["name"], "Blinky");
        assert_eq!(info["copperLayers"], 2);
        assert_eq!(info["thicknessMm"], 1.6);
    }

    #[test]
    fn bad_params_are_reported() {
        let mut s = Session::in_memory();
        assert!(matches!(s.execute("board.new", &json!({"name": 5})), Err(EngineError::BadParams(_))));
        assert!(matches!(s.execute("board.new", &json!({"name": ""})), Err(EngineError::BadParams(_))));
        assert!(matches!(s.execute("board.new", &json!([1])), Err(EngineError::BadParams(_))));
    }
}
