//! CircuitCraft's own files: a project folder with a `.ccproj` project file and a `.ccpcb` board.
//!
//! Both are JSON that starts with `format` and `version`, so a person, a script or
//! [`crate::detect`] can tell what a file is from its first line. Keys and items come out in a
//! fixed order (items sorted by id) and every item is on a line of its own (a net, a track, a pad,
//! a corner of a zone), so saving the same design twice gives byte-identical files, a change to
//! one item changes one line, and two branches that change different items merge.
//!
//! Reading treats the file as hostile: its size is capped before parsing, the parser's nesting
//! limit and the model's collection limits bound memory, an unknown format or a newer version is
//! refused with a clear message, and the board must pass [`Board::validate`]. A file must say
//! everything and nothing else: a field that is missing or that this build doesn't know is
//! refused, never guessed at or dropped on the next save.

use crate::storage::{Kind, Storage};
use circuitcraft_model::{Board, ModelError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The format version written by this build, and the newest it reads.
pub const FORMAT_VERSION: u32 = 1;

/// The `format` value of a board file.
pub const BOARD_FORMAT: &str = "circuitcraft.board";

/// The `format` value of a project file.
pub const PROJECT_FORMAT: &str = "circuitcraft.project";

/// The board file extension.
pub const BOARD_EXT: &str = "ccpcb";

/// The project file extension.
pub const PROJECT_EXT: &str = "ccproj";

/// The `format` value of a recovery file.
pub const RECOVERY_FORMAT: &str = "circuitcraft.recovery";

/// The recovery file extension.
pub const RECOVERY_EXT: &str = "ccrecovery";

/// The largest file read, in bytes. A board this size would hold millions of items.
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// A native file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// The file system refused.
    #[error("{path}: {source}")]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The file is larger than [`MAX_FILE_BYTES`].
    #[error("{0}: larger than {MAX_FILE_BYTES} bytes")]
    TooLarge(PathBuf),
    /// The text is not the expected kind of file.
    #[error("not a CircuitCraft {expected} file{found}")]
    WrongFormat {
        /// `board`, `project` or `recovery`.
        expected: &'static str,
        /// What the file says it is, if anything (`, it says "…"`).
        found: String,
    },
    /// The file was written by a newer CircuitCraft.
    #[error("written by a newer CircuitCraft (format version {0}; this build reads up to {FORMAT_VERSION})")]
    TooNew(u64),
    /// The JSON is malformed or doesn't match the format.
    #[error("malformed file: {0}")]
    Malformed(String),
    /// The board parsed but is inconsistent.
    #[error("invalid board: {0}")]
    Invalid(#[from] ModelError),
    /// A project folder has no project file, or more than one, or names a board it can't use.
    #[error("{0}")]
    Project(String),
    /// The path is not a regular file (a folder, a pipe or a device).
    #[error("{0}: not a regular file")]
    NotAFile(PathBuf),
    /// Saving would replace a file that belongs to another project.
    #[error("{0} already exists; save somewhere else, or pass overwrite to replace it")]
    WouldOverwrite(PathBuf),
}

/// What a board file looks like on disk.
#[derive(Serialize)]
struct BoardOut<'a> {
    format: &'static str,
    version: u32,
    board: &'a Board,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardIn {
    format: String,
    version: u64,
    board: Board,
}

/// A project: its name and the file names of its documents, relative to the project folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// The project's name.
    pub name: String,
    /// The board file, a plain file name in the project folder (`Blinky.ccpcb`).
    pub board: String,
}

#[derive(Serialize)]
struct ProjectOut<'a> {
    format: &'static str,
    version: u32,
    #[serde(flatten)]
    project: &'a Project,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectIn {
    format: String,
    version: u64,
    name: String,
    board: String,
}

/// A recovery file: a board with unsaved work, and where it came from.
#[derive(Serialize)]
struct RecoveryOut<'a> {
    format: &'static str,
    version: u32,
    from: Option<&'a str>,
    board: &'a Board,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryIn {
    format: String,
    version: u64,
    #[serde(deserialize_with = "written")]
    from: Option<String>,
    board: Board,
}

/// A field that may be `null` but must be written.
fn written<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

/// What a list of recovery files shows of one, read without building its board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryHeader {
    /// The board's name.
    pub name: String,
    /// The board file the board was last opened from or saved to, as text for people to read;
    /// `None` for a board that was never saved.
    pub from: Option<String>,
}

#[derive(Deserialize)]
struct RecoveryPeek {
    format: String,
    version: u64,
    #[serde(deserialize_with = "written")]
    from: Option<String>,
    board: BoardPeek,
}

/// The name of a board; the rest of it is skipped as it is read.
#[derive(Deserialize)]
struct BoardPeek {
    name: String,
}

/// Just the header, read when a full parse fails, to say why in the user's terms. It skips
/// everything else, so it also reads a file from a newer build.
#[derive(Deserialize)]
struct Header {
    format: Option<String>,
    version: Option<u64>,
}

/// The JSON of a file, laid out by [`Layout`], with a final newline.
fn pretty<T: Serialize>(v: &T) -> Result<String, NativeError> {
    let compact = serde_json::to_string(v).map_err(|e| NativeError::Malformed(e.to_string()))?;
    let mut layout = Layout { text: &compact, at: 0, out: String::with_capacity(compact.len().saturating_add(compact.len() / 4)) };
    layout.value(0)?;
    if layout.at != compact.len() {
        return Err(layout.broken());
    }
    layout.out.push('\n');
    Ok(layout.out)
}

/// How deep the lists and objects of a file may nest. The model nests about a dozen deep.
const MAX_NESTING: usize = 64;

/// Lays compact JSON out so that every item of the design is one line of the file.
///
/// A list or an object is written on one line when it holds no list of lists or objects: a net, a
/// track, a pad, a point. Anything larger is opened up, one member to a line, and its members are
/// laid out the same way: a footprint's fields each get a line, and so does each of its pads. The
/// file itself, the board in it and each of the board's own fields are always opened up, so each
/// design setting is a line too.
///
/// The text comes from `serde_json`, so it is well formed; anything else is an error, not a panic.
struct Layout<'a> {
    text: &'a str,
    /// The byte the next value starts at.
    at: usize,
    out: String,
}

