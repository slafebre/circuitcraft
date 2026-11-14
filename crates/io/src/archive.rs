//! Zipped project folders: how the browser build takes a project in and hands one back.
//!
//! A browser can't give a program a folder, only files the person picks, and it can only hand a
//! file back as a download. So a project travels zipped: [`pack`] zips a project folder from a
//! [`Storage`], and [`unpack`] puts the files of a zip into one.
//!
//! A zip comes from outside and is hostile: names that would leave the folder (`..`, a root, a
//! drive) are refused, and the number of files and the bytes unpacked are bounded before they
//! are read, so a small zip that unpacks into gigabytes is refused, not written.

use crate::storage::Storage;
use std::io::{Cursor, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use zip::write::SimpleFileOptions;

/// The most files a zip may hold.
pub const MAX_FILES: usize = 1_000;

/// The most bytes the files of a zip may unpack to, together.
pub const MAX_UNPACKED: u64 = crate::native::MAX_FILE_BYTES;

/// The deepest a file in a zip may be, in folders and its own name, and the longest its name may
/// be. A project is flat and a zipped board folder a level or two deep; deeper names are refused
/// before they cost time.
pub const MAX_DEPTH: usize = 16;
pub const MAX_NAME_BYTES: usize = 1_024;

/// A zip could not be made or read.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The storage refused.
    #[error("{path}: {source}")]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The bytes are not a zip this build can read.
    #[error("not a zip that can be read: {0}")]
    Malformed(String),
    /// A file in the zip is named so it would land outside the folder it is unpacked into.
    #[error("{0:?}: a file in the zip may not lead out of its folder")]
    Escapes(String),
    /// The zip holds more than [`MAX_FILES`] files or unpacks to more than [`MAX_UNPACKED`] bytes.
    #[error(
        "the zip holds more than {MAX_FILES} files or {MAX_UNPACKED} bytes, or a name longer than {MAX_NAME_BYTES} bytes or deeper than {MAX_DEPTH}"
    )]
    TooLarge,
    /// There is nothing to zip.
    #[error("{0}: no project files to zip")]
    Empty(PathBuf),
    /// Unpacking goes into a new folder, and this one is there already.
    #[error("{0} is there already; a zip is unpacked into a new folder")]
    Exists(PathBuf),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ArchiveError + '_ {
    move |source| ArchiveError::Io { path: path.to_owned(), source }
}

fn malformed(e: impl std::fmt::Display) -> ArchiveError {
    ArchiveError::Malformed(e.to_string())
}

/// The kinds of file that make up a project, which [`pack`] zips: the project, its schematic
/// sheets and its board.
pub const PROJECT_FILES: &[&str] = &["ccproj", "ccsch", "ccpcb"];

/// The project files directly in `folder` ([`PROJECT_FILES`]), zipped under the folder's name
/// (`Blinky/Blinky.ccproj`); other files there are left out. The same files give the same bytes:
/// they go in sorted, each with the same date.
pub fn pack(storage: &dyn Storage, folder: &Path) -> Result<Vec<u8>, ArchiveError> {
    let project_file = |p: &PathBuf| p.extension().and_then(|e| e.to_str()).is_some_and(|e| PROJECT_FILES.contains(&e));
    let files: Vec<PathBuf> = storage.files(folder).map_err(io(folder))?.into_iter().filter(project_file).collect();
    if files.is_empty() {
        return Err(ArchiveError::Empty(folder.to_owned()));
    }
    if files.len() > MAX_FILES {
        return Err(ArchiveError::TooLarge);
    }
    let top = folder.file_name().and_then(|n| n.to_str()).filter(|n| !n.is_empty()).unwrap_or("project");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    // A fixed date (the earliest a zip can hold), so the same files always zip the same way.
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated).last_modified_time(zip::DateTime::default());
    let mut total: u64 = 0;
    for file in &files {
        let Some(name) = file.file_name().and_then(|n| n.to_str()) else { continue };
        let bytes = storage.read(file, MAX_UNPACKED.saturating_add(1)).map_err(io(file))?;
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_UNPACKED {
            return Err(ArchiveError::TooLarge);
        }
        zip.start_file(format!("{top}/{name}"), options).map_err(malformed)?;
        zip.write_all(&bytes).map_err(io(file))?;
    }
    Ok(zip.finish().map_err(malformed)?.into_inner())
}

