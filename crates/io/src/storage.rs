//! Where files live.
//!
//! Everything that reads or writes a design goes through a [`Storage`], so the same code opens
//! and saves projects on a disk ([`DiskStorage`], the desktop) and in memory ([`MemoryStorage`],
//! the browser, where a project is a zipped folder that is unpacked into memory, and tests).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Error, ErrorKind, Read as _, Result, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// What a path leads to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A regular file of this many bytes.
    File(u64),
    /// A folder.
    Folder,
    /// Something else: a pipe, a device. Reading one can block forever, so nothing reads them.
    Other,
}

/// The most entries of a folder [`Storage::files`] looks at. A folder with more is not a project
/// folder.
pub const MAX_FOLDER_ENTRIES: usize = 100_000;

/// A place that holds files in folders. It can be handed to another thread, so a session can be
/// run from one.
pub trait Storage: std::fmt::Debug + Send + Sync {
    /// What `path` leads to, following links; an error when nothing is there.
    fn kind(&self, path: &Path) -> Result<Kind>;

    /// The first `limit` bytes of a file (all of it when it is shorter). A caller that must not
    /// read a larger file asks for one byte more than it accepts and checks what it got.
    fn read(&self, path: &Path, limit: u64) -> Result<Vec<u8>>;

    /// Writes a file, replacing the one that is there. A failed write leaves the old file as it
    /// was, and nothing half-written is ever at `path`.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()>;

    /// Removes a file. Removing one that isn't there is not an error.
    fn remove(&self, path: &Path) -> Result<()>;

    /// Makes a folder, and the folders above it, unless they exist.
    fn create_folder(&self, folder: &Path) -> Result<()>;

    /// Makes a folder that must not exist yet, and the folders above it as needed, in one step
    /// nobody else can come between: when two callers ask for the same new folder, one gets it
    /// and the other an `AlreadyExists` error. Something that is there already, a folder or a
    /// file, is an error too.
    fn create_new_folder(&self, folder: &Path) -> Result<()>;

    /// The regular files directly in a folder, sorted, and no more than [`MAX_FOLDER_ENTRIES`] of
    /// what it holds. A folder that doesn't exist has none; a file is not a folder.
    fn files(&self, folder: &Path) -> Result<Vec<PathBuf>>;

    /// `path` from the root, without resolving links, so that a file opened by a relative path
    /// is still found after the working folder changes.
    fn absolute(&self, path: &Path) -> Result<PathBuf>;

    /// The folder for copies of unsaved work, kept apart from every project so that none of it
    /// ends up under version control; `None` when there is nowhere to keep them.
    fn recovery_folder(&self) -> Option<PathBuf>;
}

/// A storage that is shared: a front end keeps one handle (the browser build, to zip the files
/// up for the user) and gives the session another.
impl<S: Storage + ?Sized> Storage for std::sync::Arc<S> {
    fn kind(&self, path: &Path) -> Result<Kind> {
        (**self).kind(path)
    }
    fn read(&self, path: &Path, limit: u64) -> Result<Vec<u8>> {
        (**self).read(path, limit)
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        (**self).write(path, bytes)
    }
    fn remove(&self, path: &Path) -> Result<()> {
        (**self).remove(path)
    }
    fn create_folder(&self, folder: &Path) -> Result<()> {
        (**self).create_folder(folder)
    }
    fn create_new_folder(&self, folder: &Path) -> Result<()> {
        (**self).create_new_folder(folder)
    }
    fn files(&self, folder: &Path) -> Result<Vec<PathBuf>> {
        (**self).files(folder)
    }
    fn absolute(&self, path: &Path) -> Result<PathBuf> {
        (**self).absolute(path)
    }
    fn recovery_folder(&self) -> Option<PathBuf> {
        (**self).recovery_folder()
    }
}

/// The file system of the machine.
#[derive(Debug, Clone)]
pub struct DiskStorage {
    recovery: Option<PathBuf>,
}

impl Default for DiskStorage {
    fn default() -> Self {
        DiskStorage::new()
    }
}