impl Layout<'_> {
    fn broken(&self) -> NativeError {
        NativeError::Malformed(format!("could not lay out the file at byte {}", self.at))
    }

    fn byte(&self, at: usize) -> Option<u8> {
        self.text.as_bytes().get(at).copied()
    }

    /// Copies the text up to `end` and moves there.
    fn copy_to(&mut self, end: usize) -> Result<(), NativeError> {
        let part = self.text.get(self.at..end).ok_or_else(|| self.broken())?;
        self.out.push_str(part);
        self.at = end;
        Ok(())
    }

    fn new_line(&mut self, depth: usize) {
        self.out.push('\n');
        self.out.extend(std::iter::repeat_n("  ", depth));
    }

    /// The byte after the string that starts at `at`.
    fn string_end(&self, at: usize) -> Option<usize> {
        let mut i = at.checked_add(1)?;
        loop {
            match self.byte(i)? {
                b'"' => return i.checked_add(1),
                b'\\' => i = i.checked_add(2)?,
                _ => i = i.checked_add(1)?,
            }
        }
    }

    /// For the list or object that starts at `at`: the byte after its end, and whether it holds a
    /// list whose elements are lists or objects.
    fn span(&self, at: usize) -> Option<(usize, bool)> {
        // Whether each list or object that is open is a list.
        let mut open: Vec<bool> = Vec::new();
        let mut nested = false;
        let mut i = at;
        loop {
            match self.byte(i)? {
                b'"' => {
                    i = self.string_end(i)?;
                    continue;
                }
                b @ (b'[' | b'{') => {
                    nested |= open.last() == Some(&true);
                    if open.len() >= MAX_NESTING {
                        return None;
                    }
                    open.push(b == b'[');
                }
                b']' | b'}' => {
                    open.pop()?;
                    if open.is_empty() {
                        return i.checked_add(1).map(|end| (end, nested));
                    }
                }
                _ => {}
            }
            i = i.checked_add(1)?;
        }
    }

    /// Writes the value that starts at `self.at`, `depth` levels inside the file.
    fn value(&mut self, depth: usize) -> Result<(), NativeError> {
        let Some(first @ (b'[' | b'{')) = self.byte(self.at) else {
            // A string, a number, true, false or null: up to the next comma or closing bracket.
            let mut end = self.at;
            while let Some(b) = self.byte(end).filter(|b| !matches!(b, b',' | b']' | b'}')) {
                end = if b == b'"' { self.string_end(end).ok_or_else(|| self.broken())? } else { end + 1 };
            }
            return if end > self.at { self.copy_to(end) } else { Err(self.broken()) };
        };
        let (end, nested) = self.span(self.at).ok_or_else(|| self.broken())?;
        if end == self.at + 2 || (depth > 2 && !nested) {
            return self.copy_to(end);
        }
        self.copy_to(self.at + 1)?;
        loop {
            self.new_line(depth + 1);
            if first == b'{' {
                let colon = self.string_end(self.at).filter(|&colon| self.byte(colon) == Some(b':')).ok_or_else(|| self.broken())?;
                self.copy_to(colon + 1)?;
                self.out.push(' ');
            }
            self.value(depth + 1)?;
            match self.byte(self.at) {
                Some(b',') => self.copy_to(self.at + 1)?,
                Some(b']' | b'}') => break,
                _ => return Err(self.broken()),
            }
        }
        self.new_line(depth);
        self.copy_to(self.at + 1)
    }
}

/// A board as `.ccpcb` text.
pub fn write_board(board: &Board) -> Result<String, NativeError> {
    pretty(&BoardOut { format: BOARD_FORMAT, version: FORMAT_VERSION, board })
}

/// A board from `.ccpcb` text, checked with [`Board::validate`].
pub fn read_board(text: &str) -> Result<Board, NativeError> {
    let file: BoardIn = parse(text, BOARD_FORMAT, "board")?;
    check_header(&file.format, file.version, BOARD_FORMAT, "board")?;
    file.board.validate()?;
    Ok(file.board)
}

/// A board with unsaved work as `.ccrecovery` text. `from` is the board file it belongs to, for
/// people to read: nothing is ever written there because a recovery file says so.
pub fn write_recovery(board: &Board, from: Option<&Path>) -> Result<String, NativeError> {
    let from = from.map(|f| f.to_string_lossy());
    pretty(&RecoveryOut { format: RECOVERY_FORMAT, version: FORMAT_VERSION, from: from.as_deref(), board })
}

/// The board in a recovery file, checked with [`Board::validate`], and where it came from.
pub fn read_recovery(text: &str) -> Result<(Board, Option<String>), NativeError> {
    let file: RecoveryIn = parse(text, RECOVERY_FORMAT, "recovery")?;
    check_header(&file.format, file.version, RECOVERY_FORMAT, "recovery")?;
    file.board.validate()?;
    Ok((file.board, file.from.map(|from| clip(&from))))
}

/// The longest `from` passed on, in characters: far longer than a path, far shorter than a file.
const MAX_FROM_CHARS: usize = 4_096;

/// Text from a file, cut to a length that is safe to show and to send back.
fn clip(text: &str) -> String {
    text.chars().take(MAX_FROM_CHARS).collect()
}

/// What a recovery file says it holds, without reading its board in full or checking it.
pub fn peek_recovery(text: &str) -> Result<RecoveryHeader, NativeError> {
    let file: RecoveryPeek = parse(text, RECOVERY_FORMAT, "recovery")?;
    check_header(&file.format, file.version, RECOVERY_FORMAT, "recovery")?;
    Ok(RecoveryHeader { name: file.board.name.chars().take(circuitcraft_model::MAX_NAME_CHARS).collect(), from: file.from.map(|from| clip(&from)) })
}

/// A project as `.ccproj` text.
pub fn write_project(project: &Project) -> Result<String, NativeError> {
    pretty(&ProjectOut { format: PROJECT_FORMAT, version: FORMAT_VERSION, project })
}

/// A project from `.ccproj` text. The board it names must be a plain `.ccpcb` file name in the
/// project folder, so a project file can't point anywhere else on the disk.
pub fn read_project(text: &str) -> Result<Project, NativeError> {
    let file: ProjectIn = parse(text, PROJECT_FORMAT, "project")?;
    check_header(&file.format, file.version, PROJECT_FORMAT, "project")?;
    check_board_name(&file.board)?;
    if file.name.trim().is_empty() || file.name.chars().count() > circuitcraft_model::MAX_NAME_CHARS {
        return Err(NativeError::Project("the project name is empty or too long".to_owned()));
    }
    Ok(Project { name: file.name, board: file.board })
}

/// Parses a whole file; on failure, explains a wrong format or a newer version before falling
/// back to the parser's message.
fn parse<T: for<'de> Deserialize<'de>>(text: &str, format: &str, expected: &'static str) -> Result<T, NativeError> {
    // Some editors start a UTF-8 file with a byte-order mark; detect() accepts one, so do we.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str::<T>(text).map_err(|e| match serde_json::from_str::<Header>(text) {
        Ok(h) => match check_header(h.format.as_deref().unwrap_or_default(), h.version.unwrap_or(0), format, expected) {
            Err(why) => why,
            Ok(()) => NativeError::Malformed(e.to_string()),
        },
        Err(_) => NativeError::Malformed(e.to_string()),
    })
}

fn check_header(found: &str, version: u64, format: &str, expected: &'static str) -> Result<(), NativeError> {
    if found != format {
        let found = if found.is_empty() { String::new() } else { format!(", it says {:?}", found.chars().take(64).collect::<String>()) };
        return Err(NativeError::WrongFormat { expected, found });
    }
    if version > u64::from(FORMAT_VERSION) {
        return Err(NativeError::TooNew(version));
    }
    if version == 0 {
        return Err(NativeError::Malformed("format version 0 doesn't exist".to_owned()));
    }
    Ok(())
}

fn check_board_name(name: &str) -> Result<(), NativeError> {
    let plain = Path::new(name).file_name().is_some_and(|f| f == name) && !name.contains(['/', '\\', ':']);
    let ccpcb = Path::new(name).extension().is_some_and(|e| e == BOARD_EXT);
    if plain && ccpcb && name.len() <= 255 {
        Ok(())
    } else {
        Err(NativeError::Project(format!(
            "{:?} can't be a board file: it must be a plain .{BOARD_EXT} file name, without / \\ or :, in the project folder",
            name.chars().take(64).collect::<String>()
        )))
    }
}

