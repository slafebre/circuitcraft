//! Every asset file (images, icons, fonts, sounds, 3D models) must have a row in ATTRIBUTION.md
//! naming its author, source and licence. This keeps the clean-room promise checkable.

use std::path::{Path, PathBuf};

/// File extensions that count as assets.
const ASSET_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "svg", "ico", "icns", "bmp", "tif", "tiff", "ttf", "otf", "ttc", "woff", "woff2", "wav", "mp3", "ogg",
    "flac", "step", "stp", "wrl", "glb", "gltf", "obj",
];

/// Directories never scanned.
const SKIP_DIRS: &[&str] = &[".git", "target", "dist", "plan", ".claude"];

/// How deep the scan goes; the repository is never this deep, so this only stops a symlink loop.
const MAX_DEPTH: usize = 32;

pub fn run(root: &Path) -> Result<(), String> {
    let attribution = std::fs::read_to_string(root.join("ATTRIBUTION.md")).map_err(|e| format!("ATTRIBUTION.md: {e}"))?;
    let mut assets = Vec::new();
    collect(root, root, 0, &mut assets)?;
    let missing: Vec<&String> = assets.iter().filter(|a| !attribution.contains(&format!("`{a}`"))).collect();
    if missing.is_empty() {
        println!("assets: {} asset file(s), all listed in ATTRIBUTION.md", assets.len());
        Ok(())
    } else {
        for m in &missing {
            eprintln!("  not in ATTRIBUTION.md: {m}");
        }
        Err(format!("{} asset file(s) without an ATTRIBUTION.md row (list each as `path`)", missing.len()))
    }
}

fn collect(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path: PathBuf = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                collect(root, &path, depth + 1, out)?;
            }
        } else if kind.is_file() && is_asset(&path) {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

fn is_asset(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| ASSET_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}
