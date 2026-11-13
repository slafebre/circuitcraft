//! `cargo xtask corpus`: the public boards the importers are tested on.
//!
//! `corpus/manifest.json` pins each board by repository, commit, path and SHA-256. The designs
//! themselves are never committed and never redistributed: this command fetches them from where
//! their authors publish them, into `target/corpus/`, checks each file against its checksum,
//! imports it, and compares what came in with the counts the manifest expects. A board that fails
//! to import, or comes in with other counts than a person reviewed, fails the command.
//!
//! The manifest also has a place for the checksum of each board's reviewed render; it stays
//! empty until there is a renderer to make one.

use circuitcraft_model::{Board, NetId, Pad, PadShape};
use circuitcraft_units::Point;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The largest design file fetched or read.
const MAX_BYTES: u64 = 128 * 1024 * 1024;

/// One board of the manifest.
struct Entry {
    name: String,
    repo: String,
    commit: String,
    path: String,
    sha256: String,
    /// `kicad` or `eagle`.
    format: String,
    /// The counts a person reviewed, or `None` for a board that has none yet.
    counts: Option<Map<String, Value>>,
}

pub fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let offline = args.iter().any(|a| a == "--offline");
    let only: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    if let Some(unknown) = args.iter().find(|a| a.starts_with("--") && *a != "--offline") {
        return Err(format!("unknown option `{unknown}`; usage: cargo xtask corpus [--offline] [board ...]"));
    }
    let manifest = root.join("corpus").join("manifest.json");
    let text = std::fs::read_to_string(&manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let entries = entries(&text).map_err(|e| format!("{}: {e}", manifest.display()))?;
    if let Some(missing) = only.iter().find(|name| !entries.iter().any(|e| e.name == ***name)) {
        return Err(format!("no board `{missing}` in the manifest"));
    }
    let mut failed = 0_usize;
    for entry in entries.iter().filter(|e| only.is_empty() || only.iter().any(|name| **name == e.name)) {
        match check(root, entry, offline) {
            Ok(counts) => println!("ok    {:<22} {}", entry.name, summary(&counts)),
            Err(why) => {
                failed += 1;
                println!("FAIL  {:<22} {why}", entry.name);
            }
        }
    }
    if failed == 0 { Ok(()) } else { Err(format!("{failed} corpus board(s) failed")) }
}