/// Reads a whole regular file as text, refusing one larger than [`MAX_FILE_BYTES`] without
/// reading it all. Anything else (a folder, a pipe, a device such as `/dev/stdin`) is refused
/// before it is opened, since opening or reading one can block forever.
pub fn read_text(storage: &dyn Storage, path: &Path) -> Result<String, NativeError> {
    let io = |source| NativeError::Io { path: path.to_owned(), source };
    match storage.kind(path).map_err(io)? {
        Kind::File(len) if len > MAX_FILE_BYTES => return Err(NativeError::TooLarge(path.to_owned())),
        Kind::File(_) => {}
        Kind::Folder | Kind::Other => return Err(NativeError::NotAFile(path.to_owned())),
    }
    // One byte more than is accepted, so a file that grew since it was measured is still caught.
    let bytes = storage.read(path, MAX_FILE_BYTES + 1).map_err(io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(NativeError::TooLarge(path.to_owned()));
    }
    String::from_utf8(bytes).map_err(|_| NativeError::Malformed(format!("{}: not UTF-8 text", path.display())))
}

fn write_text(storage: &dyn Storage, path: &Path, text: &str) -> Result<(), NativeError> {
    storage.write(path, text.as_bytes()).map_err(|source| NativeError::Io { path: path.to_owned(), source })
}

/// Where an open document's files are, so that saving writes back to exactly those files. Only
/// [`open`] and [`save_project`] make one, so the board file name is always a valid one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    project: Option<PathBuf>,
    board: PathBuf,
}

impl ProjectPaths {
    /// The project folder (the folder the board file is in).
    pub fn folder(&self) -> &Path {
        self.board.parent().unwrap_or(Path::new(""))
    }

    /// The project file, or `None` for a board opened on its own.
    pub fn project(&self) -> Option<&Path> {
        self.project.as_deref()
    }

    /// The board file.
    pub fn board(&self) -> &Path {
        &self.board
    }
}

/// An absolute version of `path` (without resolving links), so a project opened by a relative
/// path still knows its folder.
fn absolute(storage: &dyn Storage, path: &Path) -> Result<PathBuf, NativeError> {
    storage.absolute(path).map_err(|source| NativeError::Io { path: path.to_owned(), source })
}

/// The file name of `path`, if it has one that is valid UTF-8. Strict on purpose: a lossy
/// conversion would write a board name into the project file that doesn't match the real file.
fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|n| n.to_str())
}

/// Saves a board into a project folder (Save As), creating the folder if needed:
///
/// - A folder without a project gets `<folder name>.ccproj` and `<folder name>.ccpcb`.
/// - A folder that holds one project is refused unless `overwrite` is set; then the board is
///   saved into that project's files, whatever they are called, so the folder still holds one
///   project.
/// - A folder with several projects is always refused.
///
/// Saving back to the files a board came from goes through [`save`] instead.
pub fn save_project(storage: &dyn Storage, folder: &Path, board: &Board, overwrite: bool) -> Result<ProjectPaths, NativeError> {
    let folder = absolute(storage, folder)?;
    let project = match project_files(storage, &folder)?.as_slice() {
        [] => {
            let stem = file_name(&folder)
                .filter(|n| !n.trim().is_empty())
                .ok_or_else(|| NativeError::Project(format!("{}: a project folder needs a name in UTF-8", folder.display())))?;
            folder.join(format!("{stem}.{PROJECT_EXT}"))
        }
        [one] if overwrite => one.clone(),
        [one] => return Err(NativeError::WouldOverwrite(one.clone())),
        many => return Err(NativeError::Project(format!("{}: {} project files in this folder; save somewhere else", folder.display(), many.len()))),
    };
    let paths = ProjectPaths { board: project.with_extension(BOARD_EXT), project: Some(project) };
    let board_name = file_name(&paths.board).ok_or_else(|| NativeError::Project(format!("{}: not a UTF-8 file name", paths.board.display())))?;
    check_board_name(board_name)?;
    if !overwrite && storage.kind(&paths.board).is_ok() {
        return Err(NativeError::WouldOverwrite(paths.board));
    }
    storage.create_folder(&folder).map_err(|source| NativeError::Io { path: folder.clone(), source })?;
    save(storage, &paths, board)?;
    Ok(paths)
}

/// Saves a board back to the files it was opened from or last saved to: the board file, and the
/// project file when there is one.
pub fn save(storage: &dyn Storage, paths: &ProjectPaths, board: &Board) -> Result<(), NativeError> {
    // Both texts first, so nothing is written if either can't be made.
    let project = match &paths.project {
        Some(file) => {
            // Always a UTF-8 name: open() took it from the project file, save_project() checked it.
            let board_name =
                file_name(&paths.board).ok_or_else(|| NativeError::Project(format!("{}: not a UTF-8 file name", paths.board.display())))?;
            Some((file, write_project(&Project { name: board.name.clone(), board: board_name.to_owned() })?))
        }
        None => None,
    };
    // The board first: a project file never names a board that isn't there.
    write_text(storage, &paths.board, &write_board(board)?)?;
    if let Some((file, text)) = project {
        write_text(storage, file, &text)?;
    }
    Ok(())
}

/// Opens a project folder, a `.ccproj` project file or a `.ccpcb` board file, and returns its
/// board and the files it came from.
pub fn open(storage: &dyn Storage, path: &Path) -> Result<(Board, ProjectPaths), NativeError> {
    let path = absolute(storage, path)?;
    let kind = storage.kind(&path).map_err(|source| NativeError::Io { path: path.clone(), source })?;
    let project_file = if kind == Kind::Folder {
        find_project_file(storage, &path)?
    } else if path.extension().is_some_and(|e| e == BOARD_EXT) {
        let board = read_board(&read_text(storage, &path)?)?;
        return Ok((board, ProjectPaths { project: None, board: path }));
    } else {
        path
    };
    let project = read_project(&read_text(storage, &project_file)?)?;
    let board_file = project_file.with_file_name(&project.board);
    let board = read_board(&read_text(storage, &board_file)?)?;
    Ok((board, ProjectPaths { project: Some(project_file), board: board_file }))
}

/// The project files in a folder: regular `.ccproj` files whose names don't start with a dot, so
/// hidden files (such as the `._` files macOS leaves on other file systems) are ignored. A folder
/// that doesn't exist has none.
fn project_files(storage: &dyn Storage, folder: &Path) -> Result<Vec<PathBuf>, NativeError> {
    let files = storage.files(folder).map_err(|source| NativeError::Io { path: folder.to_owned(), source })?;
    // Raw bytes, so a file whose name isn't UTF-8 still counts (and is then refused by name).
    let hidden = |path: &PathBuf| path.file_name().is_none_or(|n| n.as_encoded_bytes().starts_with(b"."));
    Ok(files.into_iter().filter(|path| !hidden(path) && path.extension().is_some_and(|e| e == PROJECT_EXT)).collect())
}