impl DiskStorage {
    /// The disk, with unsaved work kept in the user's own state folder for CircuitCraft:
    /// `~/Library/Application Support/CircuitCraft/Recovery` on macOS,
    /// `%LOCALAPPDATA%\CircuitCraft\Recovery` on Windows, and
    /// `$XDG_STATE_HOME/circuitcraft/recovery` (or `~/.local/state/…`) elsewhere.
    pub fn new() -> DiskStorage {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from).filter(|p| p.is_absolute());
        let recovery = if cfg!(target_os = "macos") {
            var("HOME").map(|home| home.join("Library/Application Support/CircuitCraft/Recovery"))
        } else if cfg!(windows) {
            var("LOCALAPPDATA").map(|data| data.join("CircuitCraft").join("Recovery"))
        } else {
            var("XDG_STATE_HOME").or_else(|| var("HOME").map(|home| home.join(".local/state"))).map(|state| state.join("circuitcraft/recovery"))
        };
        DiskStorage { recovery }
    }

    /// The disk, with unsaved work kept in `recovery` (or nowhere).
    pub const fn with_recovery(recovery: Option<PathBuf>) -> DiskStorage {
        DiskStorage { recovery }
    }
}

impl Storage for DiskStorage {
    fn kind(&self, path: &Path) -> Result<Kind> {
        let meta = std::fs::metadata(path)?;
        Ok(if meta.is_file() {
            Kind::File(meta.len())
        } else if meta.is_dir() {
            Kind::Folder
        } else {
            Kind::Other
        })
    }

    fn read(&self, path: &Path, limit: u64) -> Result<Vec<u8>> {
        // Checked before opening: opening a pipe or a device can block forever.
        let Kind::File(len) = self.kind(path)? else {
            return Err(Error::new(ErrorKind::InvalidInput, "not a regular file"));
        };
        let file = std::fs::File::open(path)?;
        // The size sets the first allocation; `take` still bounds a file that grows meanwhile.
        let mut bytes = Vec::with_capacity(usize::try_from(len.min(limit)).unwrap_or(0));
        file.take(limit).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Writes by way of a temporary file in the same folder, which then replaces the file:
    ///
    /// - A crash, a full disk or a failed write leaves the old file untouched, and the temporary
    ///   file is removed on every error path.
    /// - The temporary file is created new (`create_new` refuses any existing path, symlinks
    ///   included) with a random name, so a link planted in the folder can't redirect the write,
    ///   and two saves at once don't share a file.
    /// - The data is flushed to disk before the rename (on macOS `sync_all` is a full flush). On
    ///   Unix the folder is flushed after it too, so a power cut leaves the old file or the new
    ///   one; other systems don't offer that flush, so the rename itself may be lost there.
    /// - The file is replaced, not rewritten: a file that is a symlink becomes a regular file,
    ///   and the new file gets default permissions.
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        // A folder is never replaced by a file, and neither is a link that leads to one.
        // Renaming onto a folder is refused on Unix; asking first gives the same answer where it
        // might not be.
        if matches!(self.kind(path), Ok(Kind::Folder)) {
            return Err(Error::new(ErrorKind::IsADirectory, "a folder is in the way"));
        }
        // 128 random bits from the system (an item id is just that): a name nothing else has, and
        // one that doesn't depend on the target's name, so a long file name still fits.
        let tag = circuitcraft_model::ItemId::random().map_err(|e| Error::other(e.to_string()))?.as_u128();
        let temp = path.with_file_name(format!(".circuitcraft-{tag:032x}.saving"));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
        let written = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(e) = written.and_then(|()| std::fs::rename(&temp, path)) {
            let _ = std::fs::remove_file(&temp);
            return Err(e);
        }
        // Flushes the folder's entries after the rename, on Unix (other systems can't). A failure
        // here doesn't undo a save that has already happened, so it is ignored.
        #[cfg(unix)]
        if let Some(folder) = path.parent().filter(|f| !f.as_os_str().is_empty()) {
            let _ = std::fs::File::open(folder).and_then(|f| f.sync_all());
        }
        Ok(())
    }

