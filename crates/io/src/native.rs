//! CircuitCraft's own files: a project folder with a `.ccproj` project file and a `.ccpcb` board.
//!
//! Both are pretty-printed JSON that starts with `format` and `version`, so a person, a script or
//! [`crate::detect`] can tell what a file is from its first line. Keys and items come out in a
//! fixed order (items sorted by id), so saving the same design twice gives byte-identical files and
//! a change to the design gives a small diff.
//!
//! Reading treats the file as hostile: its size is capped before parsing, the parser's nesting
//! limit and the model's collection limits bound memory, an unknown format or a newer version is
//! refused with a clear message, and the board must pass [`Board::validate`].

use circuitcraft_model::{Board, ModelError};
use serde::{Deserialize, Serialize};
use std::io::{Read as _, Write as _};
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
        /// `board` or `project`.
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
struct ProjectIn {
    format: String,
    version: u64,
    name: String,
    board: String,
}

/// Just the header, read when a full parse fails, to say why in the user's terms.
#[derive(Deserialize)]
struct Header {
    format: Option<String>,
    version: Option<u64>,
}

/// The pretty-printed JSON, with a final newline.
fn pretty<T: Serialize>(v: &T) -> Result<String, NativeError> {
    let mut text = serde_json::to_string_pretty(v).map_err(|e| NativeError::Malformed(e.to_string()))?;
    text.push('\n');
    Ok(text)
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
pub fn read_text(path: &Path) -> Result<String, NativeError> {
    let io = |source| NativeError::Io { path: path.to_owned(), source };
    let meta = std::fs::metadata(path).map_err(io)?;
    if !meta.is_file() {
        return Err(NativeError::NotAFile(path.to_owned()));
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(NativeError::TooLarge(path.to_owned()));
    }
    let file = std::fs::File::open(path).map_err(io)?;
    // The size from the metadata sets the first allocation; `take` still bounds a file that grows.
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0).saturating_add(1));
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(NativeError::TooLarge(path.to_owned()));
    }
    String::from_utf8(bytes).map_err(|_| NativeError::Malformed(format!("{}: not UTF-8 text", path.display())))
}

/// Writes a file by way of a temporary file in the same folder, which then replaces it:
///
/// - A crash, a full disk or a failed write leaves the old file untouched, and the temporary file
///   is removed on every error path.
/// - The temporary file is created new (`create_new` refuses any existing path, symlinks
///   included) with a random name, so a link planted in a project folder can't redirect the
///   write, and two saves at once don't share a file.
/// - The data is flushed to disk before the rename (on macOS `sync_all` is a full flush). On Unix
///   the folder is flushed after it too, so a power cut leaves the old file or the new one; other
///   systems don't offer that flush, so the rename itself may be lost there.
/// - The file is replaced, not rewritten: a board file that is a symlink becomes a regular file,
///   and the new file gets default permissions.
fn write_atomic(path: &Path, text: &str) -> Result<(), NativeError> {
    let io = |source| NativeError::Io { path: path.to_owned(), source };
    // 128 random bits from the system (an item id is just that): a name nothing else has, and
    // one that doesn't depend on the target's name, so a long file name still fits.
    let tag = circuitcraft_model::ItemId::random().map_err(|e| NativeError::Project(format!("{}: {e}", path.display())))?.as_u128();
    let temp = path.with_file_name(format!(".circuitcraft-{tag:032x}.saving"));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp).map_err(io)?;
    let written = file.write_all(text.as_bytes()).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written.and_then(|()| std::fs::rename(&temp, path)) {
        let _ = std::fs::remove_file(&temp);
        return Err(io(e));
    }
    sync_folder(path);
    Ok(())
}

