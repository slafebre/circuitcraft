//! `cargo xtask package`: packages each published crate and builds it from its package alone,
//! the way `cargo publish` would, without publishing anything.
//!
//! Packaging several crates of one workspace makes cargo put them in a temporary registry inside
//! the target folder and build each package against the others from there. Cargo extracts those
//! crates into its own cache (`~/.cargo/registry/src/<registry>/`) once and builds them once,
//! treating a registry crate of one version as never changing. So after the model's public API
//! changes at the same version, the import crate would be verified against the model as it was
//! the first time, and fail. Each state of the published crates therefore gets a target folder
//! named after a hash of the files their packages are made from: a new name is a new registry,
//! extracted and built afresh, and an unchanged state finds its folder again. The previous
//! state's folder is renamed to the new one rather than built anew, so the crates.io
//! dependencies built in it stay built; cargo's own cache cleaning removes its extracted copies
//! of the old states once they go unused.

use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The crates other programs build on, which the check packages.
const PUBLISHED: &[&str] = &["circuitcraft-units", "circuitcraft-geom", "circuitcraft-model", "circuitcraft-import"];

/// Packages and verifies the published crates. It reads the crates.io index, so it needs a
/// network or cargo's cached copy of the index. This catches what only shows up there: a
/// dependency on a crate that isn't published, or a file the package leaves out. Changes not yet
/// committed are packaged too, so the check runs before a commit. It asks git which files the
/// packages hold, so it needs git and a git checkout.
pub fn run(root: &Path) -> Result<(), String> {
    let targets = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), |t| root.join(t));
    let base = targets.join("package-verify");
    std::fs::create_dir_all(&base).map_err(|e| format!("can't create {}: {e}", base.display()))?;
    // Held until this returns: a second run waits, so neither renames or removes the folder the
    // other is building in.
    let lock_path = base.join("lock");
    let lock = File::create(&lock_path).map_err(|e| format!("can't create {}: {e}", lock_path.display()))?;
    lock.lock().map_err(|e| format!("can't lock {}: {e}", lock_path.display()))?;

    let name = format!("{:016x}", fingerprint(root, &package_files(root)?)?);
    let target = base.join(&name);
    // Only folders this step made, named as `name` is, are renamed or removed.
    let others: Vec<PathBuf> = std::fs::read_dir(&base)
        .map_err(|e| format!("can't list {}: {e}", base.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && *p != target && p.file_name().and_then(|n| n.to_str()).is_some_and(is_state_name))
        .collect();
    let mut others = others.into_iter();
    if !target.exists()
        && let Some(previous) = others.next()
    {
        std::fs::rename(&previous, &target).map_err(|e| format!("can't rename {}: {e}", previous.display()))?;
    }
    // Any other folders are left from before the rename; they only take space, and one that
    // can't be removed does no harm.
    for old in others {
        let _ = std::fs::remove_dir_all(old);
    }

    let mut c = crate::cargo();
    c.args(["package", "--allow-dirty", "--quiet", "--target-dir"]).arg(&target);
    for p in PUBLISHED {
        c.args(["-p", p]);
    }
    crate::run(c, "cargo package (the published crates)")
}

/// Whether a folder name is one a state of the published crates gets: 16 hexadecimal digits.
fn is_state_name(name: &str) -> bool {
    name.len() == 16 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The files the packages are made from, as git lists them (tracked, or new and not ignored,
/// which is what cargo packages): each published crate's folder, and the workspace manifest and
/// lock file that fill in their manifests.
fn package_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut git = Command::new("git");
    git.current_dir(root).args(["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "Cargo.toml", "Cargo.lock"]);
    for p in PUBLISHED {
        git.arg(format!("crates/{}", p.trim_start_matches("circuitcraft-")));
    }
    let out = git.output().map_err(|e| format!("could not start `git ls-files`: {e}"))?;
    if !out.status.success() {
        return Err(format!("`git ls-files` failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout.split(|b| *b == 0).filter(|f| !f.is_empty()).map(path_of).collect())
}

/// A path from the bytes git prints, exactly as the file system names it.
#[cfg(unix)]
fn path_of(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

/// A path from the bytes git prints, which elsewhere are UTF-8.
#[cfg(not(unix))]
fn path_of(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

/// A hash of the names and contents of these files (relative to `root`), in a fixed order. A
/// file git still lists but that is gone counts as gone.
fn fingerprint(root: &Path, files: &[PathBuf]) -> Result<u64, String> {
    let mut files = files.to_vec();
    files.sort();
    files.dedup();
    let mut hasher = DefaultHasher::new();
    for f in &files {
        f.hash(&mut hasher);
        match std::fs::read(root.join(f)) {
            Ok(bytes) => Some(bytes).hash(&mut hasher),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None::<Vec<u8>>.hash(&mut hasher),
            Err(e) => return Err(format!("can't read {}: {e}", f.display())),
        }
    }
    Ok(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_follows_names_and_contents() {
        let scratch = TempDir::new("fingerprint");
        let dir = scratch.0.clone();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src").join("lib.rs"), "pub fn a() {}").unwrap();
        let print = |files: &[&str]| fingerprint(&dir, &files.iter().map(PathBuf::from).collect::<Vec<_>>()).unwrap();

        let first = print(&["src/lib.rs"]);
        assert_eq!(print(&["src/lib.rs", "src/lib.rs"]), first, "the same files give the same name");
        std::fs::write(dir.join("src").join("lib.rs"), "pub fn a() {}\npub fn b() {}").unwrap();
        let changed = print(&["src/lib.rs"]);
        assert_ne!(changed, first, "a new public function is a new state");
        std::fs::rename(dir.join("src").join("lib.rs"), dir.join("src").join("main.rs")).unwrap();
        assert_ne!(print(&["src/main.rs"]), changed, "so is a renamed file");
        assert_ne!(print(&["src/lib.rs"]), changed, "and a deleted one");
    }

    #[test]
    fn only_folders_named_like_a_state_are_touched() {
        assert!(is_state_name("0123456789abcdef"));
        assert!(!is_state_name("lock"));
        assert!(!is_state_name("0123456789abcdeg"));
        assert!(!is_state_name("0123456789abcdef0"));
    }

    /// A scratch folder of this test's own, removed when the test ends, whether it passes or not.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("circuitcraft-xtask-package-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_package_files_are_those_git_would_hand_to_cargo() {
        let root = crate::root();
        let files = package_files(&root).unwrap();
        assert!(files.contains(&PathBuf::from("crates/model/src/lib.rs")));
        assert!(files.contains(&PathBuf::from("Cargo.lock")));
        assert!(files.iter().all(|f| !f.starts_with("target") && f.file_name().is_some_and(|n| n != ".DS_Store")));
    }
}