    fn remove(&self, path: &Path) -> Result<()> {
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn create_folder(&self, folder: &Path) -> Result<()> {
        std::fs::create_dir_all(folder)
    }

    fn create_new_folder(&self, folder: &Path) -> Result<()> {
        if let Some(parent) = folder.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        // The system makes the folder or reports it is there, in one step.
        std::fs::create_dir(folder)
    }

    fn files(&self, folder: &Path) -> Result<Vec<PathBuf>> {
        // Asked first, so that listing a file is refused on every system: what listing one
        // gives differs between them.
        match self.kind(folder) {
            Ok(Kind::Folder) => {}
            Ok(_) => return Err(Error::new(ErrorKind::NotADirectory, "not a folder")),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        }
        let entries = match std::fs::read_dir(folder) {
            Ok(entries) => entries,
            // Removed since it was asked about: a folder that isn't there has no files.
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut found = Vec::new();
        for entry in entries.take(MAX_FOLDER_ENTRIES) {
            let path = entry?.path();
            if path.is_file() {
                found.push(path);
            }
        }
        found.sort();
        Ok(found)
    }

    fn absolute(&self, path: &Path) -> Result<PathBuf> {
        std::path::absolute(path)
    }

    fn recovery_folder(&self) -> Option<PathBuf> {
        self.recovery.clone()
    }
}

/// Files held in memory, for the browser and for tests. Paths are absolute from `/`; a relative
/// path is taken from `/`.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    inner: Mutex<Memory>,
    recovery: Option<PathBuf>,
}

#[derive(Debug, Default)]
struct Memory {
    files: BTreeMap<PathBuf, Vec<u8>>,
    /// Folders made on their own; a folder that holds a file exists without being listed here.
    folders: BTreeSet<PathBuf>,
}

impl Memory {
    /// Whether `path` is a folder: one that was made, or one that something lies under.
    fn is_folder(&self, path: &Path) -> bool {
        // Everything under a path sorts together, right after the path itself.
        let under = |first: Option<&PathBuf>| first.is_some_and(|p| p.starts_with(path));
        path == Path::new("/")
            || under(self.folders.range(path.to_owned()..).next())
            || under(self.files.range(path.to_owned()..).find(|(p, _)| p.as_path() != path).map(|(p, _)| p))
    }

    /// Refuses a path that leads through a file, as a disk does.
    fn check_way(&self, path: &Path) -> Result<()> {
        match path.ancestors().skip(1).any(|a| self.files.contains_key(a)) {
            true => Err(Error::new(ErrorKind::NotADirectory, "a file is in the way")),
            false => Ok(()),
        }
    }
}

impl MemoryStorage {
    /// Memory that also keeps unsaved work, in `/recovery`. Plain memory (the default) keeps
    /// none: what it holds is gone when the program is, which is when unsaved work is wanted.
    pub fn with_recovery() -> MemoryStorage {
        MemoryStorage { recovery: Some(PathBuf::from("/recovery")), ..MemoryStorage::default() }
    }

    /// A panic while the lock is held can't leave the maps half-changed (every change is one
    /// insert or remove), so a poisoned lock is still good.
    fn lock(&self) -> MutexGuard<'_, Memory> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Every file, by path: what a browser build zips up for the user to keep.
    pub fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        self.lock().files.clone()
    }
}

/// `path` from `/`, with `.` and `..` resolved by name. The names are joined with `/` on every
/// system, and never pushed onto a path: on Windows, pushing a name that reads as a drive
/// (`C:`) would replace everything before it.
fn rooted(path: &Path) -> PathBuf {
    let mut names = Vec::new();
    for part in path.components() {
        match part {
            std::path::Component::Normal(name) => names.push(name),
            std::path::Component::ParentDir => {
                names.pop();
            }
            _ => {}
        }
    }
    let mut out = std::ffi::OsString::from("/");
    for (i, name) in names.into_iter().enumerate() {
        if i > 0 {
            out.push("/");
        }
        out.push(name);
    }
    PathBuf::from(out)
}

