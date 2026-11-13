//! Sessions and the command registry.
//!
//! **Everything is a command.** Every user-visible action is a [`CommandSpec`] in [`COMMANDS`] with
//! a JSON form. Menus, shortcuts, the CLI, the control channel and MCP all reach the same commands
//! through [`Session::execute`]; front ends never change the design any other way.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod commands;

pub use commands::COMMANDS;

use circuitcraft_model::{Board, Document, ModelError};
use serde_json::Value;
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

/// One command: what it is called, where it appears and how to run it.
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
    /// The JSON parameters, documented for people and agents.
    pub params: &'static str,
    /// The JSON entry point.
    pub run: RunFn,
}

/// Looks a command up by id.
pub fn command(id: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.id == id)
}

/// One open design and everything needed to run commands on it.
#[derive(Debug, Clone)]
pub struct Session {
    /// The open board and its undo history.
    doc: Document,
    /// The project folder the board was opened from or last saved to.
    folder: Option<PathBuf>,
}

impl Default for Session {
    fn default() -> Self {
        Session::new()
    }
}

impl Session {
    /// A session with a new, empty board.
    pub fn new() -> Session {
        Session { doc: Document::new(Board::new("Untitled")), folder: None }
    }

    /// The project folder the board was opened from or last saved to, if any.
    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// The board being edited.
    pub fn board(&self) -> &Board {
        self.doc.board()
    }

    /// The open document, with its undo history.
    pub fn doc(&self) -> &Document {
        &self.doc
    }

    /// Runs a command with JSON parameters (`{}` or `null` for the defaults). Programmatic calls
    /// never open dialogs.
    ///
    /// Last-resort guard: a command that panics is caught, the design is restored to how it was
    /// before the command, and [`EngineError::Panicked`] is returned. On wasm a panic aborts, so
    /// commands must not rely on this guard.
    pub fn execute(&mut self, id: &str, params: &Value) -> Result<Value, EngineError> {
        let spec = command(id).ok_or_else(|| EngineError::UnknownCommand(id.to_owned()))?;
        let before = self.doc.checkpoint();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (spec.run)(self, params)));
        match outcome {
            Ok(result) => {
                if let Err(e) = &result {
                    log::debug!("command `{id}` failed: {e}");
                }
                result
            }
            Err(_) => {
                self.doc.restore(before);
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
            assert!(!c.label.is_empty() && !c.params.is_empty(), "{} needs a label and params doc", c.id);
        }
    }

    #[test]
    fn unknown_command_is_an_error() {
        let mut s = Session::new();
        assert_eq!(s.execute("nope", &Value::Null), Err(EngineError::UnknownCommand("nope".into())));
    }

    #[test]
    fn every_command_runs_with_default_params_without_panicking() {
        for c in COMMANDS {
            let mut s = Session::new();
            let r = s.execute(c.id, &json!({}));
            assert!(!matches!(r, Err(EngineError::Panicked(_))), "{} panicked", c.id);
        }
    }

    #[test]
    fn board_new_then_info() {
        let mut s = Session::new();
        s.execute("board.new", &json!({"name": "Blinky"})).expect("board.new");
        let info = s.execute("board.info", &Value::Null).expect("board.info");
        assert_eq!(info["name"], "Blinky");
        assert_eq!(info["copperLayers"], 2);
        assert_eq!(info["thicknessMm"], 1.6);
    }

    #[test]
    fn bad_params_are_reported() {
        let mut s = Session::new();
        assert!(matches!(s.execute("board.new", &json!({"name": 5})), Err(EngineError::BadParams(_))));
        assert!(matches!(s.execute("board.new", &json!({"name": ""})), Err(EngineError::BadParams(_))));
        assert!(matches!(s.execute("board.new", &json!([1])), Err(EngineError::BadParams(_))));
    }
}
