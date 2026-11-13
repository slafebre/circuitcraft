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
use std::io::Read as _;
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
            "the project names a board {:?}; it must be a .{BOARD_EXT} file in the project folder",
            name.chars().take(64).collect::<String>()
        )))
    }
}

/// Reads a whole file as text, refusing one larger than [`MAX_FILE_BYTES`] without reading it all.
pub fn read_text(path: &Path) -> Result<String, NativeError> {
    let io = |source| NativeError::Io { path: path.to_owned(), source };
    let file = std::fs::File::open(path).map_err(io)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(NativeError::TooLarge(path.to_owned()));
    }
    String::from_utf8(bytes).map_err(|_| NativeError::Malformed(format!("{}: not UTF-8 text", path.display())))
}

/// Writes a file so that a crash or a full disk leaves either the old file or the new one, never
/// half of one: the text goes to a temporary file next to it, which then replaces it.
fn write_atomic(path: &Path, text: &str) -> Result<(), NativeError> {
    let io = |source| NativeError::Io { path: path.to_owned(), source };
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.saving"));
    std::fs::write(&temp, text).map_err(io)?;
    std::fs::rename(&temp, path).map_err(|e| {
        let _ = std::fs::remove_file(&temp);
        io(e)
    })
}

/// Where a project's files are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectPaths {
    /// The project folder.
    pub folder: PathBuf,
    /// The project file.
    pub project: PathBuf,
    /// The board file.
    pub board: PathBuf,
}

/// Saves a board as a project folder: `<folder>/<name>.ccproj` and `<folder>/<name>.ccpcb`, where
/// `<name>` is the folder's own name. The folder is created if needed.
pub fn save_project(folder: &Path, board: &Board) -> Result<ProjectPaths, NativeError> {
    let stem = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| NativeError::Project(format!("{}: a project folder needs a name", folder.display())))?;
    std::fs::create_dir_all(folder).map_err(|source| NativeError::Io { path: folder.to_owned(), source })?;
    let project = Project { name: board.name.clone(), board: format!("{stem}.{BOARD_EXT}") };
    check_board_name(&project.board)?;
    let paths = ProjectPaths { folder: folder.to_owned(), project: folder.join(format!("{stem}.{PROJECT_EXT}")), board: folder.join(&project.board) };
    // The board first: a project file never names a board that isn't there.
    write_atomic(&paths.board, &write_board(board)?)?;
    write_atomic(&paths.project, &write_project(&project)?)?;
    Ok(paths)
}

/// Opens a project folder, a `.ccproj` project file or a `.ccpcb` board file, and returns its
/// board and the project folder it belongs to.
pub fn open(path: &Path) -> Result<(Board, PathBuf), NativeError> {
    let io = |source| NativeError::Io { path: path.to_owned(), source };
    let meta = std::fs::metadata(path).map_err(io)?;
    let project_file = if meta.is_dir() {
        find_project_file(path)?
    } else if path.extension().is_some_and(|e| e == BOARD_EXT) {
        let board = read_board(&read_text(path)?)?;
        let folder = path.parent().map(Path::to_owned).unwrap_or_default();
        return Ok((board, folder));
    } else {
        path.to_owned()
    };
    let project = read_project(&read_text(&project_file)?)?;
    let folder = project_file.parent().map(Path::to_owned).unwrap_or_default();
    let board = read_board(&read_text(&folder.join(&project.board))?)?;
    Ok((board, folder))
}

/// The one `.ccproj` file in a folder.
fn find_project_file(folder: &Path) -> Result<PathBuf, NativeError> {
    let io = |source| NativeError::Io { path: folder.to_owned(), source };
    let mut found = Vec::new();
    // Bounded: a folder with a huge number of entries is not a project folder.
    for entry in std::fs::read_dir(folder).map_err(io)?.take(100_000) {
        let path = entry.map_err(io)?.path();
        if path.extension().is_some_and(|e| e == PROJECT_EXT) {
            found.push(path);
        }
    }
    match found.len() {
        1 => found.pop().ok_or_else(|| NativeError::Project("no project file".to_owned())),
        0 => Err(NativeError::Project(format!("{}: no .{PROJECT_EXT} project file in this folder", folder.display()))),
        n => Err(NativeError::Project(format!("{}: {n} project files in this folder; open one of them", folder.display()))),
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

    #[test]
    fn save_and_open_a_project_folder() {
        let dir = TempDir::new("native");
        let folder = dir.0.join("Blinky");
        let b = blinky();
        let paths = save_project(&folder, &b).expect("save");
        assert_eq!(paths.project, folder.join("Blinky.ccproj"));
        assert_eq!(paths.board, folder.join("Blinky.ccpcb"));
        let first = std::fs::read(&paths.board).expect("board file");
        for path in [&folder, &paths.project, &paths.board] {
            let (back, at) = open(path).expect("open");
            assert_eq!(back, b);
            assert_eq!(at, folder);
        }
        save_project(&folder, &b).expect("save again");
        assert_eq!(std::fs::read(&paths.board).expect("board file"), first, "re-save must be byte-identical");
        let leftovers: Vec<_> = std::fs::read_dir(&folder)
            .expect("dir")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".saving"))
            .collect();
        assert!(leftovers.is_empty(), "no temporary files left behind");
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