/// The boards of a manifest, each field checked: what they say ends up in a URL, a file name and
/// the arguments of a program.
fn entries(text: &str) -> Result<Vec<Entry>, String> {
    let manifest: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let boards = manifest.get("boards").and_then(Value::as_array).ok_or("no `boards` list")?;
    let mut entries = Vec::with_capacity(boards.len());
    for board in boards {
        let field = |name: &str| board.get(name).and_then(Value::as_str).map(str::to_owned).ok_or(format!("a board without `{name}`"));
        let entry = Entry {
            name: field("name")?,
            repo: field("repo")?,
            commit: field("commit")?,
            path: field("path")?,
            sha256: field("sha256")?,
            format: field("format")?,
            counts: board.get("counts").and_then(Value::as_object).cloned(),
        };
        // Letters and digits, and after the first character a few marks: nothing that could be
        // read as an option, a parent folder or a host.
        let plain = |s: &str, extra: &str| {
            s.starts_with(|c: char| c.is_ascii_alphanumeric()) && s.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        let hex = |s: &str, len: usize| s.len() == len && s.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'));
        let repo_ok = entry.repo.split('/').count() == 2 && entry.repo.split('/').all(|part| plain(part, "-_."));
        let path_ok =
            !entry.path.is_empty() && entry.path.split('/').all(|part| !part.is_empty() && part != "." && part != "..") && !entry.path.contains('\\');
        let format_ok = matches!(entry.format.as_str(), "kicad" | "eagle");
        if !plain(&entry.name, "-") || !repo_ok || !hex(&entry.commit, 40) || !hex(&entry.sha256, 64) || !path_ok || !format_ok {
            return Err(format!("board `{}`: a name, repository, commit, path, checksum or format is not in the form the manifest uses", entry.name));
        }
        if entries.iter().any(|e: &Entry| e.name == entry.name) {
            return Err(format!("board `{}` is listed twice", entry.name));
        }
        entries.push(entry);
    }
    Ok(entries)
}

/// Fetches (unless it is there already), verifies, imports and counts one board.
fn check(root: &Path, entry: &Entry, offline: bool) -> Result<Map<String, Value>, String> {
    let file = file_of(root, entry);
    if !file.is_file() {
        if offline {
            return Err("not fetched yet (run without --offline)".to_owned());
        }
        fetch(entry, &file)?;
    }
    let bytes = read(&file)?;
    let found = hex(&Sha256::digest(&bytes));
    if found != entry.sha256 {
        return Err(format!("checksum {found}, expected {}; delete {} to fetch it again", entry.sha256, file.display()));
    }
    let text = String::from_utf8_lossy(&bytes);
    let imported = circuitcraft_io::import_board(&text, &entry.name).map_err(|e| format!("import failed: {e}"))?;
    let counts = counts(&imported.board);
    match &entry.counts {
        Some(expected) if *expected == counts => Ok(counts),
        Some(expected) => Err(format!("counts differ from the manifest: {}", differences(expected, &counts))),
        None => Err(format!("the manifest has no reviewed counts yet; it came in as {}", Value::Object(counts))),
    }
}

/// Where a board's file is kept: `target/corpus/<name>/board.kicad_pcb` or `board.brd`. The
/// name on disk is ours, not the repository's, so no path in a manifest names a place on disk.
fn file_of(root: &Path, entry: &Entry) -> PathBuf {
    let file_name = if entry.format == "kicad" { "board.kicad_pcb" } else { "board.brd" };
    root.join("target").join("corpus").join(&entry.name).join(file_name)
}

/// Downloads a board from its repository at the pinned commit, with `curl`.
fn fetch(entry: &Entry, file: &Path) -> Result<(), String> {
    let folder = file.parent().ok_or("no folder")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let url = format!("https://raw.githubusercontent.com/{}/{}/{}", entry.repo, entry.commit, percent_encoded(&entry.path));
    let part = file.with_extension("part");
    let status = Command::new("curl")
        // No redirects: the file is where the manifest says, or the fetch fails.
        .args(["--fail", "--silent", "--show-error", "--proto", "=https", "--max-filesize", &MAX_BYTES.to_string(), "--output"])
        .arg(&part)
        .arg(&url)
        .status()
        .map_err(|e| format!("could not start curl: {e}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("could not fetch {url} ({status})"));
    }
    std::fs::rename(&part, file).map_err(|e| format!("{}: {e}", file.display()))
}

/// A path for a URL: everything but letters, digits, `-`, `.`, `_`, `~` and `/` as `%XX`.
fn percent_encoded(path: &str) -> String {
    path.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) { char::from(b).to_string() } else { format!("%{b:02X}") }).collect()
}