/// Puts the files of a zip into `into`, a folder that must not exist yet, keeping the folders
/// they are in, and returns the folder to open: the one folder everything is in, when there is
/// one, or `into`. Folders that are only bookkeeping of the program that made the zip
/// (`__MACOSX`, names starting with a dot) are left out.
///
/// Every file is read and checked before anything is written (at most [`MAX_UNPACKED`] bytes in
/// memory), so a zip refused for what it holds writes nothing. One refused while it is written
/// (two names the system takes for one file, or a failed write) has the files it wrote removed
/// again; the folder made for the zip, and folders in it, may stay.
pub fn unpack(bytes: &[u8], storage: &dyn Storage, into: &Path) -> Result<PathBuf, ArchiveError> {
    if !new_folder_path(into) || storage.kind(into).is_ok() {
        return Err(ArchiveError::Exists(into.to_owned()));
    }
    // The zip reader lists every entry before it can be asked how many there are: refuse a zip
    // with too many first, by the mark each entry of its directory starts with.
    if central_entries(bytes) > MAX_FILES {
        return Err(ArchiveError::TooLarge);
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).map_err(malformed)?;
    if zip.len() > MAX_FILES {
        return Err(ArchiveError::TooLarge);
    }
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(malformed)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(malformed)?.into_owned();
        let Some(path) = inside(&name)? else { continue };
        let declared = entry.size();
        total = total.saturating_add(declared);
        if total > MAX_UNPACKED {
            return Err(ArchiveError::TooLarge);
        }
        let mut contents = Vec::with_capacity(usize::try_from(declared.min(1 << 20)).unwrap_or(0));
        // The size a zip declares may be a lie: read no more than it says, plus a byte to tell.
        (&mut entry).take(declared.saturating_add(1)).read_to_end(&mut contents).map_err(malformed)?;
        if contents.len() as u64 != declared {
            return Err(ArchiveError::Malformed(format!("{}: not the size the zip says", path.display())));
        }
        files.push((path, contents));
    }
    // A file can't also be a folder another file is in, and a name can't come twice. Names that
    // differ only in case count as the same: on macOS and Windows they are one file.
    let folded = |p: &Path| PathBuf::from(p.to_string_lossy().to_lowercase());
    let names: std::collections::BTreeSet<PathBuf> = files.iter().map(|(p, _)| folded(p)).collect();
    let clash = names.len() != files.len() || files.iter().any(|(p, _)| folded(p).ancestors().skip(1).any(|a| names.contains(a)));
    if clash {
        return Err(ArchiveError::Malformed("a name in the zip is used twice (whatever its case), or for a file and a folder".to_owned()));
    }
    // Made in one step nobody can come between: two callers unpacking into the same new folder
    // can't both write into it.
    storage.create_new_folder(into).map_err(|source| match source.kind() {
        std::io::ErrorKind::AlreadyExists => ArchiveError::Exists(into.to_owned()),
        _ => ArchiveError::Io { path: into.to_owned(), source },
    })?;
    let mut written: Vec<PathBuf> = Vec::with_capacity(files.len());
    for (path, contents) in &files {
        let target = into.join(path);
        // The folder is ours alone, so a file already at a name is one written here under a name
        // the system takes for the same (`ß` and `SS` on macOS, `a.` and `a` on Windows): the
        // second would replace the first.
        let wrote = if storage.kind(&target).is_ok() {
            Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "two names in the zip are one file here"))
        } else {
            target.parent().map_or(Ok(()), |parent| storage.create_folder(parent)).and_then(|()| storage.write(&target, contents))
        };
        if let Err(source) = wrote {
            for done in &written {
                let _ = storage.remove(done);
            }
            return Err(ArchiveError::Io { path: target, source });
        }
        written.push(target);
    }
    // The one folder everything is in, if there is one.
    let first = files.first().and_then(|(p, _)| p.components().next()).map(|c| c.as_os_str().to_owned());
    let common =
        first.filter(|top| files.iter().all(|(p, _)| p.components().count() > 1 && p.components().next().map(|c| c.as_os_str()) == Some(top)));
    Ok(match common {
        Some(top) => into.join(top),
        None => into.to_owned(),
    })
}

/// How many entries a zip's directory has at most: the times the mark each entry starts with
/// (`PK\x01\x02`) appears in it. A file's own bytes may hold the mark too, which only counts more.
fn central_entries(bytes: &[u8]) -> usize {
    // Counting stops one past the limit: that is enough to refuse.
    bytes.windows(4).filter(|w| *w == b"PK\x01\x02").take(MAX_FILES + 1).count()
}