impl Storage for MemoryStorage {
    fn kind(&self, path: &Path) -> Result<Kind> {
        let path = rooted(path);
        let memory = self.lock();
        match memory.files.get(&path) {
            Some(bytes) => Ok(Kind::File(bytes.len() as u64)),
            None if memory.is_folder(&path) => Ok(Kind::Folder),
            None => Err(Error::new(ErrorKind::NotFound, "no such file or folder")),
        }
    }

    fn read(&self, path: &Path, limit: u64) -> Result<Vec<u8>> {
        let memory = self.lock();
        let bytes = memory.files.get(&rooted(path)).ok_or_else(|| Error::new(ErrorKind::NotFound, "no such file"))?;
        let take = usize::try_from(limit).unwrap_or(usize::MAX).min(bytes.len());
        Ok(bytes.get(..take).unwrap_or_default().to_vec())
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let path = rooted(path);
        let mut memory = self.lock();
        memory.check_way(&path)?;
        if memory.is_folder(&path) {
            return Err(Error::new(ErrorKind::IsADirectory, "a folder is in the way"));
        }
        memory.files.insert(path, bytes.to_vec());
        Ok(())
    }

    fn remove(&self, path: &Path) -> Result<()> {
        self.lock().files.remove(&rooted(path));
        Ok(())
    }

    fn create_folder(&self, folder: &Path) -> Result<()> {
        let folder = rooted(folder);
        let mut memory = self.lock();
        if memory.files.contains_key(&folder) {
            return Err(Error::new(ErrorKind::AlreadyExists, "a file is in the way"));
        }
        memory.check_way(&folder)?;
        memory.folders.insert(folder);
        Ok(())
    }

    fn create_new_folder(&self, folder: &Path) -> Result<()> {
        let folder = rooted(folder);
        // Asked and made under one lock: nobody can make it in between.
        let mut memory = self.lock();
        if memory.files.contains_key(&folder) || memory.is_folder(&folder) {
            return Err(Error::new(ErrorKind::AlreadyExists, "there already"));
        }
        memory.check_way(&folder)?;
        memory.folders.insert(folder);
        Ok(())
    }

    fn files(&self, folder: &Path) -> Result<Vec<PathBuf>> {
        let folder = rooted(folder);
        let memory = self.lock();
        if memory.files.contains_key(&folder) {
            return Err(Error::new(ErrorKind::NotADirectory, "not a folder"));
        }
        let under = memory.files.range(folder.clone()..).map(|(p, _)| p).take_while(|p| p.starts_with(&folder));
        Ok(under.filter(|p| p.parent() == Some(folder.as_path())).take(MAX_FOLDER_ENTRIES).cloned().collect())
    }

    fn absolute(&self, path: &Path) -> Result<PathBuf> {
        Ok(rooted(path))
    }