fn read(file: &Path) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    if size > MAX_BYTES {
        return Err(format!("{}: larger than {MAX_BYTES} bytes", file.display()));
    }
    std::fs::read(file).map_err(|e| format!("{}: {e}", file.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the manifest records of an imported board.
fn counts(board: &Board) -> Map<String, Value> {
    let counts = json!({
        "copper": board.stack.copper_count(),
        "nets": board.nets.len(),
        "footprints": board.footprints.len(),
        "pads": board.footprints.iter().map(|f| f.pads.len()).sum::<usize>(),
        "pads_reached": pads_reached(board),
        "tracks": board.tracks.len(),
        "vias": board.vias.len(),
        "zones": board.zones.len(),
        "keepouts": board.keepouts.len(),
        "holes": board.holes.len(),
        "texts": board.texts.len(),
        "drawings": board.graphics.len(),
        "outline": board.outline.is_some(),
        "cutouts": board.cutouts.len(),
    });
    counts.as_object().cloned().unwrap_or_default()
}

/// How many pads have a track end or a via of their own net on them. Every other count stays the
/// same when footprints are placed turned or mirrored the wrong way; this one drops to almost
/// nothing.
fn pads_reached(board: &Board) -> usize {
    let mut ends: BTreeMap<NetId, Vec<Point>> = BTreeMap::new();
    for track in board.tracks.iter() {
        if let Some(net) = track.net {
            let (from, to) = track.path.ends();
            ends.entry(net).or_default().extend([from, to]);
        }
    }
    for via in board.vias.iter() {
        if let Some(net) = via.net {
            ends.entry(net).or_default().push(via.position);
        }
    }
    let reached = |at: Point, pad: &Pad| {
        let reach = reach(pad);
        let on = |end: &Point| (end.x.0 - at.x.0).abs() <= reach && (end.y.0 - at.y.0).abs() <= reach;
        pad.net.and_then(|net| ends.get(&net)).is_some_and(|ends| ends.iter().any(on))
    };
    board.footprints.iter().flat_map(|f| f.pads.iter().filter(move |pad| f.to_board(pad.position).is_some_and(|at| reached(at, pad)))).count()
}

/// How far from its middle a pad's copper goes at most: half its larger side.
fn reach(pad: &Pad) -> i64 {
    let shape = pad.front.as_ref().or(pad.back.as_ref()).map(|face| &face.shape).or(pad.inner.as_ref());
    match shape {
        Some(PadShape::Circle { diameter }) => diameter.0 / 2,
        Some(PadShape::Rect { width, height } | PadShape::RoundRect { width, height, .. } | PadShape::Oval { width, height }) => {
            width.0.max(height.0) / 2
        }
        Some(PadShape::Custom { outline }) => {
            let corners = std::iter::once(outline.start).chain(outline.edges.iter().map(circuitcraft_geom::Edge::end));
            corners.map(|p| p.x.0.abs().max(p.y.0.abs())).max().unwrap_or(0)
        }
        None => pad.hole.map_or(0, |hole| hole.diameter.0 / 2),
    }
}

/// `tracks 806 (expected 800), vias 74 (expected 70)`.
fn differences(expected: &Map<String, Value>, found: &Map<String, Value>) -> String {
    let keys: std::collections::BTreeSet<&String> = expected.keys().chain(found.keys()).collect();
    let differing: Vec<String> = keys
        .into_iter()
        .filter(|key| expected.get(*key) != found.get(*key))
        .map(|key| format!("{key} {} (expected {})", found.get(key).unwrap_or(&Value::Null), expected.get(key).unwrap_or(&Value::Null)))
        .collect();
    differing.join(", ")
}

fn summary(counts: &Map<String, Value>) -> String {
    let of = |key: &str| counts.get(key).cloned().unwrap_or(Value::Null);
    format!(
        "{} footprints, {} pads ({} reached by their net), {} tracks, {} vias, {} zones, outline {}",
        of("footprints"),
        of("pads"),
        of("pads_reached"),
        of("tracks"),
        of("vias"),
        of("zones"),
        of("outline")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"boards": [{"name": "demo-board", "repo": "owner/repo.name", "commit": "0123456789abcdef0123456789abcdef01234567",
        "path": "Hardware/My Board.brd", "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "format": "eagle",
        "counts": {"tracks": 1}}]}"#;

    #[test]
    fn a_manifest_entry_is_read_and_its_fields_checked() {
        let found = entries(GOOD).expect("entries");
        let entry = found.first().expect("one board");
        assert_eq!((entry.name.as_str(), entry.repo.as_str(), entry.path.as_str()), ("demo-board", "owner/repo.name", "Hardware/My Board.brd"));
        assert_eq!(entry.counts.as_ref().and_then(|c| c.get("tracks")), Some(&json!(1)));
        assert_eq!(file_of(Path::new("/r"), entry), Path::new("/r/target/corpus/demo-board/board.brd"));
        assert_eq!(percent_encoded(&entry.path), "Hardware/My%20Board.brd");
        assert_eq!(percent_encoded("a/b?c#d%é"), "a/b%3Fc%23d%25%C3%A9");
        // Each field is in the form the manifest uses: nothing that changes the URL's host, leaves
        // the corpus folder or looks like an option.
        for (from, to) in [
            ("demo-board", "../demo"),
            ("demo-board", "--output"),
            ("owner/repo.name", "owner/repo/extra"),
            ("owner/repo.name", "owner@evil.example/repo"),
            ("owner/repo.name", "../repo"),
            ("0123456789abcdef0123456789abcdef01234567", "main"),
            ("Hardware/My Board.brd", "Hardware/../../secret"),
            ("Hardware/My Board.brd", "/etc/passwd"),
            ("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "E3B0"),
            ("\"eagle\"", "\"../x\""),
        ] {
            assert!(entries(&GOOD.replacen(from, to, 1)).is_err(), "{from} -> {to}");
        }
        let twice = GOOD.replacen("]}", ", {\"name\": \"demo-board\", \"repo\": \"a/b\", \"commit\": \"0123456789abcdef0123456789abcdef01234567\", \"path\": \"x\", \"sha256\": \"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\", \"format\": \"kicad\"}]}", 1);
        assert!(entries(&twice).is_err_and(|e| e.contains("twice")));
        assert!(entries("{}").is_err() && entries("[").is_err());
    }

    #[test]
    fn counts_are_compared_field_by_field() {
        let board = Board::new("x");
        let found = counts(&board);
        assert_eq!(found.get("copper"), Some(&json!(2)));
        assert_eq!(found.get("outline"), Some(&json!(false)));
        let mut expected = found.clone();
        expected.insert("tracks".to_owned(), json!(5));
        expected.remove("vias");
        assert_eq!(differences(&expected, &found), "tracks 0 (expected 5), vias 0 (expected null)");
        assert_eq!(differences(&found, &found), "");
        // A pad is reached when a track of its own net ends on it, wherever its footprint puts it.
        let text = "(kicad_pcb (version 20221018) (net 1 \"A\") (net 2 \"B\")
  (footprint \"a:b\" (layer \"B.Cu\") (at 10 10 90)
    (pad \"1\" smd rect (at 1 2 90) (size 1 1) (layers \"B.Cu\") (net 1 \"A\"))
    (pad \"2\" smd rect (at -1 2 90) (size 1 1) (layers \"B.Cu\") (net 2 \"B\"))
    (pad \"3\" smd rect (at 5 5 90) (size 1 1) (layers \"B.Cu\")))
  (segment (start 0 0) (end 12.4 9) (width 0.2) (layer \"B.Cu\") (net 1))
  (segment (start 0 0) (end 12 11) (width 0.2) (layer \"B.Cu\") (net 1))
  (via (at 30 30) (size 0.8) (drill 0.4) (layers \"F.Cu\" \"B.Cu\") (net 2)))";
        let board = circuitcraft_io::import_board(text, "x").expect("import").board;
        // Pad 1 is at (12, 9) on the board, half a millimetre across: the first track ends on
        // it. The second ends where pad 2 is, on another net; pad 2's own via is far away.
        assert_eq!((counts(&board).get("pads"), counts(&board).get("pads_reached")), (Some(&json!(3)), Some(&json!(1))));
        assert_eq!(hex(&Sha256::digest(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn the_repository_manifest_is_well_formed() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("corpus").join("manifest.json");
        let text = std::fs::read_to_string(&path).expect("corpus/manifest.json");
        let found = entries(&text).expect("a well-formed manifest");
        assert!(found.len() >= 8, "{} boards", found.len());
        assert!(found.iter().all(|e| e.counts.is_some()), "every board has reviewed counts");
        // Every board says under which licence it is published and who made it.
        let manifest: Value = serde_json::from_str(&text).expect("json");
        for board in manifest["boards"].as_array().expect("boards") {
            for field in ["license", "by", "format"] {
                assert!(board[field].as_str().is_some_and(|v| !v.is_empty()), "{} has no {field}", board["name"]);
            }
            assert!(board.get("render").is_some(), "{} has no place for its render's checksum", board["name"]);
        }
    }
}