/// Flushes a folder's entries to disk after a rename, on Unix (other systems can't). A failure
/// here doesn't undo a save that has already happened, so it is ignored.
fn sync_folder(path: &Path) {
    #[cfg(unix)]
    if let Some(folder) = path.parent().filter(|f| !f.as_os_str().is_empty()) {
        let _ = std::fs::File::open(folder).and_then(|f| f.sync_all());
    }
    #[cfg(not(unix))]
    let _ = path;
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
fn absolute(path: &Path) -> Result<PathBuf, NativeError> {
    std::path::absolute(path).map_err(|source| NativeError::Io { path: path.to_owned(), source })
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
pub fn save_project(folder: &Path, board: &Board, overwrite: bool) -> Result<ProjectPaths, NativeError> {
    let folder = absolute(folder)?;
    let project = match project_files(&folder)?.as_slice() {
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
    if !overwrite && paths.board.exists() {
        return Err(NativeError::WouldOverwrite(paths.board));
    }
    std::fs::create_dir_all(&folder).map_err(|source| NativeError::Io { path: folder.clone(), source })?;
    save(&paths, board)?;
    Ok(paths)
}

/// Saves a board back to the files it was opened from or last saved to: the board file, and the
/// project file when there is one.
pub fn save(paths: &ProjectPaths, board: &Board) -> Result<(), NativeError> {
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
    write_atomic(&paths.board, &write_board(board)?)?;
    if let Some((file, text)) = project {
        write_atomic(file, &text)?;
    }
    Ok(())
}

/// Opens a project folder, a `.ccproj` project file or a `.ccpcb` board file, and returns its
/// board and the files it came from.
pub fn open(path: &Path) -> Result<(Board, ProjectPaths), NativeError> {
    let path = absolute(path)?;
    let meta = std::fs::metadata(&path).map_err(|source| NativeError::Io { path: path.clone(), source })?;
    let project_file = if meta.is_dir() {
        find_project_file(&path)?
    } else if path.extension().is_some_and(|e| e == BOARD_EXT) {
        let board = read_board(&read_text(&path)?)?;
        return Ok((board, ProjectPaths { project: None, board: path }));
    } else {
        path
    };
    let project = read_project(&read_text(&project_file)?)?;
    let board_file = project_file.with_file_name(&project.board);
    let board = read_board(&read_text(&board_file)?)?;
    Ok((board, ProjectPaths { project: Some(project_file), board: board_file }))
}

/// The project files in a folder: regular `.ccproj` files whose names don't start with a dot, so
/// hidden files (such as the `._` files macOS leaves on other file systems) are ignored. A folder
/// that doesn't exist has none.
fn project_files(folder: &Path) -> Result<Vec<PathBuf>, NativeError> {
    let io = |source| NativeError::Io { path: folder.to_owned(), source };
    let entries = match std::fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(io(e)),
    };
    let mut found = Vec::new();
    // Bounded: a folder with a huge number of entries is not a project folder.
    for entry in entries.take(100_000) {
        let path = entry.map_err(io)?.path();
        // Raw bytes, so a file whose name isn't UTF-8 still counts (and is then refused by name).
        let hidden = path.file_name().is_none_or(|n| n.as_encoded_bytes().starts_with(b"."));
        if !hidden && path.extension().is_some_and(|e| e == PROJECT_EXT) && path.is_file() {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// The one project file in a folder.
fn find_project_file(folder: &Path) -> Result<PathBuf, NativeError> {
    match project_files(folder)?.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(NativeError::Project(format!("{}: no .{PROJECT_EXT} project file in this folder", folder.display()))),
        many => Err(NativeError::Project(format!("{}: {} project files in this folder; open one of them", folder.display(), many.len()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use circuitcraft_model::TrackPath;
    use circuitcraft_model::layer_ids::TOP_COPPER;
    use circuitcraft_units::{Nm, Point};

    fn blinky() -> Board {
        let mut b = Board::new("Blinky");
        let gnd = b.add_net("GND").expect("net");
        let path = TrackPath::Line { from: Point::new(Nm(0), Nm(0)), to: Point::new(Nm(1_000_000), Nm(0)) };
        b.add_track(TOP_COPPER, Nm(250_000), path, Some(gnd)).expect("track");
        b.add_via(Point::new(Nm(1_000_000), Nm(0)), Nm(600_000), Nm(300_000), Some(gnd)).expect("via");
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
    }

    #[test]
    fn hostile_text_is_an_error_not_a_crash() {
        let good = write_board(&blinky()).expect("write");
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
        // Every prefix of a good file: no panics, and only the whole file reads.
        for cut in (0..good.len()).step_by(7) {
            if let Some(prefix) = good.get(..cut) {
                assert!(read_board(prefix).is_err());
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
        let paths = save_project(&folder, &b, false).expect("save");
        assert_eq!(paths.project(), Some(folder.join("Blinky.ccproj").as_path()));
        assert_eq!(paths.board(), folder.join("Blinky.ccpcb"));
        assert_eq!(paths.folder(), folder);
        let first = std::fs::read(paths.board()).expect("board file");
        for path in [&folder, &folder.join("Blinky.ccproj")] {
            let (back, at) = open(path).expect("open");
            assert_eq!(back, b);
            assert_eq!(at, paths);
        }
        save(&paths, &b).expect("save again");
        assert_eq!(std::fs::read(paths.board()).expect("board file"), first, "re-save must be byte-identical");
        assert_eq!(leftover_temp_files(&folder), 0, "no temporary files left behind");
    }

    #[test]
    fn saving_goes_back_to_the_files_that_were_opened() {
        let dir = TempDir::new("native-renamed");
        let old = dir.0.join("Blinky2");
        save_project(&old, &blinky(), false).expect("save");
        // The folder is renamed (or cloned under another name) after saving.
        let renamed = dir.0.join("my-board");
        std::fs::rename(&old, &renamed).expect("rename");
        let (mut b, paths) = open(&renamed).expect("open");
        assert_eq!(paths.project(), Some(renamed.join("Blinky2.ccproj").as_path()));
        b.add_net("VCC").expect("edit");
        save(&paths, &b).expect("save");
        assert!(!renamed.join("my-board.ccproj").exists() && !renamed.join("my-board.ccpcb").exists(), "no second project");
        let (back, _) = open(&renamed).expect("still opens");
        assert!(back.net_by_name("VCC").is_some(), "the edit is in the opened files");
    }

    #[test]
    fn a_board_opened_on_its_own_is_saved_to_the_same_file() {
        let dir = TempDir::new("native-loose");
        std::fs::create_dir_all(&dir.0).expect("dir");
        let file = dir.0.join("Blinky.ccpcb");
        std::fs::write(&file, write_board(&blinky()).expect("write")).expect("write");
        let (mut b, paths) = open(&file).expect("open");
        assert_eq!((paths.project(), paths.board(), paths.folder()), (None, file.as_path(), dir.0.as_path()));
        b.add_net("VCC").expect("edit");
        save(&paths, &b).expect("save");
        let names: Vec<_> = std::fs::read_dir(&dir.0).expect("dir").filter_map(Result::ok).map(|e| e.file_name()).collect();
        assert_eq!(names, ["Blinky.ccpcb"], "nothing written beside it");
        assert_eq!(open(&file).expect("open").0, b);
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
        assert!(matches!(save_project(&folder, &blinky(), true), Err(NativeError::Project(_))));
        assert!(matches!(save_project(&folder, &blinky(), false), Err(NativeError::WouldOverwrite(_))));
    }

    #[test]
    fn a_long_file_name_can_still_be_saved() {
        let dir = TempDir::new("native-long");
        std::fs::create_dir_all(&dir.0).expect("dir");
        // 249 + ".ccpcb" = 255 bytes, the longest name most file systems allow.
        let file = dir.0.join(format!("{}.ccpcb", "b".repeat(249)));
        std::fs::write(&file, write_board(&blinky()).expect("write")).expect("write");
        let (b, paths) = open(&file).expect("open");
        save(&paths, &b).expect("the temporary name doesn't grow with the file name");
    }

    #[test]
    fn saving_to_a_folder_never_silently_replaces_another_project() {
        let dir = TempDir::new("native-overwrite");
        let folder = dir.0.join("X");
        let other = Board::new("Other");
        save_project(&folder, &other, false).expect("first project");
        assert!(matches!(save_project(&folder, &blinky(), false), Err(NativeError::WouldOverwrite(_))));
        assert_eq!(open(&folder).expect("open").0, other, "the other project is untouched");
        // A differently named project in the folder counts too.
        let named = dir.0.join("Y");
        std::fs::create_dir_all(&named).expect("dir");
        std::fs::write(named.join("Legacy.ccproj"), "{}").expect("write");
        assert!(matches!(save_project(&named, &blinky(), false), Err(NativeError::WouldOverwrite(_))));
        // With overwrite, the board goes into the project that is there, under its own name, so
        // the folder still holds exactly one project.
        let b = blinky();
        let paths = save_project(&named, &b, true).expect("overwrite when asked");
        assert_eq!(paths.project(), Some(named.join("Legacy.ccproj").as_path()));
        assert!(!named.join("Y.ccproj").exists());
        assert_eq!(open(&named).expect("one project, which opens").0, b);
        save_project(&folder, &b, true).expect("overwrite when asked");
        assert_eq!(open(&folder).expect("open").0, b);
        // Several projects in a folder: refused even with overwrite.
        std::fs::write(folder.join("Another.ccproj"), "{}").expect("write");
        assert!(matches!(save_project(&folder, &b, true), Err(NativeError::Project(_))));
    }

    /// The old scheme wrote through a fixed temporary name (`.<file>.saving`), so a symlink planted
    /// there redirected the save. New temporary names are random and created with `create_new`;
    /// this checks that links at the old names, and a dangling one, are left alone.
    #[cfg(unix)]
    #[test]
    fn symlinks_at_the_old_temporary_names_cannot_redirect_a_save() {
        let dir = TempDir::new("native-symlink");
        let folder = dir.0.join("Blinky");
        let paths = save_project(&folder, &blinky(), false).expect("save");
        let victim = dir.0.join("victim.txt");
        std::fs::write(&victim, "precious").expect("write");
        for name in [".Blinky.ccpcb.saving", ".Blinky.ccproj.saving"] {
            std::os::unix::fs::symlink(&victim, folder.join(name)).expect("symlink");
        }
        std::os::unix::fs::symlink(dir.0.join("nowhere"), folder.join(".dangling.saving")).expect("symlink");
        save(&paths, &blinky()).expect("save");
        assert_eq!(std::fs::read_to_string(&victim).expect("victim"), "precious");
        let meta = std::fs::symlink_metadata(paths.board()).expect("board");
        assert!(meta.file_type().is_file(), "the board is a regular file, not a link");
        assert!(!dir.0.join("nowhere").exists());
    }

    #[cfg(unix)]
    #[test]
    fn only_regular_files_are_read() {
        assert!(matches!(read_text(Path::new("/dev/null")), Err(NativeError::NotAFile(_))));
        assert!(matches!(open(Path::new("/dev/null")), Err(NativeError::NotAFile(_))));
        let dir = TempDir::new("native-device");
        std::fs::create_dir_all(&dir.0).expect("dir");
        // A project whose board name resolves to something that isn't a file.
        let project = write_project(&Project { name: "P".to_owned(), board: "dev.ccpcb".to_owned() }).expect("write");
        std::fs::write(dir.0.join("P.ccproj"), project).expect("write");
        std::os::unix::fs::symlink("/dev/null", dir.0.join("dev.ccpcb")).expect("symlink");
        assert!(matches!(open(&dir.0), Err(NativeError::NotAFile(_))));
    }

    #[test]
    fn hidden_files_and_folders_are_not_project_files() {
        let dir = TempDir::new("native-hidden");
        let folder = dir.0.join("Blinky");
        let b = blinky();
        save_project(&folder, &b, false).expect("save");
        std::fs::write(folder.join("._Blinky.ccproj"), [0_u8, 5, 22, 7]).expect("AppleDouble file");
        std::fs::create_dir_all(folder.join("backup.ccproj")).expect("a folder named like a project");
        assert_eq!(open(&folder).expect("open").0, b);
    }

    /// A relative path from the working folder to a temporary project (`../../…/tmp/…`), so the
    /// test reads and writes only under the temporary folder.
    #[cfg(unix)]
    #[test]
    fn a_relative_path_opens_with_an_absolute_folder() {
        let dir = TempDir::new("native-relative");
        let folder = dir.0.join("Blinky");
        save_project(&folder, &blinky(), false).expect("save");
        let cwd = std::env::current_dir().expect("cwd");
        let ups = cwd.components().filter(|c| matches!(c, std::path::Component::Normal(_))).count();
        let mut relative: PathBuf = std::iter::repeat_n("..", ups).collect();
        relative.push(folder.strip_prefix("/").expect("absolute temp folder"));
        assert!(relative.is_relative());
        let (_, paths) = open(&relative).expect("open");
        assert!(paths.folder().is_absolute() && paths.folder().ends_with("Blinky"), "{:?}", paths.folder());
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
        assert!(matches!(open(&dir.0), Err(NativeError::Project(_))), "empty folder");
        assert!(matches!(open(&dir.0.join("missing.ccpcb")), Err(NativeError::Io { .. })));
        std::fs::write(dir.0.join("a.ccproj"), "{}").expect("write");
        std::fs::write(dir.0.join("b.ccproj"), "{}").expect("write");
        assert!(matches!(open(&dir.0), Err(NativeError::Project(_))), "two project files");
        std::fs::write(dir.0.join("not-utf8.ccpcb"), [0xff, 0xfe, 0x00]).expect("write");
        assert!(matches!(open(&dir.0.join("not-utf8.ccpcb")), Err(NativeError::Malformed(_))));
    }
}