/// Whether `path` can name a new folder: a root or a drive, if any, and then plain names only,
/// at least one. An empty path is the working folder on a disk, and `x/..` is the folder above a
/// missing `x`: both are there although asking for them fails, so neither may stand for a new
/// folder that nothing is in.
pub fn new_folder_path(path: &Path) -> bool {
    let plain = path.components().all(|c| matches!(c, Component::RootDir | Component::Prefix(_) | Component::Normal(_)));
    plain && path.components().any(|c| matches!(c, Component::Normal(_)))
}

/// A name from a zip as a path inside the folder it is unpacked into, or `None` for bookkeeping
/// to leave out; an error for a name that would lead out of the folder.
fn inside(name: &str) -> Result<Option<PathBuf>, ArchiveError> {
    // Zips name folders with `/`; a `\` or a drive (`C:`) means a name that isn't a plain path.
    if name.contains('\\') || name.contains(':') || name.starts_with('/') {
        return Err(ArchiveError::Escapes(name.chars().take(80).collect()));
    }
    if name.len() > MAX_NAME_BYTES || name.split('/').count() > MAX_DEPTH {
        return Err(ArchiveError::TooLarge);
    }
    let mut path = PathBuf::new();
    for part in Path::new(name).components() {
        match part {
            Component::Normal(part) => {
                let text = part.to_string_lossy();
                if text.starts_with('.') || text == "__MACOSX" {
                    return Ok(None);
                }
                path.push(part);
            }
            Component::CurDir => {}
            _ => return Err(ArchiveError::Escapes(name.chars().take(80).collect())),
        }
    }
    Ok((!path.as_os_str().is_empty()).then_some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Kind, MemoryStorage};

    /// Memory where another session gets in between: right after a folder is first found not to be
    /// there, it makes that folder and puts a file of its own in it.
    #[derive(Debug, Default)]
    struct Racing {
        memory: MemoryStorage,
        raced: std::sync::atomic::AtomicBool,
    }

    impl Storage for Racing {
        fn kind(&self, path: &Path) -> std::io::Result<Kind> {
            let kind = self.memory.kind(path);
            if kind.is_err() && !self.raced.swap(true, std::sync::atomic::Ordering::SeqCst) {
                self.memory.write(&path.join("theirs"), b"theirs")?;
            }
            kind
        }
        fn read(&self, path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
            self.memory.read(path, limit)
        }
        fn write(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
            self.memory.write(path, bytes)
        }
        fn remove(&self, path: &Path) -> std::io::Result<()> {
            self.memory.remove(path)
        }
        fn create_folder(&self, folder: &Path) -> std::io::Result<()> {
            self.memory.create_folder(folder)
        }
        fn create_new_folder(&self, folder: &Path) -> std::io::Result<()> {
            self.memory.create_new_folder(folder)
        }
        fn files(&self, folder: &Path) -> std::io::Result<Vec<PathBuf>> {
            self.memory.files(folder)
        }
        fn absolute(&self, path: &Path) -> std::io::Result<PathBuf> {
            self.memory.absolute(path)
        }
        fn recovery_folder(&self) -> Option<PathBuf> {
            None
        }
    }

    /// Memory that takes `ß` for `ss` and ignores case, as macOS does: names that are one file
    /// there are one file here.
    #[derive(Debug, Default)]
    struct Alike(MemoryStorage);

    impl Alike {
        fn fold(path: &Path) -> PathBuf {
            PathBuf::from(path.to_string_lossy().to_lowercase().replace('ß', "ss"))
        }
    }

    impl Storage for Alike {
        fn kind(&self, path: &Path) -> std::io::Result<Kind> {
            self.0.kind(&Self::fold(path))
        }
        fn read(&self, path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
            self.0.read(&Self::fold(path), limit)
        }
        fn write(&self, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
            self.0.write(&Self::fold(path), bytes)
        }
        fn remove(&self, path: &Path) -> std::io::Result<()> {
            self.0.remove(&Self::fold(path))
        }
        fn create_folder(&self, folder: &Path) -> std::io::Result<()> {
            self.0.create_folder(&Self::fold(folder))
        }
        fn create_new_folder(&self, folder: &Path) -> std::io::Result<()> {
            self.0.create_new_folder(&Self::fold(folder))
        }
        fn files(&self, folder: &Path) -> std::io::Result<Vec<PathBuf>> {
            self.0.files(&Self::fold(folder))
        }
        fn absolute(&self, path: &Path) -> std::io::Result<PathBuf> {
            self.0.absolute(path)
        }
        fn recovery_folder(&self) -> Option<PathBuf> {
            None
        }
    }

    fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).expect("start");
            zip.write_all(bytes).expect("write");
        }
        zip.finish().expect("finish").into_inner()
    }

    #[test]
    fn a_project_folder_is_zipped_under_its_name_and_comes_back_whole() {
        let memory = MemoryStorage::default();
        memory.write(Path::new("/work/Blinky/Blinky.ccproj"), b"project").expect("write");
        memory.write(Path::new("/work/Blinky/Blinky.ccpcb"), b"board").expect("write");
        memory.write(Path::new("/work/Blinky/notes.txt"), b"private").expect("write");
        let zipped = pack(&memory, Path::new("/work/Blinky")).expect("pack");
        assert_eq!(pack(&memory, Path::new("/work/Blinky")).expect("pack"), zipped, "the same files zip the same way");
        let names: Vec<String> =
            zip::ZipArchive::new(Cursor::new(zipped.as_slice())).expect("zip").file_names().map(|n| n.expect("name").into_owned()).collect();
        assert_eq!(names, ["Blinky/Blinky.ccpcb", "Blinky/Blinky.ccproj"], "only the project's files");
        let other = MemoryStorage::default();
        assert_eq!(unpack(&zipped, &other, Path::new("/received")).expect("unpack"), PathBuf::from("/received/Blinky"));
        assert_eq!(other.read(Path::new("/received/Blinky/Blinky.ccpcb"), 100).expect("read"), b"board");
        assert!(matches!(pack(&memory, Path::new("/nothing")), Err(ArchiveError::Empty(_))));
        // A folder is unpacked into once: a second time is refused and changes nothing.
        assert!(matches!(unpack(&zipped, &other, Path::new("/received")), Err(ArchiveError::Exists(_))));
        assert_eq!(other.snapshot().len(), 2);
    }

    #[test]
    fn files_at_the_top_open_from_the_folder_they_were_put_in() {
        let memory = MemoryStorage::default();
        let zipped = zip_of(&[("Blinky.ccproj", b"p"), ("Blinky.ccpcb", b"b"), ("__MACOSX/._Blinky.ccpcb", b"x"), (".DS_Store", b"x")]);
        assert_eq!(unpack(&zipped, &memory, Path::new("/received/Blinky")).expect("unpack"), PathBuf::from("/received/Blinky"));
        assert_eq!(memory.files(Path::new("/received/Blinky")).expect("files").len(), 2, "bookkeeping is left out");
        assert!(memory.kind(Path::new("/received/Blinky/__MACOSX")).is_err());
    }

    #[test]
    fn names_that_lead_out_of_the_folder_are_refused_and_nothing_is_written() {
        for name in ["../evil.ccpcb", "/etc/evil", "a/../../evil", "C:/evil", "a\\..\\evil"] {
            let memory = MemoryStorage::default();
            let zipped = zip_of(&[("good.ccpcb", b"g"), (name, b"x")]);
            assert!(matches!(unpack(&zipped, &memory, Path::new("/received")), Err(ArchiveError::Escapes(_))), "{name}");
            assert!(memory.snapshot().is_empty(), "{name}: nothing written");
        }
    }

    /// A zip of one stored file, with the size its central directory gives patched to `size`.
    fn lying_zip(size: u32) -> Vec<u8> {
        let mut bytes = zip_of(&[("a", b"0123456789")]);
        // The central directory entry: its signature, then 20 bytes to the compressed and 24 to
        // the uncompressed size.
        let at = bytes.windows(4).rposition(|w| w == b"PK\x01\x02").expect("a central directory");
        for offset in [20, 24] {
            if let Some(field) = bytes.get_mut(at + offset..at + offset + 4) {
                field.copy_from_slice(&size.to_le_bytes());
            }
        }
        bytes
    }

    #[test]
    fn hostile_zips_are_errors() {
        let memory = MemoryStorage::default();
        for junk in [&b""[..], b"PK\x03\x04", b"not a zip at all", &[0_u8; 1_000]] {
            assert!(matches!(unpack(junk, &memory, Path::new("/r")), Err(ArchiveError::Malformed(_))));
        }
        let many: Vec<(String, Vec<u8>)> = (0..=MAX_FILES).map(|i| (format!("f{i}"), Vec::new())).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(n, b)| (n.as_str(), b.as_slice())).collect();
        assert!(matches!(unpack(&zip_of(&refs), &memory, Path::new("/r")), Err(ArchiveError::TooLarge)));
        // A file that says it is larger or smaller than it is, and one that says it is larger than
        // all a zip may unpack to: refused, and nothing written.
        for size in [11, 5] {
            assert!(matches!(unpack(&lying_zip(size), &memory, Path::new("/r")), Err(ArchiveError::Malformed(_))), "{size}");
        }
        assert!(matches!(unpack(&lying_zip(u32::MAX), &memory, Path::new("/r")), Err(ArchiveError::TooLarge)));
        assert!(memory.snapshot().is_empty());
        // A name for a file and for the folder of another (a name twice the zip reader refuses).
        let zipped = zip_of(&[("p/a", b"1"), ("p/a/b", b"2")]);
        assert!(matches!(unpack(&zipped, &memory, Path::new("/r")), Err(ArchiveError::Malformed(_))));
        assert!(memory.snapshot().is_empty(), "nothing written");
        // A megabyte of zeros deflates to almost nothing; told honestly, it is fine.
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let deflated = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("a", deflated).expect("start");
        zip.write_all(&vec![0_u8; 1 << 20]).expect("write");
        let small = zip.finish().expect("finish").into_inner();
        assert!(small.len() < 10_000);
        assert!(unpack(&small, &memory, Path::new("/r")).is_ok());
        assert_eq!(memory.snapshot().len(), 1);
    }

    #[test]
    fn zips_that_would_cost_too_much_or_clash_are_refused_before_writing() {
        let memory = MemoryStorage::default();
        // Another session makes the folder between the look and the making: this unpack finds
        // it made, writes nothing, and leaves the other session's file as it was.
        let racing = Racing::default();
        assert!(matches!(unpack(&zip_of(&[("theirs", b"mine")]), &racing, Path::new("/race")), Err(ArchiveError::Exists(_))));
        assert_eq!(racing.memory.read(Path::new("/race/theirs"), 10).expect("read"), b"theirs");
        // Two names the storage takes for one file, which lowercasing alone doesn't catch: the
        // second is refused, and what the first wrote is removed.
        let alike = Alike::default();
        assert!(matches!(unpack(&zip_of(&[("p/straße", b"1"), ("p/STRASSE", b"2")]), &alike, Path::new("/u")), Err(ArchiveError::Io { .. })));
        assert!(alike.0.snapshot().is_empty(), "nothing left but folders");
        // Names deeper or longer than a project needs are refused before they cost anything.
        let deep = format!("{}f", "a/".repeat(MAX_DEPTH));
        let long = "a".repeat(MAX_NAME_BYTES + 1);
        for name in [deep.as_str(), long.as_str()] {
            assert!(matches!(unpack(&zip_of(&[(name, b"1")]), &memory, Path::new("/r")), Err(ArchiveError::TooLarge)));
        }
        assert!(unpack(&zip_of(&[(&format!("{}f", "a/".repeat(MAX_DEPTH - 1)), b"1")]), &MemoryStorage::default(), Path::new("/deep")).is_ok());
        for into in ["", ".", "/", "nothere/..", "x/y/../.."] {
            assert!(matches!(unpack(&zip_of(&[("f", b"1")]), &memory, Path::new(into)), Err(ArchiveError::Exists(_))), "{into:?}");
        }
        assert!(new_folder_path(Path::new("/received/1")) && new_folder_path(Path::new("received")));
        // Millions of directory entries are refused before the zip reader lists them.
        let marks = b"PK\x01\x02".repeat(1_000_000);
        let started = std::time::Instant::now();
        assert!(matches!(unpack(&marks, &memory, Path::new("/r")), Err(ArchiveError::TooLarge)));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        // Names that differ only in case are one file on macOS and Windows.
        let cased = zip_of(&[("p/A.ccpcb", b"1"), ("p/a.ccpcb", b"2")]);
        assert!(matches!(unpack(&cased, &memory, Path::new("/r")), Err(ArchiveError::Malformed(_))));
        let folder_cased = zip_of(&[("p/A", b"1"), ("p/a/b", b"2")]);
        assert!(matches!(unpack(&folder_cased, &memory, Path::new("/r")), Err(ArchiveError::Malformed(_))));
        assert!(memory.snapshot().is_empty());
    }
}