/// The one project file in a folder.
fn find_project_file(storage: &dyn Storage, folder: &Path) -> Result<PathBuf, NativeError> {
    match project_files(storage, folder)?.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(NativeError::Project(format!("{}: no .{PROJECT_EXT} project file in this folder", folder.display()))),
        many => Err(NativeError::Project(format!("{}: {} project files in this folder; open one of them", folder.display(), many.len()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{DiskStorage, MemoryStorage};

    /// The disk, for the tests that are about real files.
    static DISK: DiskStorage = DiskStorage::with_recovery(None);
    use circuitcraft_geom::{Contour, Polygon, Rect};
    use circuitcraft_model::layer_ids::{TOP_COPPER, TOP_FABRICATION, TOP_SILK};
    use circuitcraft_model::{
        Assembly, DesignSettings, Footprint, FootprintText, Hole, ItemId, Justify, Keepout, LayerId, LibraryLink, Mounting, Opening, Pad,
        PadConnection, PadShape, Rotation, Side, Slot, TextContent, TrackPath, Zone, ZoneFill,
    };
    use circuitcraft_units::{Nm, Point};

    fn at(x: i64, y: i64) -> Point {
        Point::new(Nm(x), Nm(y))
    }

    fn square(x: i64, y: i64, side: i64) -> Contour {
        Contour::rect(Rect::from_corners(at(x, y), at(x + side, y + side)))
    }

    fn blinky() -> Board {
        let mut b = Board::new("Blinky");
        let gnd = b.add_net("GND").expect("net");
        let path = TrackPath::Line { from: Point::new(Nm(0), Nm(0)), to: Point::new(Nm(1_000_000), Nm(0)) };
        b.add_track(TOP_COPPER, Nm(250_000), path, Some(gnd)).expect("track");
        let via = b.add_via(Point::new(Nm(1_000_000), Nm(0)), Nm(600_000), Nm(300_000), Some(gnd)).expect("via");
        if let Some(via) = b.vias.get_mut(via) {
            via.top_mask = Opening::Closed;
            via.bottom_mask = Opening::Open { by: None };
        }
        // A connector: a shell pin around a plated slot, a mounting hole and a surface-mount pad.
        let slot = Hole { slot: Some(Slot { length: Nm(1_700_000), rotation: Rotation(90_000) }), ..Hole::round(Nm(600_000), true) };
        let mut shell = Pad::through_hole("S1", Point::ORIGIN, PadShape::Oval { width: Nm(1_000_000), height: Nm(2_100_000) }, slot);
        shell.net = Some(gnd);
        let mounting = Pad::bare_hole(Point::new(Nm(4_000_000), Nm(0)), Hole::round(Nm(650_000), false));
        let mut smd = Pad::smd("A1", Point::new(Nm(2_000_000), Nm(0)), PadShape::Rect { width: Nm(300_000), height: Nm(1_000_000) });
        if let Some(front) = &mut smd.front {
            front.paste = Opening::Open { by: Some(Nm(-25_000)) };
        }
        // Its designator on the silkscreen, its value hidden and mirrored on the fabrication
        // drawing, and a label.
        let text = |content, layer, visible| FootprintText {
            id: b.fresh_id().expect("id"),
            content,
            visible,
            layer,
            position: Point::new(Nm(0), Nm(3_000_000)),
            height: Nm(1_000_000),
            stroke: Nm(150_000),
            rotation: Rotation(0),
            mirrored: !visible,
            justify: Justify::Left,
        };
        let texts = vec![
            text(TextContent::Reference, TOP_SILK, true),
            text(TextContent::Value, TOP_FABRICATION, false),
            text(TextContent::Fixed("A1".to_owned()), TOP_SILK, true),
        ];
        let connector = Footprint {
            id: b.fresh_id().expect("id"),
            reference: "J1".to_owned(),
            value: "USB-C".to_owned(),
            link: Some(LibraryLink { library: "Connectors".to_owned(), footprint: "USB-C-16P".to_owned() }),
            assembly: Assembly { mounting: Mounting::SurfaceMount, in_bom: true, in_placement: true, fitted: false },
            position: Point::new(Nm(5_000_000), Nm(5_000_000)),
            rotation: Rotation(0),
            side: Side::Top,
            pads: vec![shell, mounting, smd],
            graphics: Vec::new(),
            texts,
        };
        b.footprints.insert(connector).expect("footprint");
        // An outline with a round cutout, a hole outside any footprint, settings of its own, a
        // hatched ground zone with a hole that has been poured, and a via keepout.
        b.outline = Some(square(0, 0, 30_000_000));
        b.cutouts = vec![Contour::circle(at(15_000_000, 15_000_000), Nm(2_000_000)).expect("circle")];
        b.add_hole(at(3_000_000, 3_000_000), Hole::round(Nm(3_200_000), false)).expect("hole");
        b.design = DesignSettings { tent_vias: false, origin: at(0, 30_000_000), ..DesignSettings::default() };
        let ring = |x: i64, side: i64| vec![at(x, x), at(x + side, x), at(x + side, x + side), at(x, x + side)];
        let zone = Zone {
            id: b.fresh_id().expect("id"),
            layer: TOP_COPPER,
            net: Some(gnd),
            outline: square(0, 0, 30_000_000),
            holes: vec![square(20_000_000, 20_000_000, 5_000_000)],
            clearance: Nm(200_000),
            min_width: Nm(150_000),
            priority: 1,
            fill: ZoneFill::Hatched { width: Nm(200_000), gap: Nm(600_000) },
            pads: PadConnection::Relief { gap: Nm(250_000), spoke: Nm(300_000) },
            keep_islands: false,
            filled: vec![Polygon { outer: ring(200_000, 29_600_000), holes: vec![ring(19_800_000, 5_400_000)] }],
        };
        b.zones.insert(zone).expect("zone");
        let keepout = Keepout {
            id: b.fresh_id().expect("id"),
            layer: TOP_COPPER,
            outline: square(25_000_000, 0, 5_000_000),
            tracks: false,
            vias: true,
            zones: false,
        };
        b.keepouts.insert(keepout).expect("keepout");
        assert_eq!(b.validate(), Ok(()));
        b
    }

    /// A folder of our own under the system temp folder, removed when dropped.
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

    #[test]
    fn board_round_trip_is_byte_identical() {
        let b = blinky();
        let text = write_board(&b).expect("write");
        assert!(text.starts_with("{\n  \"format\": \"circuitcraft.board\",\n  \"version\": 1,\n"), "{text}");
        assert!(text.ends_with("}\n"));
        let back = read_board(&text).expect("read");
        assert_eq!(back, b);
        assert_eq!(write_board(&back).expect("write"), text);
    }

    #[test]
    fn wrong_format_and_newer_versions_are_explained() {
        let text = write_board(&blinky()).expect("write");
        let newer = text.replacen("\"version\": 1", "\"version\": 2", 1);
        assert!(matches!(read_board(&newer), Err(NativeError::TooNew(2))));
        let project = text.replacen(BOARD_FORMAT, PROJECT_FORMAT, 1);
        assert!(matches!(read_board(&project), Err(NativeError::WrongFormat { expected: "board", .. })));
        // A newer file whose board no longer parses still gets the newer-version message.
        let changed = newer.replacen("\"tracks\"", "\"tracks_v2\"", 1);
        assert!(matches!(read_board(&changed), Err(NativeError::TooNew(2))));
        assert!(matches!(read_board("{\"format\": \"something.else\"}"), Err(NativeError::WrongFormat { .. })));
        assert!(matches!(read_board(&text.replacen("\"version\": 1", "\"version\": 0", 1)), Err(NativeError::Malformed(_))));
    }

    #[test]
    fn inconsistent_boards_are_refused() {
        let b = blinky();
        let mut dangling = b.clone();
        let gnd = b.nets.iter().next().map(|n| n.id).expect("net");
        dangling.nets.remove(gnd);
        let text = write_board(&dangling).expect("write");
        assert!(matches!(read_board(&text), Err(NativeError::Invalid(_))));
        // A layer paired with itself: flipping would go nowhere.
        let mut own_pair = b.clone();
        if let Some(l) = own_pair.stack.layers.iter_mut().find(|l| l.id == TOP_SILK) {
            l.pair = Some(TOP_SILK);
        }
        let text = write_board(&own_pair).expect("write");
        assert!(matches!(read_board(&text), Err(NativeError::Invalid(ModelError::BadStack(..)))));
        // A footprint text on a layer the board doesn't have.
        let mut no_layer = b.clone();
        let connector = no_layer.footprints.iter().next().map(|f| f.id).expect("footprint");
        if let Some(t) = no_layer.footprints.get_mut(connector).and_then(|f| f.texts.first_mut()) {
            t.layer = LayerId(999);
        }
        let text = write_board(&no_layer).expect("write");
        assert!(matches!(read_board(&text), Err(NativeError::Invalid(ModelError::BadLayer("text", "existing")))));
    }

    #[test]
    fn a_file_says_everything_under_its_own_name_and_nothing_else() {
        let good = write_board(&blinky()).expect("write");
        // A field under another name or a word this build doesn't know: refused, not filled in
        // with a guess.
        for (from, to) in [
            ("\"texts\"", "\"labels\""),
            ("\"assembly\"", "\"assembly_v0\""),
            ("\"in_bom\"", "\"bom\""),
            ("\"in_placement\"", "\"placed\""),
            ("\"fitted\"", "\"populated\""),
            ("\"visible\"", "\"shown\""),
            ("\"mirrored\"", "\"mirror\""),
            ("\"content\":\"reference\"", "\"content\":\"designator\""),
            ("\"content\":\"value\"", "\"content\":\"val\""),
            ("\"fixed\":\"A1\"", "\"words\":\"A1\""),
            ("\"mounting\":\"surface_mount\"", "\"mounting\":\"glued\""),
            ("\"library\":\"Connectors\"", "\"lib\":\"Connectors\""),
            ("\"footprint\":\"USB-C-16P\"", "\"name\":\"USB-C-16P\""),
            ("\"design\"", "\"settings\""),
            ("\"tent_vias\"", "\"tented\""),
            ("\"keepouts\"", "\"rule_areas\""),
            ("\"holes\": [\n      {", "\"drills\": [\n      {"),
            ("\"min_width\"", "\"minimum\""),
            ("\"fill\": {\"hatched\"", "\"fill\": {\"crosshatched\""),
            ("\"pads\": {\"relief\"", "\"pads\": {\"thermal\""),
            ("\"vias\": true", "\"via\": true"),
            ("{\"line\":{\"from\"", "{\"segment\":{\"from\""),
            ("\"mask\":\"rules\"", "\"mask\":\"default\""),
        ] {
            assert!(good.contains(from), "{from}");
            assert!(matches!(read_board(&good.replacen(from, to, 1)), Err(NativeError::Malformed(_))), "{from} -> {to}");
        }
        // A field left out is not read as empty, and one more than this build knows is not skipped:
        // saving would drop it.
        for (field, text) in [
            ("link", "        \"link\": {\"library\":\"Connectors\",\"footprint\":\"USB-C-16P\"},\n"),
            (
                "outline",
                "    \"outline\": {\n      \"start\": {\"x\":0,\"y\":0},\n      \"edges\": [\n        {\"line\":{\"to\":{\"x\":30000000,\"y\":0}}},\n        {\"line\":{\"to\":{\"x\":30000000,\"y\":30000000}}},\n        {\"line\":{\"to\":{\"x\":0,\"y\":30000000}}}\n      ]\n    },\n",
            ),
            ("keep_islands", "        \"keep_islands\": false,\n"),
            ("net", ",\"net\":null"),
            ("slot", "\"slot\":null,"),
            ("by", "\"by\":null"),
        ] {
            assert!(good.contains(text), "{field}: {text}");
            let err = read_board(&good.replacen(text, "", 1)).err().map(|e| e.to_string()).unwrap_or_default();
            assert!(err.contains(&format!("missing field `{field}`")), "without {field}: {err}");
        }
        for (after, extra) in [
            ("\"version\": 1,", "\"comment\": 1,"),
            ("\"board\": {", "\"comment\": 1,"),
            ("\"design\": {", "\"comment\": 1,"),
            ("\"origin\": {", "\"z\":0,"),
            ("{\"number\":\"A1\",", "\"comment\":1,"),
            ("\"hole\":{", "\"depth\":1,"),
            ("{\"line\":{", "\"width\":1,"),
        ] {
            assert!(good.contains(after), "{after}");
            let err = read_board(&good.replacen(after, &format!("{after}{extra}"), 1)).err().map(|e| e.to_string()).unwrap_or_default();
            assert!(err.contains("unknown field"), "{extra} after {after}: {err}");
        }
        let project = write_project(&Project { name: "P".to_owned(), board: "P.ccpcb".to_owned() }).expect("write");
        assert!(matches!(read_project(&project.replacen("\"name\"", "\"comment\": 1,\n  \"name\"", 1)), Err(NativeError::Malformed(_))));
    }

    #[test]
    fn every_item_is_on_a_line_of_its_own() {
        let mut b = Board::new("B");
        let id = |n: u128| ItemId::from_u128(n).to_string();
        b.nets.insert(circuitcraft_model::Net { id: circuitcraft_model::NetId::from_u128(1), name: "GND".to_owned() }).expect("net");
        for n in [2, 3] {
            let path = TrackPath::Line { from: at(0, 0), to: at(n as i64 * 1_000_000, 0) };
            b.tracks
                .insert(circuitcraft_model::Track { id: ItemId::from_u128(n), layer: TOP_COPPER, width: Nm(250_000), path, net: None })
                .expect("track");
        }
        b.outline = Contour::polyline(&[at(0, 0), at(1_000_000, 0), at(0, 1_000_000)]);
        let text = write_board(&b).expect("write");
        // The header, the board's own fields and each design setting are lines; a net, a track, a
        // layer and a corner of the outline are one line each; an empty list stays closed.
        let expected = [
            "{".to_owned(),
            "  \"format\": \"circuitcraft.board\",".to_owned(),
            "    \"name\": \"B\",".to_owned(),
            "    \"design\": {".to_owned(),
            "      \"tent_vias\": true,".to_owned(),
            "      \"origin\": {\"x\":0,\"y\":0}".to_owned(),
            "        {\"id\":4,\"name\":\"Top Copper\",\"kind\":\"copper\",\"thickness\":35000,\"side\":null,\"pair\":null},".to_owned(),
            "    \"outline\": {".to_owned(),
            "      \"start\": {\"x\":0,\"y\":0},".to_owned(),
            "        {\"line\":{\"to\":{\"x\":1000000,\"y\":0}}},".to_owned(),
            "        {\"line\":{\"to\":{\"x\":0,\"y\":1000000}}}".to_owned(),
            "    \"cutouts\": [],".to_owned(),
            "    \"nets\": [".to_owned(),
            format!("      {{\"id\":\"{}\",\"name\":\"GND\"}}", id(1)),
            "    ],".to_owned(),
            format!(
                "      {{\"id\":\"{}\",\"layer\":4,\"width\":250000,\"path\":{{\"line\":{{\"from\":{{\"x\":0,\"y\":0}},\"to\":{{\"x\":2000000,\"y\":0}}}}}},\"net\":null}},",
                id(2)
            ),
            "    \"graphics\": []".to_owned(),
            "  }".to_owned(),
            "}".to_owned(),
        ];
        let mut lines = text.lines();
        for line in &expected {
            assert!(lines.any(|l| l == line), "no line {line:?}, in this order, in:\n{text}");
        }
        assert_eq!(read_board(&text).ok().as_ref(), Some(&b));
        // Changing one track changes its line and no other.
        if let Some(t) = b.tracks.get_mut(ItemId::from_u128(3)) {
            t.width = Nm(300_000);
        }
        let changed = write_board(&b).expect("write");
        let differing: Vec<_> = text.lines().zip(changed.lines()).filter(|(a, b)| a != b).collect();
        assert_eq!((text.lines().count(), differing.len()), (changed.lines().count(), 1), "{differing:?}");
        assert!(differing.iter().all(|(old, new)| old.contains(&id(3)) && new.contains("\"width\":300000")));
    }

    #[test]
    fn a_footprint_and_a_zone_open_up_to_one_pad_and_one_corner_per_line() {
        let text = write_board(&blinky()).expect("write");
        let count = |start: &str| text.lines().filter(|l| l.trim_start().starts_with(start)).count();
        assert_eq!(count("{\"number\":"), 3, "one line per pad");
        assert_eq!(count("\"reference\": \"J1\""), 1, "a footprint's own fields are lines");
        assert_eq!(count("\"assembly\": {\"mounting\":\"surface_mount\""), 1, "small objects stay on their line");
        // The poured copper: an area of four corners with a hole of four, each corner a line.
        let poured = text.split_once("\"filled\": [").map(|(_, rest)| rest).and_then(|rest| rest.split_once("\n    ]")).map(|(fill, _)| fill);
        assert_eq!(poured.map(|fill| fill.lines().filter(|l| l.trim_start().starts_with("{\"x\":")).count()), Some(8));
        // Text with brackets, quotes and commas in it is copied as it is.
        let mut b = blinky();
        b.name = "A \"board\", [really] {it is}\\".to_owned();
        let text = write_board(&b).expect("write");
        assert!(text.contains("    \"name\": \"A \\\"board\\\", [really] {it is}\\\\\",\n"), "{text}");
        assert_eq!(read_board(&text).ok(), Some(b));
    }

    #[test]
    fn laying_out_broken_text_is_an_error_not_a_crash() {
        let lay = |text: &str| {
            let mut layout = Layout { text, at: 0, out: String::new() };
            layout.value(0).map(|()| layout.out)
        };
        assert_eq!(lay("{\"a\":[1,2],\"b\":{}}").ok().as_deref(), Some("{\n  \"a\": [\n    1,\n    2\n  ],\n  \"b\": {}\n}"));
        assert_eq!(lay("7").ok().as_deref(), Some("7"));
        let deep = "[".repeat(MAX_NESTING + 1) + &"]".repeat(MAX_NESTING + 1);
        for text in ["", "{", "[1,", "{\"a\"}", "{\"a\":}", "{\"a", "[\"a\\", "{1:2}", "]", deep.as_str(), &"[".repeat(100_000)] {
            assert!(matches!(lay(text), Err(NativeError::Malformed(_))), "{:?}", text.get(..text.len().min(40)));
        }
    }

    #[test]
    fn hostile_text_is_an_error_not_a_crash() {
        let board = blinky();
        let good = write_board(&board).expect("write");
        let deep = format!("{{\"format\":\"circuitcraft.board\",\"version\":1,\"board\":{}", "[".repeat(100_000));
        let cases = [
            String::new(),
            "null".to_owned(),
            "{".to_owned(),
            "[]".to_owned(),
            deep,
            good.replacen("250000", "1e400", 1),
            good.replacen("250000", "-9223372036854775809", 1),
            good.replacen("\"Blinky\"", "\"\"", 1),
            good.get(..good.len() / 2).unwrap_or_default().to_owned(),
        ];
        for text in &cases {
            assert!(read_board(text).is_err(), "{:?}", text.get(..text.len().min(80)));
        }
        // Every prefix of a good file: no panics, and only the whole file reads. The whole file is
        // everything up to its closing brace; the newline after it can be missing.
        let whole = good.trim_end().len();
        assert_eq!(read_board(&good).ok().as_ref(), Some(&board));
        assert_eq!(read_board(good.get(..whole).unwrap_or_default()).ok().as_ref(), Some(&board));
        for cut in (0..whole).step_by(7) {
            if let Some(prefix) = good.get(..cut) {
                assert!(read_board(prefix).is_err(), "a file cut at byte {cut} of {whole} was read");
            }
        }
    }

    #[test]
    fn projects_name_a_board_in_their_own_folder() {
        let p = Project { name: "Blinky".to_owned(), board: "Blinky.ccpcb".to_owned() };
        let text = write_project(&p).expect("write");
        assert_eq!(
            text,
            "{\n  \"format\": \"circuitcraft.project\",\n  \"version\": 1,\n  \"name\": \"Blinky\",\n  \"board\": \"Blinky.ccpcb\"\n}\n"
        );
        assert_eq!(read_project(&text).ok(), Some(p));
        for bad in ["../x.ccpcb", "/etc/x.ccpcb", "sub/x.ccpcb", "C:x.ccpcb", "..\\x.ccpcb", "x.txt", "", ".."] {
            let quoted = serde_json::to_string(bad).expect("json string");
            let t = text.replacen("\"Blinky.ccpcb\"", &quoted, 1);
            assert!(matches!(read_project(&t), Err(NativeError::Project(_))), "{bad}");
        }
    }

    fn leftover_temp_files(folder: &Path) -> usize {
        std::fs::read_dir(folder)
            .map(|d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().ends_with(".saving")).count())
            .unwrap_or(0)
    }

    #[test]
    fn save_and_open_a_project_folder() {
        let dir = TempDir::new("native");
        let folder = dir.0.join("Blinky");
        let b = blinky();
        let paths = save_project(&DISK, &folder, &b, false).expect("save");
        assert_eq!(paths.project(), Some(folder.join("Blinky.ccproj").as_path()));
        assert_eq!(paths.board(), folder.join("Blinky.ccpcb"));
        assert_eq!(paths.folder(), folder);
        let first = std::fs::read(paths.board()).expect("board file");
        for path in [&folder, &folder.join("Blinky.ccproj")] {
            let (back, at) = open(&DISK, path).expect("open");
            assert_eq!(back, b);
            assert_eq!(at, paths);
        }
        save(&DISK, &paths, &b).expect("save again");
        assert_eq!(std::fs::read(paths.board()).expect("board file"), first, "re-save must be byte-identical");
        assert_eq!(leftover_temp_files(&folder), 0, "no temporary files left behind");
    }

    #[test]
    fn a_project_is_saved_and_opened_in_memory_as_on_disk() {
        let memory = MemoryStorage::default();
        let b = blinky();
        let paths = save_project(&memory, Path::new("work/Blinky"), &b, false).expect("save");
        assert_eq!((paths.project(), paths.board()), (Some(Path::new("/work/Blinky/Blinky.ccproj")), Path::new("/work/Blinky/Blinky.ccpcb")));
        for path in ["/work/Blinky", "work/Blinky/Blinky.ccproj", "/work/Blinky/Blinky.ccpcb"] {
            assert_eq!(open(&memory, Path::new(path)).expect("open").0, b, "{path}");
        }
        // The bytes are the ones the disk gets.
        let dir = TempDir::new("native-memory");
        let on_disk = save_project(&DISK, &dir.0.join("Blinky"), &b, false).expect("save");
        let held = memory.snapshot();
        assert_eq!(held.get(paths.board()), std::fs::read(on_disk.board()).ok().as_ref());
        assert_eq!(held.len(), 2, "a board file and a project file");
        // The same rules: no second project in a folder, a missing file is an error.
        assert!(matches!(save_project(&memory, Path::new("/work/Blinky"), &b, false), Err(NativeError::WouldOverwrite(_))));
        assert!(matches!(open(&memory, Path::new("/nowhere")), Err(NativeError::Io { .. })));
        assert!(matches!(open(&memory, Path::new("/work")), Err(NativeError::Project(_))), "a folder without a project");
    }

    #[test]
    fn saving_goes_back_to_the_files_that_were_opened() {
        let dir = TempDir::new("native-renamed");
        let old = dir.0.join("Blinky2");
        save_project(&DISK, &old, &blinky(), false).expect("save");
        // The folder is renamed (or cloned under another name) after saving.
        let renamed = dir.0.join("my-board");
        std::fs::rename(&old, &renamed).expect("rename");
        let (mut b, paths) = open(&DISK, &renamed).expect("open");
        assert_eq!(paths.project(), Some(renamed.join("Blinky2.ccproj").as_path()));
        b.add_net("VCC").expect("edit");
        save(&DISK, &paths, &b).expect("save");
        assert!(!renamed.join("my-board.ccproj").exists() && !renamed.join("my-board.ccpcb").exists(), "no second project");
        let (back, _) = open(&DISK, &renamed).expect("still opens");
        assert!(back.net_by_name("VCC").is_some(), "the edit is in the opened files");
    }

    #[test]
    fn a_board_opened_on_its_own_is_saved_to_the_same_file() {
        let dir = TempDir::new("native-loose");
        std::fs::create_dir_all(&dir.0).expect("dir");
        let file = dir.0.join("Blinky.ccpcb");
        std::fs::write(&file, write_board(&blinky()).expect("write")).expect("write");
        let (mut b, paths) = open(&DISK, &file).expect("open");
        assert_eq!((paths.project(), paths.board(), paths.folder()), (None, file.as_path(), dir.0.as_path()));
        b.add_net("VCC").expect("edit");
        save(&DISK, &paths, &b).expect("save");
        let names: Vec<_> = std::fs::read_dir(&dir.0).expect("dir").filter_map(Result::ok).map(|e| e.file_name()).collect();
        assert_eq!(names, ["Blinky.ccpcb"], "nothing written beside it");
        assert_eq!(open(&DISK, &file).expect("open").0, b);
    }

    /// Linux allows file names that aren't UTF-8 (macOS refuses them, so this runs on Linux only).
    #[cfg(target_os = "linux")]
    #[test]
    fn a_project_file_name_that_is_not_utf8_is_refused_not_mangled() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = TempDir::new("native-utf8");
        let folder = dir.0.join("P");
        std::fs::create_dir_all(&folder).expect("dir");
        std::fs::write(folder.join(std::ffi::OsStr::from_bytes(b"\xff.ccproj")), "{}").expect("write");
        assert!(matches!(save_project(&DISK, &folder, &blinky(), true), Err(NativeError::Project(_))));
        assert!(matches!(save_project(&DISK, &folder, &blinky(), false), Err(NativeError::WouldOverwrite(_))));
    }

    #[test]
    fn a_long_file_name_can_still_be_saved() {
        let dir = TempDir::new("native-long");
        std::fs::create_dir_all(&dir.0).expect("dir");
        // 249 + ".ccpcb" = 255 bytes, the longest name most file systems allow.
        let file = dir.0.join(format!("{}.ccpcb", "b".repeat(249)));
        std::fs::write(&file, write_board(&blinky()).expect("write")).expect("write");
        let (b, paths) = open(&DISK, &file).expect("open");
        save(&DISK, &paths, &b).expect("the temporary name doesn't grow with the file name");
    }

    #[test]
    fn saving_to_a_folder_never_silently_replaces_another_project() {
        let dir = TempDir::new("native-overwrite");
        let folder = dir.0.join("X");
        let other = Board::new("Other");
        save_project(&DISK, &folder, &other, false).expect("first project");
        assert!(matches!(save_project(&DISK, &folder, &blinky(), false), Err(NativeError::WouldOverwrite(_))));
        assert_eq!(open(&DISK, &folder).expect("open").0, other, "the other project is untouched");
        // A differently named project in the folder counts too.
        let named = dir.0.join("Y");
        std::fs::create_dir_all(&named).expect("dir");
        std::fs::write(named.join("Legacy.ccproj"), "{}").expect("write");
        assert!(matches!(save_project(&DISK, &named, &blinky(), false), Err(NativeError::WouldOverwrite(_))));
        // With overwrite, the board goes into the project that is there, under its own name, so
        // the folder still holds exactly one project.
        let b = blinky();
        let paths = save_project(&DISK, &named, &b, true).expect("overwrite when asked");
        assert_eq!(paths.project(), Some(named.join("Legacy.ccproj").as_path()));
        assert!(!named.join("Y.ccproj").exists());
        assert_eq!(open(&DISK, &named).expect("one project, which opens").0, b);
        save_project(&DISK, &folder, &b, true).expect("overwrite when asked");
        assert_eq!(open(&DISK, &folder).expect("open").0, b);
        // Several projects in a folder: refused even with overwrite.
        std::fs::write(folder.join("Another.ccproj"), "{}").expect("write");
        assert!(matches!(save_project(&DISK, &folder, &b, true), Err(NativeError::Project(_))));
    }

    /// The old scheme wrote through a fixed temporary name (`.<file>.saving`), so a symlink planted
    /// there redirected the save. New temporary names are random and created with `create_new`;
    /// this checks that links at the old names, and a dangling one, are left alone.
    #[cfg(unix)]
    #[test]
    fn symlinks_at_the_old_temporary_names_cannot_redirect_a_save() {
        let dir = TempDir::new("native-symlink");
        let folder = dir.0.join("Blinky");
        let paths = save_project(&DISK, &folder, &blinky(), false).expect("save");
        let victim = dir.0.join("victim.txt");
        std::fs::write(&victim, "precious").expect("write");
        for name in [".Blinky.ccpcb.saving", ".Blinky.ccproj.saving"] {
            std::os::unix::fs::symlink(&victim, folder.join(name)).expect("symlink");
        }
        std::os::unix::fs::symlink(dir.0.join("nowhere"), folder.join(".dangling.saving")).expect("symlink");
        save(&DISK, &paths, &blinky()).expect("save");
        assert_eq!(std::fs::read_to_string(&victim).expect("victim"), "precious");
        let meta = std::fs::symlink_metadata(paths.board()).expect("board");
        assert!(meta.file_type().is_file(), "the board is a regular file, not a link");
        assert!(!dir.0.join("nowhere").exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_regular_files_are_read() {
        assert!(matches!(read_text(&DISK, Path::new("/dev/null")), Err(NativeError::NotAFile(_))));
        assert!(matches!(open(&DISK, Path::new("/dev/null")), Err(NativeError::NotAFile(_))));
        let dir = TempDir::new("native-device");
        std::fs::create_dir_all(&dir.0).expect("dir");
        // A project whose board name resolves to something that isn't a file.
        let project = write_project(&Project { name: "P".to_owned(), board: "dev.ccpcb".to_owned() }).expect("write");
        std::fs::write(dir.0.join("P.ccproj"), project).expect("write");
        std::os::unix::fs::symlink("/dev/null", dir.0.join("dev.ccpcb")).expect("symlink");
        assert!(matches!(open(&DISK, &dir.0), Err(NativeError::NotAFile(_))));
    }

    #[test]
    fn hidden_files_and_folders_are_not_project_files() {
        let dir = TempDir::new("native-hidden");
        let folder = dir.0.join("Blinky");
        let b = blinky();
        save_project(&DISK, &folder, &b, false).expect("save");
        std::fs::write(folder.join("._Blinky.ccproj"), [0_u8, 5, 22, 7]).expect("AppleDouble file");
        std::fs::create_dir_all(folder.join("backup.ccproj")).expect("a folder named like a project");
        assert_eq!(open(&DISK, &folder).expect("open").0, b);
    }

    /// A relative path from the working folder to a temporary project (`../../…/tmp/…`), so the
    /// test reads and writes only under the temporary folder.
    #[cfg(unix)]
    #[test]
    fn a_relative_path_opens_with_an_absolute_folder() {
        let dir = TempDir::new("native-relative");
        let folder = dir.0.join("Blinky");
        save_project(&DISK, &folder, &blinky(), false).expect("save");
        let cwd = std::env::current_dir().expect("cwd");
        let ups = cwd.components().filter(|c| matches!(c, std::path::Component::Normal(_))).count();
        let mut relative: PathBuf = std::iter::repeat_n("..", ups).collect();
        relative.push(folder.strip_prefix("/").expect("absolute temp folder"));
        assert!(relative.is_relative());
        let (_, paths) = open(&DISK, &relative).expect("open");
        assert!(paths.folder().is_absolute() && paths.folder().ends_with("Blinky"), "{:?}", paths.folder());
    }

    #[test]
    fn a_recovery_file_holds_a_board_and_where_it_came_from() {
        let b = blinky();
        let text = write_recovery(&b, Some(Path::new("/work/Blinky"))).expect("write");
        assert!(text.starts_with("{\n  \"format\": \"circuitcraft.recovery\",\n  \"version\": 1,\n  \"from\": \"/work/Blinky\",\n"), "{text}");
        assert_eq!(read_recovery(&text).ok(), Some((b.clone(), Some("/work/Blinky".to_owned()))));
        assert_eq!(peek_recovery(&text).ok(), Some(RecoveryHeader { name: "Blinky".to_owned(), from: Some("/work/Blinky".to_owned()) }));
        let unsaved = write_recovery(&b, None).expect("write");
        assert_eq!(read_recovery(&unsaved).ok(), Some((b.clone(), None)));
        // It is no board file, a board file is no recovery file, and it is read as strictly.
        assert!(matches!(read_board(&text), Err(NativeError::WrongFormat { expected: "board", .. })));
        assert!(matches!(read_recovery(&write_board(&b).expect("write")), Err(NativeError::WrongFormat { expected: "recovery", .. })));
        assert!(matches!(peek_recovery(&write_board(&b).expect("write")), Err(NativeError::WrongFormat { .. })));
        assert!(matches!(read_recovery(&text.replacen("\"version\": 1", "\"version\": 2", 1)), Err(NativeError::TooNew(2))));
        assert!(matches!(read_recovery(&text.replacen("  \"from\": \"/work/Blinky\",\n", "", 1)), Err(NativeError::Malformed(_))));
        assert!(matches!(read_recovery(&text.replacen("\"from\"", "\"extra\": 1,\n  \"from\"", 1)), Err(NativeError::Malformed(_))));
        // A board that doesn't hold together is refused when it is read in full; the peek, which
        // only lists it, still says what it is called.
        let broken = text.replacen("\"track_width\": 250000", "\"track_width\": 0", 1);
        assert!(matches!(read_recovery(&broken), Err(NativeError::Invalid(_))));
        assert_eq!(peek_recovery(&broken).ok().map(|h| h.name), Some("Blinky".to_owned()));
        for text in ["", "{", "null", "{\"format\":\"circuitcraft.recovery\",\"version\":1,\"from\":null,\"board\":7}"] {
            assert!(read_recovery(text).is_err() && peek_recovery(text).is_err(), "{text}");
        }
        // The peek checks what kind of file it is, too: something shaped alike is not listed as a
        // board, and neither is one from a newer build.
        let alike =
            |format: &str, version: u32| format!("{{\"format\":\"{format}\",\"version\":{version},\"from\":null,\"board\":{{\"name\":\"B\"}}}}");
        assert_eq!(peek_recovery(&alike(RECOVERY_FORMAT, 1)).ok().map(|h| h.name), Some("B".to_owned()));
        assert!(matches!(peek_recovery(&alike("something.else", 1)), Err(NativeError::WrongFormat { .. })));
        assert!(matches!(peek_recovery(&alike(RECOVERY_FORMAT, 2)), Err(NativeError::TooNew(2))));
        // Where a board came from is shown to people and sent to agents: it is cut to a length
        // that is safe for both.
        let long = text.replacen("/work/Blinky", &"x".repeat(MAX_FROM_CHARS + 10), 1);
        assert_eq!(peek_recovery(&long).ok().and_then(|h| h.from).map(|f| f.len()), Some(MAX_FROM_CHARS));
        assert_eq!(read_recovery(&long).ok().and_then(|(_, from)| from).map(|f| f.len()), Some(MAX_FROM_CHARS));
    }

    /// A storage whose one file says it is larger than any file that is read, and can't be read.
    #[derive(Debug)]
    struct Huge;
    impl Storage for Huge {
        fn kind(&self, _: &Path) -> std::io::Result<Kind> {
            Ok(Kind::File(MAX_FILE_BYTES + 1))
        }
        fn read(&self, _: &Path, _: u64) -> std::io::Result<Vec<u8>> {
            Err(std::io::Error::other("a file this large must not be read"))
        }
        fn write(&self, _: &Path, _: &[u8]) -> std::io::Result<()> {
            Ok(())
        }
        fn remove(&self, _: &Path) -> std::io::Result<()> {
            Ok(())
        }
        fn create_folder(&self, _: &Path) -> std::io::Result<()> {
            Ok(())
        }
        fn files(&self, _: &Path) -> std::io::Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }
        fn absolute(&self, path: &Path) -> std::io::Result<PathBuf> {
            Ok(path.to_owned())
        }
        fn recovery_folder(&self) -> Option<PathBuf> {
            None
        }
    }

    #[test]
    fn a_file_that_is_too_large_is_refused_before_it_is_read() {
        assert!(matches!(read_text(&Huge, Path::new("/big.ccpcb")), Err(NativeError::TooLarge(_))));
        assert!(matches!(open(&Huge, Path::new("/big.ccpcb")), Err(NativeError::TooLarge(_))));
    }

    #[test]
    fn a_byte_order_mark_is_accepted() {
        let b = blinky();
        let text = format!("\u{feff}{}", write_board(&b).expect("write"));
        assert_eq!(read_board(&text).ok(), Some(b));
        let project = format!("\u{feff}{}", write_project(&Project { name: "P".to_owned(), board: "P.ccpcb".to_owned() }).expect("write"));
        assert!(read_project(&project).is_ok());
    }

    #[test]
    fn opening_bad_folders_fails_cleanly() {
        let dir = TempDir::new("native-bad");
        std::fs::create_dir_all(&dir.0).expect("dir");
        assert!(matches!(open(&DISK, &dir.0), Err(NativeError::Project(_))), "empty folder");
        assert!(matches!(open(&DISK, &dir.0.join("missing.ccpcb")), Err(NativeError::Io { .. })));
        std::fs::write(dir.0.join("a.ccproj"), "{}").expect("write");
        std::fs::write(dir.0.join("b.ccproj"), "{}").expect("write");
        assert!(matches!(open(&DISK, &dir.0), Err(NativeError::Project(_))), "two project files");
        std::fs::write(dir.0.join("not-utf8.ccpcb"), [0xff, 0xfe, 0x00]).expect("write");
        assert!(matches!(open(&DISK, &dir.0.join("not-utf8.ccpcb")), Err(NativeError::Malformed(_))));
    }
}