    fn recovery_folder(&self) -> Option<PathBuf> {
        self.recovery.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of our own under the system temp folder, removed when dropped.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
            let dir = std::env::temp_dir().join(format!("circuitcraft-{tag}-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("temp dir");
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// What every storage must do, run on a folder of its own.
    fn behaves_like_storage(s: &dyn Storage, root: &Path) {
        let folder = root.join("project");
        let file = folder.join("a.txt");
        assert!(s.kind(&file).is_err(), "nothing there yet");
        assert_eq!(s.files(&folder).expect("files"), Vec::<PathBuf>::new(), "a folder that doesn't exist has no files");
        // The folders above the one that is made are made with it.
        s.create_folder(&folder.join("sub/below")).expect("folders");
        s.create_folder(&folder).expect("making a folder twice is fine");
        assert_eq!(s.kind(&folder).ok(), Some(Kind::Folder));
        assert_eq!(s.kind(&folder.join("sub")).ok(), Some(Kind::Folder));
        s.write(&file, b"hello").expect("write");
        s.write(&folder.join("sub/deep.txt"), b"deep").expect("write");
        s.write(&folder.join("b.txt"), b"").expect("write");
        assert_eq!(s.kind(&file).ok(), Some(Kind::File(5)));
        assert_eq!(s.read(&file, 100).ok().as_deref(), Some(&b"hello"[..]));
        assert_eq!(s.read(&file, 4).ok().as_deref(), Some(&b"hell"[..]), "no more than the limit");
        assert_eq!(s.read(&file, 0).ok().as_deref(), Some(&b""[..]));
        s.write(&file, b"replaced").expect("replace");
        assert_eq!(s.read(&file, u64::MAX).ok().as_deref(), Some(&b"replaced"[..]));
        // Only the files directly in the folder, sorted; not the folder in it or what that holds.
        assert_eq!(s.files(&folder).expect("files"), [file.clone(), folder.join("b.txt")]);
        assert!(s.read(&folder, 10).is_err(), "a folder is not read");
        assert!(s.read(&folder.join("missing.txt"), 10).is_err());
        assert!(s.write(&folder.join("sub"), b"x").is_err(), "a folder is not replaced by a file");
        assert!(s.write(&folder.join("sub/below"), b"x").is_err(), "nor is an empty one");
        // A file is not a folder: nothing is written or made under it, and it holds no files.
        assert!(s.write(&folder.join("b.txt/c.txt"), b"x").is_err());
        assert!(s.create_folder(&folder.join("b.txt/sub")).is_err());
        assert!(s.create_folder(&folder.join("b.txt")).is_err());
        assert!(s.files(&folder.join("b.txt")).is_err());
        assert_eq!(s.kind(&folder.join("sub/below")).ok(), Some(Kind::Folder));
        assert_eq!(s.files(&folder.join("sub/below")).expect("files"), Vec::<PathBuf>::new(), "an empty folder");
        s.remove(&file).expect("remove");
        s.remove(&file).expect("removing what isn't there is fine");
        assert!(s.kind(&file).is_err());
        assert_eq!(s.files(&folder).expect("files"), [folder.join("b.txt")]);
        // A new folder is made once: asked for again, or where a file or a folder is, it is refused.
        let fresh = folder.join("new/inner");
        s.create_new_folder(&fresh).expect("a new folder, and the one above it");
        assert!(matches!(s.kind(&fresh), Ok(Kind::Folder)));
        assert_eq!(s.create_new_folder(&fresh).map_err(|e| e.kind()), Err(ErrorKind::AlreadyExists));
        assert!(s.create_new_folder(&folder.join("new")).is_err(), "a folder that is there");
        assert!(s.create_new_folder(&folder.join("b.txt")).is_err(), "a file that is there");
        // From the root: on Windows a path in memory has a root and no drive, which
        // `is_absolute` there doesn't count as absolute.
        assert!(s.absolute(Path::new("x/y.txt")).is_ok_and(|p| p.has_root() && p.ends_with("x/y.txt")));
    }

    #[test]
    fn the_disk_behaves_like_storage() {
        let dir = TempDir::new("storage");
        let disk = DiskStorage::with_recovery(Some(dir.0.join("recovery")));
        behaves_like_storage(&disk, &dir.0);
        assert_eq!(disk.recovery_folder(), Some(dir.0.join("recovery")));
        assert!(DiskStorage::with_recovery(None).recovery_folder().is_none());
        // Nothing temporary is left behind.
        let left: Vec<_> = std::fs::read_dir(dir.0.join("project")).expect("dir").filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert!(left.iter().all(|n| !n.to_string_lossy().ends_with(".saving")), "{left:?}");
    }

    #[test]
    fn memory_behaves_like_storage() {
        // Through a shared handle, as a front end holds it.
        let memory = std::sync::Arc::new(MemoryStorage::with_recovery());
        behaves_like_storage(&memory.clone(), Path::new("/work"));
        assert_eq!(memory.recovery_folder(), Some(PathBuf::from("/recovery")));
        assert_eq!(MemoryStorage::default().recovery_folder(), None, "plain memory outlives nothing, so it keeps no unsaved work");
        // Paths are taken from the root, and can't climb out of it.
        memory.write(Path::new("rel.txt"), b"1").expect("write");
        assert_eq!(memory.read(Path::new("/x/../../rel.txt"), 9).ok().as_deref(), Some(&b"1"[..]));
        assert_eq!(memory.kind(Path::new("/")).ok(), Some(Kind::Folder));
        assert!(memory.create_folder(Path::new("/rel.txt/sub")).is_err(), "a file is in the way");
        // A name that reads as a drive on Windows is a name like any other when it is not the
        // first: what comes before it stays, and the path stays under the root.
        for drive in ["/x/C:/y", "/x/C:y"] {
            assert!(memory.absolute(Path::new(drive)).is_ok_and(|p| p.has_root() && p.starts_with("/x")), "{drive}");
        }
        assert!(memory.absolute(Path::new("C:/x/y")).is_ok_and(|p| p.has_root() && p.ends_with("x/y")));
        assert_eq!(memory.absolute(Path::new("a/./b/../c")).ok(), Some(PathBuf::from("/a/c")));
        // A folder with a similar name is another folder.
        memory.write(Path::new("/a/bc/x.txt"), b"1").expect("write");
        assert!(memory.kind(Path::new("/a/b")).is_err());
        assert_eq!(memory.files(Path::new("/a/b")).expect("files"), Vec::<PathBuf>::new());
        memory.remove(Path::new("/a/bc/x.txt")).expect("remove");
        assert_eq!(
            memory.snapshot().keys().cloned().collect::<Vec<_>>(),
            [PathBuf::from("/rel.txt"), "/work/project/b.txt".into(), "/work/project/sub/deep.txt".into()]
        );
    }

    #[test]
    fn a_crowded_folder_below_does_not_hide_the_files_of_a_folder() {
        let memory = MemoryStorage::default();
        for i in 0..=MAX_FOLDER_ENTRIES {
            memory.write(&PathBuf::from(format!("/p/aaa/{i}")), b"").expect("write");
        }
        memory.write(Path::new("/p/p.ccproj"), b"").expect("write");
        assert_eq!(memory.files(Path::new("/p")).expect("files"), [PathBuf::from("/p/p.ccproj")]);
        // And no more than the limit of what the folder itself holds.
        assert_eq!(memory.files(Path::new("/p/aaa")).expect("files").len(), MAX_FOLDER_ENTRIES);
    }

    #[test]
    fn storage_can_be_handed_to_another_thread() {
        fn sendable<T: Send + Sync>() {}
        sendable::<Box<dyn Storage>>();
    }

    #[cfg(unix)]
    #[test]
    fn the_disk_reads_only_regular_files() {
        let disk = DiskStorage::with_recovery(None);
        assert_eq!(disk.kind(Path::new("/dev/null")).ok(), Some(Kind::Other));
        assert!(disk.read(Path::new("/dev/null"), 10).is_err());
    }

    /// A fixed temporary name (`.<file>.saving`) would let a symlink planted there redirect the
    /// write. Temporary names are random and created with `create_new`; this checks that a link
    /// at such a name, and a dangling one, are left alone.
    #[cfg(unix)]
    #[test]
    fn a_symlink_cannot_redirect_a_write() {
        let dir = TempDir::new("storage-symlink");
        let disk = DiskStorage::with_recovery(None);
        let target = dir.0.join("board.ccpcb");
        let victim = dir.0.join("victim.txt");
        std::fs::write(&victim, "precious").expect("write");
        std::os::unix::fs::symlink(&victim, dir.0.join(".board.ccpcb.saving")).expect("symlink");
        std::os::unix::fs::symlink(dir.0.join("nowhere"), dir.0.join(".dangling.saving")).expect("symlink");
        // The target itself is a link: it is replaced by a regular file, and what it pointed at stays.
        std::os::unix::fs::symlink(&victim, &target).expect("symlink");
        disk.write(&target, b"board").expect("write");
        assert_eq!(std::fs::read_to_string(&victim).expect("victim"), "precious");
        assert!(std::fs::symlink_metadata(&target).expect("target").file_type().is_file());
        assert!(!dir.0.join("nowhere").exists());
    }
}
